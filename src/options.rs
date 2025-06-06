use crate::app::config::App;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
// Assuming DEFAULT_PREFIX is pub const in nats_adapter or imported appropriately
use crate::adapter::nats_adapter::DEFAULT_PREFIX as NATS_DEFAULT_PREFIX;
use crate::adapter::redis_cluster_adapter::DEFAULT_PREFIX as REDIS_CLUSTER_DEFAULT_PREFIX;

// --- Enums for Driver Types ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AdapterDriver {
    #[default]
    Local,
    Redis,
    #[serde(rename = "redis-cluster")]
    RedisCluster,
    Nats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DynamoDbSettings {
    pub region: String,
    pub table_name: String,
    pub endpoint_url: Option<String>, // For local testing
    pub aws_access_key_id: Option<String>,
    pub aws_secret_access_key: Option<String>,
    pub aws_profile_name: Option<String>,
    // cache_ttl for app_manager is already in AppManagerConfig.cache.ttl
}

impl Default for DynamoDbSettings {
    fn default() -> Self {
        Self {
            region: "us-east-1".to_string(),
            table_name: "sockudo-applications".to_string(), // Default table name
            endpoint_url: None,
            aws_access_key_id: None,
            aws_secret_access_key: None,
            aws_profile_name: None,
        }
    }
}

impl std::str::FromStr for AdapterDriver {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "local" => Ok(AdapterDriver::Local),
            "redis" => Ok(AdapterDriver::Redis),
            "redis-cluster" => Ok(AdapterDriver::RedisCluster),
            "nats" => Ok(AdapterDriver::Nats),
            _ => Err(format!("Unknown adapter driver: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AppManagerDriver {
    #[default]
    Memory,
    Mysql,
    Dynamodb,
    PgSql, // Added PostgreSQL as a potential driver
}
impl std::str::FromStr for AppManagerDriver {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "memory" => Ok(AppManagerDriver::Memory),
            "mysql" => Ok(AppManagerDriver::Mysql),
            "dynamodb" => Ok(AppManagerDriver::Dynamodb),
            _ => Err(format!("Unknown app manager driver: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CacheDriver {
    #[default]
    Memory,
    Redis,
    #[serde(rename = "redis-cluster")]
    RedisCluster,
    None,
}

impl std::str::FromStr for CacheDriver {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "memory" => Ok(CacheDriver::Memory),
            "redis" => Ok(CacheDriver::Redis),
            "redis-cluster" => Ok(CacheDriver::RedisCluster),
            "none" => Ok(CacheDriver::None),
            _ => Err(format!("Unknown cache driver: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum QueueDriver {
    Memory,
    #[default]
    Redis,
    #[serde(rename = "redis-cluster")] // Add this variant
    RedisCluster,
    Sqs,
    None,
}

impl std::str::FromStr for QueueDriver {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "memory" => Ok(QueueDriver::Memory),
            "redis" => Ok(QueueDriver::Redis),
            "redis-cluster" => Ok(QueueDriver::RedisCluster), // Add this case
            "sqs" => Ok(QueueDriver::Sqs),
            "none" => Ok(QueueDriver::None),
            _ => Err(format!("Unknown queue driver: {}", s)),
        }
    }
}

// Corrected: Added AsRef<str> implementation for QueueDriver
impl AsRef<str> for QueueDriver {
    fn as_ref(&self) -> &str {
        match self {
            QueueDriver::Memory => "memory",
            QueueDriver::Redis => "redis",
            QueueDriver::RedisCluster => "redis-cluster", // Add this case
            QueueDriver::Sqs => "sqs",
            QueueDriver::None => "none",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RedisClusterQueueConfig {
    pub concurrency: u32,
    pub prefix: Option<String>,
    pub nodes: Vec<String>, // Cluster node URLs
    pub request_timeout_ms: u64,
}

impl Default for RedisClusterQueueConfig {
    fn default() -> Self {
        Self {
            concurrency: 5,
            prefix: Some("sockudo_queue:".to_string()),
            nodes: vec!["redis://127.0.0.1:6379".to_string()],
            request_timeout_ms: 5000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum MetricsDriver {
    #[default]
    Prometheus,
}

impl std::str::FromStr for MetricsDriver {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "prometheus" => Ok(MetricsDriver::Prometheus),
            _ => Err(format!("Unknown metrics driver: {}", s)),
        }
    }
}

// Corrected: Added AsRef<str> implementation for MetricsDriver
impl AsRef<str> for MetricsDriver {
    fn as_ref(&self) -> &str {
        match self {
            MetricsDriver::Prometheus => "prometheus",
        }
    }
}

// --- Main Configuration Struct ---
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerOptions {
    pub adapter: AdapterConfig,
    pub app_manager: AppManagerConfig,
    pub cache: CacheConfig,
    pub channel_limits: ChannelLimits,
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
    pub websocket_max_payload_kb: u32,
}

// --- Configuration Sub-Structs ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SqsQueueConfig {
    pub region: String,
    pub queue_url_prefix: Option<String>,
    pub visibility_timeout: i32,
    pub endpoint_url: Option<String>,
    pub max_messages: i32,
    pub wait_time_seconds: i32,
    pub concurrency: u32,
    pub fifo: bool,
    pub message_group_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdapterConfig {
    pub driver: AdapterDriver,
    pub redis: RedisAdapterConfig,
    pub cluster: RedisClusterAdapterConfig,
    pub nats: NatsAdapterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RedisAdapterConfig {
    pub requests_timeout: u64,
    pub prefix: String,
    pub redis_pub_options: HashMap<String, serde_json::Value>,
    pub redis_sub_options: HashMap<String, serde_json::Value>,
    pub cluster_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RedisClusterAdapterConfig {
    pub nodes: Vec<String>,
    pub prefix: String,
    pub request_timeout_ms: u64,
    pub use_connection_manager: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NatsAdapterConfig {
    pub servers: Vec<String>,
    pub prefix: String,
    pub request_timeout_ms: u64,
    pub username: Option<String>,
    pub password: Option<String>,
    pub token: Option<String>,
    pub connection_timeout_ms: u64,
    pub nodes_number: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppManagerConfig {
    pub driver: AppManagerDriver,
    pub array: ArrayConfig,
    pub cache: CacheSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ArrayConfig {
    pub apps: Vec<App>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheSettings {
    pub enabled: bool,
    pub ttl: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryCacheOptions {
    pub ttl: u64,
    pub cleanup_interval: u64,
    pub max_capacity: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    pub driver: CacheDriver,
    pub redis: RedisConfig,
    pub memory: MemoryCacheOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RedisConfig {
    pub prefix: Option<String>,
    pub url_override: Option<String>,
    pub cluster_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelLimits {
    pub max_name_length: u32,
    pub cache_ttl: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CorsConfig {
    pub credentials: bool,
    pub origin: Vec<String>,
    pub methods: Vec<String>,
    pub allowed_headers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DatabaseConfig {
    pub mysql: DatabaseConnection,
    pub postgres: DatabaseConnection,
    pub redis: RedisConnection,
    pub dynamodb: DynamoDbSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseConnection {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: String,
    pub table_name: String,
    pub connection_pool_size: u32,
    pub cache_ttl: u64,
    pub cache_cleanup_interval: u64,
    pub cache_max_capacity: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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
#[serde(default)]
pub struct RedisSentinel {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClusterNode {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabasePooling {
    pub enabled: bool,
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EventLimits {
    pub max_channels_at_once: u32,
    pub max_name_length: u32,
    pub max_payload_in_kb: u32,
    pub max_batch_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpApiConfig {
    pub request_limit_in_mb: u32,
    pub accept_traffic: AcceptTraffic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AcceptTraffic {
    pub memory_threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InstanceConfig {
    pub process_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub driver: MetricsDriver,
    pub host: String,
    pub prometheus: PrometheusConfig,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrometheusConfig {
    pub prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PresenceConfig {
    pub max_members_per_channel: u32,
    pub max_member_size_in_kb: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QueueConfig {
    pub driver: QueueDriver,
    pub redis: RedisQueueConfig,
    pub redis_cluster: RedisClusterQueueConfig, // Add this field
    pub sqs: SqsQueueConfig,
}

// Updated RedisQueueConfig for type safety
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RedisQueueConfig {
    pub concurrency: u32,
    pub prefix: Option<String>, // Optional prefix for this specific queue
    pub url_override: Option<String>, // Optional URL to override the global DatabaseConfig.redis.url
    pub cluster_mode: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimit {
    pub max_requests: u32,
    pub window_seconds: u64,
    pub identifier: Option<String>,
    pub trust_hops: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimiterConfig {
    pub enabled: bool,
    pub driver: CacheDriver, // Rate limiter backend often uses a cache driver
    pub api_rate_limit: RateLimit,
    pub websocket_rate_limit: RateLimit,
    pub redis: RedisConfig, // Specific Redis settings if Redis is chosen as backend
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SslConfig {
    pub enabled: bool,
    pub cert_path: String,
    pub key_path: String,
    pub passphrase: Option<String>,
    pub ca_path: Option<String>,
    pub redirect_http: bool,
    pub http_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WebhooksConfig {
    pub batching: BatchingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BatchingConfig {
    pub enabled: bool,
    pub duration: u64, // ms
}

// --- Default Implementations ---

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            adapter: AdapterConfig::default(),
            app_manager: AppManagerConfig::default(),
            cache: CacheConfig::default(),
            channel_limits: ChannelLimits::default(),
            cors: CorsConfig::default(),
            database: DatabaseConfig::default(),
            database_pooling: DatabasePooling::default(),
            debug: false,
            event_limits: EventLimits::default(),
            host: "0.0.0.0".to_string(),
            http_api: HttpApiConfig::default(),
            instance: InstanceConfig::default(),
            metrics: MetricsConfig::default(),
            mode: "production".to_string(),
            port: 6001,
            path_prefix: "/".to_string(),
            presence: PresenceConfig::default(),
            queue: QueueConfig::default(),
            rate_limiter: RateLimiterConfig::default(),
            shutdown_grace_period: 10,
            ssl: SslConfig::default(),
            user_authentication_timeout: 3600,
            webhooks: WebhooksConfig::default(),
            websocket_max_payload_kb: 64,
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

impl Default for RedisAdapterConfig {
    fn default() -> Self {
        Self {
            requests_timeout: 5000,
            prefix: "sockudo_adapter:".to_string(),
            redis_pub_options: HashMap::new(),
            redis_sub_options: HashMap::new(),
            cluster_mode: false,
        }
    }
}

impl Default for RedisClusterAdapterConfig {
    fn default() -> Self {
        Self {
            nodes: vec![],
            prefix: REDIS_CLUSTER_DEFAULT_PREFIX.to_string(),
            request_timeout_ms: 5000,
            use_connection_manager: true,
        }
    }
}

impl Default for NatsAdapterConfig {
    fn default() -> Self {
        Self {
            servers: vec!["nats://localhost:4222".to_string()],
            prefix: NATS_DEFAULT_PREFIX.to_string(),
            request_timeout_ms: 5000,
            username: None,
            password: None,
            token: None,
            connection_timeout_ms: 5000,
            nodes_number: None,
        }
    }
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl: 300,
        }
    }
}

impl Default for MemoryCacheOptions {
    fn default() -> Self {
        Self {
            ttl: 300,
            cleanup_interval: 60,
            max_capacity: 10000,
        }
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            driver: CacheDriver::default(),
            redis: RedisConfig {
                prefix: Some("sockudo_cache:".to_string()),
                url_override: None,
                cluster_mode: false,
            },
            memory: MemoryCacheOptions::default(),
        }
    }
}

impl Default for ChannelLimits {
    fn default() -> Self {
        Self {
            max_name_length: 200,
            cache_ttl: 3600,
        }
    }
}
impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            credentials: true,
            origin: vec!["*".to_string()],
            methods: vec!["GET".to_string(), "POST".to_string(), "OPTIONS".to_string()],
            allowed_headers: vec![
                "Authorization".to_string(),
                "Content-Type".to_string(),
                "X-Requested-With".to_string(),
                "Accept".to_string(),
            ],
        }
    }
}

impl Default for DatabaseConnection {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 3306,
            username: "root".to_string(),
            password: "".to_string(),
            database: "sockudo".to_string(),
            table_name: "applications".to_string(),
            connection_pool_size: 10,
            cache_ttl: 300,
            cache_cleanup_interval: 60,
            cache_max_capacity: 100,
        }
    }
}

impl Default for RedisConnection {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 6379,
            db: 0,
            username: None,
            password: None,
            key_prefix: "sockudo:".to_string(),
            sentinels: Vec::new(),
            sentinel_password: None,
            name: "mymaster".to_string(),
            cluster_nodes: Vec::new(),
        }
    }
}

impl Default for RedisSentinel {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 26379,
        }
    }
}

impl Default for ClusterNode {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 7000,
        }
    }
}

impl Default for DatabasePooling {
    fn default() -> Self {
        Self {
            enabled: true,
            min: 2,
            max: 10,
        }
    }
}

impl Default for EventLimits {
    fn default() -> Self {
        Self {
            max_channels_at_once: 100,
            max_name_length: 200,
            max_payload_in_kb: 100,
            max_batch_size: 10,
        }
    }
}

impl Default for HttpApiConfig {
    fn default() -> Self {
        Self {
            request_limit_in_mb: 10,
            accept_traffic: AcceptTraffic::default(),
        }
    }
}

impl Default for AcceptTraffic {
    fn default() -> Self {
        Self {
            memory_threshold: 0.90,
        }
    }
}

impl Default for InstanceConfig {
    fn default() -> Self {
        Self {
            process_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            driver: MetricsDriver::default(),
            host: "0.0.0.0".to_string(),
            prometheus: PrometheusConfig::default(),
            port: 9601,
        }
    }
}

impl Default for PrometheusConfig {
    fn default() -> Self {
        Self {
            prefix: "sockudo_".to_string(),
        }
    }
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            max_members_per_channel: 100,
            max_member_size_in_kb: 2,
        }
    }
}
// Updated Default for new RedisQueueConfig structure
impl Default for RedisQueueConfig {
    fn default() -> Self {
        Self {
            concurrency: 5,
            prefix: Some("sockudo_queue:".to_string()), // Default prefix for queue
            url_override: None,
            cluster_mode: false,
        }
    }
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            max_requests: 60,
            window_seconds: 60,
            identifier: Some("default".to_string()),
            trust_hops: Some(0),
        }
    }
}

impl Default for RateLimiterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            driver: CacheDriver::Memory, // Default Rate Limiter backend to Memory
            api_rate_limit: RateLimit {
                max_requests: 100,
                window_seconds: 60,
                identifier: Some("api".to_string()),
                trust_hops: Some(0),
            },
            websocket_rate_limit: RateLimit {
                max_requests: 20,
                window_seconds: 60,
                identifier: Some("websocket_connect".to_string()),
                trust_hops: Some(0),
            },
            redis: RedisConfig {
                // Specific Redis settings if Redis is chosen as backend for rate limiting
                prefix: Some("sockudo_rl:".to_string()),
                url_override: None,
                cluster_mode: false,
            },
        }
    }
}

impl Default for SslConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cert_path: "".to_string(),
            key_path: "".to_string(),
            passphrase: None,
            ca_path: None,
            redirect_http: false,
            http_port: Some(80),
        }
    }
}

impl Default for BatchingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            duration: 50,
        }
    }
}

impl ServerOptions {
    pub async fn load_from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = tokio::fs::read_to_string(path).await?;
        let options: Self = serde_json::from_str(&content)?;
        Ok(options)
    }

    pub async fn override_from_env(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // --- General Settings ---
        if let Ok(mode) = std::env::var("ENVIRONMENT") {
            self.mode = mode;
        }
        if let Ok(debug) = std::env::var("DEBUG_MODE") {
            self.debug = debug.parse()?;
        }
        if let Ok(host) = std::env::var("HOST") {
            self.host = host;
        }
        if let Ok(port) = std::env::var("PORT") {
            self.port = port.parse()?;
        }
        if let Ok(grace_period) = std::env::var("SHUTDOWN_GRACE_PERIOD") {
            self.shutdown_grace_period = grace_period.parse()?;
        }
        if let Ok(timeout) = std::env::var("USER_AUTHENTICATION_TIMEOUT") {
            self.user_authentication_timeout = timeout.parse()?;
        }
        if let Ok(payload_kb) = std::env::var("WEBSOCKET_MAX_PAYLOAD_KB") {
            self.websocket_max_payload_kb = payload_kb.parse()?;
        }
        if let Ok(id) = std::env::var("INSTANCE_PROCESS_ID") {
            self.instance.process_id = id;
        }

        // --- Driver Configuration ---
        if let Ok(driver) = std::env::var("ADAPTER_DRIVER") {
            self.adapter.driver = AdapterDriver::from_str(&driver)?;
        }
        if let Ok(driver) = std::env::var("CACHE_DRIVER") {
            self.cache.driver = CacheDriver::from_str(&driver)?;
        }
        if let Ok(driver) = std::env::var("QUEUE_DRIVER") {
            self.queue.driver = QueueDriver::from_str(&driver)?;
        }
        if let Ok(driver) = std::env::var("APP_MANAGER_DRIVER") {
            self.app_manager.driver = AppManagerDriver::from_str(&driver)?;
        }
        if let Ok(driver) = std::env::var("RATE_LIMITER_DRIVER") {
            self.rate_limiter.driver = CacheDriver::from_str(&driver)?;
        }

        // --- Database: Redis ---
        if let Ok(host) = std::env::var("DATABASE_REDIS_HOST") {
            self.database.redis.host = host;
        }
        if let Ok(port) = std::env::var("DATABASE_REDIS_PORT") {
            self.database.redis.port = port.parse()?;
        }
        if let Ok(password) = std::env::var("DATABASE_REDIS_PASSWORD") {
            self.database.redis.password = Some(password);
        }
        if let Ok(db) = std::env::var("DATABASE_REDIS_DB") {
            self.database.redis.db = db.parse()?;
        }
        if let Ok(prefix) = std::env::var("DATABASE_REDIS_KEY_PREFIX") {
            self.database.redis.key_prefix = prefix;
        }

        // --- Database: MySQL ---
        if let Ok(host) = std::env::var("DATABASE_MYSQL_HOST") {
            self.database.mysql.host = host;
        }
        if let Ok(port) = std::env::var("DATABASE_MYSQL_PORT") {
            self.database.mysql.port = port.parse()?;
        }
        if let Ok(user) = std::env::var("DATABASE_MYSQL_USERNAME") {
            self.database.mysql.username = user;
        }
        if let Ok(pass) = std::env::var("DATABASE_MYSQL_PASSWORD") {
            self.database.mysql.password = pass;
        }
        if let Ok(db) = std::env::var("DATABASE_MYSQL_DATABASE") {
            self.database.mysql.database = db;
        }
        if let Ok(table) = std::env::var("DATABASE_MYSQL_TABLE_NAME") {
            self.database.mysql.table_name = table;
        }

        // --- Database: PostgreSQL ---
        if let Ok(host) = std::env::var("DATABASE_POSTGRES_HOST") {
            self.database.postgres.host = host;
        }
        if let Ok(port) = std::env::var("DATABASE_POSTGRES_PORT") {
            self.database.postgres.port = port.parse()?;
        }
        if let Ok(user) = std::env::var("DATABASE_POSTGRES_USERNAME") {
            self.database.postgres.username = user;
        }
        if let Ok(pass) = std::env::var("DATABASE_POSTGRES_PASSWORD") {
            self.database.postgres.password = pass;
        }
        if let Ok(db) = std::env::var("DATABASE_POSTGRES_DATABASE") {
            self.database.postgres.database = db;
        }

        // --- Database: DynamoDB ---
        if let Ok(region) = std::env::var("DATABASE_DYNAMODB_REGION") {
            self.database.dynamodb.region = region;
        }
        if let Ok(table) = std::env::var("DATABASE_DYNAMODB_TABLE_NAME") {
            self.database.dynamodb.table_name = table;
        }
        if let Ok(endpoint) = std::env::var("DATABASE_DYNAMODB_ENDPOINT_URL") {
            self.database.dynamodb.endpoint_url = Some(endpoint);
        }
        if let Ok(key_id) = std::env::var("AWS_ACCESS_KEY_ID") {
            self.database.dynamodb.aws_access_key_id = Some(key_id);
        }
        if let Ok(secret) = std::env::var("AWS_SECRET_ACCESS_KEY") {
            self.database.dynamodb.aws_secret_access_key = Some(secret);
        }

        // --- Redis Cluster ---
        if let Ok(nodes) = std::env::var("REDIS_CLUSTER_NODES") {
            let node_list: Vec<String> = nodes.split(',').map(|s| s.trim().to_string()).collect();
            self.adapter.cluster.nodes = node_list.clone();
            self.queue.redis_cluster.nodes = node_list;
        }
        if let Ok(concurrency) = std::env::var("REDIS_CLUSTER_QUEUE_CONCURRENCY") {
            self.queue.redis_cluster.concurrency = concurrency.parse()?;
        }
        if let Ok(prefix) = std::env::var("REDIS_CLUSTER_QUEUE_PREFIX") {
            self.queue.redis_cluster.prefix = Some(prefix);
        }

        // --- SSL Configuration ---
        if let Ok(enabled) = std::env::var("SSL_ENABLED") {
            self.ssl.enabled = enabled.parse()?;
        }
        if let Ok(path) = std::env::var("SSL_CERT_PATH") {
            self.ssl.cert_path = path;
        }
        if let Ok(path) = std::env::var("SSL_KEY_PATH") {
            self.ssl.key_path = path;
        }
        if let Ok(redirect) = std::env::var("SSL_REDIRECT_HTTP") {
            self.ssl.redirect_http = redirect.parse()?;
        }
        if let Ok(port) = std::env::var("SSL_HTTP_PORT") {
            self.ssl.http_port = Some(port.parse()?);
        }

        // --- Metrics ---
        if let Ok(enabled) = std::env::var("METRICS_ENABLED") {
            self.metrics.enabled = enabled.parse()?;
        }
        if let Ok(host) = std::env::var("METRICS_HOST") {
            self.metrics.host = host;
        }
        if let Ok(port) = std::env::var("METRICS_PORT") {
            self.metrics.port = port.parse()?;
        }
        if let Ok(prefix) = std::env::var("METRICS_PROMETHEUS_PREFIX") {
            self.metrics.prometheus.prefix = prefix;
        }

        // --- Rate Limiter ---
        if let Ok(enabled) = std::env::var("RATE_LIMITER_ENABLED") {
            self.rate_limiter.enabled = enabled.parse()?;
        }
        if let Ok(max) = std::env::var("RATE_LIMITER_API_MAX_REQUESTS") {
            self.rate_limiter.api_rate_limit.max_requests = max.parse()?;
        }
        if let Ok(window) = std::env::var("RATE_LIMITER_API_WINDOW_SECONDS") {
            self.rate_limiter.api_rate_limit.window_seconds = window.parse()?;
        }
        if let Ok(hops) = std::env::var("RATE_LIMITER_API_TRUST_HOPS") {
            self.rate_limiter.api_rate_limit.trust_hops = Some(hops.parse()?);
        }
        if let Ok(max) = std::env::var("RATE_LIMITER_WS_MAX_REQUESTS") {
            self.rate_limiter.websocket_rate_limit.max_requests = max.parse()?;
        }
        if let Ok(window) = std::env::var("RATE_LIMITER_WS_WINDOW_SECONDS") {
            self.rate_limiter.websocket_rate_limit.window_seconds = window.parse()?;
        }

        // --- Queue: Redis ---
        if let Ok(concurrency) = std::env::var("QUEUE_REDIS_CONCURRENCY") {
            self.queue.redis.concurrency = concurrency.parse()?;
        }
        if let Ok(prefix) = std::env::var("QUEUE_REDIS_PREFIX") {
            self.queue.redis.prefix = Some(prefix);
        }

        // --- Queue: SQS ---
        if let Ok(region) = std::env::var("QUEUE_SQS_REGION") {
            self.queue.sqs.region = region;
        }
        if let Ok(timeout) = std::env::var("QUEUE_SQS_VISIBILITY_TIMEOUT") {
            self.queue.sqs.visibility_timeout = timeout.parse()?;
        }
        if let Ok(max) = std::env::var("QUEUE_SQS_MAX_MESSAGES") {
            self.queue.sqs.max_messages = max.parse()?;
        }
        if let Ok(wait) = std::env::var("QUEUE_SQS_WAIT_TIME_SECONDS") {
            self.queue.sqs.wait_time_seconds = wait.parse()?;
        }
        if let Ok(concurrency) = std::env::var("QUEUE_SQS_CONCURRENCY") {
            self.queue.sqs.concurrency = concurrency.parse()?;
        }
        if let Ok(fifo) = std::env::var("QUEUE_SQS_FIFO") {
            self.queue.sqs.fifo = fifo.parse()?;
        }
        if let Ok(endpoint) = std::env::var("QUEUE_SQS_ENDPOINT_URL") {
            self.queue.sqs.endpoint_url = Some(endpoint);
        }

        // --- Webhooks ---
        if let Ok(enabled) = std::env::var("WEBHOOK_BATCHING_ENABLED") {
            self.webhooks.batching.enabled = enabled.parse()?;
        }
        if let Ok(duration) = std::env::var("WEBHOOK_BATCHING_DURATION") {
            self.webhooks.batching.duration = duration.parse()?;
        }

        // --- NATS Adapter ---
        if let Ok(servers) = std::env::var("NATS_SERVERS") {
            self.adapter.nats.servers = servers.split(',').map(|s| s.trim().to_string()).collect();
        }
        if let Ok(user) = std::env::var("NATS_USERNAME") {
            self.adapter.nats.username = Some(user);
        }
        if let Ok(pass) = std::env::var("NATS_PASSWORD") {
            self.adapter.nats.password = Some(pass);
        }
        if let Ok(token) = std::env::var("NATS_TOKEN") {
            self.adapter.nats.token = Some(token);
        }
        if let Ok(timeout) = std::env::var("NATS_CONNECTION_TIMEOUT_MS") {
            self.adapter.nats.connection_timeout_ms = timeout.parse()?;
        }
        if let Ok(timeout) = std::env::var("NATS_REQUEST_TIMEOUT_MS") {
            self.adapter.nats.request_timeout_ms = timeout.parse()?;
        }

        // --- CORS ---
        if let Ok(origins) = std::env::var("CORS_ORIGINS") {
            self.cors.origin = origins.split(',').map(|s| s.trim().to_string()).collect();
        }
        if let Ok(methods) = std::env::var("CORS_METHODS") {
            self.cors.methods = methods.split(',').map(|s| s.trim().to_string()).collect();
        }
        if let Ok(headers) = std::env::var("CORS_HEADERS") {
            self.cors.allowed_headers = headers.split(',').map(|s| s.trim().to_string()).collect();
        }
        if let Ok(credentials) = std::env::var("CORS_CREDENTIALS") {
            self.cors.credentials = credentials.parse()?;
        }

        // --- Performance Tuning ---
        if let Ok(size) = std::env::var("DATABASE_CONNECTION_POOL_SIZE") {
            let pool_size = size.parse()?;
            self.database.mysql.connection_pool_size = pool_size;
            self.database.postgres.connection_pool_size = pool_size;
        }
        if let Ok(ttl) = std::env::var("CACHE_TTL_SECONDS") {
            let cache_ttl = ttl.parse()?;
            self.app_manager.cache.ttl = cache_ttl;
            self.channel_limits.cache_ttl = cache_ttl;
            self.database.mysql.cache_ttl = cache_ttl;
            self.database.postgres.cache_ttl = cache_ttl;
            self.cache.memory.ttl = cache_ttl;
        }
        if let Ok(interval) = std::env::var("CACHE_CLEANUP_INTERVAL") {
            let cleanup_interval = interval.parse()?;
            self.database.mysql.cache_cleanup_interval = cleanup_interval;
            self.database.postgres.cache_cleanup_interval = cleanup_interval;
            self.cache.memory.cleanup_interval = cleanup_interval;
        }
        if let Ok(capacity) = std::env::var("CACHE_MAX_CAPACITY") {
            let max_capacity = capacity.parse()?;
            self.database.mysql.cache_max_capacity = max_capacity;
            self.database.postgres.cache_max_capacity = max_capacity;
            self.cache.memory.max_capacity = max_capacity;
        }

        // Note: SOCKUDO_DEFAULT_APP_* variables would require instantiating
        // the `App` struct and adding it to `self.app_manager.array.apps`.
        // This is omitted as the `App` struct definition is not provided.

        Ok(())
    }
}
