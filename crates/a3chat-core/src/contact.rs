//! Contact list, friend requests, blocklist.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::A3chatError;
use crate::id::UserId;
use crate::validation::{validate_name, validate_url};

/// A friend entry. Mirrors `a3net-roster::Contact` (Human variant) but
/// is owned by a3chat so we can extend without touching the lower
/// crate. The two are kept in sync at the boundary in
/// `a3chat-app::ContactService`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Contact {
    pub user_id: UserId,
    pub display_name: String,
    pub avatar_url: Option<String>,
    /// Public note attached to this contact by the local user.
    pub note: String,
    /// True if starred / pinned to the top of the contact list.
    pub is_favorite: bool,
    /// True if currently blocked (no incoming or outgoing messages).
    pub is_blocked: bool,
    /// RFC3339 of when the friendship was established.
    pub added_at: DateTime<Utc>,
    /// RFC3339 of the most recent interaction (message / call / …).
    pub last_interaction_at: Option<DateTime<Utc>>,
    /// Public key bytes (Ed25519 / X25519) for E2E handshake bootstrap.
    /// Hex-encoded. Optional — clients without key material just can't
    /// establish E2E sessions with this contact.
    pub public_key: Option<String>,
}

impl Contact {
    pub fn validate(&self) -> Result<(), A3chatError> {
        if self.display_name.is_empty() {
            return Err(A3chatError::InvalidInput("display_name: empty".into()));
        }
        validate_name("display_name", &self.display_name)?;
        if let Some(url) = &self.avatar_url {
            validate_url("avatar_url", url)?;
        }
        if let Some(key) = &self.public_key {
            // Ed25519 public key is 32 bytes → 64 hex chars.
            if key.len() != 64 {
                return Err(A3chatError::InvalidInput(format!(
                    "public_key: expected 64 hex chars, got {}",
                    key.len()
                )));
            }
            if !key.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(A3chatError::InvalidInput(
                    "public_key: non-hex characters".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Lifecycle states of an outbound or inbound friend request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContactRequestStatus {
    Pending,
    Accepted,
    Rejected,
    /// Auto-expired after [`crate::contact::REQUEST_TTL_SECS`].
    Expired,
    /// Cancelled by the sender before the recipient responded.
    Cancelled,
}

impl ContactRequestStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, ContactRequestStatus::Pending)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ContactRequestStatus::Pending => "pending",
            ContactRequestStatus::Accepted => "accepted",
            ContactRequestStatus::Rejected => "rejected",
            ContactRequestStatus::Expired => "expired",
            ContactRequestStatus::Cancelled => "cancelled",
        }
    }
}

/// Default TTL for an unanswered friend request.
pub const REQUEST_TTL_SECS: i64 = 7 * 24 * 60 * 60; // 7 days

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ContactRequest {
    pub request_id: String,
    pub from_user_id: UserId,
    pub from_display_name: String,
    pub to_user_id: UserId,
    /// Short greeting from the sender (max 256 chars).
    pub message: String,
    pub status: ContactRequestStatus,
    pub created_at: DateTime<Utc>,
    pub responded_at: Option<DateTime<Utc>>,
}

impl ContactRequest {
    pub fn validate(&self) -> Result<(), A3chatError> {
        if self.request_id.is_empty() {
            return Err(A3chatError::InvalidInput("request_id: empty".into()));
        }
        validate_name("from_display_name", &self.from_display_name)?;
        if self.message.len() > 256 {
            return Err(A3chatError::InvalidInput(format!(
                "message: length {} > 256",
                self.message.len()
            )));
        }
        if self.status.is_terminal() && self.responded_at.is_none() {
            return Err(A3chatError::InvalidInput(format!(
                "responded_at: required for terminal status {:?}",
                self.status
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BlocklistEntry {
    pub user_id: UserId,
    pub display_name: String,
    pub blocked_at: DateTime<Utc>,
    pub reason: Option<String>,
}

impl BlocklistEntry {
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_name("display_name", &self.display_name)?;
        if let Some(r) = &self.reason
            && r.len() > 256
        {
            return Err(A3chatError::InvalidInput(format!(
                "reason: length {} > 256",
                r.len()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact() -> Contact {
        Contact {
            user_id: UserId::from("alice-node"),
            display_name: "Alice".into(),
            avatar_url: Some("https://example.com/a.png".into()),
            note: "Met at conference".into(),
            is_favorite: true,
            is_blocked: false,
            added_at: chrono::Utc::now(),
            last_interaction_at: None,
            public_key: Some("0".repeat(64)),
        }
    }

    #[test]
    fn contact_validates_clean() {
        assert!(contact().validate().is_ok());
    }

    #[test]
    fn contact_rejects_bad_public_key() {
        let mut c = contact();
        c.public_key = Some("short".into());
        assert!(c.validate().is_err());
    }

    #[test]
    fn contact_rejects_bad_avatar_url() {
        let mut c = contact();
        c.avatar_url = Some("ftp://x".into());
        assert!(c.validate().is_err());
    }

    #[test]
    fn request_terminal_predicates() {
        assert!(!ContactRequestStatus::Pending.is_terminal());
        assert!(ContactRequestStatus::Accepted.is_terminal());
        assert!(ContactRequestStatus::Rejected.is_terminal());
        assert!(ContactRequestStatus::Expired.is_terminal());
        assert!(ContactRequestStatus::Cancelled.is_terminal());
    }

    #[test]
    fn request_requires_responded_at_for_terminal() {
        let req = ContactRequest {
            request_id: "r1".into(),
            from_user_id: UserId::from("alice"),
            from_display_name: "Alice".into(),
            to_user_id: UserId::from("bob"),
            message: "hi".into(),
            status: ContactRequestStatus::Accepted,
            created_at: chrono::Utc::now(),
            responded_at: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn request_validates_with_responded_at() {
        let req = ContactRequest {
            request_id: "r1".into(),
            from_user_id: UserId::from("alice"),
            from_display_name: "Alice".into(),
            to_user_id: UserId::from("bob"),
            message: "hi".into(),
            status: ContactRequestStatus::Accepted,
            created_at: chrono::Utc::now(),
            responded_at: Some(chrono::Utc::now()),
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn blocklist_entry_validates() {
        let e = BlocklistEntry {
            user_id: UserId::from("spam"),
            display_name: "Spammer".into(),
            blocked_at: chrono::Utc::now(),
            reason: None,
        };
        assert!(e.validate().is_ok());
    }
}
