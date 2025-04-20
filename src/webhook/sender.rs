use hmac::KeyInit;
use crate::app::config::App;
use crate::channel::ChannelType;
use crate::error::Result;
use crate::log::Log;
use crate::token::Token;
use crate::webhook::queue::QueueManager;
use crate::webhook::types::{ClientEventData, JobData, JobPayload, WebhookEvent, AppWebhook};
use hmac::{Hmac, Mac};
use reqwest::Client as HttpClient;
use reqwest::header::{HeaderMap, HeaderValue};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio::time::sleep;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct WebhookSenderConfig {
    pub process_id: String,
    pub debug: bool,
    pub batching_enabled: bool,
    pub batching_duration: u64,  // milliseconds
    pub can_process_queues: bool,
}

impl Default for WebhookSenderConfig {
    fn default() -> Self {
        Self {
            process_id: uuid::Uuid::new_v4().to_string(),
            debug: false,
            batching_enabled: true,
            batching_duration: 50, // 50ms default
            can_process_queues: true,
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

    /// Configuration for the webhook sender
    config: WebhookSenderConfig,
}

impl WebhookSender {
    pub async fn new(queue_manager: Arc<QueueManager>, config: WebhookSenderConfig) -> Self {
        let webhook_sender = Self {
            queue_manager,
            batch: Arc::new(Mutex::new(HashMap::new())),
            batch_leaders: Arc::new(Mutex::new(HashMap::new())),
            http_client: HttpClient::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
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
        self.setup_queue_processor(WebhookEvent::ClientEvent.to_queue_name()).await;
        self.setup_queue_processor(WebhookEvent::ChannelOccupied.to_queue_name()).await;
        self.setup_queue_processor(WebhookEvent::ChannelVacated.to_queue_name()).await;
        self.setup_queue_processor(WebhookEvent::MemberAdded.to_queue_name()).await;
        self.setup_queue_processor(WebhookEvent::MemberRemoved.to_queue_name()).await;
        self.setup_queue_processor(WebhookEvent::CacheMiss.to_queue_name()).await;
    }

    /// Setup a queue processor for a specific webhook type
    async fn setup_queue_processor(&self, queue_name: &str) {
        let client = self.http_client.clone();
        let debug = self.config.debug;
        let process_id = self.config.process_id.clone();

        let processor = Box::new(move |job_data: JobData| -> Result<()> {
            let app_id = job_data.app_id.clone();
            let app_key = job_data.app_key.clone();
            let payload = job_data.payload.clone();
            let original_signature = job_data.original_signature.clone();

            // TODO: Get app config using app_manager
            // For now, we'll assume the signature is valid

            if debug {
                Log::webhook_sender_title("🚀 Processing webhook from queue.");
                Log::webhook_sender_title(format!("Processing webhook for app {}", app_id));
            }

            // In a real implementation, you would filter events based on webhook settings
            // For now, we'll just mock sending the webhook

            // Create signature for verification
            let signature = create_webhook_hmac(&serde_json::to_string(&payload).unwrap(), "app_secret");
            if signature != original_signature {
                // Signature mismatch, potential tampering
                if debug {
                    Log::error("Webhook signature mismatch - possible tampering detected");
                }
                return Ok(());
            }

            // Mock sending to webhook URL
            if debug {
                Log::webhook_sender_title("✅ Webhook processed (mock)");
                Log::webhook_sender(format!("Would send webhook to URL with payload: {:?}", payload));
            }

            Ok(())
        });

        // Register the processor with the queue
        self.queue_manager.process_queue(queue_name, processor).await.ok();

        Log::info(format!("Initialized webhook queue processor for {}", queue_name));
    }

    /// Send a client event webhook
    pub async fn send_client_event(&self, app: &App, channel: &str, event: &str, data: serde_json::Value, socket_id: Option<&str>, user_id: Option<&str>) -> Result<()> {
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

        self.send(app, event_data, WebhookEvent::ClientEvent.to_queue_name()).await
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

        self.send(app, event_data, WebhookEvent::ChannelOccupied.to_queue_name()).await
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

        self.send(app, event_data, WebhookEvent::ChannelVacated.to_queue_name()).await
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

        self.send(app, event_data, WebhookEvent::MemberAdded.to_queue_name()).await
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

        self.send(app, event_data, WebhookEvent::MemberRemoved.to_queue_name()).await
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

        self.send(app, event_data, WebhookEvent::CacheMiss.to_queue_name()).await
    }

    /// Send webhook with appropriate batching strategy
    async fn send(&self, app: &App, data: ClientEventData, queue_name: &str) -> Result<()> {
        if self.config.batching_enabled {
            self.send_webhook_by_batching(app, data, queue_name).await
        } else {
            self.send_webhook(app, vec![data], queue_name).await
        }
    }

    /// Send a batch of webhook events without batching
    async fn send_webhook(&self, app: &App, events: Vec<ClientEventData>, queue_name: &str) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        // Get current timestamp in milliseconds
        let time_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_millis() as u64;

        let payload = JobPayload {
            time_ms,
            events,
        };

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
        self.queue_manager.add_to_queue(queue_name, job_data).await?;

        if self.config.debug {
            Log::webhook_sender_title(format!("Added webhook event to {} queue", queue_name));
        }

        Ok(())
    }

    /// Send a webhook with batching support
    async fn send_webhook_by_batching(&self, app: &App, data: ClientEventData, queue_name: &str) -> Result<()> {
        // Add event to batch
        {
            let mut batches = self.batch.lock().await;
            let batch = batches.entry(queue_name.to_string())
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

                    let payload = JobPayload {
                        time_ms,
                        events,
                    };

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
                    let _ = queue_manager.add_to_queue(&queue_name, job_data.clone()).await;

                    if debug {
                        Log::webhook_sender_title(format!("Batched {} webhook events sent to {} queue",
                                                        job_data.payload.events.len(), queue_name));
                    }
                }
            });
        }

        Ok(())
    }
}

/// Create HMAC-SHA256 signature for webhook payload
pub fn create_webhook_hmac(data: &str, secret: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC can take key of any size");

    mac.update(data.as_bytes());

    let result = mac.finalize();
    let code_bytes = result.into_bytes();

    hex::encode(code_bytes)
}