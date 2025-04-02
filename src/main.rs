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
mod token;
mod websocket;
mod ws_handler;

use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::routing::post;
use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::adapter::local_adapter::LocalAdapter;
use crate::adapter::redis_adapter::{RedisAdapter, RedisAdapterConfig};
use crate::app::auth::AuthValidator;
use crate::app::config::App;
use crate::app::manager::AppManager;
use crate::http_handler::{batch_events, channel, events, terminate_user_connections, usage};
use crate::ws_handler::handle_ws_upgrade;
use crate::{
    adapter::{Adapter, ConnectionHandler},
    channel::ChannelManager,
    error::Result,
};

// Server state containing all managers
#[derive(Clone)]
struct ServerState {
    app_manager: Arc<AppManager>,
    channel_manager: Arc<RwLock<ChannelManager>>,
    connection_manager: Arc<Mutex<Box<dyn Adapter + Send + Sync>>>,
    auth_validator: Arc<AuthValidator>,
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

    let app_manager = Arc::new(AppManager::new());
    let config = RedisAdapterConfig::default();
    let mut connection_manager = match RedisAdapter::new(config).await {
        Ok(adapter) => {
            tracing::info!("Using Redis adapter");
            adapter
        }
        Err(_) => todo!(),
    };
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
    app_manager.register_app(demo_app);

    // Create server state
    let state = ServerState {
        app_manager,
        channel_manager,
        connection_manager,
        auth_validator,
    };

    // Create adapter handler
    let handler = Arc::new(ConnectionHandler::new(
        state.app_manager.clone(),
        state.channel_manager.clone(),
        state.connection_manager.clone(),
    ));

    // Setup CORS
    let cors = CorsLayer::new()
        .allow_origin("*".parse::<HeaderValue>().unwrap())
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([AUTHORIZATION, CONTENT_TYPE]);

    // Create router
    let app = Router::new()
        // WebSocket handler for Pusher protocol
        .route("/app/{key}", get(handle_ws_upgrade))
        .route("/apps/{appId}/events", post(events))
        .route("/apps/{appId}/batch_events", post(batch_events))
        .route("/apps/{app_id}/channels/{channel_name}", get(channel))
        .route(
            "/apps/{app_id}/users/{user_id}/terminate_connections",
            post(terminate_user_connections),
        )
        .route("/usage", get(usage))
        .layer(cors)
        .with_state(handler.clone());

    // Get the bind address
    let addr: String = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:6001".to_string())
        .parse()
        .expect("Invalid bind address");
    tracing::info!("Starting server on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

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
