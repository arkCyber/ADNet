//! Request / response types for [`GroupService`].
//!
//! These types bridge between the RPC wire format and the internal
//! service methods.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use a3chat_core::error::A3chatError;
use a3chat_core::group::{Group, GroupMember};
use a3chat_core::id::UserId;
use a3net_types::invariants::validate_url;

// ── Create ────────────────────────────────────────────────────────────────────

/// Request for [`super::GroupService::create`](super::GroupService::create).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGroupRequest {
    pub name: String,
    pub description: String,
    pub avatar_url: Option<String>,
    /// True for invite-only groups.
    pub is_private: bool,
}

impl CreateGroupRequest {
    /// Validates name length (1–256), description length (≤ 1024), and avatar_url.
    pub fn validate(&self) -> Result<(), A3chatError> {
        if self.name.is_empty() {
            return Err(A3chatError::InvalidInput("group name is empty".into()));
        }
        if self.name.len() > 256 {
            return Err(A3chatError::InvalidInput(format!(
                "group name length {} exceeds 256",
                self.name.len()
            )));
        }
        if self.description.len() > 1024 {
            return Err(A3chatError::InvalidInput(format!(
                "description length {} exceeds 1024",
                self.description.len()
            )));
        }
        if let Some(ref url) = self.avatar_url {
            validate_url("avatar_url", url).map_err(|e| A3chatError::InvalidInput(e.to_string()))?;
        }
        Ok(())
    }
}

/// Response returned after a successful group creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGroupResponse {
    /// The created group record.
    pub group: Group,
    /// The owner member record (already persisted in hub).
    pub owner_member: GroupMember,
}

// ── Metadata update ────────────────────────────────────────────────────────────

/// Request for updating group name and/or description.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateGroupMetadataRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
}

impl UpdateGroupMetadataRequest {
    /// Validates optional name (if provided), description (if provided), and avatar_url (if provided).
    pub fn validate(&self) -> Result<(), A3chatError> {
        if let Some(ref name) = self.name {
            if name.is_empty() {
                return Err(A3chatError::InvalidInput("group name is empty".into()));
            }
            if name.len() > 256 {
                return Err(A3chatError::InvalidInput(format!(
                    "group name length {} exceeds 256",
                    name.len()
                )));
            }
        }
        if let Some(ref desc) = self.description {
            if desc.len() > 1024 {
                return Err(A3chatError::InvalidInput(format!(
                    "description length {} exceeds 1024",
                    desc.len()
                )));
            }
        }
        if let Some(ref url) = self.avatar_url {
            validate_url("avatar_url", url).map_err(|e| A3chatError::InvalidInput(e.to_string()))?;
        }
        Ok(())
    }
}

// ── Hub ↔ Core conversion ─────────────────────────────────────────────────────

/// Convert a hub-side `GroupMember` (role as `String`) into the
/// core domain `GroupMember` (role as `MemberRole`).
///
/// Lives here because orphan rules prevent placing this in
/// `a3chat-core` (can't impl for `a3net_chatstore::GroupMember`) or
/// `a3net-chatstore` (can't impl for `a3chat_core::GroupMember`).
pub fn hub_member_to_core(hub: a3net_chatstore::GroupMember) -> GroupMember {
    use a3chat_core::group::MemberRole;
    use a3chat_core::id::UserId;
    let user_id_str = hub.user_id.clone();
    let role = MemberRole::parse(&hub.role).unwrap_or(MemberRole::Member);
    GroupMember {
        user_id: UserId::from(user_id_str.clone()),
        display_name: user_id_str,
        role,
        joined_at: hub.joined_at,
        // Preserve presence and temp-admin data from hub
        last_seen: hub.last_seen,
        is_online: hub.is_online,
        nickname: None,
        temp_admin_until: hub.temp_admin_until,
    }
}

/// Information about a member with temporary admin privileges.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempAdminInfo {
    pub user_id: UserId,
    pub display_name: String,
    pub expires_at: DateTime<Utc>,
}

/// Response for temp admin status query.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempAdminStatusResponse {
    pub user_id: UserId,
    pub has_temp_admin: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Response for listing temp admins.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempAdminListResponse {
    pub temp_admins: Vec<TempAdminInfo>,
}
