use crate::app::config::App;
use crate::error::Result;
use crate::log::Log;
use crate::webhook::queue::{QueueManager, QueueManagerFactory};
use crate::webhook::sender::{WebhookSender, WebhookSenderConfig};
use std::sync::Arc;

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
            batching: BatchingConfig::default(),
            queue_driver: "memory".to_string(),
            redis_url: None,
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
        if let Some(sender) = &self.sender {
            sender.send_member_added(app, channel, user_id).await
        } else {
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
