// src/app/memory_manager.rs
use super::config::App;
use crate::app::manager::AppManager;
use crate::error::{Error, Result};
use crate::token::{secure_compare, Token};
use crate::websocket::SocketId;
use async_trait::async_trait;
use dashmap::DashMap;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

struct CacheConfig {
    enabled: bool,
    ttl: usize,
}

pub struct MemoryAppManager {
    apps: DashMap<String, App>,
    cache: CacheConfig,
}

impl Default for MemoryAppManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryAppManager {
    pub fn new() -> Self {
        Self {
            apps: DashMap::new(),
            cache: CacheConfig {
                enabled: true,
                ttl: 1000,
            },
        }
    }
}

#[async_trait]
impl AppManager for MemoryAppManager {
    async fn init(&self) -> Result<()> {
        // In-memory implementation doesn't need initialization
        Ok(())
    }

    async fn register_app(&self, config: App) -> Result<()> {
        self.apps.insert(config.id.clone(), config);
        Ok(())
    }

    async fn update_app(&self, config: App) -> Result<()> {
        self.apps.insert(config.id.clone(), config);
        Ok(())
    }

    async fn remove_app(&self, app_id: &str) -> Result<()> {
        self.apps.remove(app_id);
        Ok(())
    }

    async fn get_apps(&self) -> Result<Vec<App>> {
        let apps = self
            .apps
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        Ok(apps)
    }

    async fn validate_key(&self, app_id: &str) -> Result<bool> {
        Ok(self.apps.contains_key(app_id))
    }

    async fn get_app_by_key(&self, key: &str) -> Result<Option<App>> {
        let app = self
            .apps
            .iter()
            .find(|app| app.key == key)
            .map(|app| app.clone());
        Ok(app)
    }

    async fn get_app(&self, app_id: &str) -> Result<Option<App>> {
        Ok(self.apps.get(app_id).map(|app| app.clone()))
    }

    async fn validate_signature(&self, app_id: &str, signature: &str, body: &str) -> Result<bool> {
        let app = self
            .get_app(app_id)
            .await?
            .ok_or_else(|| Error::InvalidAppKey)?;

        let expected = self.sign_payload(&app.secret, body).await?;
        Ok(signature == expected)
    }

    async fn sign_payload(&self, secret: &str, payload: &str) -> Result<String> {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .map_err(|e| Error::AuthError(e.to_string()))?;

        mac.update(payload.as_bytes());
        let result = mac.finalize();
        let signature = hex::encode(result.into_bytes());

        Ok(signature)
    }

    async fn validate_channel_name(&self, app_id: &str, channel: &str) -> Result<()> {
        let app = self
            .get_app(app_id)
            .await?
            .ok_or_else(|| Error::InvalidAppKey)?;

        if channel.len() > app.max_channel_name_length.unwrap_or(200) as usize {
            return Err(Error::ChannelError(format!(
                "Channel name too long. Max length is {}",
                app.max_channel_name_length.unwrap_or(200)
            )));
        }

        // Validate channel name format
        if !channel.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '=' || c == '@' || c == '.'
        }) {
            return Err(Error::ChannelError(
                "Channel name contains invalid characters".to_string(),
            ));
        }

        Ok(())
    }

    async fn can_handle_client_events(&self, app_id: &str) -> Result<bool> {
        Ok(self
            .get_app_by_key(app_id)
            .await?
            .map(|app| app.enable_client_messages)
            .unwrap_or(false))
    }

    async fn validate_user_auth(&self, socket_id: &SocketId, auth: &str) -> Result<bool> {
        // Split auth string into key and signature (format: "app_key:signature")
        let (app_key, signature) = auth
            .split_once(':')
            .ok_or_else(|| Error::AuthError("Invalid auth format".into()))?;

        // Get app config
        let app = self
            .get_app(app_key)
            .await?
            .ok_or_else(|| Error::InvalidAppKey)?;

        // Create string to sign: socket_id
        let string_to_sign = socket_id.to_string();

        // Generate expected signature
        let expected = self.sign_payload(&app.secret, &string_to_sign).await?;

        // Compare signatures
        Ok(signature == expected)
    }
}
