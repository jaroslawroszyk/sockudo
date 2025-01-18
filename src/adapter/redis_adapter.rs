use crate::adapter::local_adapter::LocalAdapter;
use crate::adapter::Adapter;
use crate::app::manager::AppManager;
use crate::channel::PresenceMemberInfo;
use crate::error::{Error, Result};
use crate::log::Log;
use crate::namespace::Namespace;
use crate::protocol::messages::PusherMessage;
use crate::websocket::{SocketId, WebSocket, WebSocketRef};
use dashmap::{DashMap, DashSet};
use fastwebsockets::WebSocketWrite;
use futures::StreamExt;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::WriteHalf;
use tokio::sync::Mutex;
use tracing_subscriber::fmt::format;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct RedisMessage {
    channel: String,
    message: PusherMessage,
    except: Option<SocketId>,
    app_id: String,
    node_id: String,
    socket_id: SocketId,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BroadcastMessage {
    node_id: String,
    channel: String,
    socket_id: SocketId,
    message: String,
    app_id: String,
}

#[derive(Clone)]
pub struct RedisAdapter {
    pub pub_client: redis::Client,
    pub pub_connection: redis::aio::MultiplexedConnection,
    pub sub_client: redis::Client,
    pub sub_connection: redis::aio::MultiplexedConnection,
    pub local_adapter: LocalAdapter,
    pub channel: String,
    pub prefix: String,
    pub node_id: String,
}

impl RedisAdapter {
    pub async fn new(redis_url: &str, prefix: &str) -> Result<Self> {
        let pub_client = redis::Client::open(redis_url)
            .map_err(|e| Error::RedisError(format!("Failed to open pub client: {}", e)))?;
        let pub_connection = pub_client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| Error::RedisError(format!("Failed to connect pub client: {}", e)))?;

        let sub_client = redis::Client::open(redis_url)
            .map_err(|e| Error::RedisError(format!("Failed to open sub client: {}", e)))?;
        let sub_connection = sub_client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| Error::RedisError(format!("Failed to connect sub client: {}", e)))?;

        let local_adapter = LocalAdapter::new();

        let adapter = Self {
            pub_client,
            pub_connection,
            sub_client,
            sub_connection,
            local_adapter,
            channel: "redis-adapter".to_string(),
            prefix: prefix.to_string(),
            node_id: Uuid::new_v4().to_string(),
        };

        tokio::spawn(adapter.clone().handle_redis_messages());

        Ok(adapter)
    }

    async fn handle_redis_messages(mut self) {
        let mut pubsub = self
            .sub_client
            .get_async_pubsub()
            .await
            .map_err(|e| Error::RedisError(format!("Failed to get pubsub: {}", e)))
            .unwrap();

        let channel = format!("{}#{}", self.prefix, self.channel);
        Log::info(&format!("Subscribing to Redis channel: {}", channel));
        pubsub
            .subscribe(&channel)
            .await
            .map_err(|e| Error::RedisError(format!("Failed to subscribe: {}", e)))
            .unwrap();

        let mut messages = pubsub.on_message();

        while let Some(msg) = messages.next().await {
            let payload: String = msg.get_payload().unwrap();
            Log::info(format!("Received Redis message: {}", payload)); // More detailed logging

            match serde_json::from_str::<BroadcastMessage>(&payload) {
                Ok(redis_msg) => {
                    if redis_msg.node_id == self.node_id {
                        continue;
                    }

                    match serde_json::from_str(&redis_msg.message) {
                        Ok(message) => {
                            if let Err(e) = self
                                .send(
                                    &redis_msg.channel,
                                    message,
                                    Some(&redis_msg.socket_id),
                                    &redis_msg.app_id,
                                )
                                .await
                            {
                                Log::error(format!("Failed to send message locally: {}", e));
                            }
                        }
                        Err(e) => Log::error(format!("Failed to deserialize message: {}", e)),
                    }
                }
                Err(e) => Log::error(format!("Failed to deserialize Redis message: {}", e)),
            }
        }
    }
}

#[async_trait::async_trait]
impl Adapter for RedisAdapter {
    // Initialize the adapter
    async fn init(&mut self) {
        self.local_adapter.init().await;
    }

    // Delegate most methods to local adapter
    async fn get_namespace(&mut self, app_id: &str) -> Option<Arc<Namespace>> {
        self.local_adapter.get_namespace(app_id).await
    }

    async fn add_socket(
        &mut self,
        socket_id: SocketId,
        socket: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
        app_id: &str,
        app_manager: &AppManager,
    ) -> Result<()> {
        self.local_adapter
            .add_socket(socket_id, socket, app_id, app_manager)
            .await
    }

    async fn get_connection(
        &mut self,
        socket_id: &SocketId,
        app_id: &str,
    ) -> Option<Arc<Mutex<WebSocket>>> {
        self.local_adapter.get_connection(socket_id, app_id).await
    }

    async fn remove_connection(&mut self, socket_id: &SocketId, app_id: &str) -> Result<()> {
        self.local_adapter
            .remove_connection(socket_id, app_id)
            .await
    }

    // Delegate remaining methods to local adapter
    async fn send_message(
        &mut self,
        app_id: &str,
        socket_id: &SocketId,
        message: PusherMessage,
    ) -> Result<()> {
        self.local_adapter
            .send_message(app_id, socket_id, message)
            .await
            .expect("TODO: panic message");
        Ok(())
    }

    // The key method - publishing messages to Redis
    async fn send(
        &mut self,
        channel: &str,
        message: PusherMessage,
        except: Option<&SocketId>,
        app_id: &str,
    ) -> Result<()> {
        self.local_adapter
            .send(channel, message.clone(), except, app_id)
            .await?;

        if let Some(namespace) = self.get_namespace(app_id).await {
            if let Err(e) = namespace.broadcast(channel, message.clone(), except).await {
                Log::error(format!("Failed to broadcast to namespace: {}", e));
            }
        }

        let broadcast_msg = BroadcastMessage {
            node_id: self.node_id.clone(),
            channel: channel.to_string(),
            socket_id: except.cloned().unwrap_or_else(|| SocketId("".to_string())),
            message: serde_json::to_string(&message).map_err(|e| {
                Error::SerializationError(format!("Failed to serialize message: {}", e))
            })?,
            app_id: app_id.to_string(),
        };

        self.pub_connection
            .publish(
                format!("{}#{}", self.prefix, self.channel), // Use prefixed channel here as well
                serde_json::to_string(&broadcast_msg).map_err(|e| {
                    Error::SerializationError(format!("Failed to serialize message: {}", e))
                })?,
            )
            .await
            .map_err(|e| Error::RedisError(format!("Failed to publish: {}", e)))?;

        Ok(())
    }

    async fn get_channel_members(
        &mut self,
        app_id: &str,
        channel: &str,
    ) -> Result<HashMap<String, PresenceMemberInfo>> {
        self.local_adapter
            .get_channel_members(app_id, channel)
            .await
    }

    async fn get_channel_sockets(
        &mut self,
        app_id: &str,
        channel: &str,
    ) -> Result<DashMap<SocketId, Arc<Mutex<WebSocket>>>> {
        // For channel sockets, we only return local sockets since WebSocket
        // connections are node-specific
        self.local_adapter
            .get_channel_sockets(app_id, channel)
            .await
    }

    async fn get_channel(&mut self, app_id: &str, channel: &str) -> Result<DashSet<SocketId>> {
        // Similar to get_channel_sockets, we only track local channel subscriptions
        self.local_adapter.get_channel(app_id, channel).await
    }

    async fn remove_channel(&mut self, app_id: &str, channel: &str) {
        // Remove channel locally - other nodes handle their own channel cleanup
        self.local_adapter.remove_channel(app_id, channel).await
    }

    async fn is_in_channel(
        &mut self,
        app_id: &str,
        channel: &str,
        socket_id: &SocketId,
    ) -> Result<bool> {
        // Check channel membership locally since sockets are node-specific
        self.local_adapter
            .is_in_channel(app_id, channel, socket_id)
            .await
    }

    async fn get_user_sockets(
        &mut self,
        user_id: &str,
        app_id: &str,
    ) -> Result<DashSet<WebSocketRef>> {
        // Get user sockets from local node only
        self.local_adapter.get_user_sockets(user_id, app_id).await
    }

    async fn cleanup_connection(&mut self, app_id: &str, ws: WebSocketRef) {
        // Clean up the connection locally
        self.local_adapter.cleanup_connection(app_id, ws).await;
    }

    async fn terminate_connection(&mut self, app_id: &str, user_id: &str) -> Result<()> {
        // Terminate connection locally
        self.local_adapter
            .terminate_connection(app_id, user_id)
            .await?;
        Ok(())
    }

    async fn add_channel_to_sockets(&mut self, app_id: &str, channel: &str, socket_id: &SocketId) {
        // Add channel to socket mapping locally
        self.local_adapter
            .add_channel_to_sockets(app_id, channel, socket_id)
            .await
    }

    async fn get_channel_socket_count(&mut self, app_id: &str, channel: &str) -> usize {
        // Get local socket count - for a global count you'd need to implement
        // a Redis-based counting mechanism
        self.local_adapter
            .get_channel_socket_count(app_id, channel)
            .await
    }

    async fn add_to_channel(
        &mut self,
        app_id: &str,
        channel: &str,
        socket_id: &SocketId,
    ) -> Result<bool> {
        // Add socket to channel locally
        self.local_adapter
            .add_to_channel(app_id, channel, socket_id)
            .await
    }

    async fn remove_from_channel(
        &mut self,
        app_id: &str,
        channel: &str,
        socket_id: &SocketId,
    ) -> Result<bool> {
        // Remove socket from channel locally
        self.local_adapter
            .remove_from_channel(app_id, channel, socket_id)
            .await
    }

    async fn get_presence_member(
        &mut self,
        app_id: &str,
        channel: &str,
        socket_id: &SocketId,
    ) -> Option<PresenceMemberInfo> {
        // Get presence member info from local node
        self.local_adapter
            .get_presence_member(app_id, channel, socket_id)
            .await
    }

    async fn terminate_user_connections(&mut self, app_id: &str, user_id: &str) -> Result<()> {
        // Terminate user connections locally first
        self.local_adapter
            .terminate_user_connections(app_id, user_id)
            .await?;

        Ok(())
    }

    async fn add_user(&mut self, ws: Arc<Mutex<WebSocket>>) -> Result<()> {
        // Add user to local node
        self.local_adapter.add_user(ws).await
    }

    async fn remove_user(&mut self, ws: Arc<Mutex<WebSocket>>) -> Result<()> {
        // Remove user from local node
        self.local_adapter.remove_user(ws).await
    }
}
