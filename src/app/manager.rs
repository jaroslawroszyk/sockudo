use std::sync::Arc;
// src/app/traits.rs
use crate::app::config::App;
use crate::error::{Error, Result};
use crate::websocket::SocketId;
use async_trait::async_trait;

/// Trait defining operations that all AppManager implementations must support
#[async_trait]
pub trait AppManager: Send + Sync + 'static {
    /// Initialize the App Manager
    async fn init(&self) -> Result<()>;

    /// Register a new application
    async fn register_app(&self, config: App) -> Result<()>;

    /// Update an existing application
    async fn update_app(&self, config: App) -> Result<()>;

    /// Remove an application
    async fn remove_app(&self, app_id: &str) -> Result<()>;

    /// Get all registered applications
    async fn get_apps(&self) -> Result<Vec<App>>;

    /// Check if an app ID is valid
    async fn validate_key(&self, app_id: &str) -> Result<bool>;

    /// Get an app by its key
    async fn get_app_by_key(&self, key: &str) -> Result<Option<App>>;

    /// Get an app by its ID
    async fn get_app(&self, app_id: &str) -> Result<Option<App>>;

    /// Validate a signature for an app
    async fn validate_signature(&self, app_id: &str, signature: &str, body: &str) -> Result<bool>;

    /// Sign a payload with an app's secret
    async fn sign_payload(&self, secret: &str, payload: &str) -> Result<String>;

    /// Validate a channel name against an app's restrictions
    async fn validate_channel_name(&self, app_id: &str, channel: &str) -> Result<()>;

    /// Check if an app can handle client events
    async fn can_handle_client_events(&self, app_id: &str) -> Result<bool>;

    /// Validate user authentication
    async fn validate_user_auth(&self, socket_id: &SocketId, auth: &str) -> Result<bool>;
}
