use crate::adapter::ConnectionHandler;
use crate::log::Log;
use crate::protocol::messages::PusherApiMessage;
use crate::utils;
use crate::websocket::SocketId;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, Response, StatusCode};
use axum::response::{IntoResponse, Response as AxumResponse}; // Renamed Response to avoid conflict
use axum::{response, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use sysinfo::System;

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

#[derive(Deserialize, Serialize, Debug)]
pub struct EventQuery {
    // Authentication related query parameters (consider validation)
    auth_key: String,
    auth_timestamp: String,
    auth_version: String,
    body_md5: String,
    auth_signature: String,
}

// --- Axum Handlers ---

/// GET /usage
/// Provides system memory usage statistics.
pub async fn usage() -> impl IntoResponse {
    let mut sys = System::new_all();
    // Refresh system data to get latest stats.
    sys.refresh_all();

    // Get memory statistics from sysinfo.
    // Note: sysinfo provides values in KiB, converting to bytes.
    let total = sys.total_memory() * 1024;
    let used = sys.used_memory() * 1024;
    let free = total.saturating_sub(used); // Use saturating_sub for safety
                                           // Calculate percentage, handle potential division by zero if total is 0.
    let percent = if total > 0 {
        (used as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    let memory_stats = MemoryStats {
        free,
        used,
        total,
        percent,
    };

    // Create the JSON response body.
    let response = UsageResponse {
        memory: memory_stats,
    };

    // Log memory usage for monitoring purposes.
    // Consider using a structured logging format.
    tracing::info!(
        "Memory usage - Total: {} bytes, Used: {} bytes, Free: {} bytes, Usage: {:.2}%",
        total,
        used,
        free,
        percent
    );

    // Return JSON response with status OK.
    (StatusCode::OK, Json(response))
}

/// POST /apps/{app_id}/events
/// Handles receiving a single Pusher API event and broadcasting it.
pub async fn events(
    Path(app_id): Path<String>,
    Query(_query): Query<EventQuery>, // Query params currently unused, but parsed. Add validation if needed.
    State(handler): State<Arc<ConnectionHandler>>,
    Json(event): Json<PusherApiMessage>, // The event payload from the request body.
) -> AxumResponse {
    // Explicit return type for better error handling
    Log::info(format!("Received event for app {}: {:?}", app_id, event));

    // Destructure the event, cloning necessary parts.
    let PusherApiMessage {
        name, // Keep name and data for caching
        data,
        channels,
        channel, // Single channel case
        socket_id,
    } = event.clone();

    // --- Application Validation ---
    let app_result = handler.app_manager.get_app(app_id.as_str()).await;
    match app_result {
        Ok(Some(_app)) => {
            // App exists, proceed.
            // TODO: Potentially use the `_app` variable for further validation if needed.
            Log::info(format!("App {} found, processing event.", app_id));
        }
        Ok(None) => {
            Log::warning(format!("App not found: {}", app_id));
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("Application with id '{}' not found", app_id) })),
            )
                .into_response();
        }
        Err(e) => {
            // Handle error during app lookup (e.g., database issue).
            Log::error(format!("Error fetching app {}: {}", app_id, e));
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to validate application" })),
            )
                .into_response();
        }
    }

    // Map the optional socket_id string to the SocketId type.
    let socket_id = socket_id.map(SocketId);

    // --- Event Broadcasting Logic ---
    // Determine target channels: either a list or a single channel.
    let target_channels: Vec<String> = match channels {
        Some(ch_list) => ch_list,
        None => match channel {
            Some(ch) => vec![ch], // Wrap single channel in a vec
            None => {
                // Neither 'channels' nor 'channel' provided - invalid request.
                Log::warning(format!(
                    "Invalid event format for app {}: Missing 'channels' or 'channel'",
                    app_id
                ));
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Request must contain 'channels' (list) or 'channel' (string)" })),
                )
                    .into_response();
            }
        },
    };

    // --- Process each target channel ---
    for target_channel in target_channels {
        // Send the message via the connection handler.
        // Note: `send_message` might need better error handling itself.
        // Assuming it logs errors internally for now.
        handler
            .send_message(
                &app_id,
                socket_id.as_ref(), // Pass optional socket ID to exclude
                event.clone(),      // Clone the event for each send
                &target_channel,
            )
            .await;

        // --- Caching Logic (if applicable) ---
        if utils::is_cache_channel(&target_channel) {
            // Serialize the cache payload safely.
            let cache_value_result = serde_json::to_string(&json!({
               "event": name, // Use destructured name
                "data": data, // Use destructured data
            }));

            match cache_value_result {
                Ok(value_str) => {
                    // Acquire cache manager lock.
                    let mut cache_manager = handler.cache_manager.lock().await;
                    let key = format!("app:{}:channel:{}:cache_miss", app_id, target_channel);
                    // Set cache value. `set` might need error handling too.
                    // Assuming it logs errors internally.
                    cache_manager.set(&key, &value_str, 3600).await; // 1 hour TTL
                    Log::info(format!("Cached event for channel: {}", target_channel));
                }
                Err(e) => {
                    // Log serialization error for caching, but don't fail the request.
                    Log::error(format!(
                        "Failed to serialize event data for caching (channel: {}): {}",
                        target_channel, e
                    ));
                }
            }
        }
    }

    // If all processing succeeded, return OK.
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

/// POST /apps/{app_id}/users/{user_id}/terminate_connections
/// Terminates all active connections for a specific user within an app.
pub async fn terminate_user_connections(
    Path((app_id, user_id)): Path<(String, String)>, // Extract app_id and user_id
    State(handler): State<Arc<ConnectionHandler>>,
) -> impl IntoResponse {
    Log::info(format!(
        "Received request to terminate connections for user {} in app {}",
        user_id, app_id
    ));

    // Get the connection manager instance.
    let connection_manager = handler.connection_manager.clone();

    // Attempt to terminate connections, handling potential errors.
    let result = connection_manager
        .lock()
        .await
        .terminate_connection(&app_id, &user_id) // Assuming this is the correct method name
        .await;

    match result {
        Ok(_) => {
            Log::info(format!(
                "Successfully initiated termination for user {} in app {}",
                user_id, app_id
            ));
            (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            // Log the error that occurred during termination.
            Log::error(format!(
                "Failed to terminate connections for user {} in app {}: {}",
                user_id, app_id, e
            ));
            // Return an appropriate error response.
            // The specific status code might depend on the nature of the error `e`.
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to terminate user connections" })),
            )
                .into_response()
        }
    }
}

/// POST /apps/{app_id}/batch_events
/// Handles receiving a batch of Pusher API events and broadcasting them.
pub async fn batch_events(
    Path(app_id): Path<String>,
    State(handler): State<Arc<ConnectionHandler>>,
    Json(batch): Json<Vec<PusherApiMessage>>, // Expecting a JSON array of events.
) -> AxumResponse {
    // Explicit return type
    Log::info(format!(
        "Received batch of {} events for app {}",
        batch.len(),
        app_id
    ));

    // Calculate incoming message size for metrics
    let batch_size = match serde_json::to_string(&batch) {
        Ok(serialized_batch) => serialized_batch.as_bytes().len(),
        Err(_) => 0,
    };

    // --- Application Validation (Optional but recommended) ---
    // Consider adding the same app validation as in the `events` handler here
    // if you want to reject batches for non-existent apps early.
    // match handler.app_manager.get_app(app_id.as_str()).await { ... }

    // --- Process each event in the batch ---
    for message in batch.iter() {
        // Validate that channels are provided for each message in the batch.
        let target_channels = match message.channels.as_ref() {
            Some(ch_list) if !ch_list.is_empty() => ch_list,
            _ => {
                // If a message in the batch lacks channels, skip it or return an error.
                // Skipping for robustness, but logging a warning.
                Log::warning(format!(
                    "Skipping message in batch for app {}: Missing 'channels'. Message: {:?}",
                    app_id, message
                ));
                continue; // Skip this message, process the next one
            }
        };

        // Map the optional socket_id for exclusion.
        let socket_id = message.socket_id.clone().map(SocketId);

        // Process each channel within the message.
        for channel in target_channels {
            // Send the message.
            handler
                .send_message(
                    &app_id,
                    socket_id.as_ref(),
                    message.clone(), // Clone message for this send
                    channel.as_str(),
                )
                .await;

            // --- Caching Logic (if applicable) ---
            if utils::is_cache_channel(channel) {
                // Serialize cache payload safely.
                let cache_value_result = serde_json::to_string(&json!({
                   "event": message.name, // Use fields from message
                    "data": message.data,
                }));

                match cache_value_result {
                    Ok(value_str) => {
                        let mut cache_manager = handler.cache_manager.lock().await;
                        let key = format!("app:{}:channel:{}:cache_miss", app_id, channel);
                        cache_manager.set(&key, &value_str, 3600).await;
                        Log::info(format!("Cached batch event for channel: {}", channel));
                    }
                    Err(e) => {
                        Log::error(format!(
                            "Failed to serialize batch event data for caching (channel: {}): {}",
                            channel, e
                        ));
                        // Continue processing other channels/messages
                    }
                }
            }
        }
    }

    // Prepare the response
    let response = json!({ "ok": true });
    let response_json = serde_json::to_string(&response).unwrap_or_default();
    let response_size = response_json.len();

    // Record metrics if available
    if let Some(metrics) = &handler.metrics {
        let metrics = metrics.lock().await;
        metrics.mark_api_message(
            &app_id,
            batch_size,    // Incoming message size
            response_size, // Outgoing message size
        );
        Log::info("Recorded API message metrics");
    }

    // Return OK after processing the entire batch.
    (StatusCode::OK, Json(response)).into_response()
}

/// GET /apps/{app_id}/channels/{channel_name}
/// Retrieves information about a specific channel within an app.
pub async fn channel(
    Path((app_id, channel_name)): Path<(String, String)>, // Extract both path params
    State(handler): State<Arc<ConnectionHandler>>,
) -> impl IntoResponse {
    Log::info(format!(
        "Request for channel info: app={}, channel={}",
        app_id, channel_name
    ));
    // Delegate fetching channel info to the handler.
    // Assuming `handler.channel` returns data suitable for JSON serialization.
    let response_data = handler
        .channel(app_id.as_str(), channel_name.as_str())
        .await;
    let metrics = handler.metrics.clone();
    if let Some(metrics_data) = metrics {
        Log::info("Metrics available, attempting to mark connection.");
        let metrics_data = metrics_data.lock().await;
        let channels_data_size = utils::data_to_bytes_flexible(Vec::from([response_data.clone()]));
        // Example: Mark a conceptual 'new connection' for health check purposes.
        // This might need adjustment based on actual metrics logic.
        metrics_data.mark_api_message(
            &app_id,
            0,                  // Assuming no outgoing message size for this request
            channels_data_size, // Assuming no incoming message size for this request
        );
        Log::info("Metrics interaction complete.");
    } else {
        // Log if metrics system is not available.
        Log::info("Metrics system not configured for this handler.");
    }
    // Return the data as JSON.
    (StatusCode::OK, Json(response_data))
}

/// GET /apps/{app_id}/up (or similar health check endpoint)
/// Basic health check, potentially interacting with metrics.
pub async fn up(
    Path(app_id): Path<String>,
    State(handler): State<Arc<ConnectionHandler>>,
) -> AxumResponse {
    // Explicit return type
    Log::info(format!("Health check received for app {}", app_id));

    // --- Metrics Interaction (Optional) ---
    // Attempt to interact with the metrics system if it's configured.
    if let Some(metrics_arc) = handler.metrics.clone() {
        Log::info("Metrics available, attempting to mark connection.");
        let metrics_data = metrics_arc.lock().await;
        // Example: Mark a conceptual 'new connection' for health check purposes.
        // This might need adjustment based on actual metrics logic.
        Log::info("Metrics interaction complete.");
    } else {
        // Log if metrics system is not available.
        Log::info("Metrics system not configured for this handler.");
        // Note: The original code returned a custom response here,
        // but we proceed to the standard "OK" response below.
        // If a different response is needed when metrics are off, adjust here.
        // Example:
        // return Response::builder()
        //     .status(StatusCode::OK)
        //     .body("OK (No metrics)".into()) // Use .into() for Body conversion
        //     .expect("Failed to build basic OK response");
    }

    // --- Standard OK Response ---
    // Build a simple OK response. Using expect is generally discouraged in handlers,
    // prefer proper error handling or Turbofish ::<> if type inference fails.
    match Response::builder()
        .status(StatusCode::OK)
        .header("X-Custom-Health", "OK") // Example custom header
        .body("OK".to_string().into()) // Convert String to Body
    {
        Ok(response) => response,
        Err(e) => {
            // If building the basic response fails (highly unlikely), return 500.
            Log::error(format!("Failed to build health check response: {}", e));
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to build health check response"})),
            )
                .into_response()
        }
    }
}

/// GET /apps/{app_id}/channels
/// Retrieves a list or summary of channels within an app.
pub async fn channels(
    Path(app_id): Path<String>,
    State(handler): State<Arc<ConnectionHandler>>,
) -> impl IntoResponse {
    Log::info(format!("Request for channels list: app={}", app_id));
    // Delegate fetching channels list to the handler.
    // Assuming `handler.channels` returns data suitable for JSON serialization.
    let channels_data = handler.channels(app_id.as_str()).await;
    let metrics = handler.metrics.clone();
    if let Some(metrics_data) = metrics {
        Log::info("Metrics available, attempting to mark connection.");
        let metrics_data = metrics_data.lock().await;
        let channels_data_size = utils::data_to_bytes_flexible(Vec::from([channels_data.clone()]));
        // Example: Mark a conceptual 'new connection' for health check purposes.
        // This might need adjustment based on actual metrics logic.
        metrics_data.mark_api_message(
            &app_id,
            0,                  // Assuming no outgoing message size for this request
            channels_data_size, // Assuming no incoming message size for this request
        );
        Log::info("Metrics interaction complete.");
    } else {
        // Log if metrics system is not available.
        Log::info("Metrics system not configured for this handler.");
    }
    // Return the data as JSON.
    (StatusCode::OK, Json(channels_data))
}

pub async fn metrics(
    State(handler): State<Arc<ConnectionHandler>>,
) -> axum::response::Response<String> {
    Log::info("Metrics endpoint called");

    let metrics_data = match handler.metrics.clone() {
        Some(metrics_data) => {
            Log::info("Metrics endpoint");
            let metrics_data = metrics_data.lock().await;
            metrics_data.get_metrics_as_plaintext().await
        }
        None => {
            return axum::response::Response::builder()
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
    axum::response::Response::builder()
        .status(200)
        .header("X-Custom-Foo", "Bar")
        .body(metrics_data)
        .unwrap()
}

/// GET /apps/{app_id}/channels/{channel_name}/users
/// Retrieves a list of users in a specific channel within an app.
pub async fn channel_users(
    Path((app_id, channel_name)): Path<(String, String)>, // Extract both path params
    State(handler): State<Arc<ConnectionHandler>>,
) -> impl IntoResponse {
    Log::info(format!(
        "Request for users in channel: app={}, channel={}",
        app_id, channel_name
    ));
    // Delegate fetching user list to the handler.
    // Assuming `handler.channel_users` returns data suitable for JSON serialization.
    let users_data = handler
        .channel_users(app_id.as_str(), channel_name.as_str())
        .await.unwrap();
    
    let metrics = handler.metrics.clone();
    if let Some(metrics_data) = metrics {
        Log::info("Metrics available, attempting to mark connection.");
        let metrics_data = metrics_data.lock().await;
        let users_data = serde_json::to_string(&users_data).unwrap_or_default();
        let users_data_size = users_data.as_bytes().len();
        // Example: Mark a conceptual 'new connection' for health check purposes.
        // This might need adjustment based on actual metrics logic.
        metrics_data.mark_api_message(
            &app_id,
            0,                  // Assuming no outgoing message size for this request
            users_data_size,    // Assuming no incoming message size for this request
        );
        Log::info("Metrics interaction complete.");
    } else {
        // Log if metrics system is not available.
        Log::info("Metrics system not configured for this handler.");
    }
    let response = json!({ "users": users_data });
    // Return the data as JSON.
    (StatusCode::OK, Json(response))
}
