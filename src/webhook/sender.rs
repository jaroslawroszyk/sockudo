// src/webhook_sender.rs (Updated)

use crate::app::config::App;
// ... other imports from your code ...
use crate::error::Result; // Assuming your Result type
use crate::log::Log;
use crate::queue::manager::{QueueManager}; // Assuming an async processor type alias
use crate::webhook::types::{AppWebhook, ClientEventData, JobData, JobPayload, Webhook, WebhookEvent, LambdaConfig};
use async_trait::async_trait;
use hmac::KeyInit;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Client as HttpClient;
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tokio::time::sleep;
use aws_sdk_lambda::{Client as LambdaClient, config::Region};
use futures::future::{join_all, Future, BoxFuture}; // Import BoxFuture
use serde_json::json;
use std::pin::Pin;
pub type JobProcessorFnAsync = Box<
    dyn Fn(JobData) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync + 'static,
>;

type HmacSha256 = Hmac<Sha256>;

// --- WebhookSenderConfig remains the same ---
#[derive(Clone)]
pub struct WebhookSenderConfig {
    pub process_id: String,
    pub debug: bool,
    pub batching_enabled: bool,
    pub batching_duration: u64, // milliseconds
    pub can_process_queues: bool,
    pub http_timeout: u64, // seconds
    pub max_retries: usize,
    pub retry_delay: u64, // milliseconds
}

impl Default for WebhookSenderConfig {
    fn default() -> Self {
        Self {
            process_id: uuid::Uuid::new_v4().to_string(),
            debug: false,
            batching_enabled: false,
            batching_duration: 50, // 50ms default
            can_process_queues: true,
            http_timeout: 30, // 30 seconds
            max_retries: 3,
            retry_delay: 1000, // 1 second
        }
    }
}


// --- WebhookSender struct remains the same ---
pub struct WebhookSender {
    queue_manager: Arc<QueueManager>, // Use dyn trait object
    batch: Arc<Mutex<HashMap<String, Vec<ClientEventData>>>>,
    batch_leaders: Arc<Mutex<HashMap<String, bool>>>,
    http_client: HttpClient,
    lambda_clients: Arc<Mutex<HashMap<String, LambdaClient>>>,
    config: WebhookSenderConfig,
}


impl WebhookSender {
    pub async fn new(queue_manager: Arc<QueueManager>, config: WebhookSenderConfig) -> Self {
        let http_client = HttpClient::builder()
            .timeout(Duration::from_secs(config.http_timeout))
            .build()
            .unwrap_or_else(|_| HttpClient::new());

        let webhook_sender = Self {
            queue_manager,
            batch: Arc::new(Mutex::new(HashMap::new())),
            batch_leaders: Arc::new(Mutex::new(HashMap::new())),
            http_client,
            lambda_clients: Arc::new(Mutex::new(HashMap::new())),
            config: config.clone(),
        };

        if config.can_process_queues {
            webhook_sender.initialize_queue_processors().await;
        }

        webhook_sender
    }

    async fn initialize_queue_processors(&self) {
        self.setup_queue_processor(WebhookEvent::ClientEvent.to_queue_name()).await;
        self.setup_queue_processor(WebhookEvent::ChannelOccupied.to_queue_name()).await;
        self.setup_queue_processor(WebhookEvent::ChannelVacated.to_queue_name()).await;
        self.setup_queue_processor(WebhookEvent::MemberAdded.to_queue_name()).await;
        self.setup_queue_processor(WebhookEvent::MemberRemoved.to_queue_name()).await;
        self.setup_queue_processor(WebhookEvent::CacheMiss.to_queue_name()).await;
    }

    /// Setup an ASYNCHRONOUS queue processor for a specific webhook type
    async fn setup_queue_processor(&self, queue_name: &str) {
        Log::info(format!("====== SETTING UP ASYNC QUEUE PROCESSOR: {} ======", queue_name));
        let client = self.http_client.clone();
        let debug = self.config.debug;
        // let process_id = self.config.process_id.clone(); // Not used in processor currently
        let max_retries = self.config.max_retries;
        let retry_delay = self.config.retry_delay;
        let lambda_clients = self.lambda_clients.clone();

        // *** FIX: Clone queue_name to an owned String BEFORE the closure ***
        let owned_queue_name = queue_name.to_string();

        // Define the ASYNCHRONOUS processor closure
        // The closure now returns a Pinned Boxed Future
        let processor: JobProcessorFnAsync = Box::new(move |job_data: JobData| {
            // Clone necessary variables for the async block
            let client_clone = client.clone();
            let lambda_clients_clone = lambda_clients.clone();
            // *** FIX: Use the owned_queue_name captured by the closure ***
            let q_name = owned_queue_name.clone();

            // Return a Pinned Boxed Future
            Box::pin(async move {
                Log::info(format!("====== PROCESSING WEBHOOK ASYNC FROM QUEUE: {} ======", q_name));
                Log::info(format!("App ID: {}", job_data.app_id));
                Log::info(format!("App Key: {}", job_data.app_key));
                Log::info(format!("Event count: {}", job_data.payload.events.len()));

                let app_id = job_data.app_id.clone();
                let event_types: Vec<String> = job_data.payload.events.iter()
                    .map(|e| e.name.clone())
                    .collect();

                Log::info(format!("Event types: {:?}", event_types));
                let app_key = job_data.app_key.clone();
                let payload = job_data.payload.clone();
                let original_signature = job_data.original_signature.clone();

                if debug {
                    Log::webhook_sender_title("🚀 Processing webhook from queue (async).");
                    Log::webhook_sender_title(format!("Processing webhook for app {}", app_id));
                }

                // --- Signature Verification ---
                // TODO: Replace mock secret with actual secret retrieval (e.g., from an AppManager)
                let app_secret = "demo-secret"; // Placeholder - fetch the actual secret for app_key/app_id
                let payload_string = match serde_json::to_string(&payload) {
                    Ok(s) => s,
                    Err(e) => {
                        Log::error(format!("Failed to serialize payload for signature check: {}", e));
                        // Ensure your error enum has a variant like SerializationError
                        return Err(crate::error::Error::SerializationError(e.to_string()));
                    }
                };
                let signature = create_webhook_hmac(&payload_string, app_secret);
                Log::info(format!("Computed signature: {}", signature));
                Log::info(format!("Original signature: {}", job_data.original_signature));
                Log::info(format!("Signatures match: {}", signature == job_data.original_signature));

                // if signature != original_signature {
                //     if debug {
                //         Log::error("Webhook signature mismatch - possible tampering detected");
                //     }
                //     // Return an actual error if signature fails
                //     return Err(crate::error::Error::Other("Signature mismatch".to_string()));
                // }

                // --- App Data Retrieval (Replace Mock) ---
                // TODO: Implement actual App lookup using app_key or app_id
                // let app_manager = get_app_manager_somehow(); // Dependency injection or global state
                // let app = match app_manager.find_by_key(&app_key).await {
                //     Ok(Some(app_data)) => app_data,
                //     Ok(None) => {
                //         Log::error(format!("App not found for key: {}", app_key));
                //         return Err(crate::error::Error::NotFound(format!("App key {}", app_key)));
                //     }
                //     Err(e) => {
                //         Log::error(format!("Error fetching app data: {}", e));
                //         return Err(e); // Propagate the error
                //     }
                // };
                // Using mock app for now:
                let mock_app = App {
                    id: app_id.clone(),
                    key: app_key.clone(),
                    secret: app_secret.to_string(), // Use the same secret used for verification
                    webhooks: Some(vec![
                        Webhook {
                            url: Some("http://localhost:3000/webhook".to_string()), // Example endpoint
                            lambda_function: None,
                            lambda: None,
                            event_types: vec![
                                WebhookEvent::ClientEvent.as_str().to_string(),
                                WebhookEvent::MemberAdded.as_str().to_string(),
                                WebhookEvent::MemberRemoved.as_str().to_string()
                            ],
                            filter: None,
                            headers: Some(HashMap::new()),
                        },
                        // Add another webhook for testing Lambda if desired
                        // Webhook {
                        //     url: None,
                        //     lambda_function: Some("my-webhook-processor-lambda".to_string()),
                        //     lambda: Some(LambdaConfig { r#async: true, region: Some("us-west-2".to_string()) }),
                        //     event_types: vec![WebhookEvent::ChannelOccupied.as_str().to_string()],
                        //     filter: None,
                        //     headers: Some(HashMap::new()),
                        // }
                    ]),
                    enable_client_messages: true, enabled: true, max_connections: 1000,
                    max_client_events_per_second: 100, max_backend_events_per_second: None,
                    max_read_requests_per_second: None, max_presence_members_per_channel: None,
                    max_presence_member_size_in_kb: None, max_channel_name_length: None,
                    max_event_channels_at_once: None, max_event_name_length: None,
                    max_event_payload_in_kb: None, max_event_batch_size: None,
                    enable_user_authentication: None,
                };
                // Use `app` instead of `mock_app` below once lookup is implemented
                let webhooks = mock_app.get_webhooks(); // Assuming this method exists

                // --- Process Webhooks ---
                let representative_event_type = payload.events.first().map(|e| e.name.as_str()).unwrap_or("");

                let applicable_webhooks: Vec<&Webhook> = webhooks.iter()
                    .filter(|w| w.event_types.iter().any(|et| et == representative_event_type))
                    .collect();

                if applicable_webhooks.is_empty() {
                    if debug {
                        Log::webhook_sender(format!("No webhooks configured for event type '{}'", representative_event_type));
                    }
                    return Ok(());
                }

                let mut spawned_futures = Vec::new(); // Store JoinHandles
                for webhook in applicable_webhooks {
                    // --- Filtering Logic ---
                    // This logic filters the *entire* webhook based on *any* event not matching.
                    // Consider filtering the `payload.events` *within* the batch if needed per webhook.
                    let mut should_send_webhook = true;
                    if let Some(filter) = &webhook.filter {
                        for event in &payload.events {
                            let mut event_matches_filter = true;
                            if let Some(starts_with) = &filter.channel_name_starts_with {
                                if !event.channel.starts_with(starts_with) { event_matches_filter = false; }
                            }
                            if event_matches_filter {
                                if let Some(ends_with) = &filter.channel_name_ends_with {
                                    if !event.channel.ends_with(ends_with) { event_matches_filter = false; }
                                }
                            }
                            if !event_matches_filter {
                                should_send_webhook = false;
                                break;
                            }
                        }
                    }

                    if !should_send_webhook {
                        if debug {
                            Log::webhook_sender(format!("Skipping webhook {:?} due to channel filter", webhook.url.as_ref().or(webhook.lambda_function.as_ref())));
                        }
                        continue;
                    }

                    // --- Payload Preparation ---
                    // TODO: Add X-Pusher-Key and X-Pusher-Signature headers if required by endpoints
                    let webhook_payload = json!({
                        "time_ms": payload.time_ms,
                        "events": payload.events, // Send all events if webhook wasn't skipped
                    });

                    // --- Spawn Tasks for Sending ---
                    if let Some(url) = webhook.url.clone() {
                        let http_client_c = client_clone.clone();
                        let headers_c = webhook.headers.clone().unwrap_or_default();
                        let payload_c = webhook_payload.clone();
                        spawned_futures.push(tokio::spawn(async move {
                            send_http_webhook(
                                http_client_c, url, payload_c, headers_c,
                                max_retries, retry_delay, debug,
                            ).await
                        }));
                    } else if let Some(lambda_function) = webhook.lambda_function.clone() {
                        let lambda_clients_c = lambda_clients_clone.clone();
                        let lambda_config_c = webhook.lambda.clone();
                        let payload_c = webhook_payload.clone();
                        spawned_futures.push(tokio::spawn(async move {
                            send_lambda_webhook(
                                lambda_clients_c, lambda_function, payload_c, lambda_config_c,
                                max_retries, retry_delay, debug,
                            ).await
                        }));
                    }
                }

                // Wait for all spawned sending tasks to complete
                let results = join_all(spawned_futures).await;

                // Check results and log errors
                let mut overall_success = true;
                for result in results {
                    match result {
                        Ok(Ok(())) => { /* Send succeeded */ }
                        Ok(Err(e)) => {
                            Log::error(format!("Webhook send operation failed: {}", e));
                            overall_success = false; // Mark failure if any webhook send fails
                        }
                        Err(e) => {
                            Log::error(format!("Webhook send task panicked or failed to join: {}", e));
                            overall_success = false; // Mark failure if task fails
                        }
                    }
                }

                // Decide if the job should be considered failed based on webhook results
                if overall_success {
                    Ok(()) // Return Ok from the main processor future
                } else {
                    // Return an error to potentially signal the queue manager to retry the job
                    Err(crate::error::Error::Other("One or more webhook sends failed".to_string()))
                }
            }) // End of Box::pin(async move { ... })
        }); // End of Box::new closure

        // Register the ASYNCHRONOUS processor with the queue manager
        Log::info("Registering async processor with queue manager");
        // Ensure queue_manager has a method accepting JobProcessorFnAsync
        match self.queue_manager.process_queue(queue_name, processor).await {
            Ok(_) => Log::info(format!("Successfully registered async processor for queue: {}", queue_name)),
            Err(e) => Log::error(format!("Failed to register async processor for queue {}: {}", queue_name, e))
        }

        Log::info(format!(
            "Initialized ASYNC webhook queue processor for {}",
            queue_name
        ));
    }

    // --- Other methods (send_client_event, send_webhook, etc.) remain largely the same ---
    // Make sure they call the correct queue methods if needed.

    /// Send a client event webhook
    pub async fn send_client_event(
        &self,
        app: &App,
        channel: &str,
        event: &str,
        data: serde_json::Value,
        socket_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<()> {
        if !app.has_client_event_webhooks() { // Assuming this method exists on App
            return Ok(());
        }

        let mut event_data = ClientEventData {
            name: WebhookEvent::ClientEvent.as_str().to_string(),
            channel: channel.to_string(),
            event: Some(event.to_string()),
            data: Some(data),
            socket_id: socket_id.map(|s| s.to_string()),
            user_id: None,
            time_ms: None, // Will be set in send_webhook
        };

        if let Some(id) = user_id {
            if channel.starts_with("presence-") { // Basic check for presence channel
                event_data.user_id = Some(id.to_string());
            }
        }

        self.send(app, event_data, WebhookEvent::ClientEvent.to_queue_name()).await
    }

    /// Send a channel occupied webhook
    pub async fn send_channel_occupied(&self, app: &App, channel: &str) -> Result<()> {
        if !app.has_channel_occupied_webhooks() { // Assuming this method exists
            return Ok(());
        }
        let event_data = ClientEventData {
            name: WebhookEvent::ChannelOccupied.as_str().to_string(),
            channel: channel.to_string(),
            event: None, data: None, socket_id: None, user_id: None, time_ms: None,
        };
        self.send(app, event_data, WebhookEvent::ChannelOccupied.to_queue_name()).await
    }

    /// Send a channel vacated webhook
    pub async fn send_channel_vacated(&self, app: &App, channel: &str) -> Result<()> {
        if !app.has_channel_vacated_webhooks() { // Assuming this method exists
            return Ok(());
        }
        let event_data = ClientEventData {
            name: WebhookEvent::ChannelVacated.as_str().to_string(),
            channel: channel.to_string(),
            event: None, data: None, socket_id: None, user_id: None, time_ms: None,
        };
        self.send(app, event_data, WebhookEvent::ChannelVacated.to_queue_name()).await
    }

    /// Send a member added webhook
    pub async fn send_member_added(&self, app: &App, channel: &str, user_id: &str) -> Result<()> {
        if !app.has_member_added_webhooks() { // Assuming this method exists
            return Ok(());
        }
        let event_data = ClientEventData {
            name: WebhookEvent::MemberAdded.as_str().to_string(),
            channel: channel.to_string(),
            user_id: Some(user_id.to_string()),
            event: None, data: None, socket_id: None, time_ms: None,
        };
        Log::info(format!("Queueing member_added webhook for user {}", user_id));
        self.send(app, event_data, WebhookEvent::MemberAdded.to_queue_name()).await
    }

    /// Send a member removed webhook
    pub async fn send_member_removed(&self, app: &App, channel: &str, user_id: &str) -> Result<()> {
        if !app.has_member_removed_webhooks() { // Assuming this method exists
            return Ok(());
        }
        let event_data = ClientEventData {
            name: WebhookEvent::MemberRemoved.as_str().to_string(),
            channel: channel.to_string(),
            user_id: Some(user_id.to_string()),
            event: None, data: None, socket_id: None, time_ms: None,
        };
        Log::info(format!("Queueing member_removed webhook for user {}", user_id));
        self.send(app, event_data, WebhookEvent::MemberRemoved.to_queue_name()).await
    }

    /// Send a cache miss webhook
    pub async fn send_cache_missed(&self, app: &App, channel: &str) -> Result<()> {
        if !app.has_cache_missed_webhooks() { // Assuming this method exists
            return Ok(());
        }
        let event_data = ClientEventData {
            name: WebhookEvent::CacheMiss.as_str().to_string(),
            channel: channel.to_string(),
            event: None, data: None, socket_id: None, user_id: None, time_ms: None,
        };
        self.send(app, event_data, WebhookEvent::CacheMiss.to_queue_name()).await
    }


    /// Send webhook with appropriate batching strategy
    async fn send(&self, app: &App, data: ClientEventData, queue_name: &str) -> Result<()> {
        Log::info(format!("Preparing to send webhook for queue: {}", queue_name));
        self.send_webhook(app, vec![data], queue_name).await
        // if self.config.batching_enabled {
        //     Log::info("Batching webhook");
        //     self.send_webhook_by_batching(app, data, queue_name).await
        // } else {
        //     Log::info("Sending webhook directly (no batching)");
        //     self.send_webhook(app, vec![data], queue_name).await
        // }
    }

    /// Send a batch of webhook events without batching (adds to queue)
    async fn send_webhook(
        &self,
        app: &App,
        events: Vec<ClientEventData>,
        queue_name: &str,
    ) -> Result<()> {
        Log::info("====== SENDING WEBHOOK(S) TO QUEUE (send_webhook) ======");
        Log::info(format!("Queue name: {}", queue_name));
        Log::info(format!("Events count: {}", events.len()));

        if events.is_empty() {
            Log::info("No events to send, returning");
            return Ok(());
        }

        // Add current time to the events
        let mut events_with_time = events; // Take ownership
        let time_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_millis() as u64; // Use u64 consistent with JobPayload

        for event in &mut events_with_time {
            event.time_ms = Some(time_ms);
        }

        let payload = JobPayload { time_ms, events: events_with_time }; // Move events_with_time

        // Create HMAC signature for payload
        let payload_json = serde_json::to_string(&payload)?; // Handle error
        Log::info(format!("Payload JSON: {}", payload_json));

        // Make sure app.secret is accessible and correct
        let signature = create_webhook_hmac(&payload_json, &app.secret);
        Log::info(format!("Created signature: {}", signature));

        let job_data = JobData {
            app_key: app.key.clone(),
            app_id: app.id.clone(),
            payload, // Move payload
            original_signature: signature,
        };

        // Add to queue for processing
        Log::info(format!("Adding job to queue: {}", queue_name));
        self.queue_manager.add_to_queue(queue_name, job_data).await?; // Propagate error

        if self.config.debug {
            Log::webhook_sender_title(format!("Added job to {} queue", queue_name));
        }

        Ok(())
    }

    /// Send a webhook with batching support
    async fn send_webhook_by_batching(
        &self,
        app: &App,
        data: ClientEventData,
        queue_name: &str,
    ) -> Result<()> {
        Log::info(format!("Adding event to batch for queue: {}", queue_name));
        // Add event to batch
        { // Scope for batch lock
            let mut batches = self.batch.lock().await;
            let batch = batches.entry(queue_name.to_string()).or_default();
            batch.push(data);
        } // Lock released

        // Check if we need to become the batch leader
        let become_leader = { // Scope for leaders lock
            let mut leaders = self.batch_leaders.lock().await;
            if !leaders.get(queue_name).copied().unwrap_or(false) {
                leaders.insert(queue_name.to_string(), true);
                true // Become the leader
            } else {
                false // Leader already exists
            }
        }; // Lock released

        // If we became the leader, start the batching timer task
        if become_leader {
            Log::info(format!("Became batch leader for queue: {}. Starting timer.", queue_name));
            // Clone necessary data for the timer task
            // Use Arc for shared data like queue_manager, batch, batch_leaders
            let app_clone = app.clone(); // Clone App data needed (or relevant fields)
            let queue_name_clone = queue_name.to_string();
            let batch_clone = self.batch.clone();
            let batch_leaders_clone = self.batch_leaders.clone();
            let queue_manager_clone = self.queue_manager.clone();
            let batching_duration = self.config.batching_duration;
            let debug = self.config.debug;

            tokio::spawn(async move {
                sleep(Duration::from_millis(batching_duration)).await;
                Log::info(format!("Batch timer finished for queue: {}", queue_name_clone));

                // Get all events from the batch for this queue
                let events_to_send = { // Scope for batch lock
                    let mut batches = batch_clone.lock().await;
                    // Remove the entry for this queue and get the events
                    batches.remove(&queue_name_clone).unwrap_or_default()
                }; // Lock released

                // Release leadership *after* processing the batch
                { // Scope for leaders lock
                    let mut leaders = batch_leaders_clone.lock().await;
                    leaders.insert(queue_name_clone.clone(), false); // Reset leader status
                    Log::info(format!("Released batch leadership for queue: {}", queue_name_clone));
                } // Lock released


                // If events were collected, send them as a single job
                if !events_to_send.is_empty() {
                    Log::info(format!("Sending batch of {} events for queue: {}", events_to_send.len(), queue_name_clone));
                    // Create a temporary WebhookSender instance or call send_webhook directly
                    // Need to reconstruct the necessary parts or pass them
                    let sender_config = WebhookSenderConfig { batching_enabled: false, ..Default::default() }; // Temp config to force direct send
                    let temp_sender = WebhookSender { // Simplified temporary sender
                        queue_manager: queue_manager_clone,
                        batch: Arc::new(Mutex::new(HashMap::new())), // Not used for direct send
                        batch_leaders: Arc::new(Mutex::new(HashMap::new())), // Not used
                        http_client: HttpClient::new(), // Create a new one or pass if needed
                        lambda_clients: Arc::new(Mutex::new(HashMap::new())), // Pass if needed
                        config: sender_config,
                    };

                    // Call send_webhook (which adds to the queue)
                    if let Err(e) = temp_sender.send_webhook(&app_clone, events_to_send, &queue_name_clone).await {
                        Log::error(format!("Error sending webhook batch for queue {}: {}", queue_name_clone, e));
                    } else if debug {
                        Log::webhook_sender_title(format!(
                            "Batched webhook job added to {} queue",
                            queue_name_clone
                        ));
                    }
                } else {
                    Log::info(format!("No events in batch for queue {} after timer.", queue_name_clone));
                }
            });
        } else {
            Log::info(format!("Batch leader already exists for queue: {}", queue_name));
        }

        Ok(())
    }
}

// --- create_webhook_hmac function remains the same ---
pub fn create_webhook_hmac(data: &str, secret: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(data.as_bytes());
    let result = mac.finalize();
    hex::encode(result.into_bytes())
}


// --- send_http_webhook function remains the same ---
async fn send_http_webhook(
    client: HttpClient,
    url: String,
    payload: serde_json::Value,
    headers: HashMap<String, String>,
    max_retries: usize,
    retry_delay_ms: u64,
    debug: bool,
) -> Result<()> { // Return your app's Result type
    if debug {
        Log::webhook_sender_title(format!("Sending webhook to URL: {}", url));
    }

    let mut request_headers = HeaderMap::new();
    request_headers.insert("Content-Type", HeaderValue::from_static("application/json"));
    // Add X-Pusher-Key and X-Pusher-Signature if needed by endpoint
    // request_headers.insert("X-Pusher-Key", HeaderValue::from_str(&app_key)?);
    // request_headers.insert("X-Pusher-Signature", HeaderValue::from_str(&signature)?);


    for (key, value) in headers {
        if let Ok(header_name) = key.parse::<reqwest::header::HeaderName>() {
            if let Ok(header_value) = value.parse::<reqwest::header::HeaderValue>() {
                // Prevent overwriting essential headers if necessary
                // if header_name != reqwest::header::CONTENT_TYPE {
                request_headers.insert(header_name, header_value);
                // }
            } else {
                Log::warning(format!("Invalid header value for key '{}': {}", key, value));
            }
        } else {
            Log::warning(format!("Invalid header name: {}", key));
        }
    }

    let mut attempts = 0;
    loop {
        let request_builder = client.post(&url)
            .headers(request_headers.clone())
            .json(&payload);

        match request_builder.send().await {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    if debug {
                        Log::webhook_sender(format!(
                            "Successfully sent webhook to {}, status: {}",
                            url, status
                        ));
                    }
                    return Ok(()); // Success
                } else {
                    let body = response.text().await.unwrap_or_else(|_| "Failed to read body".to_string());
                    Log::error(format!(
                        "Webhook attempt {} to {} failed with status: {}. Body: {}",
                        attempts + 1, url, status, body
                    ));
                    if attempts >= max_retries -1 {
                        return Err(crate::error::Error::Other(format!("HTTP error {} after {} retries", status, max_retries)));
                    }
                }
            },
            Err(e) => {
                Log::error(format!(
                    "Webhook attempt {} to {} failed with request error: {}",
                    attempts + 1, url, e
                ));
                if attempts >= max_retries - 1 {
                    return Err(crate::error::Error::Other(format!("Request error after {} retries: {}", max_retries, e)));
                }
            }
        }

        // Increment attempts and retry after delay
        attempts += 1;
        sleep(Duration::from_millis(retry_delay_ms * (attempts as u64))).await; // Exponential backoff example
    }
    // Unreachable due to loop structure, errors are returned inside
}


// --- send_lambda_webhook function remains the same ---
async fn send_lambda_webhook(
    lambda_clients: Arc<Mutex<HashMap<String, LambdaClient>>>,
    function_name: String,
    payload: serde_json::Value,
    lambda_config: Option<LambdaConfig>, // Use the type defined in your types module
    max_retries: usize,
    retry_delay_ms: u64,
    debug: bool,
) -> Result<()> { // Return your app's Result type
    if debug {
        Log::webhook_sender_title(format!("Sending webhook to Lambda: {}", function_name));
    }

    let region = lambda_config
        .as_ref()
        .and_then(|cfg| cfg.region.clone())
        .unwrap_or_else(|| "us-east-1".to_string()); // Default region

    // Get or create Lambda client for the specific region
    let client = {
        let mut clients_guard = lambda_clients.lock().await;
        if !clients_guard.contains_key(&region) {
            Log::info(format!("Creating new Lambda client for region: {}", region));
            // Load AWS config dynamically for the required region
            let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(Region::new(region.clone()))
                .load()
                .await;
            let lambda_client = LambdaClient::new(&aws_config);
            clients_guard.insert(region.clone(), lambda_client);
        }
        // Clone the client Arc for use outside the lock
        clients_guard.get(&region).unwrap().clone()
    }; // Lock released


    // let invocation_type = if lambda_config.map_or(false, |cfg| cfg.r#async) { // Access the 'async' field correctly
    //     aws_sdk_lambda::types::InvocationType::Event // Fire-and-forget
    // } else {
    //     aws_sdk_lambda::types::InvocationType::RequestResponse // Wait for response
    // };

    let invocation_type = aws_sdk_lambda::types::InvocationType::Event;

    if debug {
        Log::webhook_sender(format!("Lambda invocation type: {:?}", invocation_type));
    }

    let payload_bytes = match serde_json::to_vec(&payload) {
        Ok(bytes) => aws_sdk_lambda::primitives::Blob::new(bytes),
        Err(e) => {
            Log::error(format!("Failed to serialize payload for Lambda function {}: {}", function_name, e));
            // Return an error instead of Ok(()) if serialization fails
            return Err(crate::error::Error::SerializationError(e.to_string()));
        }
    };

    let mut attempts = 0;
    loop {
        let request_builder = client
            .invoke()
            .function_name(&function_name)
            .invocation_type(invocation_type.clone())
            .payload(payload_bytes.clone()); // Clone Blob for retries

        match request_builder.send().await {
            Ok(output) => {
                // Check for function error in the response payload (for RequestResponse)
                if let Some(function_error) = output.function_error() {
                    Log::error(format!(
                        "Lambda attempt {} for {} failed with function error: {}",
                        attempts + 1, function_name, function_error
                    ));
                    // Optionally log the payload returned on error
                    if debug {
                        if let Some(payload_blob) = output.payload() {
                            Log::webhook_sender(format!("Lambda error payload: {}", String::from_utf8_lossy(payload_blob.as_ref())));
                        }
                    }
                    if attempts >= max_retries - 1 {
                        return Err(crate::error::Error::Other(format!("Lambda function error '{}' after {} retries", function_error, max_retries)));
                    }
                }
                // Check HTTP status code (less reliable indicator for Lambda success than function_error)
                // 200 for RequestResponse success, 202 for Event success, 204 No Content (sometimes)
                else if output.status_code() >= 200 && output.status_code() < 300 {
                    if debug {
                        Log::webhook_sender(format!(
                            "Successfully invoked Lambda {}, status: {}",
                            function_name, output.status_code()
                        ));
                        // Optionally log success payload for RequestResponse
                        if invocation_type == aws_sdk_lambda::types::InvocationType::RequestResponse {
                            if let Some(payload_blob) = output.payload() {
                                Log::webhook_sender(format!("Lambda success payload: {}", String::from_utf8_lossy(payload_blob.as_ref())));
                            }
                        }
                    }
                    return Ok(()); // Success
                } else {
                    // Handle unexpected status codes
                    Log::error(format!(
                        "Lambda attempt {} for {} failed with unexpected status: {}. Function error field was None.",
                        attempts + 1, function_name, output.status_code()
                    ));
                    if attempts >= max_retries - 1 {
                        return Err(crate::error::Error::Other(format!("Lambda unexpected status {} after {} retries", output.status_code(), max_retries)));
                    }
                }
            },
            Err(e) => {
                // Handle SDK errors (network issues, permissions, etc.)
                Log::error(format!(
                    "Lambda attempt {} for {} failed with SDK error: {}",
                    attempts + 1, function_name, e
                ));
                if attempts >= max_retries - 1 {
                    // Extract specific SDK error kind if possible
                    let error_details = format!("{}", e);
                    return Err(crate::error::Error::Other(format!("Lambda SDK error after {} retries: {}", max_retries, error_details)));
                }
            }
        }

        // Increment attempts and retry after delay
        attempts += 1;
        sleep(Duration::from_millis(retry_delay_ms * (attempts as u64))).await; // Exponential backoff example
    }
    // Unreachable due to loop structure, errors are returned inside
}