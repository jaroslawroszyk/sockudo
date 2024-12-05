use crate::channel::PresenceMemberInfo;
use crate::connection::state::SocketId;
use crate::error::{Error, Result};
use crate::log::Log;
use crate::namespace::{Connection, Namespace};
use crate::protocol::messages::PusherMessage;
use fastwebsockets::{Frame, Payload, WebSocketWrite};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::io::WriteHalf;
use tokio::sync::Mutex;

pub struct ConnectionManager {
    pub namespaces: HashMap<String, Arc<Namespace>>,
}

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
        match  self.get_namespace(app_id) {
            Some(namespace) => {
                namespace.add_connection(socket_id, socket);
            }
            None => {
                Log::error(format!("Namespace not found: {}", app_id).as_str());
            }
        }
    }

    pub fn get_connection(
        &mut self,
        socket_id: &SocketId,
        app_id: String,
    ) -> Option<Arc<Mutex<Connection>>> {
        let namespace = self.get_namespace(&app_id).unwrap();
        namespace.get_connection(socket_id)
    }

    pub fn remove_connection(&mut self, socket_id: &SocketId, app_id: &str) {
        match self.get_namespace(app_id) {
            Some(namespace) => {
                namespace.remove_connection(socket_id);
            }
            None => {
                Log::error(format!("Namespace not found: {}", app_id).as_str());
            }
        }
    }

    pub async fn send_message(
        &mut self,
        app_id: &str,
        socket_id: &SocketId,
        message: PusherMessage,
    ) -> Result<()> {
        let connection = self.get_connection(socket_id, app_id.to_string()).unwrap();
        let message = match  serde_json::to_string(&message) {
            Ok(m) => m,
            Err(e) => {
                return Err(Error::ConnectionError(format!("Failed to serialize message: {}", e)));
            }
        };
        let frame = Frame::text(Payload::from(message.into_bytes()));

        // Get the sender without locking the entire connection
        let sender = {
            let conn = connection.lock().await;
            conn.message_sender.clone()
        };

        match sender.send(frame) {
            Ok(_) => {}
            Err(e) => {
                Log::error(format!("Failed to send message: {}", e).as_str());
            }
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
        match  self.get_namespace(app_id) {
            Some(namespace) => {
                namespace.broadcast(channel, message, except).await
            }
            None => {
                Log::error(format!("Namespace not found: {}", app_id).as_str());
                Err(Error::ConnectionError("Namespace not found".to_string()))
            }
        }
    }

    pub async fn get_channel_members(
        &mut self,
        app_id: &str,
        channel: &str,
    ) -> Result<HashMap<String, PresenceMemberInfo>> {
        let namespace = self.get_namespace(app_id).unwrap();
        namespace.get_channel_members(channel).await
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


    pub fn get_user_connections(&mut self, user_id: &str, app_id: &str) -> Vec<SocketId> {
        self.get_namespace(app_id)
            .unwrap()
            .get_user_connections(user_id)
    }

    pub async fn cleanup_connection(&mut self, app_id: &str, socket_id: &SocketId) {
        let namespace = self.get_namespace(app_id).unwrap();
        namespace.cleanup_connection(socket_id).await;
    }

    pub async fn terminate_connection(&mut self, app_id: &str, user_id: &str) -> Result<()> {
        let namespace = self.get_namespace(app_id).unwrap();
        match namespace.terminate_connection(user_id).await {
            Ok(_) => {}
            Err(e) => {
                Log::error(format!("Failed to terminate connection: {}", e).as_str());
            }
        }
        Ok(())
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

    pub fn remove_socket_from_channel(
        &mut self,
        app_id: &str,
        channel: &str,
        socket_id: &SocketId,
    ) -> bool {
        self.get_namespace(app_id)
            .unwrap()
            .remove_channel_from_socket(channel, socket_id)
    }

    pub fn get_channel(&mut self, app_id: &str, channel: &str) -> Option<HashSet<SocketId>> {
        self.get_namespace(app_id)
            .and_then(|ns| ns.get_channel(channel))
    }

    pub fn remove_channel(&mut self, app_id: &str, channel: &str) {
        self.get_namespace(app_id).unwrap().remove_channel(channel);
    }

    pub fn is_in_channel(&mut self, app_id: &str, channel: &str, socket_id: &SocketId) -> bool {
        self.get_namespace(app_id)
            .unwrap()
            .is_in_channel(channel, socket_id)
    }

    pub async fn get_presence_member(&mut self, app_id: &str, channel: &str, socket_id: &SocketId) -> Option<PresenceMemberInfo> {
        self.get_namespace(app_id)
            .unwrap()
            .get_presence_member(channel, socket_id).await
    }
}
