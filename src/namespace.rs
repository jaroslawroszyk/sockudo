use crate::channel::manager::PresenceMember;
use crate::channel::{ChannelManager, ChannelType, PresenceMemberInfo};
use crate::connection::state::{ConnectionState, SocketId};
use crate::error::{Error, Result};
use crate::log::Log;
use crate::protocol::messages::{MessageData, PusherMessage};
use dashmap::DashMap;
use fastwebsockets::{FragmentCollector, Frame, Payload, WebSocketWrite};
use futures::SinkExt;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::io::WriteHalf;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::instrument::WithSubscriber;

pub struct Namespace {
    pub app_id: String,
    pub connections: DashMap<SocketId, Arc<Mutex<Connection>>>,
    pub channel_sockets: DashMap<String, HashSet<SocketId>>,
    pub presence_channels: DashMap<String, DashMap<String, PresenceMemberInfo>>,
    pub users: DashMap<String, HashSet<SocketId>>,
}

pub struct Connection {
    pub state: ConnectionState,
    pub socket: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
    pub message_sender: mpsc::UnboundedSender<Frame<'static>>,
}

impl Namespace {
    pub fn new(app_id: String) -> Self {
        Self {
            app_id,
            connections: DashMap::new(),
            channel_sockets: DashMap::new(),
            presence_channels: DashMap::new(),
            users: DashMap::new(),
        }
    }

    pub fn get_presence_member(&self, channel: &str, user_id: &str) -> Option<PresenceMemberInfo> {
        self.presence_channels
            .get(channel)
            .and_then(|channel| channel.get(user_id).map(|member| member.value().clone()))
    }

    pub fn add_connection(
        &self,
        socket_id: SocketId,
        socket: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
    ) {
        let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();

        // Create connection first
        let connection = Connection {
            state: ConnectionState::new("demo-key".to_string()), // Create a new connection state
            socket,                                              // Store the original socket
            message_sender: tx,
        };

        // Insert the connection
        self.connections
            .insert(socket_id.clone(), Arc::new(Mutex::new(connection)));

        // Get a clone of the connection for the task
        let connection_ref = self.connections.get(&socket_id).unwrap().clone();

        // Spawn the task with the cloned connection reference
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let mut conn = connection_ref.lock().await;
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

    pub fn remove_connection(&self, socket_id: &SocketId) {
        self.connections.remove(socket_id);
    }

    pub async fn send_message(&self, socket_id: &SocketId, message: PusherMessage) -> Result<()> {
        if let Some(connection) = self.get_connection(socket_id) {
            let message = serde_json::to_string(&message)?;
            Log::info(message.as_str());
            let frame = Frame::text(Payload::from(message.into_bytes()));

            // Get the sender without locking the entire connection
            let sender = {
                let conn = connection.lock().await;
                conn.message_sender.clone()
            };

            // Send the frame through the channel
            sender
                .send(frame)
                .map_err(|e| Error::ConnectionError(format!("Failed to send message: {}", e)))?;
        }
        Ok(())
    }

    pub(crate) async fn broadcast(
        &self,
        channel: &str,
        message: PusherMessage,
        except: Option<&SocketId>,
    ) -> Result<()> {
        // Serialize message once
        let payload = serde_json::to_string(&message)?;

        // Log broadcast attempt
        Log::info_title(format!(
            "Broadcasting to channel '{}' - Event: '{}'",
            channel,
            message.event.as_deref().unwrap_or("unknown")
        ));

        for conn in self.connections.iter() {
            Log::info(format!("Connection: {}", conn.key().0).as_str());
            if except.map_or(true, |sid| sid != conn.key()) {
                let connection = conn.value();
                let mut conn_guard = connection.lock().await;
                if conn_guard.state.is_subscribed(channel) {
                    let frame = Frame::text(Payload::from(payload.clone().into_bytes()));
                    if let Err(e) = conn_guard.socket.write_frame(frame).await {
                        Log::error(format!(
                            "Failed to send to connection {}: {}",
                            conn.key(),
                            e
                        ));
                        continue; // Continue with other connections even if one fails
                    }
                }
            }
        }
        Log::info("Broadcast complete");

        Ok(())
    }

    pub async fn get_channel_members(
        &self,
        app_id: &str,
        channel: &str,
    ) -> Result<HashMap<String, PresenceMemberInfo>> {
        let mut members = HashMap::new();

        if let Some(socket_set) = self.channel_sockets.get(channel) {
            for socket_id in socket_set.value() {
                if let Some(connection) = self.connections.get(socket_id) {
                    let conn_guard = connection.lock().await;
                    if let Some(presence_map) = &conn_guard.state.presence {
                        if let Some(member) = presence_map.get(channel) {
                            Log::info(socket_id.to_string().as_str());
                            members.insert(socket_id.to_string(), member.clone());
                        }
                    }
                }
            }
        }

        Ok(members)
    }

    pub fn get_channel_sockets(&self, channel: &str) -> Vec<SocketId> {
        let sockets = self
            .channel_sockets
            .get(channel)
            .map(|set| set.value().iter().cloned().collect())
            .unwrap_or_default();
        Log::info(format!("Channel sockets: {:?}", sockets).as_str());
        sockets
    }

    pub fn add_presence_member(
        &self,
        channel: &str,
        socket_id: &SocketId,
        presence_data: PresenceMemberInfo,
    ) {
        self.presence_channels
            .entry(channel.to_string())
            .or_default()
            .insert(socket_id.0.clone(), presence_data);
    }

    pub fn get_presence_members(&self, channel: &str) -> HashMap<String, PresenceMemberInfo> {
        self.presence_channels
            .get(channel)
            .map(|members| {
                members
                    .iter()
                    .map(|entry| (entry.key().clone(), entry.value().clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_user_connections(&self, user_id: &str) -> Vec<SocketId> {
        // First check presence channels for this user
        let mut socket_ids = HashSet::new();

        for presence_channel in self.presence_channels.iter() {
            if let Some(member_info) = presence_channel.value().get(user_id) {
                // If we find the user in a presence channel, get their socket_id
                socket_ids.insert(SocketId(member_info.user_id.clone()));
            }
        }

        socket_ids.into_iter().collect()
    }

    pub async fn cleanup_connection(
        &self,
        channel_manager: &RwLock<ChannelManager>,
        socket_id: &SocketId,
    ) {
        let connection = self.get_connection(socket_id).unwrap();
        let mut conn_guard = connection.lock().await;
        conn_guard
            .socket
            .write_frame(Frame::close(4009, &[]))
            .await
            .ok();
    }

    pub async fn terminate_connection(
        &self,
        channel_manager: &RwLock<ChannelManager>,
        socket_id: &SocketId,
    ) -> Result<()> {
        if let Some(connection) = self.get_connection(socket_id) {
            // Send connection terminated message
            let terminate_message = PusherMessage {
                event: Some("pusher:error".to_string()),
                data: Some(MessageData::Json(json!({
                    "code": 4009,
                    "message": "Connection terminated by application server"
                }))),
                channel: None,
                name: None,
            };

            // Send termination message
            if let Err(e) = self.send_message(socket_id, terminate_message).await {
                Log::error(format!("Error sending termination message: {}", e));
            }

            // Clean up the connection and all its subscriptions
            self.cleanup_connection(channel_manager, socket_id).await;
        }
        Ok(())
    }

    pub fn add_channel_to_socket(&self, channel: &str, socket_id: &SocketId) {
        self.channel_sockets
            .entry(channel.to_string())
            .or_default()
            .insert(socket_id.clone());
    }
}
