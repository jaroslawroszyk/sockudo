use std::sync::Arc;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sysinfo::System;
use crate::adapter::ConnectionHandler;
use crate::log::Log;
use crate::protocol::messages::PusherApiMessage;
use crate::websocket::SocketId;

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
pub struct EventQuery {
    auth_key: String,
    auth_timestamp: String,
    auth_version: String,
    body_md5: String,
    auth_signature: String,
}

pub async fn events(
    Path(app_id): Path<String>,
    Query(query): Query<EventQuery>,
    State(handler): State<Arc<ConnectionHandler>>,
    Json(event): Json<PusherApiMessage>,
) -> impl IntoResponse {
    Log::info(format!("Received event: {:?}", event));
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
            let socket_id = message.clone().socket_id.map(SocketId);
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
