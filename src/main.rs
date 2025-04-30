mod adapter;
mod app;
mod cache;
mod channel;
mod error;
mod http_handler;
pub mod log;
mod metrics;
mod namespace;
mod options;
mod protocol;
mod rate_limiter;
mod token;
pub mod utils;
mod webhook;
mod websocket;
mod ws_handler;
mod queue;

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::Method;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{from_str, json, Value};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::{Mutex, RwLock};

use tower_http::cors::CorsLayer;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tracing_subscriber::fmt::format;
use crate::adapter::local_adapter::LocalAdapter;
use crate::adapter::redis_adapter::{RedisAdapter, RedisAdapterConfig};
use crate::adapter::Adapter;
use crate::adapter::ConnectionHandler;
use crate::app::auth::AuthValidator;
use crate::app::config::App;
use crate::app::manager::AppManager;
use crate::app::memory_app_manager::MemoryAppManager;
use crate::cache::manager::CacheManager;
use crate::cache::redis_cache_manager::{RedisCacheConfig, RedisCacheManager};
use crate::channel::ChannelManager;
use crate::error::Result;
use crate::http_handler::{batch_events, channel, channel_users, channels, events, metrics, terminate_user_connections, up, usage};
use crate::log::Log;
use crate::metrics::{MetricsFactory, MetricsInterface};
use crate::ws_handler::handle_ws_upgrade;

/// Server configuration struct
#[derive(Clone)]
struct ServerConfig {
    http_addr: SocketAddr,
    metrics_addr: SocketAddr,
    debug: bool,
    // Additional configuration options can be added here
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            http_addr: "127.0.0.1:6001".parse().unwrap(),
            metrics_addr: "127.0.0.1:9601".parse().unwrap(),
            debug: false,
        }
    }
}

/// Server state containing all managers
#[derive(Clone)]
struct ServerState {
    app_manager: Arc<dyn AppManager + Send + Sync>,
    channel_manager: Arc<RwLock<ChannelManager>>,
    connection_manager: Arc<Mutex<Box<dyn Adapter + Send + Sync>>>,
    auth_validator: Arc<AuthValidator>,
    cache_manager: Arc<Mutex<dyn CacheManager + Send + Sync>>,
    metrics: Option<Arc<Mutex<dyn MetricsInterface + Send + Sync>>>,
    running: Arc<AtomicBool>,
}

/// Main server struct
struct SockudoServer {
    config: ServerConfig,
    state: ServerState,
    handler: Arc<ConnectionHandler>,
}

impl SockudoServer {
    /// Create a new server instance
    async fn new(config: ServerConfig) -> Result<Self> {
        // Initialize app manager
        let app_manager = Arc::new(MemoryAppManager::new());

        // Initialize Redis adapter with configuration
        let adapter_config = RedisAdapterConfig::default();
        let connection_manager: Box<dyn Adapter + Send + Sync> =
            match RedisAdapter::new(adapter_config).await {
                Ok(adapter) => {
                    info!("Using Redis adapter");
                    Box::new(adapter)
                }
                Err(e) => {
                    warn!(
                        "Failed to initialize Redis adapter: {}, falling back to local adapter",
                        e
                    );
                    Box::new(LocalAdapter::new())
                }
            };

        // Initialize the connection manager
        let connection_manager = Arc::new(Mutex::new(connection_manager));

        // Initialize cache manager
        let cache_manager = RedisCacheManager::new(RedisCacheConfig::default()).await?;

        // Initialize channel manager
        let channel_manager =
            Arc::new(RwLock::new(ChannelManager::new(connection_manager.clone())));

        // Initialize auth validator
        let auth_validator = Arc::new(AuthValidator::new(app_manager.clone()));

        // Initialize metrics
        let metrics_driver = Arc::new(Mutex::new(
            metrics::PrometheusMetricsDriver::new(config.metrics_addr.port(), Some("sockudo"))
                .await,
        ));

        // Initialize the state
        let state = ServerState {
            app_manager: app_manager.clone(),
            channel_manager: channel_manager.clone(),
            connection_manager: connection_manager.clone(),
            auth_validator,
            cache_manager: Arc::new(Mutex::new(cache_manager)),
            metrics: Some(metrics_driver.clone()),
            running: Arc::new(AtomicBool::new(true)),
        };

        // Create connection handler
        let handler = Arc::new(ConnectionHandler::new(
            state.app_manager.clone(),
            state.channel_manager.clone(),
            state.connection_manager.clone(),
            state.cache_manager.clone(),
            state.metrics.clone(),
        ));

        Ok(Self {
            config,
            state,
            handler,
        })
    }

    /// Initialize the server
    async fn init(&self) -> Result<()> {
        // Initialize adapter
        {
            let mut connection_manager = self.state.connection_manager.lock().await;
            connection_manager.init().await;
        }

        // Register demo app (in a real environment, this would be loaded from config)
        let demo_app = App {
            id: "demo-app".to_string(),
            key: "demo-key".to_string(),
            secret: "demo-secret".to_string(),
            enable_client_messages: true,
            enabled: true,
            ..Default::default()
        };

        self.state.app_manager.register_app(demo_app).await?;

        info!("Server initialized successfully");

        Ok(())
    }

    /// Configure routes for the HTTP server
    fn configure_http_routes(&self) -> Router {
        // Create CORS layer
        let cors = CorsLayer::new()
            .allow_origin("*".parse::<HeaderValue>().unwrap())
            .allow_methods([Method::GET, Method::POST])
            .allow_headers([AUTHORIZATION, CONTENT_TYPE]);

        // Create application routes
        Router::new()
            // WebSocket handler for Pusher protocol
            .route("/app/{key}", get(handle_ws_upgrade))
            // HTTP API endpoints
            .route("/apps/{appId}/events", post(events))
            .route("/apps/{appId}/batch_events", post(batch_events))
            .route("/apps/{app_id}/channels", get(channels))
            .route("/apps/{app_id}/channels/{channel_name}", get(channel))
            .route("/apps/{app_id}/channels/{channel_name}/users", get(channel_users))
            .route(
                "/apps/{app_id}/users/{user_id}/terminate_connections",
                post(terminate_user_connections),
            )
            .route("/usage", get(usage))
            .route("/up/{app_id}", get(up))
            // Apply CORS middleware
            .layer(cors)
            .with_state(self.handler.clone())
    }

    /// Configure routes for the metrics server
    fn configure_metrics_routes(&self) -> Router {
        Router::new()
            .route("/metrics", get(metrics))
            .with_state(self.handler.clone())
    }

    /// Start the server
    async fn start(&self) -> Result<()> {
        info!("Starting Sockudo server...");

        // Initialize server components
        self.init().await?;

        // Configure HTTP router
        let http_router = self.configure_http_routes();

        // Configure metrics router
        let metrics_router = self.configure_metrics_routes();

        // Create TCP listeners
        let http_listener = TcpListener::bind(self.config.http_addr).await?;
        let metrics_listener = TcpListener::bind(self.config.metrics_addr).await?;

        info!("HTTP server listening on {}", self.config.http_addr);
        info!("Metrics server listening on {}", self.config.metrics_addr);

        // Spawn HTTP server
        let http_server = axum::serve(http_listener, http_router);
        let metrics_server = axum::serve(metrics_listener, metrics_router);

        // Get shutdown signal
        let shutdown_signal = self.shutdown_signal();

        // Spawn servers with graceful shutdown
        let running = self.state.running.clone();
        tokio::select! {
            res = http_server => {
                if let Err(err) = res {
                    error!("HTTP server error: {}", err);
                }
            }
            res = metrics_server => {
                if let Err(err) = res {
                    error!("Metrics server error: {}", err);
                }
            }
            _ = shutdown_signal => {
                info!("Shutdown signal received");
                running.store(false, Ordering::SeqCst);
            }
        }

        info!("Server shutting down");

        Ok(())
    }

    /// Graceful shutdown handler
    async fn shutdown_signal(&self) {
        let ctrl_c = async {
            signal::ctrl_c()
                .await
                .expect("Failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("Failed to install signal handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }

        info!("Shutdown signal received, starting graceful shutdown");
    }

    /// Stop the server
    async fn stop(&self) -> Result<()> {
        info!("Stopping server...");

        // Signal server to stop
        self.state.running.store(false, Ordering::SeqCst);

        // Wait for ongoing operations to complete
        tokio::time::sleep(Duration::from_secs(1)).await;

        // Perform cleanup of connections/resources
        {
            let mut connection_manager = self.state.connection_manager.lock().await;
        }

        // Close cache connections
        {
            let cache_manager = self.state.cache_manager.lock().await;
            // Add cache manager disconnect code here if needed
        }

        info!("Server stopped");

        Ok(())
    }

    pub async fn load_options_from_file<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let mut file = File::open(path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        let options: HashMap<String, Value> = from_str(&contents)?;
        self.set_options(options).await
    }

    /// Set options using dot notation
    pub async fn set_options(&mut self, options: HashMap<String, Value>) -> Result<()> {
        for (key, value) in options {
            // Special handling for app IDs to ensure they're strings
            if key.starts_with("app_manager.array.apps.") && key.contains(".id") {
                if let Value::Number(num) = &value {
                    if num.is_i64() || num.is_u64() {
                        let str_value = value.to_string();
                        self.set_option(&key, Value::String(str_value)).await?;
                        continue;
                    }
                }
            }

            self.set_option(&key, value).await?;
        }

        Ok(())
    }

    /// Set a single option using dot notation
    async fn set_option(&mut self, path: &str, value: Value) -> Result<()> {
        let parts: Vec<&str> = path.split('.').collect();

        // Handle top-level options
        match parts[0] {
            "debug" => {
                if let Value::Bool(val) = value {
                    self.config.debug = val;
                }
            }
            "host" => {
                if let Value::String(val) = value.clone() {
                    let port = self.config.http_addr.port();
                    match format!("{}:{}", val, port).parse() {
                        Ok(addr) => self.config.http_addr = addr,
                        Err(e) => return Err(error::Error::Config(format!("Invalid host: {}", e))),
                    }
                }
            }
            "port" => {
                if let Value::Number(val) = value.clone() {
                    if let Some(port) = val.as_u64() {
                        let host = self.config.http_addr.ip();
                        self.config.http_addr = std::net::SocketAddr::new(host, port as u16);
                    }
                }
            }
            "app_manager" => {
                self.handle_app_manager_options(&parts[1..], value).await?;
            }
            "adapter" => {
                self.handle_adapter_options(&parts[1..], value)?;
            }
            "cache" => {
                self.handle_cache_options(&parts[1..], value)?;
            }
            // Add more top-level options as needed
            _ => {
                // Unknown option, log a warning
                warn!("Unknown option: {}", path);
            }
        }

        Ok(())
    }

    /// Handle app manager specific options
    async fn handle_app_manager_options(&mut self, parts: &[&str], value: Value) -> Result<()> {
        if parts.is_empty() {
            return Ok(());
        }

        match parts[0] {
            "array" => {
                if parts.len() > 1 && parts[1] == "apps" {
                    // Handle apps configuration
                    self.handle_apps_configuration(&parts[2..], value).await?;
                }
            }
            "driver" => {
                // Handle driver type change (e.g., memory, mysql, dynamodb)
                if let Value::String(driver) = value {
                    // Implementation would depend on how you want to handle driver changes
                    info!("Changing app manager driver to: {}", driver);
                    // Actual implementation would reinitialize the app manager
                }
            }
            // Add more app manager options
            _ => warn!("Unknown app_manager option: {}", parts[0]),
        }

        Ok(())
    }

    /// Handle apps configuration
    async fn handle_apps_configuration(&mut self, parts: &[&str], value: Value) -> Result<()> {
        println!("{:?}, {:?}", parts, value);
        if parts.is_empty() {
            // Handle setting entire apps array
            if let Value::Array(apps_array) = value {
                // Convert JSON array to Vec<App>
                let mut apps = Vec::new();

                for app_value in apps_array {
                    match serde_json::from_value::<App>(app_value.clone())  {
                        Ok(app) => {
                            
                        }
                        Err(e) => {
                            println!("Invalid app configuration: {}", e);
                        }
                    }
                    if let Ok(app) = serde_json::from_value::<App>(app_value) {
                        apps.push(app);
                    } else {
                        warn!("Invalid app configuration");
                    }
                }

                // Register all apps
                self.register_apps(apps).await?;
            }

            return Ok(());
        }

        // Handle individual app settings
        // parts[0] should be the index of the app
        if let Some(index_str) = parts.first() {
            if let Ok(index) = index_str.parse::<usize>() {
                // Handle operations on a specific app
                if parts.len() > 1 {
                    // Get the app from the manager, modify it, and update it
                    let apps = self.state.app_manager.get_apps().await?;

                    if index < apps.len() {
                        let mut app = apps[index].clone();

                        // Modify the specific field
                        match parts[1] {
                            "id" => {
                                if let Value::String(id) = value.clone() {
                                    app.id = id;
                                } else if let Value::Number(num) = value.clone() {
                                    app.id = num.to_string();
                                }
                            }
                            "key" => {
                                if let Value::String(key) = value.clone() {
                                    app.key = key;
                                }
                            }
                            "secret" => {
                                if let Value::String(secret) = value.clone() {
                                    app.secret = secret;
                                }
                            }
                            "enable_client_messages" => {
                                if let Value::Bool(enable) = value {
                                    app.enable_client_messages = enable;
                                }
                            }
                            "enabled" => {
                                if let Value::Bool(enabled) = value {
                                    app.enabled = enabled;
                                }
                            }
                            // Add more fields as needed
                            _ => warn!("Unknown app field: {}", parts[1]),
                        }

                        // Update the app
                        self.state.app_manager.update_app(app).await?;
                    } else {
                        warn!("App index out of bounds: {}", index);
                    }
                }
            } else {
                warn!("Invalid app index: {}", index_str);
            }
        }

        Ok(())
    }

    /// Handle adapter specific options
    fn handle_adapter_options(&mut self, parts: &[&str], value: Value) -> Result<()> {
        if parts.is_empty() {
            return Ok(());
        }

        match parts[0] {
            "driver" => {
                // Handle adapter driver change
                if let Value::String(driver) = value {
                    info!("Adapter driver change requested to: {}", driver);
                    // Implementation would depend on how you want to handle adapter changes
                }
            }
            "redis" => {
                // Handle Redis adapter options
                if parts.len() > 1 {
                    self.handle_redis_adapter_options(&parts[1..], value)?;
                }
            }
            // Add more adapter types
            _ => warn!("Unknown adapter option: {}", parts[0]),
        }

        Ok(())
    }

    /// Handle Redis adapter options
    fn handle_redis_adapter_options(&mut self, parts: &[&str], value: Value) -> Result<()> {
        if parts.is_empty() {
            return Ok(());
        }

        match parts[0] {
            "prefix" => {
                if let Value::String(prefix) = value {
                    // Update Redis adapter prefix
                    // This would require modifying the adapter configuration
                    info!("Updating Redis adapter prefix to: {}", prefix);
                }
            }
            "requests_timeout" => {
                if let Value::Number(timeout) = value {
                    if let Some(timeout_ms) = timeout.as_u64() {
                        // Update Redis adapter timeout
                        info!("Updating Redis adapter timeout to: {}ms", timeout_ms);
                    }
                }
            }
            // Add more Redis options
            _ => warn!("Unknown Redis adapter option: {}", parts[0]),
        }

        Ok(())
    }

    /// Handle cache specific options
    fn handle_cache_options(&mut self, parts: &[&str], value: Value) -> Result<()> {
        if parts.is_empty() {
            return Ok(());
        }

        match parts[0] {
            "driver" => {
                // Handle cache driver change
                if let Value::String(driver) = value {
                    info!("Cache driver change requested to: {}", driver);
                    // Implementation would depend on how you want to handle cache driver changes
                }
            }
            "redis" => {
                // Handle Redis cache options
                if parts.len() > 1 {
                    self.handle_redis_cache_options(&parts[1..], value)?;
                }
            }
            // Add more cache options
            _ => warn!("Unknown cache option: {}", parts[0]),
        }

        Ok(())
    }

    /// Handle Redis cache options
    fn handle_redis_cache_options(&mut self, parts: &[&str], value: Value) -> Result<()> {
        if parts.is_empty() {
            return Ok(());
        }

        match parts[0] {
            "url" => {
                if let Value::String(url) = value {
                    // Update Redis cache URL
                    info!("Updating Redis cache URL to: {}", url);
                }
            }
            "prefix" => {
                if let Value::String(prefix) = value {
                    // Update Redis cache prefix
                    info!("Updating Redis cache prefix to: {}", prefix);
                }
            }
            // Add more Redis cache options
            _ => warn!("Unknown Redis cache option: {}", parts[0]),
        }

        Ok(())
    }

    /// Register multiple apps
    async fn register_apps(&self, apps: Vec<App>) -> Result<()> {
        for app in apps {
            // First check if the app already exists
            let existing_app = self.state.app_manager.get_app(&app.id).await?;

            if let Some(_) = existing_app {
                // App exists, update it
                self.state.app_manager.update_app(app).await?;
            } else {
                // New app, register it
                self.state.app_manager.register_app(app).await?;
            }
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Get configuration from environment
    let config = ServerConfig {
        http_addr: std::env::var("HTTP_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:6001".to_string())
            .parse()
            .expect("Invalid HTTP bind address"),
        metrics_addr: std::env::var("METRICS_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:9601".to_string())
            .parse()
            .expect("Invalid metrics bind address"),
        debug: std::env::var("DEBUG")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false),
    };

    // Create and start the server
    let mut server = SockudoServer::new(config).await?;

    server.load_options_from_file("src/config.json").await?;

    // // Or set options programmatically
    // let mut options = HashMap::new();
    // options.insert("app_manager.array.apps.0.id".to_string(), json!("demo-app"));
    // options.insert("app_manager.array.apps.0.key".to_string(), json!("demo-key"));
    // options.insert("app_manager.array.apps.0.secret".to_string(), json!("demo-secret"));
    // options.insert("app_manager.array.apps.0.enable_client_messages".to_string(), json!(true));
    // options.insert("port".to_string(), json!(6001));
    // options.insert("debug".to_string(), json!(true));

    // server.set_options(options).await?;


    // Start the server and await completion
    server.start().await?;

    Ok(())
}
