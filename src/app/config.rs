use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub app_id: String,
    pub key: String,
    pub secret: String,
    pub max_connections: usize,
    #[serde(default)]
    pub enable_client_events: bool,
    #[serde(default)]
    pub encrypted: bool,
    #[serde(default)]
    pub webhook_urls: Vec<String>,
    #[serde(default)]
    pub max_channel_name_length: usize,
    #[serde(default)]
    pub max_event_channels: usize,
    #[serde(default = "default_presence_max_member_size")]
    pub presence_max_member_size: usize,
    #[serde(default)]
    pub webhooks_enabled: bool,
}

fn default_presence_max_member_size() -> usize {
    100
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            app_id: String::new(),
            key: String::new(),
            secret: String::new(),
            max_connections: 10000,
            enable_client_events: true,
            encrypted: false,
            webhook_urls: Vec::new(),
            max_channel_name_length: 200,
            max_event_channels: 100,
            presence_max_member_size: 100,
            webhooks_enabled: false,
        }
    }
}
