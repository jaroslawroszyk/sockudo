use crate::app::manager::AppManager;
use crate::channel::PresenceMemberInfo;
use crate::connection::state::{ConnectionState, SocketId};
use crate::connection::ConnectionManager;
use crate::error::{Error, Result};
use crate::log::Log;
use crate::namespace::{Connection, Namespace};
use crate::protocol::messages::PusherMessage;
use async_trait::async_trait;
use fastwebsockets::{Frame, Payload, WebSocketWrite};
use fred::prelude::{Client, ClientLike, Config, EventInterface, PubsubInterface, TcpConfig};
use fred::types::{Builder, Message};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::WriteHalf;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
pub struct BroadcastMessage {
    node_id: String,
    channel: String,
    socket_id: SocketId,
    message: String,
}

pub struct RedisConnectionManager {
    // In-memory state (like MemoryConnectionManager)
    namespaces: HashMap<String, Arc<Namespace>>,

    // Redis clients for pub/sub
    pub_client: Client,
    sub_client: Client,

    // Redis channels for inter-node communication
    redis_channel: String,
    node_id: String,
}

impl RedisConnectionManager {
    pub async fn new() -> Result<Self> {
        let config = Config::from_url("redis://localhost:6379/1")
            .map_err(|e| Error::ConnectionError(format!("Failed to create Redis config: {}", e)))?;

        let pub_client = Builder::from_config(config.clone())
            .with_connection_config(|config| {
                config.connection_timeout = Duration::from_secs(5);
                config.tcp = TcpConfig {
                    nodelay: Some(true),
                    ..Default::default()
                };
            })
            .build()
            .map_err(|e| Error::ConnectionError(format!("Failed to create Redis client: {}", e)))?;

        let sub_client = pub_client.clone_new();

        pub_client.connect();
        sub_client.connect();

        Log::info("Connected to Redis for pub/sub");

        let manager = Self {
            namespaces: HashMap::new(),
            pub_client,
            sub_client,
            redis_channel: "pusher:broadcast".to_string(),
            node_id: Uuid::new_v4().to_string(),
        };

        manager.subscribe_to_channels().await?;

        Ok(manager)
    }

    async fn subscribe_to_channels(&self) -> Result<()> {
        let redis_channel = self.redis_channel.clone();
        let node_id = self.node_id.clone();
        let namespaces = self.namespaces.clone();

        self.sub_client
            .subscribe(vec![redis_channel.clone()])
            .await
            .map_err(|e| Error::ConnectionError(format!("Failed to subscribe: {}", e)))?;

        self.sub_client.on_message(move |msg: Message| {
            let payload = msg.value.as_string().unwrap_or_default();
            let node_id = node_id.clone();
            let namespaces = namespaces.clone();

            Box::pin(async move {
                if let Ok(broadcast_msg) = serde_json::from_str::<BroadcastMessage>(&payload) {
                    // Only process messages from other nodes
                    if broadcast_msg.node_id != node_id {
                        if let Some(namespace) = namespaces.get(&broadcast_msg.channel) {
                            if let Ok(message) = serde_json::from_str::<PusherMessage>(&broadcast_msg.message) {
                                let _ = namespace.send_message(&broadcast_msg.socket_id, message).await;
                            }
                        }
                    }
                }
                Ok(())
            })
        });

        Ok(())
    }
}

#[async_trait]
impl ConnectionManager for RedisConnectionManager {
    async fn init(&mut self) {
        Log::info("Initializing Redis pub/sub connection manager");
    }

    async fn get_namespace(&mut self, app_id: &str) -> Option<Arc<Namespace>> {
        if !self.namespaces.contains_key(app_id) {
            self.namespaces.insert(
                app_id.to_string(),
                Arc::new(Namespace::new(app_id.to_string())),
            );
        }
        self.namespaces.get(app_id).cloned()
    }

    async fn add_connection(
        &mut self,
        socket_id: SocketId,
        socket: WebSocketWrite<WriteHalf<TokioIo<Upgraded>>>,
        app_id: &str,
        app_manager: &AppManager,
    ) {
        if !self.namespaces.contains_key(app_id) {
            self.namespaces.insert(
                app_id.to_string(),
                Arc::new(Namespace::new(app_id.to_string())),
            );
        }

        if let Some(namespace) = self.namespaces.get(app_id) {
            namespace.add_connection(socket_id.clone(), socket, app_manager).await;
            Log::info(format!("Added connection {} to namespace {}", socket_id, app_id));
        }
    }

    async fn get_connection(
        &mut self,
        socket_id: &SocketId,
        app_id: String,
    ) -> Option<Arc<Mutex<Connection>>> {
        self.get_namespace(&app_id).await?.get_connection(socket_id)
    }

    async fn remove_connection(&mut self, socket_id: &SocketId, app_id: &str) {
        if let Some(namespace) = self.get_namespace(app_id).await {
            namespace.remove_connection(socket_id);
        }
    }

    async fn send_message(
        &mut self,
        app_id: &str,
        socket_id: &SocketId,
        message: PusherMessage,
    ) -> Result<()> {
        if let Some(namespace) = self.get_namespace(app_id).await {
            namespace.send_message(socket_id, message).await
        } else {
            Err(Error::ConnectionError("Namespace not found".to_string()))
        }
    }

    async fn broadcast(
        &mut self,
        channel: &str,
        message: PusherMessage,
        except: Option<&SocketId>,
        app_id: &str,
    ) -> Result<()> {
        // First broadcast locally
        if let Some(namespace) = self.get_namespace(app_id).await {
            namespace.broadcast(channel, message.clone(), except).await?;
        }

        // Then publish to Redis for other nodes
        let broadcast_msg = BroadcastMessage {
            node_id: self.node_id.clone(),
            channel: channel.to_string(),
            socket_id: except.cloned().unwrap_or_else(|| SocketId("".to_string())),
            message: serde_json::to_string(&message)?,
        };

        self.pub_client
            .publish::<String, String, String>(
                self.redis_channel.clone(),
                serde_json::to_string(&broadcast_msg)?,
            )
            .await
            .map_err(|e| Error::ConnectionError(format!("Failed to publish: {}", e)))?;

        Ok(())
    }

    // All other methods delegate to the namespace, just like MemoryConnectionManager
    async fn get_channel_members(
        &mut self,
        app_id: &str,
        channel: &str,
    ) -> Result<HashMap<String, PresenceMemberInfo>> {
        let namespace = self.get_namespace(app_id).await.unwrap();
        namespace.get_channel_members(channel).await
    }

    async fn get_channel_sockets(&self, channel: &str) -> Vec<SocketId> {
        let mut socket_ids = HashSet::new();
        for namespace in self.namespaces.values() {
            for socket_id in namespace.get_channel_sockets(channel) {
                socket_ids.insert(socket_id);
            }
        }
        socket_ids.into_iter().collect()
    }

    async fn get_channel(&mut self, app_id: &str, channel: &str) -> Option<HashSet<SocketId>> {
        self.get_namespace(app_id).await?.get_channel(channel)
    }

    async fn remove_channel(&mut self, app_id: &str, channel: &str) {
        if let Some(namespace) = self.get_namespace(app_id).await {
            namespace.remove_channel(channel);
        }
    }

    async fn is_in_channel(&mut self, app_id: &str, channel: &str, socket_id: &SocketId) -> bool {
        self.get_namespace(app_id)
            .await
            .map(|ns| ns.is_in_channel(channel, socket_id))
            .unwrap_or(false)
    }

    async fn get_user_connections(&mut self, user_id: &str, app_id: &str) -> Vec<SocketId> {
        self.get_namespace(app_id)
            .await
            .map(|ns| ns.get_user_connections(user_id))
            .unwrap_or_default()
    }

    async fn cleanup_connection(&mut self, app_id: &str, socket_id: &SocketId) {
        if let Some(namespace) = self.get_namespace(app_id).await {
            namespace.cleanup_connection(socket_id).await;
        }
    }

    async fn terminate_connection(&mut self, app_id: &str, user_id: &str) -> Result<()> {
        if let Some(namespace) = self.get_namespace(app_id).await {
            namespace.terminate_connection(user_id).await?;
        }
        Ok(())
    }

    async fn add_channel_to_sockets(&mut self, app_id: &str, channel: &str, socket_id: &SocketId) {
        if let Some(namespace) = self.get_namespace(app_id).await {
            namespace.add_channel_to_socket(channel, socket_id);
        }
    }

    async fn get_channel_socket_count(&mut self, app_id: &str, channel: &str) -> usize {
        self.sub_client.pubsub_numsub(channel).await.unwrap_or(0)
    }

    async fn add_socket_to_channel(&mut self, app_id: &str, channel: &str, socket_id: &SocketId) {
        if let Some(namespace) = self.get_namespace(app_id).await {
            namespace.add_channel_to_socket(channel, socket_id);
        }
    }

    async fn remove_socket_from_channel(
        &mut self,
        app_id: &str,
        channel: &str,
        socket_id: &SocketId,
    ) -> bool {
        self.get_namespace(app_id)
            .await
            .map(|ns| ns.remove_channel_from_socket(channel, socket_id))
            .unwrap_or(false)
    }

    async fn get_presence_member(
        &mut self,
        app_id: &str,
        channel: &str,
        socket_id: &SocketId,
    ) -> Option<PresenceMemberInfo> {
        if let Some(namespace) = self.get_namespace(app_id).await {
            namespace.get_presence_member(channel, socket_id).await
        } else {
            None
        }
    }
}