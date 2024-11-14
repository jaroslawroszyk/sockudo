use crate::channel::manager::PresenceMember;
use crate::channel::{ChannelManager, ChannelType, PresenceMemberInfo};
use crate::connection::state::{ConnectionState, SocketId};
use crate::error::{Error, Result};
use crate::log::Log;
use crate::namespace::{Connection, Namespace};
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

pub struct ConnectionManager {
    pub namespaces: HashMap<String, Arc<Namespace>>,
}

// pub struct Connection {
//     pub state: ConnectionState,
//     pub socket: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
//     pub message_sender: mpsc::UnboundedSender<Frame<'static>>,
// }

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            namespaces: HashMap::new(),
        }
    }

    pub fn get_namespace(&mut self, app_id: &str) -> Option<Arc<Namespace>> {
        if !self.namespaces.contains_key(app_id) {
            Log::info(format!("Creating new namespace for app_id: {}", app_id).as_str());
            self.namespaces.insert(
                app_id.to_string(),
                Arc::new(Namespace::new(app_id.to_string())),
            );
        }
        self.namespaces.get(app_id).cloned()
    }

    pub fn add_connection(
        &mut self,
        socket_id: SocketId,
        socket: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
        app_id: &str,
    ) {
        let namespace = self.get_namespace(app_id).unwrap();
        Log::info(format!("Adding connection to namespace: {}", app_id).as_str());
        namespace.add_connection(socket_id, socket);
    }

    pub fn get_connection(
        &mut self,
        socket_id: &SocketId,
        app_id: String,
    ) -> Option<Arc<Mutex<Connection>>> {
        Log::info(format!("Getting connection for socket_id: {}", socket_id).as_str());
        let namespace = self.get_namespace(&app_id).unwrap();
        Some(namespace.get_connection(socket_id).unwrap())
    }

    pub fn remove_connection(&mut self, socket_id: &SocketId, app_id: &str) {
        self.get_namespace(app_id)
            .unwrap()
            .remove_connection(socket_id);
    }

    pub async fn send_message(
        &mut self,
        app_id: &str,
        socket_id: &SocketId,
        message: PusherMessage,
    ) -> Result<()> {
        if let Some(connection) = self.get_connection(socket_id, app_id.parse().unwrap()) {
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
        &mut self,
        channel: &str,
        message: PusherMessage,
        except: Option<&SocketId>,
        app_id: &str,
    ) -> Result<()> {
        self.get_namespace(app_id)
            .unwrap()
            .broadcast(channel, message, except)
            .await
    }

    pub async fn get_channel_members(
        &mut self,
        app_id: &str,
        channel: &str,
    ) -> Result<HashMap<String, PresenceMemberInfo>> {
        Ok(self
            .get_namespace(app_id)
            .unwrap()
            .get_presence_members(channel))
    }

    pub fn get_channel_sockets(&self, channel: &str) -> Vec<SocketId> {
        let mut socket_ids = HashSet::new();
        for namespace in self.namespaces.values() {
            for socket_id in namespace.get_channel_sockets(channel) {
                socket_ids.insert(socket_id);
            }
        }
        socket_ids.into_iter().collect()
    }

    pub fn add_presence_member(
        &mut self,
        channel: &str,
        socket_id: &SocketId,
        presence_data: PresenceMemberInfo,
        app_id: &str,
    ) {
        self.get_namespace(app_id)
            .unwrap()
            .add_presence_member(channel, socket_id, presence_data);
    }

    pub fn get_presence_member(
        &mut self,
        channel: &str,
        socket_id: &SocketId,
        app_id: &str,
    ) -> Option<PresenceMemberInfo> {
        self.get_namespace(app_id)
            .unwrap()
            .get_presence_member(channel, socket_id.0.as_str())
    }

    pub fn get_presence_members(
        &mut self,
        channel: &str,
        app_id: &str,
    ) -> HashMap<String, PresenceMemberInfo> {
        self.get_namespace(app_id)
            .unwrap()
            .get_presence_members(channel)
    }

    pub fn get_user_connections(&mut self, user_id: &str, app_id: &str) -> Vec<SocketId> {
        self.get_namespace(app_id)
            .unwrap()
            .get_user_connections(user_id)
    }

    pub async fn cleanup_connection(
        &mut self,
        app_id: &str,
        channel_manager: &RwLock<ChannelManager>,
        socket_id: &SocketId,
    ) {
        let namespace = self.get_namespace(app_id).unwrap();
        namespace
            .cleanup_connection(channel_manager, socket_id)
            .await;
    }

    pub async fn terminate_connection(
        &mut self,
        app_id: &str,
        channel_manager: &RwLock<ChannelManager>,
        socket_id: &SocketId,
    ) -> Result<()> {
        let namespace = self.get_namespace(app_id).unwrap();
        namespace
            .terminate_connection(channel_manager, socket_id)
            .await
    }

    pub fn add_channel_to_sockets(&mut self, app_id: &str, channel: &str, socket_id: &SocketId) {
        self.get_namespace(app_id)
            .unwrap()
            .add_channel_to_socket(channel, socket_id);
    }

    pub fn get_channel_socket_count(&mut self, app_id: &str, channel: &str) -> usize {
        let sockets = self
            .get_namespace(app_id)
            .unwrap()
            .get_channel_sockets(channel);
        sockets.len()
    }

    pub fn add_socket_to_channel(&mut self, app_id: &str, channel: &str, socket_id: &SocketId) {
        self.get_namespace(app_id)
            .unwrap()
            .add_channel_to_socket(channel, socket_id);
    }
}
