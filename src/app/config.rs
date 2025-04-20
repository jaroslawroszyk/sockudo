use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::webhook::types::Webhook;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct App {
    pub id: String,
    pub key: String,
    pub secret: String,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub max_connections: u32,
    pub enable_client_messages: bool,
    pub enabled: bool,
    #[serde(default, deserialize_with = "deserialize_optional_number_from_string")]
    pub max_backend_events_per_second: Option<u32>,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub max_client_events_per_second: u32,
    #[serde(default, deserialize_with = "deserialize_optional_number_from_string")]
    pub max_read_requests_per_second: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_optional_number_from_string")]
    pub max_presence_members_per_channel: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_optional_number_from_string")]
    pub max_presence_member_size_in_kb: Option<u32>,
    #[serde(default)]
    pub max_channel_name_length: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_optional_number_from_string")]
    pub max_event_channels_at_once: Option<u32>,
    #[serde(default)]
    pub max_event_name_length: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_optional_number_from_string")]
    pub max_event_payload_in_kb: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_optional_number_from_string")]
    pub max_event_batch_size: Option<u32>,
    #[serde(default)]
    pub enable_user_authentication: Option<bool>,
    #[serde(default)]
    pub webhooks: Option<Vec<Webhook>>,
}

// Helper functions to deserialize numbers from strings
fn deserialize_number_from_string<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = String::deserialize(deserializer)?;
    value.parse::<u32>().map_err(D::Error::custom)
}

fn deserialize_optional_number_from_string<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = Option::<String>::deserialize(deserializer)?;
    match value {
        Some(s) => Ok(Some(s.parse::<u32>().map_err(D::Error::custom)?)),
        None => Ok(None),
    }
}

// Implementation with default values
impl Default for App {
    fn default() -> Self {
        Self {
            id: String::new(),
            key: String::new(),
            secret: String::new(),
            max_connections: 0,
            enable_client_messages: false,
            enabled: false,
            max_backend_events_per_second: None,
            max_client_events_per_second: 0,
            max_read_requests_per_second: None,
            max_presence_members_per_channel: None,
            max_presence_member_size_in_kb: None,
            max_channel_name_length: None,
            max_event_channels_at_once: None,
            max_event_name_length: None,
            max_event_payload_in_kb: None,
            max_event_batch_size: None,
            enable_user_authentication: None,
            webhooks: None,
        }
    }
}
