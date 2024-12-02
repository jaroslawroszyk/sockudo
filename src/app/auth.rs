use super::manager::AppManager;
use crate::app::config::AppConfig;
use crate::connection::state::SocketId;
use crate::error::Error;
use crate::token::{secure_compare, Token};
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
        socket_id: SocketId,
        app_key: &str,
        user_data: String,
        auth: String,
    ) -> Result<bool, Error> {
        let app = self.app_manager.get_app(app_key).ok_or(Error::InvalidKey)?;
        let is_valid =
            self.sign_in_token_is_valid(socket_id.0, user_data, auth.to_string(), app.clone());
        Ok(is_valid)
    }

    pub fn sign_in_token_is_valid(
        &self,
        socket_id: String,
        user_data: String,
        expected_signature: String,
        app_config: AppConfig,
    ) -> bool {
        let signature = self.sing_in_token_for_user_data(socket_id, user_data, app_config);
        secure_compare(&signature, &expected_signature)
    }

    pub fn sing_in_token_for_user_data(
        &self,
        socket_id: String,
        user_data: String,
        app_config: AppConfig,
    ) -> String {
        let decoded_string = format!("{}::user::{}", socket_id, user_data);
        let signature = Token::new(app_config.key, app_config.secret);
        signature.sign(decoded_string)
    }
}
