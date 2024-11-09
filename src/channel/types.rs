use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::log::Log;

#[derive(Debug, Clone, PartialEq)]
pub enum ChannelType {
    Public,
    Private,
    Presence,
    PrivateEncrypted,
}

impl ChannelType {
    pub fn from_name(channel_name: &String) -> Self {
        Log::info(format!("From name: {:?}", channel_name));
        match channel_name.split_once('-') {
            Some(("private", "encrypted")) => Self::PrivateEncrypted,
            Some(("private", _)) => Self::Private,
            Some(("presence", _)) => Self::Presence,
            _ => Self::Public,
        }
    }

    pub fn requires_authentication(&self) -> bool {
        matches!(
            self,
            ChannelType::Private | ChannelType::Presence | ChannelType::PrivateEncrypted
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceMemberInfo {
    pub user_id: String,
    pub user_info: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub name: String,
    pub channel_type: ChannelType,
    pub subscribers: HashMap<String, SubscriptionInfo>,
}

#[derive(Debug, Clone)]
pub struct SubscriptionInfo {
    pub socket_id: String,
    pub presence_info: Option<PresenceMemberInfo>,
}