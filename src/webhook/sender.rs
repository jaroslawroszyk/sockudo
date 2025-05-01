use crate::app::config::App;
use crate::channel::ChannelType;
use crate::error::Result;
use crate::log::Log;
use crate::token::Token;
use crate::webhook::types::{AppWebhook, ClientEventData, JobData, JobPayload, Webhook, WebhookEvent};
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
use crate::queue::manager::QueueManager;
use aws_sdk_lambda::{Client as LambdaClient, config::Region};
use futures::future::{join_all, Future};
use serde_json::json;
use std::pin::Pin;

type HmacSha256 = Hmac<Sha256>;

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

pub struct WebhookSender {
    /// Queue manager for processing webhooks
    queue_manager: Arc<QueueManager>,

    /// Batched events waiting to be sent
    batch: Arc<Mutex<HashMap<String, Vec<ClientEventData>>>>,

    /// Batch processing flags per queue
    batch_leaders: Arc<Mutex<HashMap<String, bool>>>,

    /// HTTP client for sending webhooks
    http_client: HttpClient,

    /// AWS Lambda client cache
    lambda_clients: Arc<Mutex<HashMap<String, LambdaClient>>>,

    /// Configuration for the webhook sender
    config: WebhookSenderConfig,
}

impl WebhookSender {
    pub async fn new(queue_manager: Arc<QueueManager>, config: WebhookSenderConfig) -> Self {
        // Create HTTP client with configurable timeout
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

        // Initialize queue processors if enabled
        if config.can_process_queues {
            webhook_sender.initialize_queue_processors().await;
        }

        webhook_sender
    }

    /// Initialize queue processors for webhook types
    async fn initialize_queue_processors(&self) {
        // Setup queue processors for the different webhook types
        self.setup_queue_processor(WebhookEvent::ClientEvent.to_queue_name())
            .await;
        self.setup_queue_processor(WebhookEvent::ChannelOccupied.to_queue_name())
            .await;
        self.setup_queue_processor(WebhookEvent::ChannelVacated.to_queue_name())
            .await;
        self.setup_queue_processor(WebhookEvent::MemberAdded.to_queue_name())
            .await;
        self.setup_queue_processor(WebhookEvent::MemberRemoved.to_queue_name())
            .await;
        self.setup_queue_processor(WebhookEvent::CacheMiss.to_queue_name())
            .await;
    }

    /// Setup a queue processor for a specific webhook type
    async fn setup_queue_processor(&self, queue_name: &str) {
        let client = self.http_client.clone();
        let debug = self.config.debug;
        let process_id = self.config.process_id.clone();
        let max_retries = self.config.max_retries;
        let retry_delay = self.config.retry_delay;
        let lambda_clients = self.lambda_clients.clone();

        let processor = Box::new(move |job_data: JobData| -> Result<()> {
            let app_id = job_data.app_id.clone();
            let app_key = job_data.app_key.clone();
            let payload = job_data.payload.clone();
            let original_signature = job_data.original_signature.clone();

            if debug {
                Log::webhook_sender_title("🚀 Processing webhook from queue.");
                Log::webhook_sender_title(format!("Processing webhook for app {}", app_id));
            }

            // Create signature for verification
            let signature =
                create_webhook_hmac(&serde_json::to_string(&payload).unwrap(), "app_secret");
            if signature != original_signature {
                // Signature mismatch, potential tampering
                if debug {
                    Log::error("Webhook signature mismatch - possible tampering detected");
                }
                return Ok(());
            }

            // We'd normally use app_manager here to get app info
            // For now, we mock an App object with the available info
            let mock_app = App {
                id: app_id.clone(),
                key: app_key.clone(),
                secret: "app_secret".to_string(), // This would normally come from app_manager
                enable_client_messages: true,
                enabled: true,
                max_connections: 1000,
                max_client_events_per_second: 100,
                max_backend_events_per_second: None,
                max_read_requests_per_second: None,
                max_presence_members_per_channel: None,
                max_presence_member_size_in_kb: None,
                max_channel_name_length: None,
                max_event_channels_at_once: None,
                max_event_name_length: None,
                max_event_payload_in_kb: None,
                max_event_batch_size: None,
                enable_user_authentication: None,
                webhooks: Some(vec![
                    Webhook {
                        url: Some("http://localhost:3000/webhook".to_string()),
                        lambda_function: None,
                        lambda: None,
                        event_types: vec![WebhookEvent::ClientEvent.as_str().to_string()],
                        filter: None,
                        headers: Some(HashMap::new()),
                    }
                ]),
            };

            // Process webhooks
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let webhooks = mock_app.get_webhooks();
                let event_type = WebhookEvent::ClientEvent.as_str();

                // Filter webhooks by event type
                let applicable_webhooks: Vec<&Webhook> = webhooks.iter()
                    .filter(|w| w.event_types.iter().any(|et| et == event_type))
                    .collect();

                if applicable_webhooks.is_empty() {
                    if debug {
                        Log::webhook_sender("No webhooks configured for this event type");
                    }
                    return Ok(());
                }

                // Process each webhook
                // Use Pin<Box<dyn Future>> instead of Box<dyn Future + Unpin>
                let mut futures: Vec<Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>> = Vec::new();
                for webhook in applicable_webhooks {
                    // Apply channel filters if applicable
                    if let Some(filter) = &webhook.filter {
                        let mut should_send = true;

                        // Check each event for channel filtering
                        for event in &payload.events {
                            if let Some(starts_with) = &filter.channel_name_starts_with {
                                if !event.channel.starts_with(starts_with) {
                                    should_send = false;
                                    break;
                                }
                            }

                            if let Some(ends_with) = &filter.channel_name_ends_with {
                                if !event.channel.ends_with(ends_with) {
                                    should_send = false;
                                    break;
                                }
                            }
                        }

                        if !should_send {
                            continue;
                        }
                    }

                    // Create webhook payload
                    let webhook_payload = json!({
                        "time_ms": payload.time_ms,
                        "events": payload.events,
                        "app_id": app_id,
                        "app_key": app_key
                    });

                    // Create a pinned boxed future that can be stored in the futures vector
                    let future: Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> =
                        if let Some(url) = &webhook.url {
                            // Send to HTTP URL
                            Box::pin(send_http_webhook(
                                client.clone(),
                                url.clone(),
                                webhook_payload.clone(),
                                webhook.headers.clone().unwrap_or_default(),
                                max_retries,
                                retry_delay,
                                debug,
                            ))
                        } else if let Some(lambda_function) = &webhook.lambda_function {
                            // Send to Lambda function
                            Box::pin(send_lambda_webhook(
                                lambda_clients.clone(),
                                lambda_function.clone(),
                                webhook_payload.clone(),
                                webhook.lambda.clone(),
                                max_retries,
                                retry_delay,
                                debug,
                            ))
                        } else {
                            // Neither URL nor Lambda function specified
                            Box::pin(std::future::ready(Ok(())))
                        };

                    futures.push(future);
                }

                // Wait for all webhook sends to complete
                if !futures.is_empty() {
                    join_all(futures).await;
                }

                Ok(())
            })
        });

        // Register the processor with the queue
        self.queue_manager
            .process_queue(queue_name, processor)
            .await
            .ok();

        Log::info(format!(
            "Initialized webhook queue processor for {}",
            queue_name
        ));
    }

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
        if !app.has_client_event_webhooks() {
            return Ok(());
        }

        let mut event_data = ClientEventData {
            name: WebhookEvent::ClientEvent.as_str().to_string(),
            channel: channel.to_string(),
            event: Some(event.to_string()),
            data: Some(data),
            socket_id: socket_id.map(|s| s.to_string()),
            user_id: None,
            time_ms: None,
        };

        // Add user_id for presence channels if provided
        if let Some(id) = user_id {
            if channel.starts_with("presence-") {
                event_data.user_id = Some(id.to_string());
            }
        }

        self.send(app, event_data, WebhookEvent::ClientEvent.to_queue_name())
            .await
    }

    /// Send a channel occupied webhook
    pub async fn send_channel_occupied(&self, app: &App, channel: &str) -> Result<()> {
        if !app.has_channel_occupied_webhooks() {
            return Ok(());
        }

        let event_data = ClientEventData {
            name: WebhookEvent::ChannelOccupied.as_str().to_string(),
            channel: channel.to_string(),
            event: None,
            data: None,
            socket_id: None,
            user_id: None,
            time_ms: None,
        };

        self.send(
            app,
            event_data,
            WebhookEvent::ChannelOccupied.to_queue_name(),
        )
            .await
    }

    /// Send a channel vacated webhook
    pub async fn send_channel_vacated(&self, app: &App, channel: &str) -> Result<()> {
        if !app.has_channel_vacated_webhooks() {
            return Ok(());
        }

        let event_data = ClientEventData {
            name: WebhookEvent::ChannelVacated.as_str().to_string(),
            channel: channel.to_string(),
            event: None,
            data: None,
            socket_id: None,
            user_id: None,
            time_ms: None,
        };

        self.send(
            app,
            event_data,
            WebhookEvent::ChannelVacated.to_queue_name(),
        )
            .await
    }

    /// Send a member added webhook
    pub async fn send_member_added(&self, app: &App, channel: &str, user_id: &str) -> Result<()> {
        if !app.has_member_added_webhooks() {
            return Ok(());
        }

        let event_data = ClientEventData {
            name: WebhookEvent::MemberAdded.as_str().to_string(),
            channel: channel.to_string(),
            event: None,
            data: None,
            socket_id: None,
            user_id: Some(user_id.to_string()),
            time_ms: None,
        };
        Log::info(format!("{} added webhook", user_id));

        self.send(app, event_data, WebhookEvent::MemberAdded.to_queue_name())
            .await
    }

    /// Send a member removed webhook
    pub async fn send_member_removed(&self, app: &App, channel: &str, user_id: &str) -> Result<()> {
        if !app.has_member_removed_webhooks() {
            return Ok(());
        }

        let event_data = ClientEventData {
            name: WebhookEvent::MemberRemoved.as_str().to_string(),
            channel: channel.to_string(),
            event: None,
            data: None,
            socket_id: None,
            user_id: Some(user_id.to_string()),
            time_ms: None,
        };

        self.send(app, event_data, WebhookEvent::MemberRemoved.to_queue_name())
            .await
    }

    /// Send a cache miss webhook
    pub async fn send_cache_missed(&self, app: &App, channel: &str) -> Result<()> {
        if !app.has_cache_missed_webhooks() {
            return Ok(());
        }

        let event_data = ClientEventData {
            name: WebhookEvent::CacheMiss.as_str().to_string(),
            channel: channel.to_string(),
            event: None,
            data: None,
            socket_id: None,
            user_id: None,
            time_ms: None,
        };

        self.send(app, event_data, WebhookEvent::CacheMiss.to_queue_name())
            .await
    }

    /// Send webhook with appropriate batching strategy
    async fn send(&self, app: &App, data: ClientEventData, queue_name: &str) -> Result<()> {
        Log::info(format!("{} added webhook", queue_name));
        if self.config.batching_enabled {
            Log::info("batching webhook");
            self.send_webhook_by_batching(app, data, queue_name).await
        } else {
            Log::info("batching not set");
            self.send_webhook(app, vec![data], queue_name).await
        }
    }

    /// Send a batch of webhook events without batching
    async fn send_webhook(
        &self,
        app: &App,
        events: Vec<ClientEventData>,
        queue_name: &str,
    ) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        // Add current time to the events
        let mut events_with_time = events;
        let time_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_millis() as u64;

        for event in &mut events_with_time {
            event.time_ms = Some(time_ms);
        }

        let payload = JobPayload { time_ms, events: events_with_time };

        // Create HMAC signature for payload
        let payload_json = serde_json::to_string(&payload)?;
        let signature = create_webhook_hmac(&payload_json, &app.secret);

        let job_data = JobData {
            app_key: app.key.clone(),
            app_id: app.id.clone(),
            payload,
            original_signature: signature,
        };

        // Add to queue for processing
        self.queue_manager
            .add_to_queue(queue_name, job_data)
            .await?;

        if self.config.debug {
            Log::webhook_sender_title(format!("Added webhook event to {} queue", queue_name));
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
        // Add event to batch
        {
            let mut batches = self.batch.lock().await;
            let batch = batches
                .entry(queue_name.to_string())
                .or_insert_with(Vec::new);
            batch.push(data);
        }

        // Check if we need to become the batch leader
        let mut become_leader = false;
        {
            let mut leaders = self.batch_leaders.lock().await;
            let leader_exists = leaders.get(queue_name).copied().unwrap_or(false);

            if !leader_exists {
                // Become the batch leader
                leaders.insert(queue_name.to_string(), true);
                become_leader = true;
            }
        }

        // If we're the leader, start the batching timer
        if become_leader {
            let app_key = app.key.clone();
            let app_id = app.id.clone();
            let app_secret = app.secret.clone();
            let queue_name = queue_name.to_string();
            let batch = self.batch.clone();
            let batch_leaders = self.batch_leaders.clone();
            let queue_manager = self.queue_manager.clone();
            let batching_duration = self.config.batching_duration;
            let debug = self.config.debug;

            tokio::spawn(async move {
                // Wait for batch duration
                sleep(Duration::from_millis(batching_duration)).await;

                // Get all events from batch
                let events = {
                    let mut batches = batch.lock().await;
                    if let Some(batch_events) = batches.get_mut(&queue_name) {
                        // Take all events and replace with empty vec
                        std::mem::take(batch_events)
                    } else {
                        Vec::new()
                    }
                };

                // Release leadership
                {
                    let mut leaders = batch_leaders.lock().await;
                    leaders.insert(queue_name.clone(), false);
                }

                // If we have events, send them
                if !events.is_empty() {
                    // Get current timestamp in milliseconds
                    let time_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_else(|_| Duration::from_secs(0))
                        .as_millis() as u64;

                    // Add time to events
                    let mut events_with_time = events;
                    for event in &mut events_with_time {
                        event.time_ms = Some(time_ms);
                    }

                    let payload = JobPayload { time_ms, events: events_with_time };

                    // Create HMAC signature for payload
                    let payload_json = serde_json::to_string(&payload).unwrap_or_default();
                    let signature = create_webhook_hmac(&payload_json, &app_secret);

                    let job_data = JobData {
                        app_key,
                        app_id,
                        payload,
                        original_signature: signature,
                    };

                    // Add to queue for processing
                    let _ = queue_manager
                        .add_to_queue(&queue_name, job_data.clone())
                        .await;

                    if debug {
                        Log::webhook_sender_title(format!(
                            "Batched {} webhook events sent to {} queue",
                            job_data.payload.events.len(),
                            queue_name
                        ));
                    }
                }
            });
        }

        Ok(())
    }
}

/// Create HMAC-SHA256 signature for webhook payload
pub fn create_webhook_hmac(data: &str, secret: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");

    mac.update(data.as_bytes());

    let result = mac.finalize();
    let code_bytes = result.into_bytes();

    hex::encode(code_bytes)
}

/// Send webhook via HTTP
async fn send_http_webhook(
    client: HttpClient,
    url: String,
    payload: serde_json::Value,
    headers: HashMap<String, String>,
    max_retries: usize,
    retry_delay_ms: u64,
    debug: bool,
) -> Result<()> {
    if debug {
        Log::webhook_sender_title(format!("Sending webhook to URL: {}", url));
    }

    // Create request headers
    let mut request_headers = HeaderMap::new();
    request_headers.insert("Content-Type", HeaderValue::from_static("application/json"));

    // Add custom headers
    for (key, value) in headers {
        if let Ok(header_name) = key.parse::<reqwest::header::HeaderName>() {
            if let Ok(header_value) = value.parse::<reqwest::header::HeaderValue>() {
                request_headers.insert(header_name, header_value);
            }
        }
    }

    // Attempt to send with retries
    let mut attempts = 0;
    let mut last_error = None;

    while attempts < max_retries {
        match client.post(&url)
            .headers(request_headers.clone())
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => {
                if response.status().is_success() {
                    if debug {
                        Log::webhook_sender(format!(
                            "Successfully sent webhook to {}, status: {}",
                            url, response.status()
                        ));
                    }
                    return Ok(());
                } else {
                    last_error = Some(format!("HTTP error: {}", response.status()));
                    if debug {
                        Log::webhook_sender(format!(
                            "Failed to send webhook to {}, status: {}",
                            url, response.status()
                        ));
                    }
                }
            },
            Err(e) => {
                last_error = Some(format!("Request error: {}", e));
                if debug {
                    Log::webhook_sender(format!(
                        "Failed to send webhook to {}, error: {}",
                        url, e
                    ));
                }
            }
        }

        // Increment attempts and retry after delay
        attempts += 1;
        if attempts < max_retries {
            sleep(Duration::from_millis(retry_delay_ms)).await;
        }
    }

    // Return error after all retries failed
    if let Some(error) = last_error {
        Log::error(format!("Webhook to {} failed after {} attempts: {}", url, max_retries, error));
    }

    Ok(()) // We don't fail the overall process for webhook errors
}

/// Send webhook via AWS Lambda
async fn send_lambda_webhook(
    lambda_clients: Arc<Mutex<HashMap<String, LambdaClient>>>,
    function_name: String,
    payload: serde_json::Value,
    lambda_config: Option<crate::webhook::types::LambdaConfig>,
    max_retries: usize,
    retry_delay_ms: u64,
    debug: bool,
) -> Result<()> {
    if debug {
        Log::webhook_sender_title(format!("Sending webhook to Lambda: {}", function_name));
    }

    // Get or create Lambda client for the region
    let region = lambda_config
        .as_ref()
        .and_then(|cfg| cfg.region.clone())
        .unwrap_or_else(|| "us-east-1".to_string());

    let client = {
        let mut clients = lambda_clients.lock().await;
        if !clients.contains_key(&region) {
            // Create AWS config
            let aws_config = aws_config::from_env()
                .region(Region::new(region.clone()))
                .load()
                .await;

            // Create Lambda client
            let lambda_client = LambdaClient::new(&aws_config);
            clients.insert(region.clone(), lambda_client);
        }

        clients.get(&region).unwrap().clone()
    };

    // Check if async invocation is requested
    let invocation_type = if lambda_config
        .as_ref()
        .and_then(|cfg| cfg.async_execution)
        .unwrap_or(false)
    {
        aws_sdk_lambda::types::InvocationType::Event
    } else {
        aws_sdk_lambda::types::InvocationType::RequestResponse
    };

    // Convert payload to bytes
    let payload_bytes = match serde_json::to_vec(&payload) {
        Ok(bytes) => bytes,
        Err(e) => {
            Log::error(format!("Failed to serialize Lambda payload: {}", e));
            return Ok(());
        }
    };

    // Attempt to invoke Lambda with retries
    let mut attempts = 0;
    let mut last_error = None;

    while attempts < max_retries {
        let request = client
            .invoke()
            .function_name(&function_name)
            .invocation_type(invocation_type.clone())
            .payload(aws_sdk_lambda::primitives::Blob::new(payload_bytes.clone()));

        match request.send().await {
            Ok(response) => {
                if response.status_code() == 200 || response.status_code() == 202 || response.status_code() == 204 {
                    if debug {
                        Log::webhook_sender(format!(
                            "Successfully invoked Lambda {}, status: {}",
                            function_name, response.status_code()
                        ));
                    }
                    return Ok(());
                } else {
                    last_error = Some(format!("Lambda error: status {}", response.status_code()));
                    if debug {
                        Log::webhook_sender(format!(
                            "Failed to invoke Lambda {}, status: {}",
                            function_name, response.status_code()
                        ));
                    }
                }
            },
            Err(e) => {
                last_error = Some(format!("Lambda invocation error: {}", e));
                if debug {
                    Log::webhook_sender(format!(
                        "Failed to invoke Lambda {}, error: {}",
                        function_name, e
                    ));
                }
            }
        }

        // Increment attempts and retry after delay
        attempts += 1;
        if attempts < max_retries {
            sleep(Duration::from_millis(retry_delay_ms)).await;
        }
    }

    // Return error after all retries failed
    if let Some(error) = last_error {
        Log::error(format!("Lambda webhook to {} failed after {} attempts: {}", function_name, max_retries, error));
    }

    Ok(()) // We don't fail the overall process for webhook errors
}