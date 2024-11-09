use super::manager::AppManager;
use crate::error::Error;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct ChannelAuth {
    pub channel_name: String,
    pub socket_id: String,
    #[serde(default)]
    pub user_data: Option<String>,
}

pub struct AuthValidator {
    app_manager: Arc<AppManager>,
}

#[derive(Debug)]
pub struct AuthValidationResult {
    pub is_valid: bool,
    pub signature: String,
}

impl AuthValidator {
    pub fn new(app_manager: Arc<AppManager>) -> Self {
        Self { app_manager }
    }

    pub async fn validate_channel_auth(
        &self,
        app_key: &str,
        auth: &str,
        channel_auth: &ChannelAuth,
    ) -> Result<AuthValidationResult, Error> {
        let app = self.app_manager
            .get_app(app_key)
            .ok_or_else(|| Error::InvalidAppKey)?;

        // Create authentication string
        let auth_string = match &channel_auth.user_data {
            Some(user_data) => {
                format!("{}:{}:{}", channel_auth.socket_id, channel_auth.channel_name, user_data)
            }
            None => {
                format!("{}:{}", channel_auth.socket_id, channel_auth.channel_name)
            }
        };

        // Generate signature
        let signature = self.app_manager.sign_payload(&app.secret, &auth_string)?;
        let expected_auth = format!("{}:{}", app_key, signature);

        Ok(AuthValidationResult {
            is_valid: auth == expected_auth,
            signature: expected_auth,
        })
    }
}
