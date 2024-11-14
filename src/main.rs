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
    extract::{Path, Query, State, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use fastwebsockets::upgrade;
use log::Log;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::app::auth::{AuthValidator, ChannelAuth};
use crate::app::config::AppConfig;
use crate::app::manager::AppManager;
use crate::connection::state::SocketId;
use crate::protocol::messages::{PusherApiMessage, PusherMessage};
use crate::{
    channel::ChannelManager,
    connection::{ConnectionHandler, ConnectionManager},
    error::{Error, Result},
};

// Server state containing all managers
#[derive(Clone)]
struct ServerState {
    app_manager: Arc<AppManager>,
    channel_manager: Arc<RwLock<ChannelManager>>,
    connection_manager: Arc<Mutex<ConnectionManager>>,
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
    let channel_manager = Arc::new(RwLock::new(ChannelManager::new()));
    let connection_manager = Arc::new(Mutex::new(ConnectionManager::new()));
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
        .layer(cors)
        .with_state(handler.clone());

    // Get the bind address
    let addr: String = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:6001".to_string())
        .parse()
        .expect("Invalid bind address");
    let mesage = json!({
        "message": "Server started",
        "address": addr
    });
    Log::info(mesage);
    // Start the server
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
    tracing::debug!("WebSocket upgrade request for app_key: {}", app_key);
    Log::info("Test");
    // Handle upgrade
    let (response, fut) = ws.upgrade().unwrap();
    tokio::task::spawn(async move {
        if let Err(e) = handler.handle_socket(fut, app_key).await {
            eprintln!("Error in websocket connection: {}", e);
        }
    });
    response
}
#[derive(Deserialize, Serialize)]
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
    let channel_manager = match handler.app_manager.get_channel_manager(&app_id) {
        Some(cm) => cm,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": "App not found"
                })),
            )
                .into_response();
        }
    };
    let event_name = event.clone().name.clone().unwrap();
    let event_data = event.clone().data.clone().unwrap();
    for channel in event.channels.unwrap() {
        channel_manager
            .write()
            .await
            .broadcast_to_channel(
                &app_id,
                &handler.connection_manager,
                &channel,
                event_name.clone(),
                serde_json::to_value(event_data.clone()).unwrap(),
                None,
            )
            .await
            .expect("REASON")
    }
    (
        StatusCode::OK,
        Json(json!({
            "ok": "true"
        })),
    )
        .into_response()
}

pub async fn terminate_user_connections(
    app_id: &str,
    user_id: &str,
    connection_manager: &mut Arc<Mutex<ConnectionManager>>,
    channel_manager: &Arc<RwLock<ChannelManager>>,
) {
    let socket_ids = connection_manager
        .lock()
        .await
        .get_user_connections(user_id, app_id);
    // Iterate over the socket IDs and terminate the connections
    for socket_id in socket_ids {
        connection_manager
            .lock()
            .await
            .terminate_connection(app_id, channel_manager, &socket_id)
            .await
            .expect("TODO: panic message");
    }
}

pub async fn batch_events(
    Path(app_id): Path<String>,
    State(handler): State<Arc<ConnectionHandler>>,
    Json(batch): Json<Vec<PusherApiMessage>>,
) -> impl IntoResponse {
    let channel_manager = match handler.app_manager.get_channel_manager(&app_id) {
        Some(cm) => cm,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": "App not found"
                })),
            )
                .into_response();
        }
    };
    for message in batch.iter() {
        let event_name = message.clone().name.clone().unwrap();
        let event_data = message.clone().data.clone().unwrap();
        for channel in message.channels.clone().unwrap() {
            channel_manager
                .write()
                .await
                .broadcast_to_channel(
                    &app_id,
                    &handler.connection_manager,
                    &channel,
                    event_name.clone(),
                    serde_json::to_value(event_data.clone()).unwrap(),
                    None,
                )
                .await
                .expect("REASON")
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

// Authentication handler for private and presence channels
// async fn handle_auth(
//     State(state): State<ServerState>,
//     Json(auth_request): Json<AuthRequest>,
// ) -> Result<impl IntoResponse> {
//     let is_valid = state
//         .auth_validator
//         .validate_channel_auth(
//             SocketId(auth_request.socket_id.clone()),
//             &auth_request.app_key,
//             auth_request.user_data.clone().unwrap_or_default(),
//             auth_request.auth.clone(),
//         )
//         .await?;

//     if is_valid {
//         state
//             .connection_manager
//             .lock()
//             .await
//             .add_connection(&auth_request.app_key);
//     }

//     Ok((
//         StatusCode::OK,
//         Json(AuthResponse {
//             auth: "OK".to_string(),
//         }),
//     )
//         .into_response())
// }

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
