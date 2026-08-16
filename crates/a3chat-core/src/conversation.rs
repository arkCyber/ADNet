//! Conversation metadata — both DM (1:1) and Group.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::A3chatError;
use crate::id::{ConversationId, UserId, validate_id};
use crate::validation::{validate_name, validate_ordered};

/// Discriminator: direct message or group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    Dm,
    Group,
}

impl ConversationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ConversationKind::Dm => "dm",
            ConversationKind::Group => "group",
        }
    }
}

/// Lightweight per-conversation metadata — what the chat-list UI
/// renders without opening the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConversationMeta {
    pub conversation_id: ConversationId,
    pub kind: ConversationKind,
    /// Display title — peer display_name for DMs, group name for groups.
    pub title: String,
    /// Hex `NodeId` of the *other* participant for DMs; empty for groups.
    pub peer_user_id: Option<UserId>,
    /// Last message preview (truncated to ~64 chars by the sender).
    pub last_message_preview: String,
    /// Unix timestamp of the most recent message.
    pub last_activity: i64,
    /// Total number of messages stored for this user in this conversation.
    pub message_count: u32,
    /// Number of messages with `read_at == None` (unread badge).
    pub unread_count: u32,
    /// For DMs: whether the other side is currently online.
    pub peer_online: bool,
    /// `true` if this conversation has been muted (no OS notifications).
    pub muted: bool,
    /// `true` if this conversation has been pinned to the top.
    pub pinned: bool,
}

impl ConversationMeta {
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_id("conversation_id", self.conversation_id.as_str())?;
        validate_name("title", &self.title)?;
        if self.unread_count > self.message_count {
            return Err(A3chatError::InvalidInput(format!(
                "unread_count {} > message_count {}",
                self.unread_count, self.message_count
            )));
        }
        if self.last_activity < 0 {
            return Err(A3chatError::InvalidInput(format!(
                "negative last_activity {}",
                self.last_activity
            )));
        }
        Ok(())
    }
}

/// Full conversation record — what `chat.conversation.open` returns.
/// Includes membership / peer info needed for the chat panel UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConversationRecord {
    pub meta: ConversationMeta,
    /// For groups: the member list. Empty for DMs.
    pub members: Vec<crate::group::GroupMember>,
    /// RFC3339 — when this conversation was first created on this node.
    pub created_at: DateTime<Utc>,
    /// RFC3339 — last time any field on this record changed locally.
    pub updated_at: DateTime<Utc>,
}

impl ConversationRecord {
    pub fn validate(&self) -> Result<(), A3chatError> {
        self.meta.validate()?;
        for (i, m) in self.members.iter().enumerate() {
            m.validate().map_err(|e| match e {
                A3chatError::InvalidInput(msg) => {
                    A3chatError::InvalidInput(format!("members[{i}]: {msg}"))
                }
                other => other,
            })?;
        }
        validate_ordered("updated_at vs created_at", self.created_at, self.updated_at)
            .map_err(A3chatError::InvalidInput)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str) -> ConversationMeta {
        ConversationMeta {
            conversation_id: ConversationId::from(id),
            kind: ConversationKind::Dm,
            title: "Alice".into(),
            peer_user_id: Some(UserId::from("alice-node-id")),
            last_message_preview: "hi".into(),
            last_activity: 100,
            message_count: 1,
            unread_count: 1,
            peer_online: true,
            muted: false,
            pinned: false,
        }
    }

    #[test]
    fn meta_validates_clean() {
        assert!(meta("dm:abc").validate().is_ok());
    }

    #[test]
    fn meta_rejects_unread_overflow() {
        let mut m = meta("dm:abc");
        m.unread_count = 5;
        m.message_count = 2;
        assert!(m.validate().is_err());
    }

    #[test]
    fn meta_rejects_negative_last_activity() {
        let mut m = meta("dm:abc");
        m.last_activity = -1;
        assert!(m.validate().is_err());
    }

    #[test]
    fn kind_serializes_snake_case() {
        let v = serde_json::to_value(ConversationKind::Dm).unwrap();
        assert_eq!(v, serde_json::json!("dm"));
        let v = serde_json::to_value(ConversationKind::Group).unwrap();
        assert_eq!(v, serde_json::json!("group"));
    }
}
