use super::ConnectionManager;
use crate::app::manager::AppManager;
use crate::channel::{ChannelType, PresenceMemberInfo};
use crate::connection::state::SocketId;
use crate::log::Log;
use crate::{
    channel::ChannelManager,
    error::Error,
    protocol::{
        constants::{ACTIVITY_TIMEOUT, PONG_TIMEOUT},
    },
};
use axum::http::response;
use fastwebsockets::{
    upgrade, FragmentCollector, FragmentCollectorRead, Frame, OpCode, Payload, WebSocket,
    WebSocketError,
};
use futures::{SinkExt, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;
use serde_json::{json, Value};
use tokio::io::AsyncBufReadExt;
use tokio::{
    sync::RwLock,
    time::{timeout, Duration},
};
use crate::protocol::messages::{MessageData, PusherApiMessage, PusherMessage};

pub struct ConnectionHandler {
    pub(crate) app_manager: Arc<AppManager>,
    pub(crate) channel_manager: Arc<RwLock<ChannelManager>>,
    pub(crate) connection_manager: Arc<ConnectionManager>,
}

impl ConnectionHandler {
    pub fn new(
        app_manager: Arc<AppManager>,
        channel_manager: Arc<RwLock<ChannelManager>>,
        connection_manager: Arc<ConnectionManager>,
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
            .ok_or_else(|| Error::InvalidAppKey).unwrap();
        let socket = fut.await?;

        let (socket_rx, socket_tx) = socket.split(tokio::io::split);
        let socket_id = SocketId::new();
        // Add connection to manager
        self
            .connection_manager
            .add_connection(socket_id.clone(), socket_tx);
        
        self.send_connection_established(&socket_id)
            .await
            .expect("msg");
        
        let mut socket_rx = FragmentCollectorRead::new(socket_rx);
        loop {
            let frame = socket_rx
                .read_frame(&mut move |_| async { Ok::<_, WebSocketError>(()) })
                .await?;
            match frame.opcode {
                OpCode::Close => break,
                OpCode::Text | OpCode::Binary => {
                    if let Err(e) = self.handle_message(frame, &socket_id, app.clone().app_id).await {
                        tracing::error!("Error handling message: {}", e);
                        self.send_error(&socket_id, e)
                            .await
                            .expect("Failed to send error message");
                    }
                }
                OpCode::Ping => {
                    if let Some(conn) = self.connection_manager.get_connection(&socket_id) {
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
        self.handle_disconnect(&socket_id).await;
        Ok(())
    }

    pub async fn handle_message(
        &self,
        frame: Frame<'static>,
        socket_id: &SocketId,
        app_id: String,
    ) -> crate::error::Result<()> {
        let msg = String::from_utf8(frame.payload.to_vec()).expect("Eroare");
        let message: PusherMessage = serde_json::from_str(&msg)?;
        Log::info(format!("Message: {:?}", message));

        match message.clone().event.unwrap().as_str() {
            "pusher:ping" => {
                self.handle_ping(socket_id).await?;
            }
            "pusher:subscribe" => {
                self.handle_subscribe(socket_id, &app_id, message).await?;
            }
            "pusher:unsubscribe" => {
                self.handle_unsubscribe(socket_id, message.clone()).await?;
            }
            // "pusher:unsubscribe" => {
            //     self.handle_signin(socket_id, message.clone()).await?;
            // }
            _ => {
                self.handle_client_event(socket_id, message.clone().event.unwrap(), message.channel, serde_json::to_value(message.data).unwrap()).await?;
                
            }
        }

        Ok(())
    }

    pub async fn handle_ping(&self, socket_id: &SocketId) -> crate::error::Result<()> {
        // if let Some(conn) = self.connection_manager.get_connection(socket_id) {
        //     let mut conn = conn.lock().await;
        //     conn.state.update_ping();
        // }
        self.connection_manager
            .send_message(socket_id, PusherMessage {
                channel: None,
                name: None,
                event: Some("pusher:pong".to_string()),
                data: None,
            }).await?;
        Ok(())
    }

    pub async fn handle_subscribe(
        &self,
        socket_id: &SocketId,
        app_id: &str,
        message: PusherMessage,
    ) -> crate::error::Result<()> {

        let channel_manager = self.app_manager
            .get_channel_manager(app_id)
            .ok_or_else(|| Error::InvalidAppKey)?;
    
    
        let channel = match message.clone().data.unwrap() {
            MessageData::String(data) => data,
            MessageData::Structured {
                channel,
                ..
            } => channel.unwrap(),
            MessageData::Json(data) =>  {
                // get channel from data
                data.get("channel").unwrap().as_str().unwrap().to_string()
            },
        };
        
        let channel_manager = channel_manager.write().await;
        let message_data = message.clone().data.unwrap();
        let app = self.app_manager.get_app(app_id).unwrap();
        let response = match message_data {
            MessageData::String(data) => {
                let passed_signature = data;
                let is_authenticated = channel_manager.signature_is_valid(app, socket_id, &passed_signature.to_string(), message.clone());
                channel_manager
                    .subscribe(socket_id.0.as_str(), &message, is_authenticated)
                    .await.expect("Failed to subscribe")
            },
            MessageData::Json(data) => {
                let passed_signature = data.get("auth").unwrap().as_str().unwrap();
                let is_authenticated = channel_manager.signature_is_valid(app, socket_id, &passed_signature.to_string(), message.clone());
                channel_manager
                    .subscribe(socket_id.0.as_str(), &message, is_authenticated)
                    .await.expect("Failed to subscribe")
            },
            MessageData::Structured {
                channel,
                extra, ..
            } => {
                let passed_signature = extra.get("auth").unwrap().as_str().unwrap();
                let is_authenticated = channel_manager.signature_is_valid(app, socket_id, &passed_signature.to_string(), message.clone());
                Log::info(format!("Is authenticated: {:?}", is_authenticated));
                channel_manager
                    .subscribe(socket_id.0.as_str(), &message, is_authenticated)
                    .await.expect("Failed to subscribe")
            }
        };

         // Add to subscribed channels first
        {
            let connection = self.connection_manager.get_connection(&socket_id).unwrap();
            let mut conn = connection.lock().await;
            conn.state.subscribed_channels.insert(channel.clone());
            Log::info(format!("Subscribed channels: {:?}", conn.state.subscribed_channels));
        } // Lock is released here
        
        if !response.success {
            self.send_error(socket_id, Error::AuthError("Invalid authentication signature".into()))
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
            self.connection_manager
                .channel_sockets
                .entry(channel.clone())
                .or_default()
                .insert(socket_id.clone());
        
            // Get members without any locks
            let mut members = self.connection_manager.get_presence_members(&channel);
            Log::info(format!("Members before add: {:?}", members));
        
            // Send member_added event
            if !members.contains_key(&socket_id.0) {
                Log::info("Member not found in members");
                let member_added = PusherMessage {
                    channel: Some(channel.clone()),
                    name: None,
                    event: Some("pusher_internal:member_added".to_string()),
                    data: Some(MessageData::Json(serde_json::to_value(presence_data.clone())?))
                };
                members.insert(socket_id.0.clone(), presence_data.clone());
                self.connection_manager.add_presence_member(&channel.clone(), &socket_id, presence_data.clone());
                // Send without holding any locks
                self.connection_manager.broadcast(&channel.clone(), member_added, None).await?;
             
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
                data: Some(MessageData::Json(serde_json::to_value(presence_message)?))
            };
        
            Log::info(format!("Sending subscription succeeded: {:?}", subscription_succeeded));
        
            // Send final message without any locks
            match self.connection_manager.send_message(&socket_id, subscription_succeeded).await {
                Ok(_) => Log::info("Subscription succeeded sent successfully"),
                Err(e) => Log::error(format!("Failed to send subscription succeeded: {:?}", e))
            }
        
            Log::info("Presence channel subscription completed");
            return Ok(());
        }
     
        
        let response =
            PusherMessage {
                channel: Option::from(channel),
                name: None,
                event:  Option::from("pusher_internal:subscription_succeeded".to_string()),
                data: None
            };
        //};
        
        self.connection_manager
            .send_message(socket_id, response)
            .await?;

        Ok(())
    }

    async fn handle_unsubscribe(
        &self,
        socket_id: &SocketId,
        message: PusherMessage,
    ) -> crate::error::Result<()> {
        let mut channel_manager = self.channel_manager.write().await;
        
        // let channel_type = ChannelType::from_name(&message.channel.unwrap());
        // let member_info = None;
        // if channel_type == ChannelType::Presence {
        //     channel_manager.get_member_info(&data.channel, socket_id)?
        // } else {
        //     None
        // };

        // Unsubscribe from channel
        channel_manager.unsubscribe(socket_id.0.clone().as_str(), &message.channel.unwrap());
        
        // if let (ChannelType::Presence, Some(member_info)) = (channel_type, member_info) {
        //     let member_removed = OutgoingMessage::MemberRemoved {
        //         channel: data.channel,
        //         data: MemberRemovedData {
        //             user_id: member_info.user_id,
        //         },
        //     };
        // 
        //     self.connection_manager
        //         .broadcast(&data.channel, member_removed, Some(socket_id))
        //         .await?;
        // }
        Ok(())
    }

    // async fn handle_signin(
    //     &self,
    //     socket_id: &SocketId,
    //     data: PusherMessage,
    // ) -> crate::error::Result<()> {
    //     // Validate signin auth
    //     // if !self
    //     //     .app_manager
    //     //     .validate_user_auth(socket_id, &data.auth)
    //     //     .await?
    //     // {
    //     //     return Err(Error::AuthError("Invalid signin authentication".into()));
    //     // }
    // 
    //     // Parse user data
    //     let user_data: serde_json::Value = serde_json::from_str(&data.user_data)
    //         .map_err(|_| Error::AuthError("Invalid user data format".into()))?;
    // 
    //     // Update connection state with user info
    //     if let Some(connection) = self.connection_manager.get_connection(socket_id) {
    //         let mut conn = connection.lock().await;
    //         if let Some(user_id) = user_data.get("user_id").and_then(|v| v.as_str()) {
    //             conn.state.user_id = Some(user_id.to_string());
    //         }
    //     }
    // 
    //     // Send signin success message
    //     let response = OutgoingMessage::SignInSuccess {
    //         data: SignInSuccessData {
    //             user_data: data.user_data,
    //         },
    //     };
    // 
    //     self.connection_manager
    //         .send_message(socket_id, response)
    //         .await?;
    //     Ok(())
    // }

    pub async fn handle_client_event(
        &self,
        socket_id: &SocketId,
        event: String,
        channel: Option<String>,
        data: serde_json::Value,
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
            .get_connection(socket_id)
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
            .broadcast(&channel.clone().unwrap_or_default(), message, Some(socket_id))
            .await?;

        Ok(())
    }

    async fn send_error(&self, socket_id: &SocketId, error: Error) -> crate::error::Result<()> {
        let message = PusherMessage {
            channel: None,
            name: None,
            event: Some("pusher:error".to_string()),
            data: Some(MessageData::from(error.to_string())),
        };

        self.connection_manager
            .send_message(socket_id, message)
            .await
    }

    async fn send_connection_established(&self, socket_id: &SocketId) -> crate::error::Result<()> {
        let message = PusherMessage::connection_established(socket_id.0.clone());
        self.connection_manager
            .send_message(socket_id, message)
            .await
    }

    async fn handle_disconnect(&self, socket_id: &SocketId) {
        // Remove from connection manager
        self.connection_manager.remove_connection(socket_id);

        // Get all subscribed channels before removing from channel manager
        let subscribed_channels =
            if let Some(connection) = self.connection_manager.get_connection(socket_id) {
                let conn = connection.lock().await;
                conn.state.subscribed_channels.clone()
            } else {
                HashSet::new()
            };

        // Remove from channel manager and handle presence channel cleanup
        let channel_manager = self.channel_manager.read().await;

        for channel in subscribed_channels {
            channel_manager.unsubscribe(&socket_id.to_string(), &channel);
        }
    }
    
}
