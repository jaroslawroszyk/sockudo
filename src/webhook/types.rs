use crate::app::config::App;
use once_cell::sync::Lazy; // <--- IMPORT Lazy
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// --- WebhookEvent enum remains the same ---
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum WebhookEvent {
    #[serde(rename = "client_event")]
    ClientEvent,
    #[serde(rename = "channel_occupied")]
    ChannelOccupied,
    #[serde(rename = "channel_vacated")]
    ChannelVacated,
    #[serde(rename = "member_added")]
    MemberAdded,
    #[serde(rename = "member_removed")]
    MemberRemoved,
    #[serde(rename = "cache_miss")]
    CacheMiss,
}

impl WebhookEvent {
    pub fn as_str(&self) -> &'static str {
        match self {
            WebhookEvent::ClientEvent => "client_event",
            WebhookEvent::ChannelOccupied => "channel_occupied",
            WebhookEvent::ChannelVacated => "channel_vacated",
            WebhookEvent::MemberAdded => "member_added",
            WebhookEvent::MemberRemoved => "member_removed",
            WebhookEvent::CacheMiss => "cache_miss",
        }
    }

    pub fn to_queue_name(&self) -> &'static str {
        match self {
            WebhookEvent::ClientEvent => "client_event_webhooks",
            WebhookEvent::ChannelOccupied => "channel_occupied_webhooks",
            WebhookEvent::ChannelVacated => "channel_vacated_webhooks",
            WebhookEvent::MemberAdded => "member_added_webhooks",
            WebhookEvent::MemberRemoved => "member_removed_webhooks",
            WebhookEvent::CacheMiss => "cache_miss_webhooks",
        }
    }
}

// --- Other structs remain the same ---

/// Client event data structure for webhooks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientEventData {
    pub name: String,
    pub channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
}

/// Job data structure for queue processing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobData {
    pub app_key: String,
    pub app_id: String,
    pub payload: JobPayload,
    pub original_signature: String,
}

/// Payload structure for webhook jobs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobPayload {
    pub time_ms: u64,
    pub events: Vec<ClientEventData>,
}

/// Filter for webhook event targeting
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_name_starts_with: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_name_ends_with: Option<String>,
}

/// Lambda function configuration for webhook delivery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LambdaConfig {
    pub region: Option<String>,
    pub async_execution: Option<bool>,
    pub client_options: Option<HashMap<String, String>>,
}

/// Webhook configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Webhook {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lambda_function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lambda: Option<LambdaConfig>,
    pub event_types: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<WebhookFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
}

/// Define a static empty Vec<Webhook> using once_cell
static EMPTY_WEBHOOKS: Lazy<Vec<Webhook>> = Lazy::new(Vec::new);

/// Extension trait for Apps with webhook capabilities
pub trait AppWebhook {
    fn has_client_event_webhooks(&self) -> bool;
    fn has_channel_occupied_webhooks(&self) -> bool;
    fn has_channel_vacated_webhooks(&self) -> bool;
    fn has_member_added_webhooks(&self) -> bool;
    fn has_member_removed_webhooks(&self) -> bool;
    fn has_cache_missed_webhooks(&self) -> bool;
    fn get_webhooks(&self) -> &Vec<Webhook>;
}

// --- Assume App struct exists in crate::app::config ---
// namespace crate::app::config {
//     pub struct App {
//         pub webhooks: Option<Vec<Webhook>>,
//         // ... other fields
//     }
// }

impl AppWebhook for App {
    fn has_client_event_webhooks(&self) -> bool {
        self.webhooks.as_ref().map_or(false, |webhooks| {
            webhooks.iter().any(|webhook| {
                webhook
                    .event_types
                    .iter()
                    .any(|event_type| event_type == WebhookEvent::ClientEvent.as_str())
            })
        })
    }

    fn has_channel_occupied_webhooks(&self) -> bool {
        self.webhooks.as_ref().map_or(false, |webhooks| {
            webhooks.iter().any(|webhook| {
                webhook
                    .event_types
                    .iter()
                    .any(|event_type| event_type == WebhookEvent::ChannelOccupied.as_str())
            })
        })
    }

    fn has_channel_vacated_webhooks(&self) -> bool {
        self.webhooks.as_ref().map_or(false, |webhooks| {
            webhooks.iter().any(|webhook| {
                webhook
                    .event_types
                    .iter()
                    .any(|event_type| event_type == WebhookEvent::ChannelVacated.as_str())
            })
        })
    }

    fn has_member_added_webhooks(&self) -> bool {
        self.webhooks.as_ref().map_or(false, |webhooks| {
            webhooks.iter().any(|webhook| {
                webhook
                    .event_types
                    .iter()
                    .any(|event_type| event_type == WebhookEvent::MemberAdded.as_str())
            })
        })
    }

    fn has_member_removed_webhooks(&self) -> bool {
        self.webhooks.as_ref().map_or(false, |webhooks| {
            webhooks.iter().any(|webhook| {
                webhook
                    .event_types
                    .iter()
                    .any(|event_type| event_type == WebhookEvent::MemberRemoved.as_str())
            })
        })
    }

    fn has_cache_missed_webhooks(&self) -> bool {
        self.webhooks.as_ref().map_or(false, |webhooks| {
            webhooks.iter().any(|webhook| {
                webhook
                    .event_types
                    .iter()
                    .any(|event_type| event_type == WebhookEvent::CacheMiss.as_str())
            })
        })
    }

    // --- FIX IS HERE ---
    fn get_webhooks(&self) -> &Vec<Webhook> {
        // Return ref to webhooks inside App if Some, otherwise return ref to static empty vec
        self.webhooks.as_ref().unwrap_or(&EMPTY_WEBHOOKS)
    }
    // --- END FIX ---
}

// Add webhook configuration to App struct
// This method is fine as it returns an Option<&Vec<Webhook>>
impl App {
    pub fn webhooks(&self) -> Option<&Vec<Webhook>> {
        self.webhooks.as_ref()
    }
}
