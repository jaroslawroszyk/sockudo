use super::ConnectionManager;
use crate::app::auth::AuthValidator;
use crate::app::config::AppConfig;
use crate::app::manager::AppManager;
use crate::channel::{ChannelType, PresenceMemberInfo};
use crate::connection::state::SocketId;
use crate::log::Log;
use crate::protocol::messages::{ErrorData, MessageData, PusherApiMessage, PusherMessage};
use crate::{channel::ChannelManager, error::Error};
use fastwebsockets::{upgrade, FragmentCollectorRead, Frame, OpCode, WebSocketError};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tracing_subscriber::fmt::format;

pub struct ConnectionHandler {
    pub(crate) app_manager: Arc<AppManager>,
    pub(crate) channel_manager: Arc<RwLock<ChannelManager>>,
    pub(crate) connection_manager: Arc<Mutex<ConnectionManager>>,
}

impl ConnectionHandler {
    pub fn new(
        app_manager: Arc<AppManager>,
        channel_manager: Arc<RwLock<ChannelManager>>,
        connection_manager: Arc<Mutex<ConnectionManager>>,
    ) -> Self {
        Self {
            app_manager,
            channel_manager,
            connection_manager,
        }
    }

    pub async fn handle_socket(
        &self,
        fut: upgrade::UpgradeFut,
        app_key: String,
    ) -> Result<(), WebSocketError> {
        // Validate app key
        let app = self
            .app_manager
            .get_app_by_key(&app_key)
            .ok_or_else(|| Error::InvalidAppKey)
            .unwrap();
        let socket = fut.await?;

        let (socket_rx, socket_tx) = socket.split(tokio::io::split);
        let socket_id = SocketId::new();
        {
            let mut connection_manager = self.connection_manager.lock().await;
            // Remove existing connections for this socket ID
            if connection_manager.get_connection(&socket_id, app.app_id.clone()).is_some() {
                Log::info(format!("Cleaning up existing connection for socket: {}", socket_id));
                connection_manager.remove_connection(&socket_id, &app.app_id);
            }

            // Only add new connection after cleanup
            connection_manager.add_connection(socket_id.clone(), socket_tx, &app.app_id);
        }

        self.send_connection_established(&app.app_id, &socket_id)
            .await
            .expect("msg");

        let mut socket_rx = FragmentCollectorRead::new(socket_rx);
        Log::info(format!("New connection established: {}", socket_id));
        loop {
            let frame = socket_rx
                .read_frame(&mut move |_| async { Ok::<_, WebSocketError>(()) })
                .await?;
            match frame.opcode {
                OpCode::Close => {
                    Log::info(format!("Closing socket: {}", socket_id));
                    match self.handle_disconnect(&app.app_id, &socket_id).await {
                        Ok(_) => {
                            Log::info(format!("Disconnected socket: {}", socket_id));
                        }
                        Err(e) => {
                            tracing::error!("Error handling disconnect: {}", e);
                        }
                    }
                    break;
                }
                OpCode::Text | OpCode::Binary => {
                    if let Err(e) = self.handle_message(frame, &socket_id, app.clone()).await {
                        tracing::error!("Error handling message: {}", e);
                    }
                }
                OpCode::Ping => {
                    if let Some(conn) = self
                        .connection_manager
                        .lock()
                        .await
                        .get_connection(&socket_id, app.clone().app_id)
                    {
                        let mut conn = conn.lock().await;
                        conn.state.update_ping();
                    }
                }
                _ => {
                    tracing::warn!("Unsupported opcode: {:?}", frame.opcode);
                    continue;
                }
            }
        }
        Ok(())
    }

    pub async fn handle_message(
        &self,
        frame: Frame<'static>,
        socket_id: &SocketId,
        app: AppConfig,
    ) -> crate::error::Result<()> {
        let msg = String::from_utf8(frame.payload.to_vec()).expect("Eroare");
        let message: PusherMessage = serde_json::from_str(&msg)?;
        let event = match message.clone().event {
            Some(e) => e,
            None => {
                return Err(Error::InvalidEventName("Event name is required".into()));
            }
        };
        match event.as_str() {
            "pusher:ping" => match self.handle_ping(&app.app_id, socket_id).await {
                Ok(_) => {}
                Err(e) => {
                    self.send_error(&app.app_id, socket_id, e, message.channel)
                        .await
                        .expect("Failed to send error message");
                    self.connection_manager
                        .lock()
                        .await
                        .cleanup_connection(&app.app_id, socket_id)
                        .await;
                }
            },
            "pusher:subscribe" => {
                match self
                    .handle_subscribe(socket_id, &app.app_id, message.clone())
                    .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        self.send_error(&app.app_id, socket_id, e, message.channel)
                            .await
                            .expect("Failed to send error message");
                        self.connection_manager
                            .lock()
                            .await
                            .cleanup_connection(&app.app_id, socket_id)
                            .await;
                    }
                }
            }
            "pusher:unsubscribe" => {
                match self
                    .handle_unsubscribe(socket_id, message.clone(), &app.app_id)
                    .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        self.send_error(&app.app_id, socket_id, e, message.channel)
                            .await
                            .expect("Failed to send error message");
                        self.connection_manager
                            .lock()
                            .await
                            .cleanup_connection(&app.app_id, socket_id)
                            .await;
                    }
                }
            }
            "pusher:signin" => match self
                .handle_signin(socket_id, message.clone(), app.clone())
                .await
            {
                Ok(_) => {}
                Err(e) => {

                    self.send_error(&app.app_id, socket_id, e, Some(message.channel.unwrap()))
                        .await
                        .expect("Failed to send error message");
                    let mut connection_manager = self.connection_manager.lock().await;
                    connection_manager
                        .cleanup_connection(&app.app_id, socket_id)
                        .await;
                }
            },
            _ => {
                if event.starts_with("client-") {
                    Log::info("Handling client event");
                    match self
                        .handle_client_event(
                            &app.app_id,
                            socket_id,
                            message.clone().event.unwrap(),
                            message.clone().channel,
                            serde_json::to_value(message.clone().data).unwrap(),
                        )
                        .await
                    {
                        Ok(_) => {}
                        Err(e) => {
                            Log::error(&format!("Failed to broadcast: {:?}", e));
                            self.send_error(&app.app_id, socket_id, e, message.channel)
                                .await
                                .expect("Failed to send error message");
                            self.connection_manager
                                .lock()
                                .await
                                .cleanup_connection(&app.app_id, socket_id)
                                .await;
                        }
                    }
                } else {
                    Log::warning(json!({
                        "info": "'Message event handler not implemented.",
                        "event": event,
                    }));
                }
            }
        }

        Ok(())
    }

    pub async fn handle_ping(
        &self,
        app_id: &str,
        socket_id: &SocketId,
    ) -> crate::error::Result<()> {
        // if let Some(conn) = self.connection_manager.get_connection(socket_id) {
        //     let mut conn = conn.lock().await;
        //     conn.state.update_ping();
        // }
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
            .await?;
        Ok(())
    }

    pub async fn handle_subscribe(
        &self,
        socket_id: &SocketId,
        app_id: &str,
        message: PusherMessage,
    ) -> crate::error::Result<()> {
        Log::info("Handling subscription started");
        // Extract channel first
        let channel = match message.clone().data.unwrap() {
            MessageData::String(data) => data,
            MessageData::Structured { channel, .. } => channel.unwrap(),
            MessageData::Json(data) => data.get("channel").unwrap().as_str().unwrap().to_string(),
        };

        let app = self
            .app_manager
            .get_app(app_id)
            .ok_or_else(|| Error::InvalidAppKey)?;

        // Handle authentication
        let is_authenticated = {
            let channel_manager = self.channel_manager.read().await; // Use read lock for validation
            let signature = match message.clone().data.unwrap() {
                MessageData::String(sig) => sig,
                MessageData::Json(data) => data
                    .get("auth")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                MessageData::Structured { extra, .. } => extra
                    .get("auth")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
            };

            // Early return for unauthenticated private/presence channels
            if (channel.starts_with("presence-") || channel.starts_with("private-"))
                && signature.is_empty()
            {
                return Err(Error::AuthError("Authentication required".into()));
            }

            channel_manager.signature_is_valid(app.clone(), socket_id, &signature, message.clone())
        };

        // Subscribe to channel
        let subscription_result = {
            let channel_manager = self.channel_manager.write().await;
            match channel_manager
                .subscribe(
                    socket_id.0.as_str(),
                    &message,
                    &channel,
                    is_authenticated,
                    app_id,
                )
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    Log::error(format!("Error subscribing to channel: {:?}", e));
                    return Err(Error::ChannelError("Failed to subscribe".into()));
                }
            }
        };

        if !subscription_result.success {
            return self
                .send_error(
                    app_id,
                    socket_id,
                    Error::AuthError("Invalid authentication signature".into()),
                    Some(channel),
                )
                .await;
        }
        // Add to connection's subscribed channels
        {
            let mut connection_manager = self.connection_manager.lock().await;
            if let Some(conn) =
                connection_manager.get_connection(socket_id, app_id.parse().unwrap())
            {
                let mut conn_guard = conn.lock().await;
                conn_guard.state.subscribed_channels.insert(channel.clone());

                if ChannelType::from_name(&channel) == ChannelType::Presence {
                    let presence = subscription_result.member.clone().unwrap();
                    conn_guard.state.user_id = Option::from(presence.user_id.clone());
                    let presence_info = PresenceMemberInfo {
                        user_id: presence.user_id.clone(),
                        user_info: Some(presence.user_info.clone()),
                    };

                    if let Some(ref mut presence_map) = conn_guard.state.presence {
                        // If map exists, just insert new info
                        presence_map.insert(channel.clone(), presence_info);
                    } else {
                        // If no map exists (None), create new HashMap and insert first entry
                        let mut new_presence_map = HashMap::new();
                        new_presence_map.insert(channel.clone(), presence_info);
                        conn_guard.state.presence = Some(new_presence_map);
                    }
                }
               //  connection_manager.add_socket_to_channel(app_id, &channel, socket_id);
            }
        }

        // Handle presence channel
        if ChannelType::from_name(&channel) == ChannelType::Presence {
            if let Some(presence) = subscription_result.member {
                let user_id = presence.user_id.clone();
                let presence_info = PresenceMemberInfo {
                    user_id: presence.user_id,
                    user_info: Some(presence.user_info),
                };

                // Handle presence data in a single lock scope
                let members = {
                    let mut connection_manager = self.connection_manager.lock().await;

                    // Add presence member
                    // connection_manager.add_presence_member(
                    //     &channel,
                    //     &user_id,
                    //     presence_info.clone(),
                    //     app_id,
                    // );

                    // Get current members
                    Log::info(format!("Presence members on line 387:"));
                    let members = connection_manager.get_channel_members(&channel, app_id).await?;


                    // Always broadcast member_added to other subscribers
                    let member_added = PusherMessage {
                        channel: Some(channel.clone()),
                        name: None,
                        event: Some("pusher_internal:member_added".to_string()),
                        data: Some(MessageData::Json(serde_json::to_value(
                            presence_info.clone(),
                        )?)),
                    };

                    // Broadcast to all other subscribers
                    connection_manager
                        .broadcast(&channel, member_added, Some(socket_id), app_id)
                        .await?;

                    members
                };

                let presence_message = json!({
                    "presence": {
                        "ids": members.keys().collect::<Vec<&String>>(),
                        "hash": members.iter()
                            .map(|(k, v)| (k.clone(), v.user_info.clone()))
                            .collect::<HashMap<String, Option<Value>>>(),
                        "count": members.len()
                    }
                });

                let subscription_succeeded = PusherMessage {
                    channel: Some(channel.clone()),
                    name: None,
                    event: Some("pusher_internal:subscription_succeeded".to_string()),
                    data: Some(MessageData::Json(serde_json::to_value(presence_message)?)),
                };

                match self
                    .connection_manager
                    .lock()
                    .await
                    .send_message(app_id, socket_id, subscription_succeeded)
                    .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        Log::error(&format!("Failed to send presence message: {:?}", e));
                    }
                }
            }
        } else {
            // Regular channel subscription response
            let response = PusherMessage {
                channel: Some(channel.clone()),
                name: None,
                event: Some("pusher:pusher_internal:subscription_succeeded".to_string()),
                data: Some(MessageData::String("".to_string())),
            };

            match self
                .connection_manager
                .lock()
                .await
                .send_message(app_id, socket_id, response)
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    Log::error(&format!("Failed to send subscription response: {:?}", e));
                }
            }
        }

        Log::success("Subscription completed successfully");
        Ok(())
    }

    async fn handle_unsubscribe(
        &self,
        socket_id: &SocketId,
        message: PusherMessage,
        app_id: &str,
    ) -> crate::error::Result<()> {
        let channel_name = message.channel.unwrap();
        let channel_type = ChannelType::from_name(&channel_name);

        // Get all required locks at once
        let channel_manager = self.channel_manager.write().await;
        let mut conn_manager = self.connection_manager.lock().await;

        // Handle unsubscribe

        // Handle presence channel specific logic
        if let ChannelType::Presence = channel_type {
            let member = conn_manager.get_presence_member(&channel_name, app_id, socket_id).await;

            if let Some(member) = member {
                // // Update connection manager state
                // conn_manager.remove_socket_from_channel(app_id, &channel_name, socket_id);
                // conn_manager.remove_presence_member(&channel_name, &member.user_id, app_id);
                match channel_manager
                    .unsubscribe(
                        socket_id.0.as_str(),
                        &channel_name,
                        app_id,
                        Some(&member.user_id),
                    )
                    .await
                {
                    Ok(_) => Log::success("Unsubscribed successfully"),
                    Err(e) => Log::error(format!("Error unsubscribing: {:?}", e)),
                }

                // Update connection state if it exists
                if let Some(conn) = conn_manager.get_connection(socket_id, app_id.parse().unwrap())
                {
                    let mut conn = conn.lock().await;
                    if let Some(presence) = conn.state.presence.as_mut() {
                        presence.remove(&channel_name);
                    }
                }

                // Broadcast member removal
                let member_removed = PusherMessage {
                    channel: Some(channel_name.clone()),
                    name: None,
                    event: Some("pusher_internal:member_removed".to_string()),
                    data: Some(MessageData::Json(json!({
                        "user_id": member.user_id,
                    }))),
                };

                match conn_manager
                    .broadcast(&channel_name, member_removed, Some(socket_id), app_id)
                    .await
                {
                    Ok(_) => Log::success("Broadcasted member_removed"),
                    Err(e) => Log::error(&format!("Error broadcasting member_removed: {:?}", e)),
                }
            }
        } else {
            match channel_manager
                .unsubscribe(socket_id.0.as_str(), &channel_name, app_id, None)
                .await
            {
                Ok(_) => Log::success("Unsubscribed successfully"),
                Err(e) => Log::error(format!("Error unsubscribing: {:?}", e)),
            }
        }

        Ok(())
    }

    async fn handle_signin(
        &self,
        socket_id: &SocketId,
        data: PusherMessage,
        app: AppConfig,
    ) -> Result<(), Error> {
        let (user_data, auth) = {
            let message_data = data
                .data
                .ok_or_else(|| Error::AuthError("Missing data in signin message".into()))?;

            let extract_field = |field: &str| -> Result<String, Error> {
                match &message_data {
                    MessageData::String(data) => Ok(data.clone()),
                    MessageData::Json(data) => data
                        .get(field)
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .ok_or_else(|| Error::AuthError(format!("Missing {} field", field))),
                    MessageData::Structured { extra, .. } => extra
                        .get(field)
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .ok_or_else(|| Error::AuthError(format!("Missing {} field", field))),
                }
            };

            (extract_field("user_data")?, extract_field("auth")?)
        };

        // Parse user data once
        let user_info: Value = serde_json::from_str(&user_data)
            .map_err(|e| Error::AuthError(format!("Invalid user data: {}", e)))?;

        // Validate auth
        let auth_validator = AuthValidator::new(self.app_manager.clone());
        let is_valid = auth_validator
            .validate_channel_auth(socket_id.clone(), &app.key, user_data.clone(), auth)
            .await?;

        if !is_valid {
            return Err(Error::AuthError("Invalid authentication".into()));
        }

        // Update connection in a single lock scope
        {
            let mut connection_manager = self.connection_manager.lock().await;

            let connection = connection_manager
                .get_connection(socket_id, app.app_id.parse().unwrap())
                .ok_or_else(|| Error::ConnectionNotFound)?;

            // Update user info
            {
                let mut conn = connection.lock().await;
                conn.state.user = Some(user_info.clone());
            }
        }

        // Send success message
        let success_message = PusherMessage {
            channel: None,
            name: None,
            event: Some("pusher:signin_success".to_string()),
            data: Some(MessageData::Json(user_info)),
        };

        let mut connection_manager = self.connection_manager.lock().await;
        connection_manager
            .send_message(&app.app_id, socket_id, success_message)
            .await?;

        Log::success(format!("Signin successful for socket: {}", socket_id));
        Ok(())
    }

    pub async fn handle_client_event(
        &self,
        app_id: &str,
        socket_id: &SocketId,
        event: String,
        channel: Option<String>,
        data: Value,
    ) -> crate::error::Result<()> {
        Log::info(format!(
            "Handling client event '{}' for socket: {}",
            event, socket_id
        ));
        let channel_name = channel.clone()
            .ok_or_else(|| Error::ClientEventError("Channel name is required".into()))?;

        // Validate event name format
        if !event.starts_with("client-") {
            return Err(Error::InvalidEventName(
                "Client events must start with 'client-'".into(),
            ));
        }

        // Validate channel type first to fail fast
        let channel_type = ChannelType::from_name(&channel_name);
        if !matches!(channel_type, ChannelType::Private | ChannelType::Presence) {
            return Err(Error::ClientEventError(
                "Client events can only be sent to private or presence channels".into(),
            ));
        }

        // Get connection and verify client events permission with minimal lock time
        let app_key = {
            let mut connection_manager = self.connection_manager.lock().await;
            let connection = connection_manager
                .get_connection(socket_id, app_id.parse().unwrap())
                .ok_or_else(|| Error::ConnectionNotFound)?;
            let conn = connection.lock().await;
            conn.state.app_key.clone()
        };

        // Verify client events are enabled
        if !self.app_manager.can_handle_client_events(&app_key) {
            return Err(Error::ClientEventError(
                "Client events are not enabled for this app".into(),
            ));
        }

        // Verify channel subscription with minimal lock time
        {
            let mut connection_manager = self.connection_manager.lock().await;
            if !connection_manager.is_in_channel(app_id, &channel_name, socket_id) {
                return Err(Error::ClientEventError(format!(
                    "Client {} is not subscribed to channel {}",
                    socket_id,
                    channel_name
                )));
            }
        }

        // Prepare message for broadcast
        let message = PusherMessage {
            channel: Some(channel_name.clone()),
            name: None,
            event: Some(event.clone()),
            data: Some(MessageData::Json(data)),
        };

        // Broadcast message
        let broadcast_result = {
            let mut connection_manager = self.connection_manager.lock().await;
            connection_manager
                .broadcast(&channel_name, message, Some(socket_id), app_id)
                .await
        };

        // Handle broadcast result
        match broadcast_result {
            Ok(_) => {
                Log::info(format!(
                    "Successfully broadcast client event '{}' to channel '{}'",
                    event,
                    channel_name
                ));
                Ok(())
            }
            Err(e) => {
                Log::error(format!(
                    "Failed to broadcast client event '{}' to channel '{}': {}",
                    event,
                    channel_name,
                    e
                ));
                Err(e)
            }
        }
    }

    async fn send_error(
        &self,
        app_id: &str,
        socket_id: &SocketId,
        error: Error,
        channel: Option<String>,
    ) -> crate::error::Result<()> {
        let error = ErrorData {
            message: error.to_string(),
            code: Some(error.close_code()),
        };
        let message = PusherMessage {
            channel: Some(channel.unwrap().to_string()),
            name: None,
            event: Some("pusher:error".to_string()),
            data: Some(MessageData::Json(serde_json::to_value(error)?)),
        };

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
    ) -> crate::error::Result<()> {
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
    ) -> Result<(), Box<dyn std::error::Error>> {
        Log::info(format!(
            "=== Starting disconnect sequence for socket: {} ===",
            socket_id
        ));

        // Get connection and data with minimal lock time
        let (subscription_channels, user_id) = {
            let mut connection_manager = self.connection_manager.lock().await;
            let connection = match connection_manager.get_connection(socket_id, app_id.parse()?) {
                Some(conn) => conn,
                None => {
                    Log::warning(format!("No connection found for socket: {}", socket_id));
                    return Ok(());
                }
            };

            // Get data we need while holding the lock
            let conn = connection.lock().await;
            (
                conn.state.subscribed_channels.clone(),
                conn.state.user_id.clone(),
            )
        };

        // Process each channel subscription
        if !subscription_channels.is_empty() {
            Log::info(format!(
                "Processing {} channels for disconnecting socket: {}",
                subscription_channels.len(),
                socket_id
            ));

            // Take write lock on channel manager once for all operations
            let channel_manager = self.channel_manager.write().await;

            for channel in subscription_channels {
                Log::info(format!("Processing unsubscribe for channel: {}", channel));

                // Handle unsubscribe for this channel
                match channel_manager
                    .unsubscribe(socket_id.0.as_str(), &channel, app_id, user_id.as_deref())
                    .await
                {
                    Ok(response) => {
                        if !response.left {
                            Log::warning(format!(
                                "Socket {} was already removed from channel {}",
                                socket_id, channel
                            ));
                            continue;
                        }

                        // Only handle presence channel specific logic
                        if channel.starts_with("presence-") {
                            if let Some(ref user_id) = user_id {
                                // Check members with minimal lock time
                                let should_broadcast = {
                                    let mut connection_manager =
                                        self.connection_manager.lock().await;
                                    Log::info(format!("Presence members on line 818"));
                                    let members = connection_manager
                                        .get_channel_members(app_id, &channel)
                                        .await?;

                                    !members.contains_key(user_id)
                                };

                                if should_broadcast {
                                    let message = PusherMessage {
                                        channel: Some(channel.clone()),
                                        name: None,
                                        event: Some("pusher_internal:member_removed".to_string()),
                                        data: Some(MessageData::Json(json!({
                                            "user_id": user_id,
                                        }))),
                                    };

                                    // Broadcast with minimal lock time
                                    {
                                        let mut connection_manager =
                                            self.connection_manager.lock().await;
                                        connection_manager
                                            .broadcast(&channel, message, Some(socket_id), app_id)
                                            .await
                                            .map_err(|e| {
                                                Log::error(format!(
                                                    "Failed to broadcast member_removed for channel {}: {}",
                                                    channel, e
                                                ));
                                                e
                                            })?;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        Log::error(format!(
                            "Failed to unsubscribe socket {} from channel {}: {}",
                            socket_id, channel, e
                        ));
                        // Continue processing other channels despite error
                    }
                }
            }
        }

        // Finally remove the connection
        {
            let mut connection_manager = self.connection_manager.lock().await;
            connection_manager.remove_connection(socket_id, app_id);
            Log::info(format!(
                "Successfully removed connection for socket: {}",
                socket_id
            ));
        }

        Log::info(format!(
            "=== Completed disconnect sequence for socket: {} ===",
            socket_id
        ));
        Ok(())
    }
    pub async fn channel(&self, app_id: &str, channel_name: &str) -> Value {
        let socket_count = self
            .connection_manager
            .lock()
            .await
            .get_channel_socket_count(app_id, channel_name);
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
                serde_json::to_value(message.data).unwrap(),
            )),
            channel: Some(channel.to_string()),
            name: None,
        };
        match self
            .connection_manager
            .lock()
            .await
            .broadcast(channel, message, socket_id, app_id)
            .await
        {
            Ok(_) => {}
            Err(e) => {
                Log::error(format!("Failed to broadcast message: {:?}", e));
            }
        }
    }
}
