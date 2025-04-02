use crate::adapter::adapter::Adapter;
use crate::app::auth::AuthValidator;
use crate::app::config::App;
use crate::app::manager::AppManager;
use crate::cache::manager::CacheManager;
use crate::channel::{ChannelType, PresenceMemberInfo};
use crate::log::Log;
use crate::protocol::messages::{ErrorData, MessageData, PusherApiMessage, PusherMessage};
use crate::websocket::{SocketId, WebSocketRef};
use crate::{channel::ChannelManager, error::{Error, Result}};
use fastwebsockets::{upgrade, FragmentCollectorRead, Frame, OpCode, WebSocketError};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::RwLock;

pub struct ConnectionHandler {
    pub(crate) app_manager: Arc<dyn AppManager + Send + Sync>,
    pub(crate) channel_manager: Arc<RwLock<ChannelManager>>,
    pub(crate) connection_manager: Arc<Mutex<Box<dyn Adapter + Send + Sync>>>,
    pub(crate) cache_manager: Arc<Mutex<dyn CacheManager + Send + Sync>>,
}

impl ConnectionHandler {
    pub fn new(
        app_manager: Arc<dyn AppManager + Send + Sync>,
        channel_manager: Arc<RwLock<ChannelManager>>,
        connection_manager: Arc<Mutex<Box<dyn Adapter + Send + Sync>>>,
        cache_manager: Arc<Mutex<dyn CacheManager + Send + Sync>>,
    ) -> Self {
        Self {
            app_manager,
            channel_manager,
            connection_manager,
            cache_manager,
        }
    }

    pub async fn send_missed_cache_if_exists(
        &self,
        app_id: &str,
        socket_id: &SocketId,
        channel: &str,
    ) -> Result<()> {
        let mut cache_manager = self.cache_manager.lock().await;
        let key = format!("app:{}:channel:{}:cache_miss", app_id, channel);
        let cache = cache_manager.get(key.as_str()).await?;
        match cache {
            Some(cache) => {
                let cache_message: PusherMessage = serde_json::from_str(&cache)?;
                self.connection_manager
                    .lock()
                    .await
                    .send_message(app_id, socket_id, cache_message)
                    .await?;
            }
            _ => {
                Log::info(format!("No missed cache for channel: {}", channel));
            }
        }
        Ok(())
    }

    pub async fn handle_socket(
        &self,
        fut: upgrade::UpgradeFut,
        app_key: String,
    ) -> Result<()> {
        // Get app by key - this needs to handle both sync and potentially async implementations
        let app = self.app_manager.get_app_by_key(&app_key).await.unwrap();
        if app.is_none() {
            return Err(Error::InvalidAppKey);
        }
        let app = app.unwrap();

        let socket = fut.await?;
        let (socket_rx, socket_tx) = socket.split(tokio::io::split);
        let socket_id = SocketId::new();
        Log::info(format!("New socket: {}", socket_id));

        // Handle adapter setup in a single lock scope
        {
            let mut connection_manager = self.connection_manager.lock().await;
            if let Some(conn) = connection_manager.get_connection(&socket_id, &app.id).await {
                connection_manager
                    .cleanup_connection(&app.id, WebSocketRef(conn))
                    .await;
            }
            connection_manager
                .add_socket(socket_id.clone(), socket_tx, &app.id, &self.app_manager)
                .await
                .map_err(|e| {
                    Log::error(format!("Failed to add socket: {}", e));
                    WebSocketError::ConnectionClosed
                })?;
        }

        if let Err(e) = self.send_connection_established(&app.id, &socket_id).await {
            self.send_error(&app.id, &socket_id, &e, None)
                .await
                .map_err(|e| {
                    Log::error(format!("Failed to send connection established: {}", e));
                    WebSocketError::ConnectionClosed
                })?;
            return Ok(());
        }

        let mut socket_rx = FragmentCollectorRead::new(socket_rx);

        while let Ok(frame) = socket_rx
            .read_frame(&mut move |_| async { Ok::<_, WebSocketError>(()) })
            .await
        {
            match frame.opcode {
                OpCode::Close => {
                    if let Err(e) = self.handle_disconnect(&app.id, &socket_id).await {
                        Log::error(format!("Disconnect error for socket {}: {}", socket_id, e));
                    }
                    break;
                }
                OpCode::Text | OpCode::Binary => {
                    if let Err(e) = self.handle_message(frame, &socket_id, app.clone()).await {
                        Log::error(format!(
                            "Message handling error for socket {}: {}",
                            socket_id, e
                        ));
                    }
                }
                OpCode::Ping => {
                    let mut connection_manager = self.connection_manager.lock().await;
                    if let Some(conn) = connection_manager.get_connection(&socket_id, &app.id).await
                    {
                        let mut conn = conn.lock().await;
                        conn.state.update_ping();
                    }
                }
                _ => {
                    Log::warning(format!("Unsupported opcode: {:?}", frame.opcode));
                }
            }
        }

        Ok(())
    }

    pub async fn handle_message(
        &self,
        frame: Frame<'static>,
        socket_id: &SocketId,
        app: App,
    ) -> Result<()> {
        let msg = String::from_utf8(frame.payload.to_vec())
            .map_err(|e| Error::InvalidMessageFormat(format!("Invalid UTF-8: {}", e)))?;

        let message: PusherMessage = serde_json::from_str(&msg)
            .map_err(|e| Error::InvalidMessageFormat(format!("Invalid JSON: {}", e)))?;

        Log::info(format!("Received message: {:?}", message));

        // Extract values we need after the match before moving message
        let event = message
            .event
            .as_deref()
            .ok_or_else(|| Error::InvalidEventName("Event name is required".into()))?;
        let channel = message.channel.clone(); // Clone channel before potential move

        let result = match event {
            "pusher:ping" => self.handle_ping(&app.id, socket_id).await,
            "pusher:subscribe" => self.handle_subscribe(socket_id, &app.id, &message).await,
            "pusher:unsubscribe" => {
                let message = message.clone();
                self.handle_unsubscribe(socket_id, &message, &app.id).await
            }
            "pusher:signin" => {
                let message = message.clone();
                self.handle_signin(socket_id, message, &app).await
            }
            _ if event.starts_with("client-") => {
                self.handle_client_event(
                    &app.id,
                    socket_id,
                    event,
                    message.channel.as_deref(),
                    message
                        .data
                        .and_then(|d| serde_json::to_value(d).ok())
                        .unwrap_or_default(),
                )
                    .await
            }
            _ => Ok(()),
        };

        if let Err(e) = result {
            self.send_error(&app.id, socket_id, &e, channel).await?;

            let mut connection_manager = self.connection_manager.lock().await;
            if let Some(conn) = connection_manager.get_connection(socket_id, &app.id).await {
                connection_manager
                    .cleanup_connection(&app.id, WebSocketRef(conn))
                    .await;
            }

            return Err(Error::ClientEventError(format!(
                "Failed to handle event: {}, error: {}",
                event, e
            )));
        }

        Ok(())
    }

    pub async fn handle_ping(
        &self,
        app_id: &str,
        socket_id: &SocketId,
    ) -> Result<()> {
        self.connection_manager
            .lock()
            .await
            .send_message(
                app_id,
                socket_id,
                PusherMessage {
                    channel: None,
                    name: None,
                    event: Some("pusher:pong".to_string()),
                    data: None,
                },
            )
            .await
    }

    fn extract_signature(&self, message: &PusherMessage) -> Result<String> {
        match &message.data {
            Some(MessageData::String(sig)) => Ok(sig.to_string()),
            Some(MessageData::Json(data)) => Ok(data
                .get("auth")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()),
            Some(MessageData::Structured { extra, .. }) => Ok(extra
                .get("auth")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()),
            None => Ok(String::new()),
        }
    }

    pub async fn handle_subscribe(
        &self,
        socket_id: &SocketId,
        app_id: &str,
        message: &PusherMessage,
    ) -> Result<()> {
        // Extract channel without cloning
        let channel = match &message.data {
            Some(MessageData::String(data)) => data,
            Some(MessageData::Structured { channel, .. }) => channel
                .as_ref()
                .ok_or_else(|| Error::ChannelError("Missing channel".into()))?,
            Some(MessageData::Json(data)) => data
                .get("channel")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::ChannelError("Missing channel".into()))?,
            None => return Err(Error::ChannelError("Missing channel data".into())),
        };

        // Validate app
        let app = self.app_manager.get_app(app_id).await.unwrap();
        if app.is_none() {
            return Err(Error::InvalidAppKey);
        }
        
        // Validate authentication in a single read lock scope
        let is_authenticated = {
            let channel_manager = self.channel_manager.read().await;
            let signature = self.extract_signature(message)?;

            if (channel.starts_with("presence-") || channel.starts_with("private-"))
                && signature.is_empty()
            {
                return Err(Error::AuthError("Authentication required".into()));
            }

            channel_manager.signature_is_valid(app.unwrap(), socket_id, &signature, message.clone())
        };

        // Subscribe to channel with write lock
        let subscription_result = {
            let channel_manager = self.channel_manager.write().await;
            channel_manager
                .subscribe(
                    socket_id.0.as_str(),
                    message,
                    channel,
                    is_authenticated,
                    app_id,
                )
                .await
                .map_err(|e| {
                    Log::error(format!("Error subscribing to channel: {:?}", e));
                    Error::ChannelError("Failed to subscribe".into())
                })?
        };

        if !subscription_result.success {
            return self
                .send_error(
                    app_id,
                    socket_id,
                    &Error::AuthError("Invalid authentication signature".into()),
                    Some(channel.to_string()),
                )
                .await;
        }

        // Update adapter state with presence information if needed
        let channel_type = ChannelType::from_name(channel);
        let presence_data = if channel_type == ChannelType::Presence {
            subscription_result.member.as_ref().map(|presence| {
                (
                    presence.user_id.as_str(),
                    PresenceMemberInfo {
                        user_id: presence.user_id.clone(),
                        user_info: Some(presence.user_info.clone()),
                    },
                )
            })
        } else {
            None
        };

        // Update adapter state in a single lock scope
        {
            let mut connection_manager = self.connection_manager.lock().await;
            if let Some(conn) = connection_manager.get_connection(socket_id, app_id).await {
                let mut conn_guard = conn.lock().await;
                conn_guard
                    .state
                    .subscribed_channels
                    .insert(channel.to_string());

                if let Some((user_id, presence_info)) = presence_data {
                    conn_guard.state.user_id = Some(user_id.to_string());

                    if let Some(ref mut presence_map) = conn_guard.state.presence {
                        presence_map.insert(channel.to_string(), presence_info);
                    } else {
                        let mut new_presence_map = HashMap::new();
                        new_presence_map.insert(channel.to_string(), presence_info);
                        conn_guard.state.presence = Some(new_presence_map);
                    }
                }
            }
        }

        // Handle presence channel specific logic
        if channel_type == ChannelType::Presence {
            if let Some(presence) = subscription_result.member {
                let user_id = &presence.user_id;
                let presence_info = PresenceMemberInfo {
                    user_id: user_id.clone(),
                    user_info: Some(presence.user_info.clone()),
                };

                // Handle presence data and sending in a single lock scope
                let members = {
                    let mut connection_manager = self.connection_manager.lock().await;
                    let members = connection_manager
                        .get_channel_members(app_id, channel)
                        .await?;

                    let member_added = PusherMessage::member_added(
                        channel.to_string(),
                        user_id.clone(),
                        presence_info.user_info.clone(),
                    );

                    connection_manager
                        .send(channel, member_added, Some(socket_id), app_id)
                        .await?;

                    members
                };

                // Create presence message without unnecessary cloning
                let presence_message = json!({
                    "presence": {
                        "ids": members.keys().collect::<Vec<&String>>(),
                        "hash": members.iter()
                            .map(|(k, v)| (k.as_str(), v.user_info.clone()))
                            .collect::<HashMap<&str, Option<Value>>>(),
                        "count": members.len()
                    }
                });

                let subscription_succeeded = PusherMessage::subscription_succeeded(
                    channel.to_string(),
                    Some(presence_message),
                );

                self.connection_manager
                    .lock()
                    .await
                    .send_message(app_id, socket_id, subscription_succeeded)
                    .await
                    .map_err(|e| {
                        Log::error(format!("Failed to send presence message: {:?}", e));
                        e
                    })?;
            }
        } else {
            // Regular channel subscription response
            let response = PusherMessage::subscription_succeeded(channel.to_string(), None);
            self.connection_manager
                .lock()
                .await
                .send_message(app_id, socket_id, response)
                .await
                .map_err(|e| {
                    Log::error(format!("Failed to send subscription response: {:?}", e));
                    e
                })?;
        }

        // If we have a missed cache for this channel, send it
        self.send_missed_cache_if_exists(app_id, socket_id, channel).await?;

        Ok(())
    }

    async fn handle_unsubscribe(
        &self,
        socket_id: &SocketId,
        message: &PusherMessage,
        app_id: &str,
    ) -> Result<()> {
        let channel_name = message
            .channel
            .as_ref()
            .ok_or_else(|| Error::ChannelError("Channel name is required".into()))?;

        let channel_type = ChannelType::from_name(channel_name);

        match channel_type {
            ChannelType::Presence => {
                // Get presence member first to minimize lock time
                let member = {
                    let mut conn_manager = self.connection_manager.lock().await;
                    conn_manager
                        .get_presence_member(app_id, channel_name, socket_id)
                        .await
                };

                if let Some(member) = member {
                    // Handle unsubscribe
                    let channel_manager = self.channel_manager.write().await;
                    channel_manager
                        .unsubscribe(
                            socket_id.0.as_str(),
                            channel_name,
                            app_id,
                            Some(&member.user_id),
                        )
                        .await
                        .map_err(|e| {
                            Log::error(format!("Error unsubscribing: {:?}", e));
                            e
                        })?;

                    // Update adapter state
                    {
                        let mut conn_manager = self.connection_manager.lock().await;
                        if let Some(conn) = conn_manager.get_connection(socket_id, app_id).await {
                            let mut conn = conn.lock().await;
                            if let Some(presence) = conn.state.presence.as_mut() {
                                presence.remove(channel_name);
                            }
                            conn.state.subscribed_channels.remove(channel_name);
                        }

                        // send member removal within the same lock
                        let member_removed =
                            PusherMessage::member_removed(channel_name.to_string(), member.user_id);

                        conn_manager
                            .send(channel_name, member_removed, Some(socket_id), app_id)
                            .await
                            .map_err(|e| {
                                Log::error(format!("Error sending member_removed: {:?}", e));
                                e
                            })?;
                    }
                }
            }
            _ => {
                // Simple unsubscribe for non-presence channels
                let channel_manager = self.channel_manager.write().await;
                channel_manager
                    .unsubscribe(socket_id.0.as_str(), channel_name, app_id, None)
                    .await
                    .map_err(|e| {
                        Log::error(format!("Error unsubscribing: {:?}", e));
                        e
                    })?;
            }
        }

        Ok(())
    }

    async fn handle_signin(
        &self,
        socket_id: &SocketId,
        data: PusherMessage,
        app: &App,
    ) -> Result<()> {
        // Extract and validate message data
        let message_data = data
            .data
            .ok_or_else(|| Error::AuthError("Missing data in signin message".into()))?;

        // Extract fields efficiently
        let (user_data, auth) = {
            let extract_field = |field: &str| -> Result<&str> {
                match &message_data {
                    MessageData::String(data) => Ok(data.as_str()),
                    MessageData::Json(data) => data
                        .get(field)
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| Error::AuthError(format!("Missing {} field", field))),
                    MessageData::Structured { extra, .. } => extra
                        .get(field)
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| Error::AuthError(format!("Missing {} field", field))),
                }
            };
            (extract_field("user_data")?, extract_field("auth")?)
        };

        // Parse user data
        let user_info: Value = serde_json::from_str(user_data)
            .map_err(|e| Error::AuthError(format!("Invalid user data: {}", e)))?;

        // Validate auth
        let auth_validator = AuthValidator::new(self.app_manager.clone());
        let is_valid = auth_validator
            .validate_channel_auth(socket_id.clone(), &app.key, user_data, auth)
            .await?;

        if !is_valid {
            return Err(Error::AuthError("Connection not authorized.".into()));
        }

        // Lock connection manager once for all operations
        let mut connection_manager = self.connection_manager.lock().await;

        // Get existing connection
        let connection = connection_manager
            .get_connection(socket_id, &app.id)
            .await
            .ok_or_else(|| Error::ConnectionNotFound)?;

        {
            // Update user info in connection state and get socket
            let mut conn = connection.lock().await;
            conn.state.user = Some(user_info.clone());

            // Take the socket safely using Option::take
            let socket = conn
                .socket
                .take()
                .ok_or_else(|| Error::ConnectionError("Socket not found".into()))?;

            drop(conn);

            // Add socket
            connection_manager
                .add_socket(
                    socket_id.clone(),
                    socket,
                    app.id.as_str(),
                    &self.app_manager,
                )
                .await
                .map_err(|e| {
                    Log::error(format!("Failed to add socket after signin: {}", e));
                    Error::ConnectionError("Failed to add socket".into())
                })?;

            // Add user
            if let Err(e) = connection_manager.add_user(connection.clone()).await {
                Log::error(format!("Failed to add user: {}", e));
            }
        }

        // Send success message
        let success_message = PusherMessage {
            channel: None,
            name: None,
            event: Some("pusher:signin_success".into()),
            data: Some(MessageData::Json(user_info)),
        };

        connection_manager
            .send_message(&app.id, socket_id, success_message)
            .await?;

        Ok(())
    }

    async fn handle_client_event(
        &self,
        app_id: &str,
        socket_id: &SocketId,
        event: &str,
        channel: Option<&str>,
        data: Value,
    ) -> Result<()> {
        // Get channel name without cloning
        let channel_name =
            channel.ok_or_else(|| Error::ClientEventError("Channel name is required".into()))?;

        // Validate event name format - no need to clone string for starts_with check
        if !event.starts_with("client-") {
            return Err(Error::InvalidEventName(
                "Client events must start with 'client-'".into(),
            ));
        }

        // Validate channel type first to fail fast
        let channel_type = ChannelType::from_name(channel_name);
        if !matches!(channel_type, ChannelType::Private | ChannelType::Presence) {
            return Err(Error::ClientEventError(
                "Client events can only be sent to private or presence channels".into(),
            ));
        }

        // Get adapter and verify client events permission in a single lock scope
        let app_key = {
            let mut connection_manager = self.connection_manager.lock().await;
            let connection = connection_manager
                .get_connection(socket_id, app_id)
                .await
                .ok_or_else(|| Error::ConnectionNotFound)?;

            // Extract app key first while adapter is still valid
            let app_key = connection.lock().await.state.get_app_key();
            app_key // Return app_key from the block
        };

        // Verify client events are enabled
        if !self.app_manager.can_handle_client_events(&app_key).await.unwrap() {
            return Err(Error::ClientEventError(
                "Client events are not enabled for this app".into(),
            ));
        }

        // Get the local connection state before checking channel subscription
        // to understand what channels the client thinks they're subscribed to
        let subscribed_channels = {
            let mut connection_manager = self.connection_manager.lock().await;
            if let Some(connection) = connection_manager.get_connection(socket_id, app_id).await {
                let conn = connection.lock().await;
                conn.state.subscribed_channels.clone()
            } else {
                HashSet::new()
            }
        };

        // Log state for debugging
        Log::info(format!(
            "Socket {} subscribed channels: {:?}",
            socket_id, subscribed_channels
        ));
        Log::info(format!(
            "Checking if socket {} is in channel {}",
            socket_id, channel_name
        ));

        // Check if the client thinks they're subscribed to this channel
        if !subscribed_channels.contains(channel_name) {
            Log::warning(format!(
                "Socket {} not subscribed to {} in connection state",
                socket_id, channel_name
            ));
        }

        // Verify channel subscription with additional logging
        let is_subscribed = {
            let mut connection_manager = self.connection_manager.lock().await;
            connection_manager
                .is_in_channel(app_id, channel_name, socket_id)
                .await?
        };

        // If not subscribed, log and return error
        if !is_subscribed {
            // Check if there's a mismatch in the channel name
            for subscribed in &subscribed_channels {
                if subscribed.to_lowercase() == channel_name.to_lowercase() {
                    Log::warning(format!(
                        "Case mismatch between subscribed channel {} and requested channel {}",
                        subscribed, channel_name
                    ));
                }
            }

            return Err(Error::ClientEventError(format!(
                "Client {} is not subscribed to channel {}",
                socket_id, channel_name
            )));
        }

        // Prepare message for send - only clone strings when constructing the message
        let message = PusherMessage {
            channel: Some(channel_name.to_string()),
            name: None,
            event: Some(event.to_string()),
            data: Some(MessageData::Json(data)),
        };

        // send message in a single lock scope
        self.connection_manager
            .lock()
            .await
            .send(channel_name, message, Some(socket_id), app_id)
            .await
    }

    async fn send_error(
        &self,
        app_id: &str,
        socket_id: &SocketId,
        error: &Error,
        channel: Option<String>,
    ) -> Result<()> {
        let error = ErrorData {
            message: error.to_string(),
            code: Some(error.close_code()),
        };
        let message = PusherMessage::error(error.code.unwrap_or(4000), error.message, channel);
        self.connection_manager
            .lock()
            .await
            .send_message(app_id, socket_id, message)
            .await
    }

    async fn send_connection_established(
        &self,
        app_id: &str,
        socket_id: &SocketId,
    ) -> Result<()> {
        let message = PusherMessage::connection_established(socket_id.0.clone());
        self.connection_manager
            .lock()
            .await
            .send_message(app_id, socket_id, message)
            .await
    }

    async fn handle_disconnect(
        &self,
        app_id: &str,
        socket_id: &SocketId,
    ) -> Result<()> {
        // First, get all the data we need
        let (subscription_channels, user_id) = {
            let mut connection_manager = self.connection_manager.lock().await;
            let connection = match connection_manager.get_connection(socket_id, app_id).await {
                Some(conn) => conn,
                None => {
                    Log::warning(format!("No connection found for socket: {}", socket_id));
                    return Ok(());
                }
            };

            let conn = connection.lock().await;
            (
                conn.state.subscribed_channels.clone(),
                conn.state.user_id.clone(),
            )
        };

        // Process channel unsubscriptions first
        if !subscription_channels.is_empty() {
            Log::info(format!(
                "Processing {} channels for disconnecting socket: {}",
                subscription_channels.len(),
                socket_id
            ));

            let channel_manager = self.channel_manager.write().await;

            for channel in subscription_channels {
                Log::info(format!("Unsubscribing from channel: {}", channel));

                if let Err(e) = channel_manager
                    .unsubscribe(socket_id.0.as_str(), &channel, app_id, user_id.as_deref())
                    .await
                {
                    Log::error(format!("Error unsubscribing from channel {}: {}", channel, e));
                    continue;
                }

                // Handle presence channel logic
                if channel.starts_with("presence-") && user_id.is_some() {
                    let should_broadcast = {
                        let mut connection_manager = self.connection_manager.lock().await;
                        let members = connection_manager
                            .get_channel_members(app_id, &channel)
                            .await?;
                        !members.contains_key(user_id.as_ref().unwrap())
                    };

                    if should_broadcast {
                        let message = PusherMessage::member_removed(
                            channel.clone(),
                            user_id.clone().unwrap(),
                        );

                        let mut connection_manager = self.connection_manager.lock().await;
                        connection_manager
                            .send(&channel, message, Some(socket_id), app_id)
                            .await?;
                    }
                }
            }
        }

        // Only remove the connection after all channel processing is complete
        {
            let mut connection_manager = self.connection_manager.lock().await;
            let _ = connection_manager
                .remove_connection(socket_id, app_id)
                .await;
            Log::info(format!("Successfully removed connection for socket: {}", socket_id));
        }

        Ok(())
    }

    pub async fn channel(&self, app_id: &str, channel_name: &str) -> Value {
        let socket_count = self
            .connection_manager
            .lock()
            .await
            .get_channel_socket_count(app_id, channel_name)
            .await;
        let response = json!({
            "occupied": socket_count > 0,
            "subscription_count": socket_count,
        });

        response
    }

    pub async fn send_message(
        &self,
        app_id: &str,
        socket_id: Option<&SocketId>,
        message: PusherApiMessage,
        channel: &str,
    ) {
        let message = PusherMessage {
            event: message.name,
            data: Option::from(MessageData::Json(
                serde_json::to_value(message.data).unwrap_or(Value::Null),
            )),
            channel: Some(channel.to_string()),
            name: None,
        };

        match self
            .connection_manager
            .lock()
            .await
            .send(channel, message, socket_id, app_id)
            .await
        {
            Ok(_) => {
                Log::info(format!("Message sent to channel {} successfully", channel));
            }
            Err(e) => {
                Log::error(format!("Failed to send message to channel {}: {:?}", channel, e));
            }
        }
    }
}