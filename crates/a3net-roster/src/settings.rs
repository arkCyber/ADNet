//! Per-user friend-request accept-mode settings.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FriendRequestMode {
    AutoAccept,
    RequireConfirmation,
}

impl FriendRequestMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FriendRequestMode::AutoAccept => "auto_accept",
            FriendRequestMode::RequireConfirmation => "require_confirmation",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "auto_accept" => FriendRequestMode::AutoAccept,
            "require_confirmation" => FriendRequestMode::RequireConfirmation,
            _ => return None,
        })
    }
}

/// Per-user friend-request policy. Persisted in the `friend_request_settings`
/// collection of the original `ContactDirectorySnapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendRequestSetting {
    pub user_id: String,
    pub mode: String, // see [`FriendRequestMode::as_str`]
    /// Unix seconds.
    pub updated_at: u64,
}

impl FriendRequestSetting {
    pub fn new(user_id: impl Into<String>, mode: FriendRequestMode) -> Self {
        Self {
            user_id: user_id.into(),
            mode: mode.as_str().to_string(),
            updated_at: 0,
        }
    }

    pub fn parsed_mode(&self) -> Option<FriendRequestMode> {
        FriendRequestMode::from_str(&self.mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_round_trip() {
        assert_eq!(
            FriendRequestMode::from_str(FriendRequestMode::AutoAccept.as_str()),
            Some(FriendRequestMode::AutoAccept)
        );
        assert_eq!(
            FriendRequestMode::from_str(FriendRequestMode::RequireConfirmation.as_str()),
            Some(FriendRequestMode::RequireConfirmation)
        );
        assert_eq!(FriendRequestMode::from_str("nope"), None);
    }
}