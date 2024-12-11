mod app;
mod channel;
mod connection;
mod error;
pub mod log;
mod namespace;
mod protocol;
mod token;

use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::routing::post;
use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use fastwebsockets::upgrade;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use sysinfo::System;
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::app::auth::AuthValidator;
use crate::app::config::AppConfig;
use crate::app::manager::AppManager;
use crate::connection::state::SocketId;
use crate::log::Log;
use crate::protocol::messages::PusherApiMessage;
use crate::{
    channel::ChannelManager,
    connection::{ConnectionHandler, ConnectionManager},
    error::Result,
};
use connection::memory_manager::MemoryConnectionManager;
use crate::connection::redis_manager::RedisConnectionManager;

// Server state containing all managers
#[derive(Clone)]
struct ServerState {
    app_manager: Arc<AppManager>,
    channel_manager: Arc<RwLock<ChannelManager>>,
    connection_manager: Arc<Mutex<Box<dyn ConnectionManager + Send + Sync>>>,
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
    let connection_manager = RedisConnectionManager::new().await?;
    let app_manager = Arc::new(AppManager::new());
    let connection_manager: Arc<Mutex<Box<dyn ConnectionManager + Send + Sync>>> =
        Arc::new(Mutex::new(Box::new(connection_manager)));

    let channel_manager = Arc::new(RwLock::new(ChannelManager::new(connection_manager.clone())));
    let auth_validator = Arc::new(AuthValidator::new(app_manager.clone()));

    // Register demo app (you would typically load this from config)
    let demo_app = AppConfig {
        app_id: "demo-app".to_string(),
        key: "demo-key".to_string(),
        secret: "demo-secret".to_string(),
        enable_client_events: true,
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

    // Create connection handler
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
        .route("/app/:key", get(handle_ws_upgrade))
        .route("/apps/:appId/events", post(events))
        .route("/apps/:appId/batch_events", post(batch_events))
        .route("/apps/:app_id/channels/:channel_name", get(channel))
        .route(
            "/apps/:app_id/users/:user_id/terminate_connections",
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

// WebSocket upgrade handler
async fn handle_ws_upgrade(
    Path(app_key): Path<String>,
    Query(params): Query<ConnectionQuery>,
    ws: upgrade::IncomingUpgrade,
    State(handler): State<Arc<ConnectionHandler>>,
) -> impl IntoResponse {
    let (response, fut) = ws.upgrade().unwrap();
    tokio::task::spawn(async move {
        if let Err(e) = handler.handle_socket(fut, app_key).await {
            eprintln!("Error in websocket connection: {}", e);
        }
    });
    response
}

#[derive(Serialize)]
struct MemoryStats {
    free: u64,
    used: u64,
    total: u64,
    percent: f64,
}

#[derive(Serialize)]
struct UsageResponse {
    memory: MemoryStats,
}

pub async fn usage() -> impl IntoResponse {
    let mut sys = System::new_all();
    sys.refresh_all();

    // Get memory statistics
    let total = sys.total_memory() * 1024; // Convert to bytes
    let used = sys.used_memory() * 1024;
    let free = total - used;
    let percent = (used as f64 / total as f64) * 100.0;

    let memory_stats = MemoryStats {
        free,
        used,
        total,
        percent,
    };

    // Create response
    let response = UsageResponse {
        memory: memory_stats,
    };

    // Log memory usage
    tracing::info!(
        "Memory usage - Total: {} bytes, Used: {} bytes, Free: {} bytes, Usage: {:.2}%",
        total,
        used,
        free,
        percent
    );

    // Return JSON response
    Json(response)
}

#[derive(Deserialize, Serialize, Debug)]
struct EventQuery {
    auth_key: String,
    auth_timestamp: String,
    auth_version: String,
    body_md5: String,
    auth_signature: String,
}

async fn events(
    Path(app_id): Path<String>,
    Query(query): Query<EventQuery>,
    State(handler): State<Arc<ConnectionHandler>>,
    Json(event): Json<PusherApiMessage>,
) -> impl IntoResponse {
    let PusherApiMessage {
        name: _,
        data: _,
        channels,
        channel,
        socket_id,
    } = event.clone();
    let app = handler.app_manager.get_app(app_id.as_str());
    if app.is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "App not found"
            })),
        )
            .into_response();
    }
    let socket_id = socket_id.map(SocketId);
    match channels {
        Some(channels) => {
            for channel in channels {
                handler
                    .send_message(&app_id, socket_id.as_ref(), event.clone(), channel.as_str())
                    .await;
            }
        }
        None => {
            handler
                .send_message(
                    &app_id,
                    socket_id.as_ref(),
                    event.clone(),
                    channel.expect("REASON").as_str(),
                )
                .await;
        }
    }
    (StatusCode::OK, Json(json!({"ok": "true"}))).into_response()
}

pub async fn terminate_user_connections(
    Path(app_id): Path<String>,
    Path(user_id): Path<String>,
    State(handler): State<Arc<ConnectionHandler>>,
) -> impl IntoResponse {
    let connection_manager = handler.connection_manager.clone();
    connection_manager
        .lock()
        .await
        .terminate_connection(&app_id, &user_id)
        .await
        .expect("REASON");
    (
        StatusCode::OK,
        Json(json!({
            "ok": "true"
        })),
    )
        .into_response()
}

pub async fn batch_events(
    Path(app_id): Path<String>,
    State(handler): State<Arc<ConnectionHandler>>,
    Json(batch): Json<Vec<PusherApiMessage>>,
) -> impl IntoResponse {
    for message in batch.iter() {
        for channel in message.channels.clone().unwrap() {
            let socket_id = match message.clone().socket_id {
                Some(socket_id) => Some(SocketId(socket_id)),
                None => None,
            };
            handler
                .send_message(
                    &app_id,
                    socket_id.as_ref(),
                    message.clone(),
                    channel.as_str(),
                )
                .await;
        }
    }
    (
        StatusCode::OK,
        Json(json!({
            "ok": "true"
        })),
    )
        .into_response()
}

pub async fn channel(
    Path((app_id, channel_name)): Path<(String, String)>,
    State(handler): State<Arc<ConnectionHandler>>,
) -> impl IntoResponse {
    let response = handler
        .channel(app_id.as_str(), channel_name.as_str())
        .await;
    (StatusCode::OK, Json(response)).into_response()
}

// Query parameters for WebSocket connection
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
