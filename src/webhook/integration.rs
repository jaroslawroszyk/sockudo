use crate::app::config::App;
use crate::error::Result;
use crate::log::Log;
use crate::webhook::sender::{WebhookSender, WebhookSenderConfig};
use std::sync::Arc;
use crate::queue::manager::{QueueManager, QueueManagerFactory};
use crate::webhook::types::AppWebhook;

/// Webhook server configuration
#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub enabled: bool,
    pub batching: BatchingConfig,
    pub queue_driver: String,
    pub redis_url: Option<String>,
    pub redis_prefix: Option<String>,
    pub redis_concurrency: Option<usize>,
    pub process_id: String,
    pub debug: bool,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            batching: BatchingConfig {
                enabled: false,
                duration: 10,
            },
            queue_driver: "redis".to_string(),
            redis_url: Some("redis://localhost:6379".to_string()),
            redis_prefix: Some("sockudo".to_string()),
            redis_concurrency: Some(5),
            process_id: uuid::Uuid::new_v4().to_string(),
            debug: false,
        }
    }
}

/// Webhook batching configuration
#[derive(Debug, Clone)]
pub struct BatchingConfig {
    pub enabled: bool,
    pub duration: u64, // in milliseconds
}

impl Default for BatchingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            duration: 50, // 50ms default batch duration
        }
    }
}

/// Server webhook handling integration
pub struct WebhookIntegration {
    sender: Option<Arc<WebhookSender>>,
    config: WebhookConfig,
}

impl WebhookIntegration {
    /// Create a new webhook integration
    pub async fn new(config: WebhookConfig) -> Result<Self> {
        Log::info("====== WEBHOOK INTEGRATION SETUP ======");
        Log::info(format!("Webhook enabled: {}", config.enabled));
        Log::info(format!("Batching enabled: {}", config.batching.enabled));
        Log::info(format!("Queue driver: {}", config.queue_driver));
        Log::info(format!("Redis URL: {:?}", config.redis_url));
        if !config.enabled {
            Log::info("Webhooks are disabled");
            return Ok(Self {
                sender: None,
                config,
            });
        }

        // Create queue manager based on configuration
        let queue_manager = QueueManagerFactory::create(
            &config.queue_driver,
            config.redis_url.as_deref(),
            config.redis_prefix.as_deref(),
            config.redis_concurrency,
        )
        .await?;

        // Create webhook sender
        let sender_config = WebhookSenderConfig {
            process_id: config.process_id.clone(),
            debug: config.debug,
            batching_enabled: config.batching.enabled,
            batching_duration: config.batching.duration,
            can_process_queues: true,
            http_timeout: 10,
            max_retries: 10,
            retry_delay: 10,
        };

        let queue_manager = Arc::new(QueueManager::new(queue_manager));
        let sender = WebhookSender::new(queue_manager, sender_config).await;

        Log::info("Webhook integration initialized");

        Ok(Self {
            sender: Some(Arc::new(sender)),
            config,
        })
    }

    /// Check if webhooks are enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enabled && self.sender.is_some()
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
        if let Some(sender) = &self.sender {
            sender
                .send_client_event(app, channel, event, data, socket_id, user_id)
                .await
        } else {
            Ok(())
        }
    }

    /// Send a channel occupied webhook
    pub async fn send_channel_occupied(&self, app: &App, channel: &str) -> Result<()> {
        if let Some(sender) = &self.sender {
            sender.send_channel_occupied(app, channel).await
        } else {
            Ok(())
        }
    }

    /// Send a channel vacated webhook
    pub async fn send_channel_vacated(&self, app: &App, channel: &str) -> Result<()> {
        if let Some(sender) = &self.sender {
            sender.send_channel_vacated(app, channel).await
        } else {
            Ok(())
        }
    }

    /// Send a member added webhook
    pub async fn send_member_added(&self, app: &App, channel: &str, user_id: &str) -> Result<()> {
        Log::info("====== MEMBER ADDED WEBHOOK ======");
        Log::info(format!("App ID: {}", app.id));
        Log::info(format!("Channel: {}", channel));
        Log::info(format!("User ID: {}", user_id));

        // Check if the app has webhook config
        Log::info(format!("App has webhooks: {}", app.webhooks.is_some()));

        // Check if the app has member_added webhooks specifically
        if !app.has_member_added_webhooks() {
            Log::info("App doesn't have member_added webhooks configured");
            return Ok(());
        }

        Log::info("App has member_added webhooks, sending event");

        // Rest of the existing code...
        if let Some(sender) = &self.sender {
            sender.send_member_added(app, channel, user_id).await
        } else {
            Log::info("Webhook sender is not initialized");
            Ok(())
        }
    }

    /// Send a member removed webhook
    pub async fn send_member_removed(&self, app: &App, channel: &str, user_id: &str) -> Result<()> {
        if let Some(sender) = &self.sender {
            sender.send_member_removed(app, channel, user_id).await
        } else {
            Ok(())
        }
    }

    /// Send a cache miss webhook
    pub async fn send_cache_missed(&self, app: &App, channel: &str) -> Result<()> {
        if let Some(sender) = &self.sender {
            sender.send_cache_missed(app, channel).await
        } else {
            Ok(())
        }
    }
}
