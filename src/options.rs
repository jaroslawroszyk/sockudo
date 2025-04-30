use crate::app::config::App;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerOptions {
    pub adapter: AdapterConfig,
    pub app_manager: AppManagerConfig,
    pub cache: CacheConfig,
    pub channel_limits: ChannelLimits,
    pub cluster: ClusterConfig,
    pub cors: CorsConfig,
    pub database: DatabaseConfig,
    pub database_pooling: DatabasePooling,
    pub debug: bool,
    pub event_limits: EventLimits,
    pub host: String,
    pub http_api: HttpApiConfig,
    pub instance: InstanceConfig,
    pub metrics: MetricsConfig,
    pub mode: String,
    pub port: u16,
    pub path_prefix: String,
    pub presence: PresenceConfig,
    pub queue: QueueConfig,
    pub rate_limiter: RateLimiterConfig,
    pub shutdown_grace_period: u64,
    pub ssl: SslConfig,
    pub user_authentication_timeout: u64,
    pub webhooks: WebhooksConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqsQueueConfig {
    /// AWS region
    pub region: String,
    /// URL prefix for SQS queues, omitting queue name
    pub queue_url_prefix: Option<String>,
    /// Visibility timeout in seconds (how long a message is invisible after being received)
    pub visibility_timeout: i32,
    /// Optional endpoint URL (for local testing with LocalStack)
    pub endpoint_url: Option<String>,
    /// How many messages to receive at once
    pub max_messages: i32,
    /// Wait time in seconds for long polling (0-20)
    pub wait_time_seconds: i32,
    /// Processing concurrency per queue
    pub concurrency: u32,
    /// Use standard queue (false) or FIFO queue (true)
    pub fifo: bool,
    /// Message group ID for FIFO queues
    pub message_group_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterConfig {
    pub driver: String,
    pub redis: RedisAdapterConfig,
    pub cluster: ClusterAdapterConfig,
    pub nats: NatsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisAdapterConfig {
    pub requests_timeout: u64,
    pub prefix: String,
    pub redis_pub_options: HashMap<String, serde_json::Value>,
    pub redis_sub_options: HashMap<String, serde_json::Value>,
    pub cluster_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterAdapterConfig {
    pub requests_timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatsConfig {
    pub requests_timeout: u64,
    pub prefix: String,
    pub servers: Vec<String>,
    pub user: Option<String>,
    pub pass: Option<String>,
    pub token: Option<String>,
    pub timeout: u64,
    pub nodes_number: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppManagerConfig {
    pub driver: String,
    pub array: ArrayConfig,
    pub cache: CacheSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArrayConfig {
    pub apps: Vec<App>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheSettings {
    pub enabled: bool,
    pub ttl: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    pub driver: String,
    pub redis: RedisConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    pub redis_options: HashMap<String, serde_json::Value>,
    pub cluster_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelLimits {
    pub max_name_length: u32,
    pub cache_ttl: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    pub hostname: String,
    pub hello_interval: u64,
    pub check_interval: u64,
    pub node_timeout: u64,
    pub master_timeout: u64,
    pub port: u16,
    pub prefix: String,
    pub ignore_process: bool,
    pub broadcast: String,
    pub unicast: Option<String>,
    pub multicast: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorsConfig {
    pub credentials: bool,
    pub origin: Vec<String>,
    pub methods: Vec<String>,
    pub allowed_headers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub mysql: DatabaseConnection,
    pub postgres: DatabaseConnection,
    pub redis: RedisConnection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConnection {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConnection {
    pub host: String,
    pub port: u16,
    pub db: u32,
    pub username: Option<String>,
    pub password: Option<String>,
    pub key_prefix: String,
    pub sentinels: Vec<RedisSentinel>,
    pub sentinel_password: Option<String>,
    pub name: String,
    pub cluster_nodes: Vec<ClusterNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisSentinel {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterNode {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabasePooling {
    pub enabled: bool,
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLimits {
    pub max_channels_at_once: String,
    pub max_name_length: String,
    pub max_payload_in_kb: String,
    pub max_batch_size: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpApiConfig {
    pub request_limit_in_mb: String,
    pub accept_traffic: AcceptTraffic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptTraffic {
    pub memory_threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub process_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub driver: String,
    pub host: String,
    pub prometheus: PrometheusConfig,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrometheusConfig {
    pub prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceConfig {
    pub max_members_per_channel: String,
    pub max_member_size_in_kb: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueConfig {
    pub driver: String,
    pub redis: RedisQueueConfig,
    pub sqs: SqsQueueConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisQueueConfig {
    pub concurrency: u32,
    pub redis_options: HashMap<String, serde_json::Value>,
    pub cluster_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimiterConfig {
    pub driver: String,
    pub default_limit_per_second: u32,
    pub default_window_seconds: u64,
    pub api_rate_limit: RateLimit,
    pub websocket_rate_limit: RateLimit,
    pub redis: RedisConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimit {
    pub max_requests: u32,
    pub window_seconds: u64,
    pub identifier: Option<String>,
}

// Default implementation for RateLimiterConfig
impl Default for RateLimiterConfig {
    fn default() -> Self {
        Self {
            driver: "memory".to_string(),
            default_limit_per_second: 60,
            default_window_seconds: 60,
            api_rate_limit: RateLimit {
                max_requests: 60,
                window_seconds: 60,
                identifier: Some("api".to_string()),
            },
            websocket_rate_limit: RateLimit {
                max_requests: 10,
                window_seconds: 60,
                identifier: Some("websocket".to_string()),
            },
            redis: RedisConfig {
                redis_options: HashMap::new(),
                cluster_mode: false,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SslConfig {
    pub cert_path: String,
    pub key_path: String,
    pub passphrase: String,
    pub ca_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhooksConfig {
    pub batching: BatchingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchingConfig {
    pub enabled: bool,
    pub duration: u64,
}

// Example implementation of default values
impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            adapter: AdapterConfig {
                driver: "redis".to_string(),
                redis: RedisAdapterConfig {
                    requests_timeout: 5000,
                    prefix: "pusher".to_string(),
                    redis_pub_options: HashMap::new(),
                    redis_sub_options: HashMap::new(),
                    cluster_mode: false,
                },
                cluster: ClusterAdapterConfig {
                    requests_timeout: 5000,
                },
                nats: NatsConfig {
                    requests_timeout: 5000,
                    prefix: "pusher".to_string(),
                    servers: vec!["nats://localhost:4222".to_string()],
                    user: None,
                    pass: None,
                    token: None,
                    timeout: 5000,
                    nodes_number: None,
                },
            },
            rate_limiter: RateLimiterConfig::default(),
            // ... other default values
            debug: false,
            port: 6001,
            host: "0.0.0.0".to_string(),
            path_prefix: "/".to_string(),
            shutdown_grace_period: 10,
            // Initialize other fields with sensible defaults
            ..Default::default() // This requires implementing Default for all sub-structs
        }
    }
}

impl Default for SqsQueueConfig {
    fn default() -> Self {
        Self {
            region: "us-east-1".to_string(),
            queue_url_prefix: None,
            visibility_timeout: 30,
            endpoint_url: None,
            max_messages: 10,
            wait_time_seconds: 5,
            concurrency: 5,
            fifo: false,
            message_group_id: Some("default".to_string()),
        }
    }
}
