mod adapter;
mod app;
mod cache;
mod channel;
mod error;
mod http_handler;
pub mod log;
mod namespace;
mod options;
mod protocol;
mod rate_limiter;
mod token;
pub mod utils;
mod websocket;
mod ws_handler;
mod metrics;
mod webhook;

use std::collections::HashMap;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::routing::post;
use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use aws_sdk_dynamodb::types::KeyType::Hash;
use axum::response::Response;
use tokio::io::join;
use tokio::join;
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::adapter::local_adapter::LocalAdapter;
use crate::adapter::redis_adapter::{RedisAdapter, RedisAdapterConfig};
use crate::app::auth::AuthValidator;
use crate::app::config::App;
use crate::app::manager::AppManager;
use crate::app::memory_app_manager::MemoryAppManager;
use crate::cache::manager::CacheManager;
use crate::cache::redis_cache_manager::{RedisCacheConfig, RedisCacheManager};
use crate::http_handler::{batch_events, channel, channels, events, terminate_user_connections, up, usage};
use crate::ws_handler::handle_ws_upgrade;
use crate::{
    adapter::{Adapter, ConnectionHandler},
    channel::ChannelManager,
    error::Result,
};
use rate_limiter::{create_rate_limiter, middleware as rate_limit_middleware, RateLimitConfig};
use crate::log::Log;
use crate::metrics::{MetricsFactory, MetricsInterface, PrometheusMetricsDriver};

// Server state containing all managers
#[derive(Clone)]
struct ServerState {
    app_manager: Arc<dyn AppManager>,
    channel_manager: Arc<RwLock<ChannelManager>>,
    connection_manager: Arc<Mutex<Box<dyn Adapter + Send + Sync>>>,
    auth_validator: Arc<AuthValidator>,
    cache_manager: Arc<Mutex<dyn CacheManager + Send + Sync>>,
    metrics: Option<Arc<Mutex<dyn MetricsInterface + Send + Sync>>>,
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

    // Create managers

    let app_manager = Arc::new(MemoryAppManager::new());
    let config = RedisAdapterConfig::default();
    let mut connection_manager = match RedisAdapter::new(config.clone()).await {
        Ok(adapter) => {
            tracing::info!("Using Redis adapter");
            adapter
        }
        Err(_) => todo!(),
    };
    let cache_manager = RedisCacheManager::new(RedisCacheConfig::default())
        .await?;
    connection_manager.init().await;
    // let connection_manager = LocalAdapter::new();
    let connection_manager: Arc<Mutex<Box<dyn Adapter + Send + Sync>>> =
        Arc::new(Mutex::new(Box::new(connection_manager)));

    let channel_manager = Arc::new(RwLock::new(ChannelManager::new(connection_manager.clone())));
    let auth_validator = Arc::new(AuthValidator::new(app_manager.clone()));

    // Register demo app (you would typically load this from config)
    let demo_app = App {
        id: "demo-app".to_string(),
        key: "demo-key".to_string(),
        secret: "demo-secret".to_string(),
        enable_client_messages: true,
        enabled: true,
        ..Default::default()
    };
    app_manager.register_app(demo_app).await.expect("TODO: panic message");
    let metrics_driver = PrometheusMetricsDriver::new(9601, Some("sockudo")).await;
    let metrics_driver = Arc::new(Mutex::new(metrics_driver));

    // Create server state
    let state = ServerState {
        app_manager,
        channel_manager,
        connection_manager,
        auth_validator,
        cache_manager: Arc::new(Mutex::new(cache_manager)),
        metrics: Some(metrics_driver.clone()),
    };

    // Create adapter handler
    let handler = Arc::new(ConnectionHandler::new(
        state.app_manager.clone(),
        state.channel_manager.clone(),
        state.connection_manager.clone(),
        state.cache_manager.clone(),
        state.metrics.clone(),
    ));

    // Setup CORS
    let cors = CorsLayer::new()
        .allow_origin("*".parse::<HeaderValue>().unwrap())
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([AUTHORIZATION, CONTENT_TYPE]);
    let api_limiter = Arc::new(rate_limiter::memory_limiter::MemoryRateLimiter::new(
        60, // 60 requests
        60, // per minute
    ));

    // Create a Redis-based rate limiter for WebSocket connections
    let redis_client =
        redis::Client::open("redis://127.0.0.1:6379/").expect("Failed to create Redis client");

    let ws_limiter = Arc::new(
        rate_limiter::redis_limiter::RedisRateLimiter::new(
            redis_client,
            "sockudo".to_string(),
            10, // 10 connections
            60, // per minute
        )
        .await
        .expect("Failed to create Redis rate limiter"),
    );

    // Create router
    let http_server = Router::new()
        // WebSocket handler for Pusher protocol
        .route("/app/{key}", get(handle_ws_upgrade))
        .route("/apps/{appId}/events", post(events))
        .route("/apps/{appId}/batch_events", post(batch_events))
        .route("/apps/{app_id}/channels", get(channels))
        // .route_layer(rate_limit_middleware::with_path_limiter(
        //     api_limiter.clone(),
        //     rate_limit_middleware::RateLimitOptions {
        //         key_prefix: Some("apps_api".to_string()),
        //         ..Default::default()
        //     },
        // ))
        .route("/apps/{app_id}/channels/{channel_name}", get(channel))
        .route(
            "/apps/{app_id}/users/{user_id}/terminate_connections",
            post(terminate_user_connections),
        )
        .route("/usage", get(usage))
        .route("/up/{app_id}", get(up))
        // .route_layer(rate_limit_middleware::with_ip_limiter(
        //     api_limiter.clone(),
        //     rate_limit_middleware::RateLimitOptions {
        //         include_headers: true,
        //         fail_open: true,
        //         key_prefix: Some("api".to_string()),
        //     },
        // ))
        .layer(cors)
        .with_state(handler.clone());
    // Get the bind address
    let http_addr: String = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:6001".to_string())
        .parse()
        .expect("Invalid bind address");

    let metrics_server = Router::new()
        .route("/metrics", get(metrics))
        .with_state(handler.clone());
    let metrics_addr: String = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:9601".to_string())
        .parse()
        .expect("Invalid bind address");

    // Start the servers


    tracing::info!("Starting server on {}", http_addr);
    tracing::info!("Starting metrics server on {}", metrics_addr);
    let http_listener = tokio::net::TcpListener::bind(http_addr).await?;
    let metrics_listener = tokio::net::TcpListener::bind(metrics_addr).await?;
    let http_server = axum::serve(http_listener, http_server);
    let metrics_server = axum::serve(metrics_listener, metrics_server);

    join!(
        http_server,
        metrics_server,
    );


    Ok(())
}

// Query parameters for WebSocket adapter
#[derive(Debug, Deserialize)]
struct ConnectionQuery {
    protocol: Option<u8>,
    client: Option<String>,
    version: Option<String>,
}

// Authentication request payload
#[derive(Debug, Deserialize)]
struct AuthRequest {
    app_key: String,
    channel_name: String,
    socket_id: String,
    auth: String,
    user_data: Option<String>,
}

// Authentication response
#[derive(Debug, Serialize)]
struct AuthResponse {
    auth: String,
}

// Graceful shutdown handler
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
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

    tracing::info!("Shutdown signal received, starting graceful shutdown");
}

pub async fn metrics(State(handler): State<Arc<ConnectionHandler>>) -> Response<String> {
    Log::info("Metrics endpoint called");

    let metrics_data = match handler.metrics.clone() {
        Some(metrics_data) => {
            Log::info("Metrics endpoint");
            let metrics_data = metrics_data.lock().await;
            metrics_data.mark_horizontal_adapter_request_received("sockudo");
            metrics_data.get_metrics_as_plaintext().await
        },
        None => {
            return Response::builder()
                .status(200)
                .header("X-Custom-Foo", "Bar")
                .body("No metrics available".to_string())
                .unwrap();
        }
    };

    // Simple solution using a tuple
    Log::info(format!("Metrics: {:?}", metrics_data));
    let mut headers = HashMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4"),
    );
    let response = Response::builder()
        .status(200)
        .header("X-Custom-Foo", "Bar")
        .body(metrics_data)
        .unwrap();
    response
}
