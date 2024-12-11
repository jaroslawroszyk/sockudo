use crate::app::manager::AppManager;
use crate::channel::PresenceMemberInfo;
use crate::connection::state::{ConnectionState, SocketId};
use crate::error::{Error, Result};
use crate::log::Log;
use crate::protocol::messages::PusherMessage;
use dashmap::DashMap;
use fastwebsockets::{Frame, Payload, WebSocketWrite};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use std::cmp::PartialEq;
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tokio::io::WriteHalf;
use tokio::sync::{mpsc, Mutex};
// Combine all channel-related data into one structure

pub struct Connection {
    pub state: ConnectionState,
    pub socket: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
    pub message_sender: mpsc::UnboundedSender<Frame<'static>>,
}

pub struct Namespace {
    pub app_id: String,
    pub connections: DashMap<SocketId, Arc<Mutex<Connection>>>,
    pub channels: DashMap<String, HashSet<SocketId>>,
    pub users: DashMap<String, HashSet<SocketId>>,
}

impl PartialEq<String> for SocketId {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
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

    pub async fn add_connection(
        &self,
        socket_id: SocketId,
        socket: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
        app_manager: &AppManager,
    ) {
        let (tx, mut rx) = mpsc::unbounded_channel();

        let mut connection = Connection {
            state: ConnectionState::new(),
            socket,
            message_sender: tx,
        };

        let app = app_manager.get_app(&self.app_id).unwrap();
        connection.state.app = Some(app);

        let connection = Arc::new(Mutex::new(connection));

        // First insert the connection
        self.connections.insert(socket_id.clone(), connection.clone());

        // Log that connection was added
        Log::info(format!("Connection {} added to namespace {}", socket_id, self.app_id));

        // Then spawn the message handler
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
        let channel_data = self.channels.get(channel);
        match channel_data {
            Some(channel_data) => {
                if channel_data.deref().contains(socket_id) {
                    self.get_connection(socket_id)
                } else {
                    None
                }
            }
            None => None,
        }
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
        let mut failed_sockets = Vec::new();

        // Get and collect socket IDs first, releasing the read lock
        let socket_ids: Vec<SocketId> = if let Some(channel_data) = self.channels.get(channel) {
            channel_data
                .iter()
                .filter(|socket_id| !except.map_or(false, |sid| sid == *socket_id))
                .cloned()
                .collect()
        } else {
            return Ok(());
        };

        // Process each socket without holding the channel lock
        for socket_id in socket_ids {
            match self.get_connection(&socket_id) {
                Some(connection) => {
                    let frame = Frame::text(Payload::from(payload.clone().into_bytes()));
                    if let Err(e) = connection.lock().await.message_sender.send(frame) {
                        Log::error(format!("Failed to send message to {}: {}", socket_id, e));
                        failed_sockets.push(socket_id);
                    }
                }
                None => {
                    Log::warning(format!("Connection not found for socket {}", socket_id));
                    failed_sockets.push(socket_id);
                }
            }
        }

        // Clean up failed sockets
        if !failed_sockets.is_empty() {
            if let Some(mut channel_data) = self.channels.get_mut(channel) {
                for socket_id in &failed_sockets {
                    channel_data.deref_mut().remove(socket_id);
                }
                if channel_data.deref().is_empty() {
                    drop(channel_data); // Drop the reference before removing
                    self.channels.remove(channel);
                }
            }
        }

        Ok(())
    }

    pub async fn get_channel_members(
        &self,
        channel: &str,
    ) -> Result<HashMap<String, PresenceMemberInfo>> {
        let mut presence_members = HashMap::new();

        // Get all socket IDs first
        let socket_ids: Vec<SocketId> = match self.get_channel(channel) {
            Some(data) => data.iter().cloned().collect(),
            None => return Ok(HashMap::new()),
        };

        for socket_id in socket_ids {
            if let Some(connection) = self.get_connection(&socket_id) {
                // Scope the lock to only what we need
                let presence_data = {
                    let conn = connection.lock().await;
                    conn.state.presence.clone()
                }; // Lock is released here
                   // Process data without holding any locks
                if let Some(presence_map) = presence_data {
                    if let Some(presence_info) = presence_map.get(channel).cloned() {
                        let user_id = presence_info.user_id.clone();
                        presence_members.insert(user_id, presence_info);
                    }
                }
            }
        }
        Log::success("Presence members retrieved");

        Ok(presence_members)
    }

    pub fn get_channel_sockets(&self, channel: &str) -> Vec<SocketId> {
        let mut sockets = Vec::new();
        let channel_data = self.get_channel(channel).unwrap();
        for socket in channel_data.iter() {
            sockets.push(socket.clone());
        }
        sockets
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
            let message = PusherMessage::error(
                4009,
                "You got disconnected by the application".to_string(),
                None,
            );

            let conn = connection.lock().await;
            // Send the error message through message_sender
            let error_frame = Frame::text(Payload::from(
                serde_json::to_string(&message).unwrap().into_bytes(),
            ));

            // Send close frame through message_sender
            let close_frame = Frame::close(4009, &[]);
            let _ = conn.message_sender.send(error_frame);
            let _ = conn.message_sender.send(close_frame);
        }

        // Cleanup from channels
        let mut empty_channels = Vec::new();
        for mut channel_entry in self.channels.iter_mut() {
            let channel_data = channel_entry.value_mut();
            channel_data.remove(socket_id);

            if channel_data.is_empty() {
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
            .insert(socket_id.clone());
    }

    pub fn remove_channel_from_socket(&self, channel: &str, socket_id: &SocketId) -> bool {
        Log::info(format!(
            "Removing socket {} from channel {}",
            socket_id, channel
        ));

        if let Some(mut channel_data) = self.channels.get_mut(channel) {
            channel_data.deref_mut().remove(socket_id);
            if channel_data.deref().is_empty() {
                // Drop the reference before removing from DashMap
                drop(channel_data);
                self.channels.remove(channel);
            }
        }

        Log::success("Socket removed from channel");
        true
    }

    pub fn remove_connection(&self, socket_id: &SocketId) {
        self.connections.remove(socket_id);
    }

    pub fn get_channel(&self, channel: &str) -> Option<HashSet<SocketId>> {
        let entry = self.channels.entry(channel.to_string());
        let channel_data = entry.or_insert_with(|| HashSet::new());

        Some(channel_data.value().clone())
    }

    pub fn remove_channel(&self, channel: &str) {
        self.channels.remove(channel);
    }

    pub fn is_in_channel(&self, channel: &str, socket_id: &SocketId) -> bool {
        self.get_channel(channel)
            .map_or(false, |channel_data| channel_data.contains(socket_id))
    }

    pub async fn get_presence_member(
        &self,
        channel: &str,
        socket_id: &SocketId,
    ) -> Option<PresenceMemberInfo> {
        let connection = self.get_connection(socket_id);
        let presence_data = connection.unwrap().lock().await.state.presence.clone();
        presence_data.unwrap().get(channel).cloned()
    }
}
