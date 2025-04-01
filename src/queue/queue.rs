use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientEventData {
    pub name: String,
    pub channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<HashMap<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobData {
    pub app_key: String,
    pub app_id: String,
    pub payload: JobPayload,
    pub original_pusher_signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobPayload {
    pub time_ms: i64,
    pub events: Vec<ClientEventData>,
}

impl Default for JobData {
    fn default() -> Self {
        Self {
            app_key: String::new(),
            app_id: String::new(),
            payload: JobPayload {
                time_ms: 0,
                events: Vec::new(),
            },
            original_pusher_signature: String::new(),
        }
    }
}

#[async_trait]
pub trait Queue {
    async fn add_to_queue(&mut self, job_data: JobData);
    async fn process_queue(&mut self, job_data: JobData);
    async fn disconnect(&mut self);
}
