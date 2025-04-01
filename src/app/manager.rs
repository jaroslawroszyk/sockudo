use super::config::App;
use crate::error::Error;
use crate::websocket::SocketId;
use dashmap::DashMap;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub struct AppManager {
    apps: DashMap<String, App>,
    // channel_managers: DashMap<String, Arc<RwLock<ChannelManager>>>,
}

impl Default for AppManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AppManager {
    pub fn new() -> Self {
        Self {
            apps: DashMap::new(),
        }
    }

    pub fn register_app(&self, config: App) {
        self.apps.insert(config.id.clone(), config.clone());
    }
    pub fn validate_key(&self, app_id: &str) -> bool {
        self.apps.contains_key(app_id)
    }

    pub fn get_app_by_key(&self, key: &str) -> Option<App> {
        self.apps
            .iter()
            .find(|app| app.key == key)
            .map(|app| app.clone())
    }

    pub fn get_app(&self, app_id: &str) -> Option<App> {
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

        if channel.len() > app.max_channel_name_length.unwrap() as usize {
            return Err(Error::ChannelError(format!(
                "Channel name too long. Max length is {}",
                app.max_channel_name_length.unwrap()
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
        self.get_app_by_key(app_id)
            .map(|app| app.enable_client_messages)
            .unwrap_or(false)
    }

    pub async fn validate_user_auth(
        &self,
        socket_id: &SocketId,
        auth: &str,
    ) -> Result<bool, Error> {
        // Split auth string into key and signature (format: "app_key:signature")
        let (app_key, signature) = auth
            .split_once(':')
            .ok_or_else(|| Error::AuthError("Invalid auth format".into()))?;

        // Get app config
        let app = self.get_app(app_key).ok_or_else(|| Error::InvalidAppKey)?;

        // Create string to sign: socket_id
        let string_to_sign = socket_id.to_string();

        // Generate expected signature
        let expected = self.sign_payload(&app.secret, &string_to_sign)?;

        // Compare signatures
        Ok(signature == expected)
    }
}
