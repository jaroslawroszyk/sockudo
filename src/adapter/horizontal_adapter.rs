use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::adapter::local_adapter::LocalAdapter;
use crate::adapter::Adapter;
use crate::channel::PresenceMemberInfo;
use crate::error::{Error, Result};
use crate::log::Log;
use crate::websocket::SocketId;
use async_trait::async_trait;
use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::time::sleep;
use uuid::Uuid;

/// Request types for horizontal communication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RequestType {
    ChannelMembers,
    ChannelSockets,
    ChannelSocketsCount,
    SocketExistsInChannel,
    TerminateUserConnections,
}

/// Request body for horizontal communication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestBody {
    pub request_id: String,
    pub node_id: String,
    pub app_id: String,
    pub request_type: RequestType,
    pub channel: Option<String>,
    pub socket_id: Option<String>,
    pub user_id: Option<String>,
}

/// Response body for horizontal requests
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseBody {
    pub request_id: String,
    pub node_id: String,
    pub app_id: String,
    pub members: HashMap<String, PresenceMemberInfo>,
    pub socket_ids: Vec<String>,
    pub sockets_count: usize,
    pub exists: bool,
}

/// Message for broadcasting events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastMessage {
    pub node_id: String,
    pub app_id: String,
    pub channel: String,
    pub message: String,
    pub except_socket_id: Option<String>,
}

/// Request tracking struct
#[derive(Clone)]
struct PendingRequest {
    start_time: Instant,
    app_id: String,
    responses: Vec<ResponseBody>,
}

/// Base horizontal adapter
pub struct HorizontalAdapter {
    /// Unique node ID
    pub node_id: String,

    /// Local adapter for handling local connections
    pub local_adapter: LocalAdapter,

    /// Pending requests map
    pub pending_requests: HashMap<String, PendingRequest>,

    /// Timeout for requests in milliseconds
    pub requests_timeout: u64,
}

impl HorizontalAdapter {
    /// Create a new horizontal adapter
    pub fn new() -> Self {
        Self {
            node_id: Uuid::new_v4().to_string(),
            local_adapter: LocalAdapter::new(),
            pending_requests: HashMap::new(),
            requests_timeout: 5000, // Default 5 seconds
        }
    }

    /// Start the request cleanup task
    pub fn start_request_cleanup(&mut self) {
        // Clone data needed for the task
        let node_id = self.node_id.clone();
        let timeout = self.requests_timeout;
        let mut pending_requests_clone = self.pending_requests.clone();

        // Spawn a background task to clean up stale requests
        tokio::spawn(async move {
            loop {
                sleep(Duration::from_millis(1000)).await;

                // Find and process expired requests
                let now = Instant::now();
                let mut expired_requests = Vec::new();

                // We can't modify pending_requests while iterating
                for (request_id, request) in &pending_requests_clone {
                    if now.duration_since(request.start_time).as_millis() > timeout as u128 {
                        expired_requests.push(request_id.clone());
                    }
                }

                // Process expired requests
                for request_id in expired_requests {
                    Log::warning(format!("Request {} expired", request_id));
                    pending_requests_clone.remove(&request_id);
                }
            }
        });
    }

    /// Process a received request from another node
    pub async fn process_request(&mut self, request: RequestBody) -> Result<ResponseBody> {
        Log::info(format!(
            "Processing request from node {}: {:?}",
            request.node_id, request.request_type
        ));

        // Skip processing our own requests
        if request.node_id == self.node_id {
            return Err(Error::Other("Ignoring own request".into()));
        }

        // Initialize empty response
        let mut response = ResponseBody {
            request_id: request.request_id.clone(),
            node_id: self.node_id.clone(),
            app_id: request.app_id.clone(),
            members: HashMap::new(),
            socket_ids: Vec::new(),
            sockets_count: 0,
            exists: false,
        };

        // Process based on request type
        match request.request_type {
            RequestType::ChannelMembers => {
                if let Some(channel) = &request.channel {
                    // Get channel members from local adapter
                    let members = self
                        .local_adapter
                        .get_channel_members(&request.app_id, channel)
                        .await?;
                    response.members = members;
                }
            }
            RequestType::ChannelSockets => {
                if let Some(channel) = &request.channel {
                    // Get channel sockets from local adapter
                    let channel = self
                        .local_adapter
                        .get_channel(&request.app_id, channel)
                        .await?;
                    response.socket_ids = channel
                        .iter()
                        .map(|socket_id| socket_id.0.clone())
                        .collect();
                }
            }
            RequestType::ChannelSocketsCount => {
                if let Some(channel) = &request.channel {
                    // Get channel socket count from local adapter
                    response.sockets_count = self
                        .local_adapter
                        .get_channel_socket_count(&request.app_id, channel)
                        .await;
                }
            }
            RequestType::SocketExistsInChannel => {
                if let (Some(channel), Some(socket_id)) = (&request.channel, &request.socket_id) {
                    // Check if socket exists in channel
                    let socket_id = SocketId(socket_id.clone());
                    response.exists = self
                        .local_adapter
                        .is_in_channel(&request.app_id, channel, &socket_id)
                        .await?;
                }
            }
            RequestType::TerminateUserConnections => {
                if let Some(user_id) = &request.user_id {
                    // Terminate user connections locally
                    let _ = self
                        .local_adapter
                        .terminate_user_connections(&request.app_id, user_id)
                        .await;
                    response.exists = true;
                }
            }
        }

        // Return the response
        Ok(response)
    }

    /// Process a response received from another node
    pub async fn process_response(&mut self, response: ResponseBody) -> Result<()> {
        // Get the pending request
        if let Some(request) = self.pending_requests.get_mut(&response.request_id) {
            // Add response to the list
            request.responses.push(response);
        }

        Ok(())
    }

    /// Send a request to other nodes and wait for responses
    pub async fn send_request(
        &mut self,
        app_id: &str,
        request_type: RequestType,
        channel: Option<&str>,
        socket_id: Option<&str>,
        user_id: Option<&str>,
        expected_node_count: usize,
    ) -> Result<ResponseBody> {
        // Generate a new request ID
        let request_id = Uuid::new_v4().to_string();

        // Create the request
        let request = RequestBody {
            request_id: request_id.clone(),
            node_id: self.node_id.clone(),
            app_id: app_id.to_string(),
            request_type,
            channel: channel.map(String::from),
            socket_id: socket_id.map(String::from),
            user_id: user_id.map(String::from),
        };

        // Add to pending requests
        self.pending_requests.insert(
            request_id.clone(),
            PendingRequest {
                start_time: Instant::now(),
                app_id: app_id.to_string(),
                responses: Vec::new(),
            },
        );

        // Serialize the request
        let request_json = serde_json::to_string(&request)?;

        // This would be implemented by the specific adapter (Redis, etc.)
        // self.broadcast_request(request_json).await?;

        // Wait for responses
        let timeout = self.requests_timeout;
        let start = Instant::now();

        // Maximum nodes to wait for (don't wait for more than expected_node_count)
        let max_nodes = if expected_node_count > 1 {
            expected_node_count - 1
        } else {
            0
        };

        // Combine the results
        let mut combined_response = ResponseBody {
            request_id: request_id.clone(),
            node_id: self.node_id.clone(),
            app_id: app_id.to_string(),
            members: HashMap::new(),
            socket_ids: Vec::new(),
            sockets_count: 0,
            exists: false,
        };

        // Wait for responses until timeout or we have enough responses
        while start.elapsed().as_millis() < timeout as u128 {
            // Check if we have the request
            if let Some(request) = self.pending_requests.get(&request_id) {
                // Check if we have enough responses
                if request.responses.len() >= max_nodes {
                    break;
                }
            } else {
                // Request was removed, something went wrong
                return Err(Error::Other("Request was removed".into()));
            }

            // Sleep a bit to avoid busy waiting
            sleep(Duration::from_millis(10)).await;
        }

        // Get all responses
        if let Some(request) = self.pending_requests.remove(&request_id) {
            // Combine the results
            for response in request.responses {
                // Add members
                combined_response.members.extend(response.members);

                // Add socket IDs
                combined_response.socket_ids.extend(response.socket_ids);

                // Add socket count
                combined_response.sockets_count += response.sockets_count;

                // If any node says socket exists, it exists
                combined_response.exists = combined_response.exists || response.exists;
            }
        }

        // Return the combined response
        Ok(combined_response)
    }
}
