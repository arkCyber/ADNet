//! Presence — online / away / offline / invisible.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::A3chatError;
use crate::id::UserId;

/// Per-user presence state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceStatus {
    Online,
    Away,
    Offline,
    Invisible,
}

impl PresenceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PresenceStatus::Online => "online",
            PresenceStatus::Away => "away",
            PresenceStatus::Offline => "offline",
            PresenceStatus::Invisible => "invisible",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "online" => PresenceStatus::Online,
            "away" => PresenceStatus::Away,
            "offline" => PresenceStatus::Offline,
            "invisible" => PresenceStatus::Invisible,
            _ => return None,
        })
    }
    /// `true` if the user is logically "available" — visible to
    /// contacts regardless of how they presented themselves.
    pub fn is_visible(self) -> bool {
        matches!(self, PresenceStatus::Online | PresenceStatus::Away)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Presence {
    pub user_id: UserId,
    pub status: PresenceStatus,
    /// Optional human-readable status message (e.g. "In a meeting").
    pub status_message: Option<String>,
    pub last_changed: DateTime<Utc>,
}

fn validate_status_message(field: &str, msg: Option<&String>) -> Result<(), A3chatError> {
    if let Some(m) = msg
        && m.len() > 256
    {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: length {} > 256",
            m.len()
        )));
    }
    Ok(())
}

impl Presence {
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_status_message("status_message", self.status_message.as_ref())
    }
}

/// Notification payload — what the daemon pushes on `/rpc/stream`
/// when a contact's presence changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PresenceEvent {
    pub user_id: UserId,
    pub status: PresenceStatus,
    pub status_message: Option<String>,
    pub timestamp: DateTime<Utc>,
}

impl PresenceEvent {
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_status_message("status_message", self.status_message.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trip() {
        for s in [
            PresenceStatus::Online,
            PresenceStatus::Away,
            PresenceStatus::Offline,
            PresenceStatus::Invisible,
        ] {
            assert_eq!(PresenceStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(PresenceStatus::parse("nope"), None);
    }

    #[test]
    fn visible_predicate() {
        assert!(PresenceStatus::Online.is_visible());
        assert!(PresenceStatus::Away.is_visible());
        assert!(!PresenceStatus::Offline.is_visible());
        assert!(!PresenceStatus::Invisible.is_visible());
    }

    #[test]
    fn presence_event_validates_clean() {
        let p = PresenceEvent {
            user_id: UserId::from("alice"),
            status: PresenceStatus::Online,
            status_message: None,
            timestamp: chrono::Utc::now(),
        };
        assert!(p.validate().is_ok());
    }

    #[test]
    fn presence_event_rejects_oversize_status_message() {
        let p = PresenceEvent {
            user_id: UserId::from("alice"),
            status: PresenceStatus::Online,
            status_message: Some("x".repeat(257)),
            timestamp: chrono::Utc::now(),
        };
        assert!(p.validate().is_err());
    }
}
