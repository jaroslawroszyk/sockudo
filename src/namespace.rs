use crate::app::manager::AppManager;
use crate::channel::PresenceMemberInfo;
use crate::error::{Error, Result};
use crate::log::Log;
use crate::protocol::messages::PusherMessage;
use crate::websocket::{ConnectionState, SocketId, WebSocket, WebSocketRef};
use dashmap::{DashMap, DashSet};
use fastwebsockets::{Frame, Payload, WebSocketWrite};
use futures::future;
use futures::future::join_all;
use futures::stream::{self, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use tokio::io::WriteHalf;
use tokio::sync::{mpsc, Mutex, Semaphore};
// Combine all channel-related data into one structure

pub struct Namespace {
    pub app_id: String,
    pub sockets: DashMap<SocketId, Arc<Mutex<WebSocket>>>,
    pub channels: DashMap<String, DashSet<SocketId>>,
    pub users: DashMap<String, DashSet<WebSocketRef>>,
}

impl Namespace {
    pub fn new(app_id: String) -> Self {
        Self {
            app_id,
            sockets: DashMap::new(),
            channels: DashMap::new(),
            users: DashMap::new(),
        }
    }

    pub async fn add_socket(
        &self,
        socket_id: SocketId,
        socket: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
        app_manager: &Arc<dyn AppManager + Send + Sync>
    ) {
        // Create channel for message passing
        let (tx, mut rx) = mpsc::unbounded_channel();

        // Initialize connection with socket in Option
        let mut connection = WebSocket {
            state: ConnectionState::new(),
            socket: Some(socket),
            message_sender: tx,
        };
        match app_manager.get_app(&self.app_id).await {
            Ok(app) => {
                if app.is_none() {
                    Log::error(format!("App not found for app_id: {}", self.app_id));
                    return;
                }
                connection.state.app = app;
            }
            Err(e) => {
                Log::error(format!("Failed to get app: {}", e));
            }
        }
        // Set app state

        // Wrap connection in Arc<Mutex>
        let connection = Arc::new(Mutex::new(connection));

        // Store connection
        self.sockets.insert(socket_id.clone(), connection.clone());

        // Spawn message handler task
        let connection_clone = connection.clone();
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let mut connection = connection_clone.lock().await;
                // Try to write to socket if it exists
                if let Some(socket) = &mut connection.socket {
                    match socket.write_frame(frame).await {
                        Ok(_) => continue,
                        Err(e) => {
                            // Log::error(format!("Failed to send message to {}: {}", socket_id, e));
                            break;
                        }
                    }
                } else {
                    // Socket has been taken, stop the message handler
                    break;
                }
            }
        });
    }

    pub fn get_connection(&self, socket_id: &SocketId) -> Option<Arc<Mutex<WebSocket>>> {
        self.sockets.get(socket_id).map(|conn| conn.clone())
    }

    pub fn get_connection_from_channel(
        &self,
        channel: &str,
        socket_id: &SocketId,
    ) -> Option<Arc<Mutex<WebSocket>>> {
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
        // Serialize message once and wrap in Arc
        let payload = Arc::new(serde_json::to_string(&message)?);

        // Get channel's DashSet of socket IDs
        let socket_ids = self.get_channel(channel).unwrap();

        for socket_id in socket_ids.iter() {
            if except.is_none() || except.unwrap().0 != socket_id.0 {
                if let Some(connection) = self.get_connection(&*socket_id) {
                    let payload = payload.clone();
                    let frame = Frame::text(Payload::from(payload.as_bytes().to_vec()));

                    let conn = connection.lock().await;
                    conn.message_sender.send(frame).map_err(|e| {
                        Error::ConnectionError(format!("Failed to send message: {}", e))
                    })?;
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
        let socket_ids = self.get_channel(channel).unwrap(); // Get all socket IDs first

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

        Ok(presence_members)
    }

    pub fn get_channel_sockets(&self, channel: &str) -> DashMap<SocketId, Arc<Mutex<WebSocket>>> {
        let sockets = DashMap::new();

        if let Some(channel_data) = self.channels.get(channel) {
            for socket_id in channel_data.iter() {
                if let Some(connection) = self.get_connection(&socket_id) {
                    sockets.insert(socket_id.clone(), connection);
                }
            }
        }

        sockets
    }

    pub async fn get_user_sockets(&self, user_id: &str) -> Result<DashSet<WebSocketRef>> {
        let sockets = DashSet::new();
        if let Some(user_data) = self.users.get(user_id) {
            for socket in user_data.iter() {
                let socket_id = socket.0.lock().await.state.socket_id.clone();
                let conn = self.get_connection(&socket_id).unwrap();
                sockets.insert(WebSocketRef(conn.clone()));
            }
        }
        Ok(sockets)
    }

    pub async fn cleanup_connection(&self, ws: WebSocketRef) {
        let socket_id = {
            let ws = ws.0.lock().await;
            ws.state.socket_id.clone()
        };

        // Send disconnect message
        let message = PusherMessage::error(
            4009,
            "You got disconnected by the application".to_string(),
            None,
        );

        // Create frames outside the lock
        let error_frame = Frame::text(Payload::from(
            serde_json::to_string(&message).unwrap().into_bytes(),
        ));
        let close_frame = Frame::close(4009, &[]);

        // Send frames
        let ws_ref = ws.0.lock().await;
        let _ = ws_ref.message_sender.send(error_frame);
        let _ = ws_ref.message_sender.send(close_frame);
        drop(ws_ref); // Explicitly drop lock

        // Cleanup from channels using DashMap/DashSet methods
        let mut empty_channels = Vec::new();
        self.channels.iter_mut().for_each(|mut channel_entry| {
            channel_entry.value_mut().remove(&socket_id);
            if channel_entry.value().is_empty() {
                empty_channels.push(channel_entry.key().clone());
            }
        });

        // Remove empty channels
        for channel in empty_channels {
            self.channels.remove(&channel);
        }

        // Cleanup from users using DashMap/DashSet methods
        let mut empty_users = Vec::new();
        self.users.iter_mut().for_each(|mut user_entry| {
            user_entry.value_mut().remove(&ws);
            if user_entry.value().is_empty() {
                empty_users.push(user_entry.key().clone());
            }
        });

        // Remove empty users
        for user in empty_users {
            self.users.remove(&user);
        }

        // Remove socket
        self.sockets.remove(&socket_id);
    }

    pub async fn terminate_user_connections(&self, user_id: &str) -> Result<()> {
        let connections = self.get_user_sockets(user_id).await;
        let mut futures = Vec::new();

        match connections {
            Ok(connections) => {
                for connection in connections.iter() {
                    futures.push(self.cleanup_connection(connection.clone()));
                }
                join_all(futures).await;
            }
            Err(e) => {
                Log::error(format!("Failed to get user connections: {}", e));
            }
        }

        Ok(())
    }

    pub fn add_channel_to_socket(&self, channel: &str, socket_id: &SocketId) -> bool {
        self.channels
            .entry(channel.to_string())
            .or_default()
            .insert(socket_id.clone())
    }

    pub fn remove_channel_from_socket(&self, channel: &str, socket_id: &SocketId) -> bool {
        // Log::info(format!(
        //     "Removing socket {} from channel {}",
        //     socket_id, channel
        // ));

        if let Some(mut channel_data) = self.channels.get_mut(channel) {
            channel_data.deref_mut().remove(socket_id);
            if channel_data.deref().is_empty() {
                // Drop the reference before removing from DashMap
                drop(channel_data);
                self.channels.remove(channel);
            }
        }

        // Log::success("Socket removed from channel");
        true
    }

    pub fn remove_connection(&self, socket_id: &SocketId) {
        self.sockets.remove(socket_id);
    }

    pub fn get_channel(&self, channel: &str) -> Result<DashSet<SocketId>> {
        let entry = self.channels.entry(channel.to_string());
        let channel_data = entry.or_insert_with(|| DashSet::new());

        Ok(channel_data.value().clone())
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

    pub async fn add_user(&self, ws: Arc<Mutex<WebSocket>>) -> Result<()> {
        let user = ws.lock().await.state.user.clone();
        if let Some(user) = user {
            let user_id = user.get("id").unwrap().as_str().unwrap();
            match self.users.get_mut(user_id) {
                Some(mut user_data) => {
                    user_data.deref_mut().insert(WebSocketRef(ws.clone()));
                }
                None => {
                    let user_data = DashSet::new();
                    user_data.insert(WebSocketRef(ws.clone()));
                    self.users.insert(user_id.to_string(), user_data);
                }
            }
        }
        Ok(())
    }

    pub async fn remove_user(&self, ws: Arc<Mutex<WebSocket>>) -> Result<()> {
        let user = ws.lock().await.state.user.clone();
        if let Some(user) = user {
            let user_id = user.get("id").unwrap().as_str().unwrap();
            if let Some(mut user_data) = self.users.get_mut(user_id) {
                user_data.deref_mut().remove(&WebSocketRef(ws.clone()));
                if user_data.deref().is_empty() {
                    self.users.remove(user_id);
                }
            }
        }
        Ok(())
    }
}
