use super::types::{Channel, ChannelType, PresenceMemberInfo, SubscriptionInfo};
use crate::app::config::AppConfig;
use crate::connection::state::SocketId;
use crate::connection::ConnectionManager;
use crate::error::Error;
use crate::log::Log;
use crate::protocol::messages::{ApiMessageData, MessageData, PusherApiMessage, PusherMessage};
use crate::token;
use crate::token::Token;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing_subscriber::fmt::format;

#[derive(Debug)]
struct PresenceData {
    ids: Vec<String>,
    hash: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceMember {
    pub(crate) user_id: String,
    pub(crate) user_info: Value,
    pub(crate) socket_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResponse {
    pub(crate) success: bool,
    pub channel_connections: Option<i32>,
    pub auth_error: Option<String>,
    pub member: Option<PresenceMember>,
    pub error_message: Option<String>,
    pub error_code: Option<i32>,
    pub _type: Option<String>,
}

pub struct ChannelManager {
    channels: DashMap<String, Channel>,
}

impl Default for ChannelManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelManager {
    pub fn new() -> Self {
        Self {
            channels: DashMap::new(),
        }
    }

    pub async fn subscribe(
        &self,
        socket_id: &str,
        data: &PusherMessage,
        is_authenticated: bool,
    ) -> Result<JoinResponse, Error> {
        let channel_name = match data.clone().data.unwrap() {
            MessageData::String(data) => data,
            MessageData::Structured { channel, .. } => channel.unwrap(),
            MessageData::Json(data) => {
                // get channel from data
                data.get("channel").unwrap().as_str().unwrap().to_string()
            }
        };

        let channel_type = ChannelType::from_name(&channel_name);

        // Check if channel requires authentication
        if channel_type.requires_authentication() && !is_authenticated {
            return Err(Error::AuthError(
                "Channel requires authentication".to_string(),
            ));
        }

        // get curren tsokcet base don socket_id

        // Get or create channel
        let subscription = SubscriptionInfo {
            socket_id: socket_id.to_string(),
            presence_info: None,
        };

        let mut channel = self
            .channels
            .entry(channel_name.clone())
            .or_insert_with(|| Channel {
                name: channel_name.clone(),
                channel_type: channel_type.clone(),
                subscribers: HashMap::new(),
            });

        if channel_type == ChannelType::Presence {
            let member: PresenceMember = self.parse_presence_data(&data.clone().data)?;
            return Ok(JoinResponse {
                success: true,
                channel_connections: Some(channel.subscribers.len() as i32),
                auth_error: None,
                member: Option::from(member),
                error_message: None,
                error_code: None,
                _type: None,
            });
        }

        // Add subscriber to channel
        channel
            .subscribers
            .insert(socket_id.to_string(), subscription);

        // Prepare subscription success response
        Ok(JoinResponse {
            success: true,
            channel_connections: Some(channel.subscribers.len() as i32),
            auth_error: None,
            member: None,
            error_message: None,
            error_code: None,
            _type: None,
        })
    }

    pub fn unsubscribe(&self, socket_id: &str, channel_name: &str) {
        Log::info(format!("Unsubscribing {} from {}", socket_id, channel_name));

        // First check if we need to remove the channel
        let should_remove = {
            if let Some(mut channel) = self.channels.get_mut(channel_name) {
                Log::info(format!("Channel before: {:?}", channel));
                channel.subscribers.remove(socket_id);
                channel.subscribers.is_empty()
            } else {
                false
            }
        }; // The mut reference is dropped here

        if should_remove {
            Log::info(format!("Removing empty channel: {}", channel_name));
            match self.channels.remove(channel_name) {
                Some(_) => Log::info(format!("Channel {} removed", channel_name)),
                None => Log::info(format!("Channel {} not found", channel_name)),
            }
        }

        // Log final state
        if let Some(channel) = self.channels.get(channel_name) {
            Log::info(format!("Channel after: {:?}", channel));
        }
    }

    pub fn remove_connection(&self, socket_id: &str) {
        // Remove connection from all channels
        let channels: Vec<String> = self
            .channels
            .iter()
            .map(|entry| entry.key().clone())
            .collect();

        for channel_name in channels {
            self.unsubscribe(socket_id, &channel_name);
        }
    }

    pub fn get_channel_subscribers(&self, channel_name: &str) -> Option<Vec<String>> {
        self.channels
            .get(channel_name)
            .map(|channel| channel.subscribers.keys().cloned().collect())
    }

    pub fn is_client_subscribed(&self, socket_id: &str, channel_name: &str) -> bool {
        self.channels
            .get(channel_name)
            .map(|channel| channel.subscribers.contains_key(socket_id))
            .unwrap_or(false)
    }

    fn parse_presence_data(&self, data: &Option<MessageData>) -> Result<PresenceMember, Error> {
        let channel_data = data
            .as_ref()
            .ok_or_else(|| Error::ChannelError("Missing presence channel data".to_string()))?;
        match channel_data {
            MessageData::Structured {
                user_data,
                channel_data,
                channel,
                extra,
            } => {
                Log::info(format!("channel_data: {:?}", channel_data.clone()));
                let channel_data: Value =
                    serde_json::from_str(channel_data.clone().unwrap().as_str()).unwrap();
                let user_id = channel_data
                    .get("user_id")
                    .ok_or_else(|| {
                        Error::ChannelError("Missing user_id in presence data".to_string())
                    })?
                    .as_str()
                    .ok_or_else(|| {
                        Error::ChannelError("Invalid user_id in presence data".to_string())
                    })?
                    .to_string();
                let user_info = channel_data.get("user_info").cloned().unwrap_or_default();
                let socket_id = extra
                    .get("socket_id")
                    .map(|s| s.as_str().unwrap().to_string());
                Ok(PresenceMember {
                    user_id,
                    user_info,
                    socket_id,
                })
            }
            MessageData::Json(data) => {
                let user_id = data
                    .get("user_id")
                    .ok_or_else(|| {
                        Error::ChannelError("Missing user_id in presence data".to_string())
                    })?
                    .as_str()
                    .ok_or_else(|| {
                        Error::ChannelError("Invalid user_id in presence data".to_string())
                    })?
                    .to_string();
                let user_info = data.get("user_info").cloned().unwrap_or_default();
                let socket_id = data
                    .get("socket_id")
                    .map(|s| s.as_str().unwrap().to_string());
                Ok(PresenceMember {
                    user_id,
                    user_info,
                    socket_id,
                })
            }
            _ => Err(Error::ChannelError(
                "Invalid presence channel data".to_string(),
            )),
        }
    }

    pub(crate) fn get_presence_data(&self, channel: &Channel) -> PresenceData {
        let mut ids = Vec::new();
        let mut hash = HashMap::new();

        for sub in channel.subscribers.values() {
            if let Some(presence_info) = &sub.presence_info {
                ids.push(presence_info.user_id.clone());
                if let Some(user_info) = &presence_info.user_info {
                    hash.insert(presence_info.user_id.clone(), user_info.clone());
                }
            }
        }

        PresenceData { ids, hash }
    }

    // Broadcasting methods
    pub async fn broadcast_to_channel(
        &self,
        app_id: &str,
        connection_manager: &Arc<Mutex<ConnectionManager>>,
        channel_name: &str,
        event: String,
        data: Value,
        except_socket_id: Option<&str>,
    ) -> Result<(), Error> {
        let message = PusherMessage {
            channel: Option::from(channel_name.to_string()),
            data: Option::from(MessageData::Json(data)),
            name: Option::from(event.clone()),
            event: Option::from(event),
        };

        if let Some(channel) = self.channels.get(channel_name) {
            //log channel subscribers
            Log::info(format!(
                "Channel subscribers: {:?}",
                channel.subscribers.clone()
            ));
            for (socket_id, _) in channel.subscribers.iter() {
                if Some(socket_id.as_str()) != except_socket_id {
                    if let Err(e) = connection_manager
                        .lock()
                        .await
                        .send_message(app_id, &SocketId(socket_id.to_string()), message.clone())
                        .await
                    {
                        tracing::error!("Failed to send message to {}: {}", socket_id, e);
                        // Continue sending to other subscribers even if one fails
                        continue;
                    }
                }
            }
        }
        Log::info("Finished");
        Ok(())
    }

    pub fn get_channel_type(&self, channel_name: &str) -> Option<ChannelType> {
        self.channels
            .get(channel_name)
            .map(|c| c.channel_type.clone())
    }

    pub fn signature_is_valid(
        &self,
        app_config: AppConfig,
        socket_id: &SocketId,
        signature: &String,
        message: PusherMessage,
    ) -> bool {
        let expected = Self::get_expected_signature(app_config, socket_id, message);
        *signature == expected
    }
    pub fn get_expected_signature(
        app_config: AppConfig,
        socket_id: &SocketId,
        message: PusherMessage,
    ) -> String {
        let (key, secret) = (app_config.key, app_config.secret);
        let token = Token::new(key.clone(), secret);
        let string_to_sign = format!(
            "{}:{}",
            key,
            token.sign(Self::get_data_to_sign_for_signature(socket_id, message))
        );
        string_to_sign
    }

    fn get_data_to_sign_for_signature(socket_id: &SocketId, message: PusherMessage) -> String {
        let message_data: MessageData = message.data.unwrap();

        match message_data {
            MessageData::Structured {
                channel_data,
                channel,
                user_data: _,
                ..
            } => {
                let channel_type = ChannelType::from_name(&channel.clone().unwrap());
                if channel_type == ChannelType::Presence {
                    format!(
                        "{}:{}:{}",
                        socket_id,
                        channel.unwrap(),
                        channel_data.unwrap()
                    )
                } else {
                    format!("{}:{}", socket_id, channel.unwrap())
                }
            }
            _ => panic!("Invalid message data"),
        }
    }
    pub async fn get_channel_sockets(&self, channel: &str) -> Vec<SocketId> {
        let mut sockets = Vec::new();
        if let Some(channel) = self.channels.get(channel) {
            for (socket_id, _) in channel.subscribers.iter() {
                sockets.push(SocketId(socket_id.clone()));
            }
        }
        sockets
    }
}
