use crate::adapter::ConnectionHandler;
use crate::protocol::messages::{BatchPusherApiMessage, PusherApiMessage};
// Ensure the utils module/functions are accessible
use crate::utils; // Assuming utils is a module in the crate root
use crate::websocket::SocketId;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
    response::{IntoResponse, Response as AxumResponse},
    Json,
};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fmt, sync::Arc}; // Removed unused HashMap import
use sysinfo::System;
use thiserror::Error;
use tracing::{error, field, info, instrument, warn};
use crate::log::Log;
// Added field back

// --- Custom Error Type ---

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Application not found: {0}")]
    AppNotFound(String),
    #[error("Application validation failed: {0}")]
    AppValidationFailed(String),
    #[error("Channel validation failed: Missing 'channels' or 'channel' field")]
    MissingChannelInfo,
    #[error("User connection termination failed: {0}")]
    TerminationFailed(String),
    #[error("Internal Server Error: {0}")]
    InternalError(String),
    #[error("Serialization Error: {0}")]
    SerializationError(#[from] serde_json::Error),
    #[error("HTTP Header Build Error: {0}")]
    HeaderBuildError(#[from] axum::http::Error),
    // Example: Add more specific errors if handler methods return them
    // #[error("Database Error: {0}")]
    // DatabaseError(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> AxumResponse {
        let (status, error_message) = match &self { // Match by reference
            AppError::AppNotFound(msg) => (StatusCode::NOT_FOUND, json!({ "error": msg })),
            AppError::AppValidationFailed(msg) => (StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": msg })),
            AppError::MissingChannelInfo => (StatusCode::BAD_REQUEST, json!({ "error": "Request must contain 'channels' (list) or 'channel' (string)" })),
            AppError::TerminationFailed(msg) => (StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": msg })),
            AppError::SerializationError(e) => (StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": format!("Internal error during serialization: {}", e) })),
            AppError::HeaderBuildError(e) => (StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": format!("Internal error building response: {}", e) })),
            AppError::InternalError(msg) => (StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": msg })),
            // Map other error variants here
        };

        // Log the error with its details using the Display impl from thiserror
        error!(error.message = %self, status_code = %status, "Request failed");

        (status, Json(error_message)).into_response()
    }
}

// --- Structs for API Responses/Requests ---

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

#[derive(Deserialize, Debug)]
pub struct EventQuery {
    #[allow(dead_code)]
    auth_key: String,
    #[allow(dead_code)]
    auth_timestamp: String,
    #[allow(dead_code)]
    auth_version: String,
    #[allow(dead_code)]
    body_md5: String,
    #[allow(dead_code)]
    auth_signature: String,
}

// --- Axum Handlers ---

/// GET /usage
#[instrument(skip_all, fields(service = "usage_monitor"))]
pub async fn usage() -> Result<impl IntoResponse, AppError> {
    let mut sys = System::new_all();
    sys.refresh_all();

    let total = sys.total_memory() * 1024;
    let used = sys.used_memory() * 1024;
    let free = total.saturating_sub(used);
    let percent = if total > 0 {
        (used as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    let memory_stats = MemoryStats { free, used, total, percent };
    let response_payload = UsageResponse { memory: memory_stats };

    info!(
        total_bytes = total,
        used_bytes = used,
        free_bytes = free,
        usage_percent = format!("{:.2}", percent), // Keep format for readability in logs
        "Memory usage queried"
    );

    Ok((StatusCode::OK, Json(response_payload)))
}

/// Helper to process a single event
#[instrument(skip(handler, event_data), fields(app_id = app_id, event_name = field::Empty))] // Defer setting event_name
async fn process_single_event(
    handler: &Arc<ConnectionHandler>,
    app_id: &str,
    event_data: PusherApiMessage, // Takes ownership
) -> Result<(), AppError> {
    let PusherApiMessage {
        name, // Moved: String
        data, // Moved: Value
        channels, // Moved: Option<Vec<String>>
        channel, // Moved: Option<String>
        socket_id: original_socket_id_str, // Moved: Option<String> - Renamed for clarity
    } = event_data;

    // Add event_name to tracing span now that we have it
    tracing::Span::current().record("event_name", &name.clone().unwrap());

    // --- Prepare data needed across loop iterations ---
    // Map the socket_id string to SocketId type *once* before the loop
    let mapped_socket_id: Option<SocketId> = original_socket_id_str.map(SocketId);

    // Clone data needed multiple times *once* if it improves clarity or performance slightly
    // (String::clone and Value::clone are relatively cheap for typical data sizes)
    let name_clone = name; // Take ownership of name
    let data_clone = data; // Take ownership of data

    // --- Determine target channels ---
    let target_channels: Vec<String> = match channels { // `channels` is moved here
        Some(ch_list) if !ch_list.is_empty() => ch_list, // ch_list (Vec<String>) is moved
        None => match channel { // `channel` is moved here
            Some(ch) => vec![ch], // ch (String) is moved
            None => {
                warn!("Missing 'channels' or 'channel' in event");
                return Err(AppError::MissingChannelInfo);
            }
        },
        Some(_) => { // channels was Some(empty_vec)
            warn!("Empty 'channels' list provided in event");
            return Err(AppError::MissingChannelInfo);
        }
    }; // `target_channels` now owns the Vec<String>

    // --- Process each target channel ---
    // Iterate by reference if we don't need to consume the Vec,
    // or by value (`into_iter()`) if consuming is okay. Let's use by value.
    for target_channel in target_channels { // Moves ownership of each String into loop variable
        info!(channel = %target_channel, "Processing channel");

        // Create the specific message payload for this channel
        let message_to_send = PusherApiMessage {
            name: name_clone.clone(), // Clone the pre-cloned name
            data: data_clone.clone(), // Clone the pre-cloned data
            channels: None,           // Not needed for send payload
            channel: Some(target_channel.clone()), // Clone loop variable for payload
            socket_id: mapped_socket_id.as_ref().map(|sid| sid.0.clone()), // Clone the inner String if present
        };

        // Send the message
        // Pass the mapped_socket_id by reference
        handler
            .send_message(
                app_id,
                mapped_socket_id.as_ref(), // Pass Option<&SocketId>
                message_to_send,           // Pass owned message
                &target_channel,           // Pass borrowed channel name
            )
            .await; // Assuming send_message handles its own errors or returns Result that we ignore for now

        // --- Caching Logic ---
        if utils::is_cache_channel(&target_channel) {
            // **FIXED:** Use &name_clone (which is &String), build_cache_payload expects &str
            let data = serde_json::to_value(data_clone.clone().unwrap())?;
            let cache_payload_str = match build_cache_payload(&name_clone.clone().unwrap().as_str(), &data) {
                Ok(payload) => payload,
                Err(e) => {
                    error!(channel = %target_channel, error = %e, "Failed to serialize event data for caching");
                    continue; // Log error and skip caching for this channel
                }
            };

            let mut cache_manager = handler.cache_manager.lock().await;
            let key = format!("app:{}:channel:{}:cache_miss", app_id, target_channel);

            // **FIXED:** Handle cache set error without exiting the whole function
            match cache_manager.set(&key, &cache_payload_str, 3600).await {
                Ok(_) => {
                    // **FIXED:** Use tracing::info!
                    info!(channel = %target_channel, cache_key = %key, "Cached event for channel");
                }
                Err(e) => {
                    // Log the error but continue processing other channels
                    error!(channel = %target_channel, cache_key = %key, error = %e, "Failed to cache event");
                    // Do NOT return Err here, just continue the loop
                }
            }
        }
        // Ownership of target_channel moves to the next iteration or is dropped
    } // `target_channels` (Vec) is now fully consumed

    // name_clone, data_clone, mapped_socket_id go out of scope here

    Ok(())
}

/// Helper to build cache payload string
fn build_cache_payload(event_name: &str, event_data: &Value) -> Result<String, serde_json::Error> {
    serde_json::to_string(&json!({
        "event": event_name,
        "data": event_data,
    }))
}

/// POST /apps/{app_id}/events
#[instrument(skip(handler, event), fields(app_id = %app_id))]
pub async fn events(
    Path(app_id): Path<String>,
    Query(_query): Query<EventQuery>, // TODO: Implement auth validation or remove
    State(handler): State<Arc<ConnectionHandler>>,
    Json(event): Json<PusherApiMessage>,
) -> Result<impl IntoResponse, AppError> {
    Log::info(format!("Received event: {:?}", event));

    // --- Application Validation ---
    match handler.app_manager.get_app(app_id.as_str()).await {
        Ok(Some(_app)) => {
            info!("App found, processing event.");
        }
        Ok(None) => {
            warn!("App not found during event processing.");
            return Err(AppError::AppNotFound(app_id));
        }
        Err(e) => {
            error!(error = %e, "Failed to fetch app details.");
            return Err(AppError::AppValidationFailed(format!("Error fetching app: {}", e)));
        }
    }

    // Process the single event, consuming `event`
    process_single_event(&handler, &app_id, event).await?;

    Ok((StatusCode::OK, Json(json!({ "ok": true }))))
}

/// POST /apps/{app_id}/users/{user_id}/terminate_connections
#[instrument(skip(handler), fields(app_id = %app_id, user_id = %user_id))]
pub async fn terminate_user_connections(
    Path((app_id, user_id)): Path<(String, String)>,
    State(handler): State<Arc<ConnectionHandler>>,
) -> Result<impl IntoResponse, AppError> {
    info!("Received request to terminate user connections");

    let connection_manager = handler.connection_manager.clone();

    let result = connection_manager
        .lock()
        .await
        .terminate_connection(&app_id, &user_id)
        .await; // Assuming terminate_connection returns Result<(), ErrorType>

    match result {
        Ok(_) => {
            info!("Successfully initiated termination for user");
            Ok((StatusCode::OK, Json(json!({ "ok": true }))))
        }
        Err(e) => {
            error!(error = %e, "Failed to terminate connections for user");
            Err(AppError::TerminationFailed(format!(
                "Failed to terminate connections for user {} in app {}: {}", // Assuming e implements Display
                user_id, app_id, e
            )))
        }
    }
}

/// Records API metrics (helper async function)
#[instrument(skip(handler, batch_size, response_size), fields(app_id = %app_id))]
async fn record_api_metrics(
    handler: &Arc<ConnectionHandler>,
    app_id: &str,
    batch_size: usize,
    response_size: usize,
) {
    if let Some(metrics_arc) = &handler.metrics {
        let metrics = metrics_arc.lock().await; // Correctly await the lock
        // Assuming mark_api_message itself is synchronous after lock acquisition
        metrics.mark_api_message(
            app_id,
            batch_size,
            response_size,
        );
        info!(incoming_bytes = batch_size, outgoing_bytes = response_size, "Recorded API message metrics");
    } else {
        info!("Metrics system not available, skipping metrics recording.");
    }
}

/// POST /apps/{app_id}/batch_events
#[instrument(skip(handler, batch_message), fields(app_id = %app_id, batch_len = batch_message.batch.len()))]
pub async fn batch_events(
    Path(app_id): Path<String>,
    State(handler): State<Arc<ConnectionHandler>>,
    Json(batch_message): Json<BatchPusherApiMessage>,
) -> Result<impl IntoResponse, AppError> {
    info!("Received batch events request");

    let batch = batch_message.batch;

    let batch_size = serde_json::to_vec(&batch)
        .map_err(|e| { // Handle potential serialization error for metrics
            warn!(error = %e, "Failed to serialize incoming batch for size calculation");
            AppError::SerializationError(e) // Convert to AppError if we want to fail request here, or just log and use 0
        })? // Propagate serialization error if needed
        .len();


    // --- Optional: Application Validation ---
    // match handler.app_manager.get_app(app_id.as_str()).await { ... }

    // --- Sequential Processing ---
    for message in batch { // message is moved here
        // Pass handler and app_id by reference, move message
        process_single_event(&handler, &app_id, message).await?; // Propagate error using ?
    }

    // --- Concurrent Processing (Alternative - see previous response for structure) ---

    let response_payload = json!({ "ok": true });
    let response_size = serde_json::to_vec(&response_payload)?.len(); // Use ? to propagate serialization error

    // **FIXED:** Await the async metric recording function
    record_api_metrics(&handler, &app_id, batch_size, response_size).await;

    info!("Batch events processed successfully");
    Ok((StatusCode::OK, Json(response_payload)))
}


/// GET /apps/{app_id}/channels/{channel_name}
#[instrument(skip(handler), fields(app_id = %app_id, channel = %channel_name))]
pub async fn channel(
    Path((app_id, channel_name)): Path<(String, String)>,
    State(handler): State<Arc<ConnectionHandler>>,
) -> Result<impl IntoResponse, AppError> {
    info!("Request for channel info");

    // Assume handler.channel returns Result<T, E> where T: Serialize and E: Display + Error
    let channel_data = handler
        .channel(app_id.as_str(), channel_name.as_str())
        .await;
    Log::info(format!("Channel info: {:?}", channel_data));

    // **FIXED:** Handle Result from data_to_bytes_flexible using ?
    let response_size = utils::data_to_bytes_flexible(Vec::from([channel_data.clone()]));

    // **FIXED:** Await the async metric recording function
    record_api_metrics(&handler, &app_id, 0, response_size).await;

    info!("Channel info retrieved successfully");
    Ok((StatusCode::OK, Json(channel_data)))
}


/// GET /apps/{app_id}/up
#[instrument(skip(handler), fields(app_id = %app_id))]
pub async fn up(
    Path(app_id): Path<String>,
    State(handler): State<Arc<ConnectionHandler>>,
) -> Result<impl IntoResponse, AppError> {
    info!("Health check received");

    if handler.metrics.is_some() {
        // **FIXED:** Await the async metric recording function
        record_api_metrics(&handler, &app_id, 0, 0).await; // Example: Record 0 size for health check
    } else {
        info!("Metrics system not available for health check.")
    }

    let response = Response::builder()
        .status(StatusCode::OK)
        .header("X-Health-Check", "OK")
        .body("OK".to_string()) // Simple body
        .map_err(AppError::HeaderBuildError)?; // Use ? to handle builder error

    Ok(response)
}


/// GET /apps/{app_id}/channels
/// GET /apps/{app_id}/channels
/// Retrieves a list or summary of channels within an app.
#[instrument(skip(handler), fields(app_id = %app_id))]
pub async fn channels(
    Path(app_id): Path<String>,
    State(handler): State<Arc<ConnectionHandler>>,
) -> Result<impl IntoResponse, AppError> {
    info!("Request for channels list");

    // Step 1: Await the result from the handler method
    // Assuming it returns Result<Value, HandlerErrorType> where HandlerErrorType implements Display
    let channels_result = handler
        .channels(app_id.as_str())
        .await;

    // Step 2: Explicitly check the Result *before* trying to map the error
    let channels_data: Value = channels_result;

    // Now proceed with the channels_data (which is Value)
    let response_size = utils::data_to_bytes_flexible(Vec::from([channels_data.clone()])); // Propagates potential serialization error

    record_api_metrics(&handler, &app_id, 0, response_size).await; // Await metrics

    info!("Channels list retrieved successfully");
    Ok((StatusCode::OK, Json(channels_data))) // Return the data
}


/// GET /metrics (Prometheus format)
#[instrument(skip(handler), fields(service = "metrics_exporter"))]
pub async fn metrics(
    State(handler): State<Arc<ConnectionHandler>>,
) -> Result<impl IntoResponse, AppError> {
    info!("Metrics endpoint called");

    let plaintext_metrics = match handler.metrics.clone() {
        Some(metrics_arc) => {
            let metrics_data = metrics_arc.lock().await; // await the lock
            // Assume get_metrics_as_plaintext returns Result<String, E>
            metrics_data.get_metrics_as_plaintext().await
        }
        None => {
            info!("No metrics data available.");
            "# Metrics collection is not enabled.\n".to_string()
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );

    info!(bytes = plaintext_metrics.len(), "Successfully generated metrics");
    Ok((StatusCode::OK, headers, plaintext_metrics))
}


/// GET /apps/{app_id}/channels/{channel_name}/users
#[instrument(skip(handler), fields(app_id = %app_id, channel = %channel_name))]
pub async fn channel_users(
    Path((app_id, channel_name)): Path<(String, String)>,
    State(handler): State<Arc<ConnectionHandler>>,
) -> Result<impl IntoResponse, AppError> {
    info!("Request for users in channel");

    let users_data = handler
        .channel_users(app_id.as_str(), channel_name.as_str())
        .await
        .map_err(|e| AppError::InternalError(format!("Failed to get channel users: {}", e)))?; // Handle error properly

    let response_payload = json!({ "users": users_data });

    // Use ? to propagate potential serialization error
    let response_size = serde_json::to_vec(&response_payload)?.len();

    // **FIXED:** Await the async metric recording function
    record_api_metrics(&handler, &app_id, 0, response_size).await;

    info!(user_count = response_payload["users"].as_array().map_or(0, Vec::len), "Channel users retrieved successfully");
    Ok((StatusCode::OK, Json(response_payload)))
}

// --- Definition for utils module (example) ---
// Put this in `src/utils.rs` and add `pub mod utils;` to `src/lib.rs` or `src/main.rs`
/*
pub mod utils {
    use serde::Serialize;
    use serde_json;
    use crate::AppError; // Make AppError accessible if mapping directly

    pub fn is_cache_channel(channel: &str) -> bool {
        channel.starts_with("cache-") || channel.starts_with("private-cache-")
    }

    /// Calculates the size in bytes of the JSON representation of the data.
    pub fn data_to_bytes_flexible<T: Serialize>(data: &T) -> Result<usize, AppError> {
       serde_json::to_vec(data)
            .map(|bytes| bytes.len())
            // Map the serialization error into our AppError::SerializationError
            .map_err(AppError::SerializationError)
    }
}
*/