use crate::channel::PresenceMemberInfo;
use crate::connection::state::{ConnectionState, SocketId};
use crate::error::{Error, Result};
use crate::log::Log;
use crate::protocol::messages::PusherMessage;
use dashmap::{DashMap, DashSet};
use fastwebsockets::{Frame, Payload, WebSocketWrite};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::io::WriteHalf;
use tokio::sync::{mpsc, Mutex};

// Combine all channel-related data into one structure
#[derive(Default, Clone)]
pub struct ChannelData {
    pub sockets: DashSet<SocketId>,
    pub presence_members: Option<DashMap<String, PresenceMemberInfo>>,
}

pub struct Connection {
    pub state: ConnectionState,
    pub socket: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
    pub message_sender: mpsc::UnboundedSender<Frame<'static>>,
}

pub struct Namespace {
    pub app_id: String,
    pub connections: DashMap<SocketId, Arc<Mutex<Connection>>>,
    pub channels: DashMap<String, ChannelData>,
    pub users: DashMap<String, HashSet<SocketId>>,
}

impl Namespace {
    pub fn new(app_id: String) -> Self {
        Self {
            app_id,
            connections: DashMap::new(),
            channels: DashMap::new(),
            users: DashMap::new(),
        }
    }

    pub fn add_connection(
        &self,
        socket_id: SocketId,
        socket: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
    ) {
        let (tx, mut rx) = mpsc::unbounded_channel();

        let connection = Connection {
            state: ConnectionState::new("demo-key".to_string()),
            socket,
            message_sender: tx,
        };

        let connection = Arc::new(Mutex::new(connection));
        self.connections
            .insert(socket_id.clone(), connection.clone());

        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let mut conn = connection.lock().await;
                if let Err(e) = conn.socket.write_frame(frame).await {
                    Log::error(format!("Failed to send message to {}: {}", socket_id, e));
                    break;
                }
            }
        });
    }

    pub fn get_connection(&self, socket_id: &SocketId) -> Option<Arc<Mutex<Connection>>> {
        self.connections.get(socket_id).map(|conn| conn.clone())
    }

    pub fn get_connection_from_channel(
        &self,
        channel: &str,
        socket_id: &SocketId,
    ) -> Option<Arc<Mutex<Connection>>> {
        if let Some(channel_data) = self.channels.get(channel) {
            if channel_data.sockets.contains(socket_id) {
                return self.get_connection(socket_id);
            }
        }
        None
    }

    pub async fn send_message(&self, socket_id: &SocketId, message: PusherMessage) -> Result<()> {
        if let Some(connection) = self.get_connection(socket_id) {
            let message = serde_json::to_string(&message)?;
            let frame = Frame::text(Payload::from(message.into_bytes()));

            let conn = connection.lock().await;
            conn.message_sender
                .send(frame)
                .map_err(|e| Error::ConnectionError(format!("Failed to send message: {}", e)))?;
        }
        Ok(())
    }

    pub async fn broadcast(
        &self,
        channel: &str,
        message: PusherMessage,
        except: Option<&SocketId>,
    ) -> Result<()> {
        let payload = serde_json::to_string(&message)?;

        if let Some(channel_data) = self.channels.get(channel) {
            // Create a vec to store failed socket_ids for potential cleanup
            let mut failed_sockets = Vec::new();

            // Iterate through the DashSet safely
            for socket_id in channel_data.sockets.iter() {
                // Skip if this is the socket we want to exclude
                if except.map_or(false, |sid| sid.0 == socket_id.0) {
                    continue;
                }

                // Try to send the message
                match self.get_connection(&socket_id) {
                    Some(connection) => {
                        let frame = Frame::text(Payload::from(payload.clone().into_bytes()));
                        if let Err(e) = connection.lock().await.message_sender.send(frame) {
                            Log::error(format!("Failed to send message to {}: {}", socket_id.0, e));
                            failed_sockets.push(socket_id.clone());
                        }
                    }
                    None => {
                        Log::warning(format!("Connection not found for socket {}", socket_id.0));
                        failed_sockets.push(socket_id.clone());
                    }
                }
            }

            // Cleanup failed sockets
            if !failed_sockets.is_empty() {
                for socket_id in failed_sockets {
                    Log::info(format!("Cleaning up failed socket: {}", socket_id));
                    channel_data.sockets.remove(&socket_id);
                    // You might want to trigger additional cleanup here
                }
            }
        }

        Ok(())
    }

    pub async fn get_channel_members(
        &self,
        _app_id: &str,
        channel: &str,
    ) -> Result<HashMap<String, PresenceMemberInfo>> {
        if let Some(channel_data) = self.channels.get(channel) {
            if let Some(presence_members) = &channel_data.presence_members {
                return Ok(presence_members
                    .iter()
                    .map(|entry| (entry.key().clone(), entry.value().clone()))
                    .collect());
            }
        }
        Ok(HashMap::new())
    }

    pub fn get_channel_sockets(&self, channel: &str) -> Vec<SocketId> {
        match self.channels.get(channel) {
            Some(channel_data) => {
                // DashSet::iter() gives us references, collect them into a Vec
                channel_data.sockets
                    .iter()
                    .map(|socket_id| socket_id.clone())
                    .collect()
            }
            None => Vec::new()
        }
    }

    pub fn add_presence_member(
        &self,
        channel: &str,
        user_id: &str,
        presence_data: PresenceMemberInfo,
    ) {
        let mut channel_data = self.channels.entry(channel.to_string()).or_default();
        if channel_data.presence_members.is_none() {
            channel_data.presence_members = Some(DashMap::new());
        }

        if let Some(presence_members) = &channel_data.presence_members {
            presence_members.insert(user_id.to_string(), presence_data);
        }
    }

    pub fn get_presence_members(&self, channel: &str) -> HashMap<String, PresenceMemberInfo> {
        self.channels
            .get(channel)
            .and_then(|channel_data| channel_data.presence_members.clone())
            .map(|presence_members| {
                presence_members
                    .iter()
                    .map(|entry| (entry.key().clone(), entry.value().clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_user_connections(&self, user_id: &str) -> Vec<SocketId> {
        self.users
            .get(user_id)
            .map(|sockets| sockets.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn cleanup_connection(&self, socket_id: &SocketId) {
        // Send disconnect message
        if let Some(connection) = self.get_connection(socket_id) {
            let message =
                PusherMessage::error(4009, "You got disconnected by the application".to_string());
            let mut conn = connection.lock().await;
            let _ = conn
                .socket
                .write_frame(Frame::text(Payload::from(
                    serde_json::to_string(&message).unwrap().into_bytes(),
                )))
                .await;
            let _ = conn.socket.write_frame(Frame::close(4009, &[])).await;
        }

        // Cleanup from channels
        let mut empty_channels = Vec::new();
        for mut channel_entry in self.channels.iter_mut() {
            let channel_data = channel_entry.value_mut();
            channel_data.sockets.remove(socket_id);

            if channel_data.sockets.is_empty() {
                empty_channels.push(channel_entry.key().clone());
            }
        }

        // Remove empty channels
        for channel in empty_channels {
            self.channels.remove(&channel);
        }

        // Cleanup from users
        let mut empty_users = Vec::new();
        for mut user_entry in self.users.iter_mut() {
            let sockets = user_entry.value_mut();
            sockets.remove(socket_id);

            if sockets.is_empty() {
                empty_users.push(user_entry.key().clone());
            }
        }

        // Remove empty users
        for user in empty_users {
            self.users.remove(&user);
        }

        // Remove connection
        self.connections.remove(socket_id);
    }

    pub async fn terminate_connection(&self, user_id: &str) -> Result<()> {
        let socket_ids = self.get_user_connections(user_id);
        for socket_id in socket_ids {
            self.cleanup_connection(&socket_id).await;
        }
        Ok(())
    }

    pub fn add_channel_to_socket(&self, channel: &str, socket_id: &SocketId) {
        self.channels
            .entry(channel.to_string())
            .or_default()
            .sockets
            .insert(socket_id.clone());
    }

    pub fn remove_channel_from_socket(&self, channel: &str, socket_id: &SocketId) {
        if let Some(mut channel_data) = self.channels.get_mut(channel) {
            channel_data.sockets.remove(socket_id);
            if channel_data.sockets.is_empty() {
                self.channels.remove(channel);
            }
        }
    }

    pub fn remove_presence_member(&self, channel: &str, user_id: &str) {
        Log::warning(format!("Removing presence member: {}, {}", channel, user_id).as_str());
        if let Some(channel_data) = self.channels.get_mut(channel) {
            if let Some(presence_members) = &channel_data.presence_members {
                presence_members.remove(user_id);
            }
        }
    }

    pub fn get_presence_member(&self, channel: &str, user_id: &str) -> Option<PresenceMemberInfo> {
        self.channels.get(channel).and_then(|channel_data| {
            channel_data
                .presence_members
                .clone()
                .and_then(|members| members.get(user_id).map(|info| info.clone()))
        })
    }

    pub fn remove_connection(&self, socket_id: &SocketId) {
        self.connections.remove(socket_id);
    }

    pub fn get_channel(&self, channel: &str) -> Option<ChannelData> {
        if let Some(channel_data) = self.channels.get(channel) {
            Some(channel_data.clone())
        } else {
            self.channels.insert(
                channel.to_string(),
                ChannelData {
                    sockets: DashSet::new(),
                    presence_members: None,
                },
            );
            Some(self.channels.get(channel).unwrap().clone())
        }
    }

    pub fn remove_channel(&self, channel: &str) {
        self.channels.remove(channel);
    }

    pub fn is_in_channel(&self, channel: &str, socket_id: &SocketId) -> bool {
        self.channels
            .get(channel)
            .map(|channel_data| channel_data.sockets.contains(socket_id))
            .unwrap_or(false)
    }
}
