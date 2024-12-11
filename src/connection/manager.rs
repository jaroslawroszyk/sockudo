use crate::app::manager::AppManager;
use crate::channel::PresenceMemberInfo;
use crate::connection::state::SocketId;
use crate::error::{Error, Result};
use crate::namespace::{Connection, Namespace};
use crate::protocol::messages::PusherMessage;
use fastwebsockets::WebSocketWrite;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::io::WriteHalf;
use tokio::sync::Mutex;

#[async_trait::async_trait]
pub trait ConnectionManager: Send + Sync {
    async fn init(&mut self);
    // Namespace management
    async fn get_namespace(&mut self, app_id: &str) -> Option<Arc<Namespace>>;

    // Connection management
    async fn add_connection(
        &mut self,
        socket_id: SocketId,
        socket: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
        app_id: &str,
        app_manager: &AppManager,
    );

    async fn get_connection(
        &mut self,
        socket_id: &SocketId,
        app_id: String,
    ) -> Option<Arc<Mutex<Connection>>>;

    async fn remove_connection(&mut self, socket_id: &SocketId, app_id: &str);

    // Message handling
    async fn send_message(
        &mut self,
        app_id: &str,
        socket_id: &SocketId,
        message: PusherMessage,
    ) -> Result<()>;

    async fn broadcast(
        &mut self,
        channel: &str,
        message: PusherMessage,
        except: Option<&SocketId>,
        app_id: &str,
    ) -> Result<()>;

    // Channel management
    async fn get_channel_members(
        &mut self,
        app_id: &str,
        channel: &str,
    ) -> Result<HashMap<String, PresenceMemberInfo>>;

    async fn get_channel_sockets(&self, channel: &str) -> Vec<SocketId>;

    async fn get_channel(&mut self, app_id: &str, channel: &str) -> Option<HashSet<SocketId>>;

    async fn remove_channel(&mut self, app_id: &str, channel: &str);

    async fn is_in_channel(&mut self, app_id: &str, channel: &str, socket_id: &SocketId) -> bool;

    // User management
    async fn get_user_connections(&mut self, user_id: &str, app_id: &str) -> Vec<SocketId>;

    // Cleanup operations
    async fn cleanup_connection(&mut self, app_id: &str, socket_id: &SocketId);

    async fn terminate_connection(&mut self, app_id: &str, user_id: &str) -> Result<()>;

    // Channel-socket operations
    async fn add_channel_to_sockets(&mut self, app_id: &str, channel: &str, socket_id: &SocketId);

    async fn get_channel_socket_count(&mut self, app_id: &str, channel: &str) -> usize;

    async fn add_socket_to_channel(&mut self, app_id: &str, channel: &str, socket_id: &SocketId);

    async fn remove_socket_from_channel(
        &mut self,
        app_id: &str,
        channel: &str,
        socket_id: &SocketId,
    ) -> bool;

    // Presence channel operations
    async fn get_presence_member(
        &mut self,
        app_id: &str,
        channel: &str,
        socket_id: &SocketId,
    ) -> Option<PresenceMemberInfo>;
}
