//! Group metadata, membership, invitations.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::A3chatError;
use crate::id::{ConversationId, UserId, validate_id};
use crate::validation::{MAX_MEMBERS, validate_name, validate_ordered};

/// Role inside a group conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    Owner,
    Admin,
    Member,
}

impl MemberRole {
    pub fn can_administer(self) -> bool {
        matches!(self, MemberRole::Owner | MemberRole::Admin)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            MemberRole::Owner => "owner",
            MemberRole::Admin => "admin",
            MemberRole::Member => "member",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "owner" => MemberRole::Owner,
            "admin" => MemberRole::Admin,
            "member" => MemberRole::Member,
            _ => return None,
        })
    }
}

/// Full group record — what `chat.conversation.open` returns for a
/// group conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Group {
    pub conversation_id: ConversationId,
    pub name: String,
    pub description: String,
    pub avatar_url: Option<String>,
    pub owner_id: UserId,
    /// Total member count (denormalized for cheap badge rendering).
    pub member_count: u32,
    /// RFC3339 — group creation time.
    pub created_at: DateTime<Utc>,
    /// RFC3339 — most recent message / member change.
    pub last_activity: DateTime<Utc>,
    /// Per-group monotonic sequence. The local sender's sequence for
    /// group messages is offset by this; useful for gap detection
    /// inside a single group.
    pub last_sequence: u32,
    /// True if the group is invite-only (cannot be joined without
    /// an accepted invitation).
    pub is_private: bool,
    /// True if the group has been dissolved. Dissolved groups are
    /// read-only and never accept new messages.
    pub is_dissolved: bool,
}

impl Group {
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_id("conversation_id", self.conversation_id.as_str())?;
        validate_name("name", &self.name)?;
        if self.description.len() > 1024 {
            return Err(A3chatError::InvalidInput(format!(
                "description: length {} > 1024",
                self.description.len()
            )));
        }
        if let Some(url) = &self.avatar_url {
            crate::validation::validate_url("avatar_url", url)?;
        }
        validate_id("owner_id", self.owner_id.as_str())?;
        if (self.member_count as usize) > MAX_MEMBERS {
            return Err(A3chatError::InvalidInput(format!(
                "member_count {} > {MAX_MEMBERS}",
                self.member_count
            )));
        }
        validate_ordered(
            "last_activity vs created_at",
            self.created_at,
            self.last_activity,
        )
        .map_err(A3chatError::InvalidInput)?;
        if self.last_sequence >= crate::message::MAX_SEQUENCE {
            return Err(A3chatError::InvalidInput(format!(
                "last_sequence {} >= {}",
                self.last_sequence,
                crate::message::MAX_SEQUENCE
            )));
        }
        Ok(())
    }
}

/// One row in the group roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GroupMember {
    pub user_id: UserId,
    pub display_name: String,
    pub role: MemberRole,
    pub joined_at: DateTime<Utc>,
    /// RFC3339 of the most recent message / presence seen from this
    /// member. `None` if never seen.
    pub last_seen: Option<DateTime<Utc>>,
    /// `true` if the member is currently online (presence cached).
    pub is_online: bool,
    /// Per-group nickname override (the "群昵称").
    pub nickname: Option<String>,
}

impl GroupMember {
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_id("user_id", self.user_id.as_str())?;
        validate_name("display_name", &self.display_name)?;
        if let Some(n) = &self.nickname {
            validate_name("nickname", n)?;
        }
        if let Some(seen) = self.last_seen
            && seen < self.joined_at
        {
            return Err(A3chatError::InvalidInput(format!(
                "last_seen {seen} < joined_at {}",
                self.joined_at
            )));
        }
        Ok(())
    }
}

/// Lifecycle states of a group invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvitationStatus {
    Pending,
    Accepted,
    Rejected,
    Expired,
    Cancelled,
}

impl InvitationStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, InvitationStatus::Pending)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            InvitationStatus::Pending => "pending",
            InvitationStatus::Accepted => "accepted",
            InvitationStatus::Rejected => "rejected",
            InvitationStatus::Expired => "expired",
            InvitationStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GroupInvitation {
    pub invitation_id: String,
    pub conversation_id: ConversationId,
    pub group_name: String,
    pub inviter_id: UserId,
    pub inviter_name: String,
    pub invitee_id: UserId,
    pub status: InvitationStatus,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl GroupInvitation {
    pub fn validate(&self) -> Result<(), A3chatError> {
        if self.invitation_id.is_empty() {
            return Err(A3chatError::InvalidInput("invitation_id: empty".into()));
        }
        validate_id("conversation_id", self.conversation_id.as_str())?;
        validate_name("group_name", &self.group_name)?;
        validate_id("inviter_id", self.inviter_id.as_str())?;
        validate_name("inviter_name", &self.inviter_name)?;
        validate_id("invitee_id", self.invitee_id.as_str())?;
        if self.expires_at < self.created_at {
            return Err(A3chatError::InvalidInput(format!(
                "expires_at {} < created_at {}",
                self.expires_at, self.created_at
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group() -> Group {
        Group {
            conversation_id: ConversationId::from("grp:abc"),
            name: "team".into(),
            description: "Engineering chat".into(),
            avatar_url: None,
            owner_id: UserId::from("alice"),
            member_count: 3,
            created_at: chrono::Utc::now(),
            last_activity: chrono::Utc::now(),
            last_sequence: 1,
            is_private: true,
            is_dissolved: false,
        }
    }

    #[test]
    fn group_validates_clean() {
        assert!(group().validate().is_ok());
    }

    #[test]
    fn group_rejects_oversize_member_count() {
        let mut g = group();
        g.member_count = (MAX_MEMBERS as u32) + 1;
        assert!(g.validate().is_err());
    }

    #[test]
    fn group_rejects_inverted_timestamps() {
        let mut g = group();
        g.last_activity = g.created_at - chrono::Duration::seconds(1);
        assert!(g.validate().is_err());
    }

    #[test]
    fn member_role_predicates() {
        assert!(MemberRole::Owner.can_administer());
        assert!(MemberRole::Admin.can_administer());
        assert!(!MemberRole::Member.can_administer());
    }

    #[test]
    fn member_role_round_trip() {
        for r in [MemberRole::Owner, MemberRole::Admin, MemberRole::Member] {
            assert_eq!(MemberRole::parse(r.as_str()), Some(r));
        }
        assert_eq!(MemberRole::parse("nope"), None);
    }

    #[test]
    fn member_rejects_inverted_seen() {
        let m = GroupMember {
            user_id: UserId::from("alice"),
            display_name: "Alice".into(),
            role: MemberRole::Member,
            joined_at: chrono::Utc::now(),
            last_seen: Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
            is_online: false,
            nickname: None,
        };
        assert!(m.validate().is_err());
    }

    #[test]
    fn invitation_validates() {
        let now = chrono::Utc::now();
        let inv = GroupInvitation {
            invitation_id: "i1".into(),
            conversation_id: ConversationId::from("grp:abc"),
            group_name: "G".into(),
            inviter_id: UserId::from("a"),
            inviter_name: "A".into(),
            invitee_id: UserId::from("b"),
            status: InvitationStatus::Pending,
            created_at: now,
            expires_at: now + chrono::Duration::seconds(60),
        };
        assert!(inv.validate().is_ok());
    }

    #[test]
    fn invitation_rejects_inverted_expiry() {
        let now = chrono::Utc::now();
        let inv = GroupInvitation {
            invitation_id: "i1".into(),
            conversation_id: ConversationId::from("grp:abc"),
            group_name: "G".into(),
            inviter_id: UserId::from("a"),
            inviter_name: "A".into(),
            invitee_id: UserId::from("b"),
            status: InvitationStatus::Pending,
            created_at: now,
            expires_at: now - chrono::Duration::seconds(1),
        };
        assert!(inv.validate().is_err());
    }
}
