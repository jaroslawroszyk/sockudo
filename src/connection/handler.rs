use super::ConnectionManager;
use crate::app::auth::AuthValidator;
use crate::app::config::AppConfig;
use crate::app::manager::AppManager;
use crate::channel::{ChannelType, PresenceMemberInfo};
use crate::connection::state::SocketId;
use crate::log::Log;
use crate::protocol::messages::{MessageData, PusherApiMessage, PusherMessage};
use crate::{
    channel::ChannelManager,
    error::Error,
    protocol::constants::{ACTIVITY_TIMEOUT, PONG_TIMEOUT},
};
use axum::http::response;
use axum::response::IntoResponse;
use axum::Json;
use fastwebsockets::{
    upgrade, FragmentCollector, FragmentCollectorRead, Frame, OpCode, Payload, WebSocket,
    WebSocketError,
};
use futures::{SinkExt, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
use tokio::sync::Mutex;
use tokio::{
    sync::RwLock,
    time::{timeout, Duration},
};

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
        // Add connection to manager
        self.connection_manager.lock().await.add_connection(
            socket_id.clone(),
            socket_tx,
            &app.app_id.clone(),
        );

        self.send_connection_established(&app.app_id, &socket_id)
            .await
            .expect("msg");

        let mut socket_rx = FragmentCollectorRead::new(socket_rx);
        loop {
            let frame = socket_rx
                .read_frame(&mut move |_| async { Ok::<_, WebSocketError>(()) })
                .await?;
            match frame.opcode {
                OpCode::Close => {
                    Log::warning("Closing connection");
                    break;
                }
                OpCode::Text | OpCode::Binary => {
                    if let Err(e) = self.handle_message(frame, &socket_id, app.clone()).await {
                        tracing::error!("Error handling message: {}", e);
                        self.send_error(&app.app_id, &socket_id, e)
                            .await
                            .expect("Failed to send error message");
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

        // Cleanup
        self.handle_disconnect(&app.app_id, &socket_id).await;

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
        Log::info(format!("Message: {:?}", message));

        match message.clone().event.unwrap().as_str() {
            "pusher:ping" => {
                self.handle_ping(&app.app_id, socket_id).await?;
            }
            "pusher:subscribe" => {
                self.handle_subscribe(socket_id, &app.app_id, message)
                    .await?;
            }
            "pusher:unsubscribe" => {
                self.handle_unsubscribe(socket_id, message.clone(), &app.app_id)
                    .await?;
            }
            "pusher:signin" => {
                self.handle_signin(socket_id, message.clone(), app).await;
            }
            _ => {
                self.handle_client_event(
                    &*app.app_id,
                    socket_id,
                    message.clone().event.unwrap(),
                    message.channel,
                    serde_json::to_value(message.data).unwrap(),
                )
                .await?;
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
        let channel_manager = self
            .app_manager
            .get_channel_manager(app_id)
            .ok_or_else(|| Error::InvalidAppKey)?;

        let channel = match message.clone().data.unwrap() {
            MessageData::String(data) => data,
            MessageData::Structured { channel, .. } => channel.unwrap(),
            MessageData::Json(data) => {
                // get channel from data
                data.get("channel").unwrap().as_str().unwrap().to_string()
            }
        };

        let channel_manager = channel_manager.write().await;
        let message_data = message.clone().data.unwrap();
        let app = self.app_manager.get_app(app_id).unwrap();
        let response = match message_data {
            MessageData::String(data) => {
                let passed_signature = data;
                let is_authenticated = channel_manager.signature_is_valid(
                    app,
                    socket_id,
                    &passed_signature.to_string(),
                    message.clone(),
                );
                channel_manager
                    .subscribe(socket_id.0.as_str(), &message, is_authenticated)
                    .await
                    .expect("Failed to subscribe")
            }
            MessageData::Json(data) => {
                let passed_signature = data.get("auth").unwrap().as_str().unwrap();
                let is_authenticated = channel_manager.signature_is_valid(
                    app,
                    socket_id,
                    &passed_signature.to_string(),
                    message.clone(),
                );
                channel_manager
                    .subscribe(socket_id.0.as_str(), &message, is_authenticated)
                    .await
                    .expect("Failed to subscribe")
            }
            MessageData::Structured { channel, extra, .. } => {
                let passed_signature = extra.get("auth").unwrap().as_str().unwrap();
                let is_authenticated = channel_manager.signature_is_valid(
                    app,
                    socket_id,
                    &passed_signature.to_string(),
                    message.clone(),
                );
                Log::info(format!("Is authenticated: {:?}", is_authenticated));
                channel_manager
                    .subscribe(socket_id.0.as_str(), &message, is_authenticated)
                    .await
                    .expect("Failed to subscribe")
            }
        };

        // Add to subscribed channels first
        {
            let connection = self
                .connection_manager
                .lock()
                .await
                .get_connection(&socket_id, app_id.parse().unwrap())
                .unwrap();
            let mut conn = connection.lock().await;
            conn.state.subscribed_channels.insert(channel.clone());
            self.connection_manager.lock().await.add_socket_to_channel(
                app_id,
                channel.as_str(),
                socket_id,
            );
        } // Lock is released here

        if !response.success {
            self.send_error(
                app_id,
                socket_id,
                Error::AuthError("Invalid authentication signature".into()),
            )
            .await?;
            return Ok(());
        }

        if ChannelType::from_name(&channel) == ChannelType::Presence {
            let presence = response.member.unwrap();
            let presence_data = PresenceMemberInfo {
                user_id: presence.user_id,
                user_info: Option::from(presence.user_info),
            };

            // Release channel_manager lock by dropping it explicitly
            drop(channel_manager);

            // Add presence data first (no locks needed due to DashMap)

            // Add to channel sockets (also using DashMap)
            self.connection_manager.lock().await.add_channel_to_sockets(
                app_id,
                &channel,
                &socket_id.clone(),
            );

            // Get members without any locks
            let mut members = self
                .connection_manager
                .lock()
                .await
                .get_presence_members(&channel.clone(), app_id);
            Log::info(format!("Members before add: {:?}", members));

            // Send member_added event
            if !members.contains_key(&socket_id.0) {
                Log::info("Member not found in members");
                let member_added = PusherMessage {
                    channel: Some(channel.clone()),
                    name: None,
                    event: Some("pusher_internal:member_added".to_string()),
                    data: Some(MessageData::Json(serde_json::to_value(
                        presence_data.clone(),
                    )?)),
                };
                members.insert(socket_id.0.clone(), presence_data.clone());
                self.connection_manager.lock().await.add_presence_member(
                    &channel.clone(),
                    socket_id,
                    presence_data.clone(),
                    app_id,
                );
                // Send without holding any locks
                self.connection_manager
                    .lock()
                    .await
                    .broadcast(&channel.clone(), member_added, None, app_id)
                    .await?;
            }

            // Create and send subscription succeeded message
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

            Log::info(format!(
                "Sending subscription succeeded: {:?}",
                subscription_succeeded
            ));

            // Send final message without any locks
            match self
                .connection_manager
                .lock()
                .await
                .send_message(app_id, socket_id, subscription_succeeded)
                .await
            {
                Ok(_) => Log::info("Subscription succeeded sent successfully"),
                Err(e) => Log::error(format!("Failed to send subscription succeeded: {:?}", e)),
            }

            Log::info("Presence channel subscription completed");
            return Ok(());
        }

        let response = PusherMessage {
            channel: Option::from(channel),
            name: None,
            event: Option::from("pusher_internal:subscription_succeeded".to_string()),
            data: None,
        };
        //};

        self.connection_manager
            .lock()
            .await
            .send_message(app_id, socket_id, response)
            .await?;

        Ok(())
    }

    async fn handle_unsubscribe(
        &self,
        socket_id: &SocketId,
        message: PusherMessage,
        app_id: &str,
    ) -> crate::error::Result<()> {
        let channel_manager = self.channel_manager.write().await;
        let channel_name = message.clone().channel.unwrap();
        let channel_type = ChannelType::from_name(&channel_name.clone());
        let member = self.connection_manager.lock().await.get_presence_member(
            &channel_name.clone(),
            &socket_id,
            app_id,
        );

        // Unsubscribe from channel
        channel_manager.unsubscribe(socket_id.0.clone().as_str(), &channel_name.clone());

        if let ChannelType::Presence = channel_type {
            let member_removed = PusherMessage {
                channel: Some(channel_name.clone()),
                name: None,
                event: Some("pusher_internal:member_removed".to_string()),
                data: Some(MessageData::Json(json!({
                    "user_id": member.unwrap().user_id,
                }))),
            };
            self.connection_manager
                .lock()
                .await
                .broadcast(
                    channel_name.as_str(),
                    member_removed,
                    Option::from(socket_id),
                    app_id,
                )
                .await?;
        }
        Ok(())
    }

    async fn handle_signin(
        &self,
        socket_id: &SocketId,
        data: PusherMessage,
        app: AppConfig,
    ) -> Result<(), Error> {
        Log::info_title(format!("Processing signin for socket: {}", socket_id));

        // Extract user_data and auth once, with proper error handling
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
        // Validate client event
        // if !event.starts_with("client-") {
        //     return Err(Error::InvalidEventName(
        //         "Client events must start with 'client-'".into(),
        //     ));
        // }

        // Check if client events are enabled for this app
        let connection = self
            .connection_manager
            .lock()
            .await
            .get_connection(socket_id, app_id.parse().unwrap())
            .ok_or_else(|| Error::ConnectionNotFound)?;
        let conn = connection.lock().await;

        if !self
            .app_manager
            .can_handle_client_events(&conn.state.app_key)
        {
            return Err(Error::ClientEventError(
                "Client events are not enabled for this app".into(),
            ));
        }

        // Validate channel type (must be private or presence)
        // let channel_type = ChannelType::from_name(&channel);
        // if !matches!(channel_type, ChannelType::Private | ChannelType::Presence) {
        //     return Err(Error::ClientEventError(
        //         "Client events can only be sent to private or presence channels".into(),
        //     ));
        // }
        //
        // // Verify the client is subscribed to the channel
        // if !conn.state.is_subscribed(&channel) {
        //     return Err(Error::ClientEventError(
        //         "Client must be subscribed to channel to send events".into(),
        //     ));
        // }

        // Create the event message
        let message = PusherMessage {
            channel: channel.clone(),
            name: Some(event),
            event: None,
            data: Some(MessageData::Json(data)),
        };

        // Broadcast to all subscribers except sender
        self.connection_manager
            .lock()
            .await
            .broadcast(
                &channel.clone().unwrap_or_default(),
                message,
                Some(socket_id),
                app_id,
            )
            .await?;

        Ok(())
    }

    async fn send_error(
        &self,
        app_id: &str,
        socket_id: &SocketId,
        error: Error,
    ) -> crate::error::Result<()> {
        let message = PusherMessage {
            channel: None,
            name: None,
            event: Some("pusher:error".to_string()),
            data: Some(MessageData::from(error.to_string())),
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

    async fn handle_disconnect(&self, app_id: &str, socket_id: &SocketId) {
        Log::info(format!("Handling disconnect for socket: {:?}", socket_id));

        // First collect all the data we need
        let data = {
            let mut connection_manager = self.connection_manager.lock().await;
            if let Some(conn) = connection_manager.get_connection(socket_id, app_id.to_string()) {
                let conn_guard = conn.lock().await;
                conn_guard.state.subscribed_channels.clone()
            } else {
                HashSet::new()
            }
        };

        // If we have channels to process
        if let Some(channel_manager) = self.app_manager.get_channel_manager(app_id) {
            for channel in data.clone() {
                Log::info(format!("Unsubscribing {} from {}", socket_id, channel));

                // First handle presence channel cleanup if needed
                if ChannelType::from_name(&channel) == ChannelType::Presence {
                    // Get presence data first
                    let member_info = {
                        let mut connection_manager = self.connection_manager.lock().await;
                        connection_manager
                            .get_presence_members(&channel, app_id)
                            .get(&socket_id.0)
                            .cloned()
                    };

                    // If we have presence info, handle member removal
                    if let Some(member_info) = member_info {
                        let member_removed = PusherMessage {
                            channel: Some(channel.clone()),
                            name: None,
                            event: Some("pusher_internal:member_removed".to_string()),
                            data: Some(MessageData::Json(json!({
                                "user_id": member_info.user_id
                            }))),
                        };

                        // Broadcast in a separate lock scope
                        {
                            let mut connection_manager = self.connection_manager.lock().await;
                            if let Err(e) = connection_manager
                                .broadcast(&channel, member_removed, Some(socket_id), app_id)
                                .await
                            {
                                Log::error(format!("Failed to broadcast member removal: {}", e));
                            }
                        }
                    }
                }

                // Now do the unsubscribe in a separate sco

                // Remove from connection manager's tracking
            }
            let channel_manager = channel_manager.write().await;
            Log::info(format!("Mortii ma0-tii: {:?}", data));
            for channel in data {
                Log::info("Before unsubscribe");
                channel_manager.unsubscribe(socket_id.0.clone().as_str(), &channel);
                Log::info("After unsubscribe");
            }
        }

        // Finally remove the connection

        Log::success(format!("Successfully disconnected socket: {:?}", socket_id));
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
}
