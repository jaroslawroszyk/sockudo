use std::sync::Arc;
use super::config::AppConfig;
use crate::error::Error;
use dashmap::DashMap;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use tokio::sync::RwLock;
use crate::channel::ChannelManager;
use crate::connection::state::SocketId;
use crate::protocol::messages::PusherMessage;
use crate::token::Token;

type HmacSha256 = Hmac<Sha256>;

pub struct AppManager {
    apps: DashMap<String, AppConfig>,
    channel_managers: DashMap<String, Arc<RwLock<ChannelManager>>>,
}

impl AppManager {
    pub fn new() -> Self {
        let map = DashMap::new();
        map.insert(
            "demo-app".to_string(),
            AppConfig {
                app_id: "demo-app".to_string(),
                key: "demo-key".to_string(),
                secret: "demo-secret".to_string(),
                max_connections: 0,
                enable_client_events: true,
                encrypted: false,
                webhook_urls: vec![],
                max_channel_name_length: 200,
                max_event_channels: 0,
                presence_max_member_size: 0,
                webhooks_enabled: false,
            },
        );
        Self {
            apps: {
                map
            },
            channel_managers: DashMap::new(),
        }
    }

    pub fn register_app(&self, config: AppConfig) {
        self.apps.insert(config.app_id.clone(), config.clone());
        self.channel_managers.insert(
            config.clone().app_id,
            Arc::new(RwLock::new(ChannelManager::new()))
        );
    }

    pub fn get_channel_manager(&self, app_id: &str) -> Option<Arc<RwLock<ChannelManager>>> {
        self.channel_managers.get(app_id).map(|cm| cm.clone())
    }

    pub fn validate_key(&self, app_id: &str) -> bool {
        self.apps.contains_key(app_id)
    }
    
    pub fn get_app_by_key(&self, key: &str) -> Option<AppConfig> {
        self.apps.iter().find(|app| app.key == key).map(|app| app.clone())
    }

    pub fn get_app(&self, app_id: &str) -> Option<AppConfig> {
        self.apps.get(app_id).map(|app| app.clone())
    }

    pub fn validate_signature(
        &self,
        app_id: &str,
        signature: &str,
        body: &str,
    ) -> Result<bool, Error> {
        let app = self.get_app(app_id).ok_or_else(|| Error::InvalidAppKey)?;

        let expected = self.sign_payload(&app.secret, body)?;
        Ok(signature == expected)
    }

    pub fn sign_payload(&self, secret: &str, payload: &str) -> Result<String, Error> {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .map_err(|e| Error::AuthError(e.to_string()))?;

        mac.update(payload.as_bytes());
        let result = mac.finalize();
        let signature = hex::encode(result.into_bytes());

        Ok(signature)
    }

    pub fn validate_channel_name(&self, app_id: &str, channel: &str) -> Result<(), Error> {
        let app = self.get_app(app_id).ok_or_else(|| Error::InvalidAppKey)?;

        if channel.len() > app.max_channel_name_length {
            return Err(Error::ChannelError(format!(
                "Channel name too long. Max length is {}",
                app.max_channel_name_length
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

    pub fn can_handle_client_events(&self, app_id: &str) -> bool {
        self.get_app(app_id)
            .map(|app| app.enable_client_events)
            .unwrap_or(false)
    }

    pub async fn validate_user_auth(&self, socket_id: &SocketId, auth: &str) -> Result<bool, Error> {
        // Split auth string into key and signature (format: "app_key:signature")
        let (app_key, signature) = auth.split_once(':')
            .ok_or_else(|| Error::AuthError("Invalid auth format".into()))?;

        // Get app config
        let app = self.get_app(app_key)
            .ok_or_else(|| Error::InvalidAppKey)?;

        // Create string to sign: socket_id
        let string_to_sign = socket_id.to_string();

        // Generate expected signature
        let expected = self.sign_payload(&app.secret, &string_to_sign)?;

        // Compare signatures
        Ok(signature == expected)
    }
}
