use super::types::ChannelType;
use super::PresenceMemberInfo;
use crate::app::config::AppConfig;
use crate::connection::state::SocketId;
use crate::connection::ConnectionManager;
use crate::error::Error;
use crate::log::Log;
use crate::protocol::messages::{MessageData, PusherMessage};
use crate::token::{secure_compare, Token};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaveResponse {
    pub(crate) left: bool,
    remaining_connections: Option<usize>,
    member: Option<PresenceMember>,
}

pub struct ChannelManager {
    connection_manager: Arc<Mutex<Box<dyn ConnectionManager + Send + Sync>>>,
}

impl ChannelManager {
    pub fn new(connection_manager: Arc<Mutex<Box<dyn ConnectionManager + Send + Sync>>>) -> Self {
        Self { connection_manager }
    }

    pub async fn subscribe(
        &self,
        socket_id: &str,
        data: &PusherMessage,
        channel_name: &str,
        is_authenticated: bool,
        app_id: &str,
    ) -> Result<JoinResponse, Error> {
        let channel_type = ChannelType::from_name(channel_name);

        if channel_type.requires_authentication() && !is_authenticated {
            return Err(Error::AuthError("Channel requires authentication".into()));
        }

        let socket_id = SocketId(socket_id.to_string());

        // Scope the connection_manager lock
        let mut connection_manager = self.connection_manager.lock().await;

        Log::info(format!(
            "Starting subscribe for socket: {} to channel: {}, app_id: {}",
            socket_id.0, channel_name, app_id
        ));

        // Check for existing subscription
        let is_in_channel = connection_manager.is_in_channel(app_id, channel_name, &socket_id).await;
        if is_in_channel {
            Log::warning(format!(
                "Socket {} already subscribed to channel {}",
                socket_id.0, channel_name
            ));
            return Ok(JoinResponse {
                success: true,
                channel_connections: Some(
                    connection_manager
                        .get_channel(app_id, channel_name)
                        .await
                        .unwrap()
                        .len() as i32,
                ),
                member: None,
                auth_error: None,
                error_message: None,
                error_code: None,
                _type: None,
            });
        }

        // Handle presence channel subscription
        let member = if channel_type == ChannelType::Presence {
            let member = self.parse_presence_data(&data.data)?;
            let presence_info = PresenceMemberInfo {
                user_id: member.user_id.clone(),
                user_info: Some(member.user_info.clone()),
            };
            Some(member)
        } else {
            None
        };

        // Atomic operation to add socket
        connection_manager.add_socket_to_channel(app_id, channel_name, &socket_id.clone()).await;
        let total_connections = connection_manager
            .get_channel(app_id, channel_name)
            .await
            .unwrap()
            .len();

        Log::info(format!(
            "After subscribe - Channel: {}, Total sockets: {}, Socket: {}",
            channel_name, total_connections, socket_id
        ));
        let channel = connection_manager
            .get_channel(app_id, channel_name)
            .await
            .unwrap();
        Log::warning(format!(
            "After subscribe Channel: {}, Sockets: {:?}",
            channel_name, channel
        ));

        Ok(JoinResponse {
            success: true,
            channel_connections: Some(total_connections as i32),
            member,
            auth_error: None,
            error_message: None,
            error_code: None,
            _type: None,
        })
    }

    pub async fn unsubscribe(
        &self,
        socket_id: &str,
        channel_name: &str,
        app_id: &str,
        user_id: Option<&str>,
    ) -> Result<LeaveResponse, Error> {
        let socket_id_to_remove = SocketId(socket_id.to_string());
        Log::info(format!(
            "Starting unsubscribe for socket: {} from channel: {}",
            socket_id_to_remove, channel_name
        ));

        let mut connection_manager = self.connection_manager.lock().await;
        Log::info("Members on line 155");
        let members = connection_manager
            .get_channel_members(app_id, channel_name)
            .await?;
        Log::info(format!("Members: {:?}", members));

        // Handle presence channel logic first
        let member = if ChannelType::from_name(channel_name) == ChannelType::Presence {
            if let Some(user_id) = user_id {
                let member = match members.get(user_id) {
                    Some(member) => {
                        let member = PresenceMember {
                            user_id: member.user_id.clone(),
                            user_info: member.user_info.clone().unwrap_or_default(),
                            socket_id: None,
                        };
                        member
                    }
                    None => {
                        Log::warning(format!(
                            "No user_id provided for presence channel unsubscribe 1: {}",
                            channel_name
                        ));
                        return Ok(LeaveResponse {
                            left: false,
                            remaining_connections: Some(
                                connection_manager
                                    .get_channel(app_id, channel_name)
                                    .await
                                    .unwrap()
                                    .len(),
                            ),
                            member: None,
                        });
                    }
                };
                Some(member)
            } else {
                Log::warning(format!(
                    "No user_id provided for presence channel unsubscribe: {}",
                    channel_name
                ));
                None
            }
        } else {
            None
        };
        Log::success(format!("Member: {:?}", member));
        // Atomic remove operation
        // list sockets before and after to check if socket was removed
        let socket_removed = connection_manager.remove_socket_from_channel(
            app_id,
            channel_name,
            &socket_id_to_remove,
        ).await;
        Log::success(format!("Socket removed: {:?}", socket_removed));
        let remaining_connections = connection_manager
            .get_channel(app_id, channel_name)
            .await
            .unwrap()
            .len();
        Log::info(format!(
            "After unsubscribe - Channel: {}, Sockets: {:?}",
            channel_name,
            connection_manager
                .get_channel(app_id, channel_name)
                .await
                .unwrap()
        ));

        // Check if channel should be removed
        if remaining_connections == 0 {
            Log::info("Removing empty channel");
            connection_manager.remove_channel(app_id, channel_name).await;
            Log::info(format!("Removed empty channel: {}", channel_name));
        }

        Ok(LeaveResponse {
            left: socket_removed,
            remaining_connections: Some(remaining_connections),
            member,
        })
    }

    // Helper method to parse presence data
    fn parse_presence_data(&self, data: &Option<MessageData>) -> Result<PresenceMember, Error> {
        let channel_data = data
            .as_ref()
            .ok_or_else(|| Error::ChannelError("Missing presence data".into()))?;

        match channel_data {
            MessageData::Structured {
                channel_data,
                extra,
                ..
            } => {
                let data: Value = serde_json::from_str(
                    channel_data
                        .as_ref()
                        .ok_or_else(|| Error::ChannelError("Missing channel_data".into()))?,
                )?;

                self.extract_presence_member(&data, extra)
            }
            MessageData::Json(data) => self.extract_presence_member(data, &Default::default()),
            _ => Err(Error::ChannelError("Invalid presence data format".into())),
        }
    }

    // Helper to extract presence member info
    fn extract_presence_member(
        &self,
        data: &Value,
        extra: &HashMap<String, Value>,
    ) -> Result<PresenceMember, Error> {
        let user_id = data
            .get("user_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::ChannelError("Invalid user_id".into()))?
            .to_string();

        let user_info = data.get("user_info").cloned().unwrap_or_default();

        let socket_id = extra
            .get("socket_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        Ok(PresenceMember {
            user_id,
            user_info,
            socket_id,
        })
    }

    pub fn signature_is_valid(
        &self,
        app_config: AppConfig,
        socket_id: &SocketId,
        signature: &str,
        message: PusherMessage,
    ) -> bool {
        let expected = Self::get_expected_signature(app_config, socket_id, message);
        secure_compare(signature, &expected)
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
}
