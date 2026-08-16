//! Group chat & 1-to-1 direct messaging — typed wire records.
//!
//! Ported from
//! `Exodus@src-backup/src-tauri/src/microservice/group_chat_service.rs`.
//! All records are pure serde-friendly types so they can travel over
//! [`a3net-gossip`] topics, [`a3net-ipc`] JSON-RPC, or HTTP webhook
//! without any extra translation layer.
//!
//! # Aerospace-grade invariants (DO-178C)
//!
//! Every record carries an explicit `validate()` constructor that
//! enforces, at the boundary:
//!
//! - identifiers are non-empty ASCII without control characters
//!   ([`invariants::validate_id`]);
//! - content / names are length-bounded ([`invariants::MAX_CONTENT_LEN`],
//!   [`invariants::MAX_NAME_LEN`]);
//! - `MessageType`, `MemberRole`, `InvitationStatus` are real enums, not
//!   unchecked strings;
//! - `sequence` is wrapped in [`invariants::Sequence`], enforcing
//!   `seq < MAX_SEQUENCE`;
//! - `edited_at >= timestamp` whenever `is_edited = true`;
//! - `created_at <= expires_at` for invitations;
//! - `joined_at <= last_seen` for members.
//!
//! Deserialising a record with serde does **not** trigger validation
//! (serde is the wrong layer for invariants because of forward/backward
//! compat). Every IPC entry point and gossip subscriber must call
//! `validate()` before admitting a record into the application state.
//!
//! # Integrity hash
//!
//! The integrity hash is shared with [`crate::integrity`]:
//! - `direct_hash` for 1-to-1 messages;
//! - `group_hash` for group messages.
//!
//! The V2 schema additionally covers `is_edited` + `edited_at`, so an
//! edit that does not re-stamp the hash fails verification.

use serde::{Deserialize, Serialize};

use crate::content::ContentHash;
use crate::error::{AdnetError, Result};
use crate::integrity::{VerifyOutcome, group_hash as compute_group_hash, preflight_group};
use crate::invariants::{
    self, AttachmentKind, InvitationStatus, MAX_ATTACHMENTS, MAX_MEMBERS, MAX_MENTIONS, MemberRole,
    MessageType, Sequence, validate_content, validate_id, validate_name, validate_ordered,
};
use crate::node::NodeId;

/// Record-level validation contract. Every typed record in
/// `a3net-types` implements this so the IPC layer can run a uniform
/// gate at every boundary. The contract is intentionally minimal so
/// the IPC layer can iterate over heterogeneous records in a single
/// `T: Validate` generic.
pub trait Validate {
    fn validate(&self) -> Result<()>;
}

/// Maximum per-sender sequence number before cycling. Matches the
/// Exodus reference implementation.
pub const MAX_SEQUENCE: u32 = 9999;

/// Group metadata — describes a multi-user chat room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GroupChat {
    pub group_id: String,
    pub name: String,
    pub description: String,
    pub avatar_url: Option<String>,
    pub owner_id: String,
    pub member_ids: Vec<String>,
    pub admin_ids: Vec<String>,
    pub is_private: bool,
    pub created_at: u64,
    pub last_activity: u64,
    pub message_count: u32,
    pub public_account_id: Option<String>,
    pub last_sequence: u32,
    pub assistant_id: Option<String>,
}

impl GroupChat {
    /// Validate every field of the group record. Returns `Ok(())` if
    /// the record satisfies the documented invariants, or the first
    /// [`AdnetError::Validation`] failure otherwise.
    pub fn validate(&self) -> Result<()> {
        validate_id("group_id", &self.group_id)?;
        validate_name("name", &self.name)?;
        validate_name("description", &self.description)?;
        if let Some(url) = &self.avatar_url {
            invariants::validate_url("avatar_url", url)?;
        }
        validate_id("owner_id", &self.owner_id)?;
        if self.member_ids.len() > MAX_MEMBERS {
            return Err(AdnetError::Validation(format!(
                "member_ids: {} exceeds {MAX_MEMBERS}",
                self.member_ids.len()
            )));
        }
        for (i, m) in self.member_ids.iter().enumerate() {
            validate_id(&format!("member_ids[{i}]"), m)?;
        }
        for (i, a) in self.admin_ids.iter().enumerate() {
            validate_id(&format!("admin_ids[{i}]"), a)?;
        }
        validate_ordered(
            "last_activity vs created_at",
            self.created_at,
            self.last_activity,
        )?;
        Sequence::new(self.last_sequence, MAX_SEQUENCE)?;
        Ok(())
    }
}

/// A message in a group chat, addressed by `(group_id, message_id)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GroupMessage {
    pub message_id: String,
    pub group_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub content: String,
    pub message_type: MessageType,
    pub attachments: Vec<MessageAttachment>,
    pub reply_to: Option<String>,
    pub mentions: Vec<String>,
    pub timestamp: u64,
    pub is_edited: bool,
    pub edited_at: Option<u64>,
    pub sequence: u32,
    pub integrity_hash: Option<String>,
}

impl GroupMessage {
    /// Validate every field. On success the message is safe to store,
    /// gossip, or sign.
    pub fn validate(&self) -> Result<()> {
        validate_id("message_id", &self.message_id)?;
        validate_id("group_id", &self.group_id)?;
        validate_id("sender_id", &self.sender_id)?;
        validate_name("sender_name", &self.sender_name)?;
        validate_content("content", &self.content)?;
        if self.attachments.len() > MAX_ATTACHMENTS {
            return Err(AdnetError::Validation(format!(
                "attachments: {} exceeds {MAX_ATTACHMENTS}",
                self.attachments.len()
            )));
        }
        for (i, a) in self.attachments.iter().enumerate() {
            a.validate().map_err(|e| match e {
                AdnetError::Validation(msg) => {
                    AdnetError::Validation(format!("attachments[{i}]: {msg}"))
                }
                other => other,
            })?;
        }
        if let Some(r) = &self.reply_to {
            validate_id("reply_to", r)?;
        }
        if self.mentions.len() > MAX_MENTIONS {
            return Err(AdnetError::Validation(format!(
                "mentions: {} exceeds {MAX_MENTIONS}",
                self.mentions.len()
            )));
        }
        for (i, m) in self.mentions.iter().enumerate() {
            validate_id(&format!("mentions[{i}]"), m)?;
        }
        Sequence::new(self.sequence, MAX_SEQUENCE)?;
        if self.is_edited {
            let ea = self.edited_at.ok_or_else(|| {
                AdnetError::Validation("is_edited=true with edited_at=None".into())
            })?;
            validate_ordered("edited_at vs timestamp", self.timestamp, ea)?;
        } else if self.edited_at.is_some() {
            return Err(AdnetError::Validation(
                "edited_at set while is_edited=false".into(),
            ));
        }
        // Run the integrity pre-flight as well so empty / oversize
        // fields cannot reach the hasher even if the caller bypassed
        // validate_content.
        preflight_group(&self.group_id, &self.sender_id, &self.content)?;
        Ok(())
    }

    /// Re-compute and stamp the [`GroupMessage::integrity_hash`] field
    /// for this message in place. The hash covers
    /// `(group_id, sender_id, content, sequence, timestamp,
    /// is_edited, edited_at)` so an edit that does not re-stamp the
    /// hash will fail verification.
    pub fn stamp_integrity_hash(&mut self) {
        self.integrity_hash = Some(self.compute_hash());
    }

    /// Compute the hash without storing it. Useful for cross-checking.
    pub fn compute_hash(&self) -> String {
        let edit_part: &[u8] = if self.is_edited {
            // Mix the edit flag + edited_at into the digest. We use the
            // raw bytes so a missing edited_at (which validate()
            // disallows when is_edited=true) is impossible here in
            // practice.
            match self.edited_at {
                Some(_ea) => b"edited",
                None => b"edited-no-time",
            }
        } else {
            b"original"
        };
        let mut base = compute_group_hash(
            &self.group_id,
            &self.sender_id,
            &self.content,
            self.sequence,
            self.timestamp,
        );
        // Domain-separate the edit metadata using the generic builder
        // so the original v1 hash and the new v2 hash differ.
        let edit_hash = crate::integrity::hash_fields([
            base.as_bytes(),
            edit_part,
            &self.edited_at.unwrap_or(0).to_le_bytes(),
        ]);
        base.clear();
        base.push_str(&edit_hash);
        base
    }

    /// Strict verifier returning a [`VerifyOutcome`]. Distinct from the
    /// legacy [`GroupMessage::verify_integrity`] (bool) so callers can
    /// react to `Missing` vs `Mismatch`.
    pub fn verify_integrity_outcome(&self) -> VerifyOutcome {
        let computed = self.compute_hash();
        match &self.integrity_hash {
            Some(h) if h == &computed => VerifyOutcome::Valid,
            Some(_) => VerifyOutcome::Mismatch,
            None => VerifyOutcome::Missing,
        }
    }

    /// `true` only when the integrity hash is present **and** matches.
    pub fn verify_integrity(&self) -> bool {
        matches!(self.verify_integrity_outcome(), VerifyOutcome::Valid)
    }
}

/// A blob attached to a chat message. `blob_hash` is a [`ContentHash`]
/// in the local a3net-blob store; `thumbnail_hash` (when present) is a
/// separate content-addressed preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessageAttachment {
    pub attachment_id: String,
    pub file_type: AttachmentKind,
    pub blob_hash: String,
    pub file_name: String,
    pub file_size: u64,
    pub thumbnail_hash: Option<String>,
}

impl MessageAttachment {
    pub fn validate(&self) -> Result<()> {
        validate_id("attachment_id", &self.attachment_id)?;
        // file_type is an enum, no further check needed.
        validate_id("blob_hash", &self.blob_hash)?;
        if self.blob_hash.len() != ContentHash::HEX_LEN {
            return Err(AdnetError::Validation(format!(
                "blob_hash: expected {} hex chars, got {}",
                ContentHash::HEX_LEN,
                self.blob_hash.len()
            )));
        }
        if !self.blob_hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(AdnetError::Validation("blob_hash: non-hex chars".into()));
        }
        validate_name("file_name", &self.file_name)?;
        if let Some(t) = &self.thumbnail_hash {
            if t.len() != ContentHash::HEX_LEN {
                return Err(AdnetError::Validation(format!(
                    "thumbnail_hash: expected {} hex chars, got {}",
                    ContentHash::HEX_LEN,
                    t.len()
                )));
            }
            if !t.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(AdnetError::Validation(
                    "thumbnail_hash: non-hex chars".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Roster entry for a single member of a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GroupMember {
    pub agent_id: String,
    pub agent_name: String,
    pub role: MemberRole,
    pub joined_at: u64,
    pub last_seen: u64,
    pub is_online: bool,
    pub nickname: Option<String>,
}

impl GroupMember {
    pub fn validate(&self) -> Result<()> {
        validate_id("agent_id", &self.agent_id)?;
        validate_name("agent_name", &self.agent_name)?;
        validate_ordered("last_seen vs joined_at", self.joined_at, self.last_seen)?;
        if let Some(n) = &self.nickname {
            validate_name("nickname", n)?;
        }
        Ok(())
    }
}

/// Pending invitation to join a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GroupInvitation {
    pub invitation_id: String,
    pub group_id: String,
    pub group_name: String,
    pub inviter_id: String,
    pub inviter_name: String,
    pub invitee_id: String,
    pub status: InvitationStatus,
    pub created_at: u64,
    pub expires_at: u64,
}

impl GroupInvitation {
    pub fn validate(&self) -> Result<()> {
        validate_id("invitation_id", &self.invitation_id)?;
        validate_id("group_id", &self.group_id)?;
        validate_name("group_name", &self.group_name)?;
        validate_id("inviter_id", &self.inviter_id)?;
        validate_name("inviter_name", &self.inviter_name)?;
        validate_id("invitee_id", &self.invitee_id)?;
        validate_ordered("expires_at vs created_at", self.created_at, self.expires_at)?;
        Ok(())
    }
}

/// A 1-to-1 chat conversation between two agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DirectChat {
    pub chat_id: String,
    pub user_a: String,
    pub user_b: String,
    pub created_at: u64,
    pub last_activity: u64,
    pub message_count: u32,
}

impl DirectChat {
    /// Deterministic chat id derived from two user ids (sorted
    /// lexicographically so `chat(a, b) == chat(b, a)`).
    pub fn chat_id_for(user_a: &str, user_b: &str) -> String {
        let mut ids = [user_a, user_b];
        ids.sort();
        format!("dm:{}:{}", ids[0], ids[1])
    }

    pub fn validate(&self) -> Result<()> {
        validate_id("chat_id", &self.chat_id)?;
        validate_id("user_a", &self.user_a)?;
        validate_id("user_b", &self.user_b)?;
        validate_ordered(
            "last_activity vs created_at",
            self.created_at,
            self.last_activity,
        )?;
        Ok(())
    }
}

/// A direct (1-to-1) chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DirectMessage {
    pub message_id: String,
    pub chat_id: String,
    pub sender_id: String,
    pub receiver_id: String,
    pub content: String,
    pub message_type: MessageType,
    pub attachments: Vec<MessageAttachment>,
    pub reply_to: Option<String>,
    pub sequence: u32,
    pub timestamp: u64,
    pub integrity_hash: Option<String>,
    pub is_edited: bool,
    pub edited_at: Option<u64>,
}

impl DirectMessage {
    pub fn validate(&self) -> Result<()> {
        validate_id("message_id", &self.message_id)?;
        validate_id("chat_id", &self.chat_id)?;
        validate_id("sender_id", &self.sender_id)?;
        validate_id("receiver_id", &self.receiver_id)?;
        validate_content("content", &self.content)?;
        if self.attachments.len() > MAX_ATTACHMENTS {
            return Err(AdnetError::Validation(format!(
                "attachments: {} exceeds {MAX_ATTACHMENTS}",
                self.attachments.len()
            )));
        }
        for (i, a) in self.attachments.iter().enumerate() {
            a.validate().map_err(|e| match e {
                AdnetError::Validation(msg) => {
                    AdnetError::Validation(format!("attachments[{i}]: {msg}"))
                }
                other => other,
            })?;
        }
        if let Some(r) = &self.reply_to {
            validate_id("reply_to", r)?;
        }
        Sequence::new(self.sequence, MAX_SEQUENCE)?;
        if self.is_edited {
            let ea = self.edited_at.ok_or_else(|| {
                AdnetError::Validation("is_edited=true with edited_at=None".into())
            })?;
            validate_ordered("edited_at vs timestamp", self.timestamp, ea)?;
        } else if self.edited_at.is_some() {
            return Err(AdnetError::Validation(
                "edited_at set while is_edited=false".into(),
            ));
        }
        crate::integrity::preflight_direct(&self.sender_id, &self.receiver_id, &self.content)?;
        Ok(())
    }

    /// Re-compute and stamp the [`DirectMessage::integrity_hash`].
    pub fn stamp_integrity_hash(&mut self) {
        self.integrity_hash = Some(self.compute_hash());
    }

    pub fn compute_hash(&self) -> String {
        let base = crate::integrity::direct_hash(
            &self.sender_id,
            &self.receiver_id,
            &self.content,
            self.sequence,
            self.timestamp,
        );
        let edit_part: &[u8] = if self.is_edited {
            b"edited"
        } else {
            b"original"
        };
        crate::integrity::hash_fields([
            base.as_bytes(),
            edit_part,
            &self.edited_at.unwrap_or(0).to_le_bytes(),
        ])
    }

    pub fn verify_integrity_outcome(&self) -> VerifyOutcome {
        let computed = self.compute_hash();
        match &self.integrity_hash {
            Some(h) if h == &computed => VerifyOutcome::Valid,
            Some(_) => VerifyOutcome::Mismatch,
            None => VerifyOutcome::Missing,
        }
    }

    pub fn verify_integrity(&self) -> bool {
        matches!(self.verify_integrity_outcome(), VerifyOutcome::Valid)
    }
}

/// Read receipt acknowledging a delivered message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessageReceipt {
    pub receipt_id: String,
    pub message_id: String,
    pub receiver_id: String,
    pub sequence: u32,
    pub received_at: u64,
}

impl MessageReceipt {
    pub fn validate(&self) -> Result<()> {
        validate_id("receipt_id", &self.receipt_id)?;
        validate_id("message_id", &self.message_id)?;
        validate_id("receiver_id", &self.receiver_id)?;
        Sequence::new(self.sequence, MAX_SEQUENCE)?;
        Ok(())
    }
}

/// Last sequence number observed by `user_id` for messages from
/// `sender_id` (used for missing-message detection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UserSequence {
    pub user_id: String,
    pub sender_id: String,
    pub last_sequence: u32,
    pub updated_at: u64,
}

impl UserSequence {
    pub fn validate(&self) -> Result<()> {
        validate_id("user_id", &self.user_id)?;
        validate_id("sender_id", &self.sender_id)?;
        Sequence::new(self.last_sequence, MAX_SEQUENCE)?;
        Ok(())
    }
}

/// Last sequence number observed by `user_id` in `group_id` (used for
/// missing-message detection in group chats).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GroupSequence {
    pub user_id: String,
    pub group_id: String,
    pub last_sequence: u32,
    pub updated_at: u64,
}

impl GroupSequence {
    pub fn validate(&self) -> Result<()> {
        validate_id("user_id", &self.user_id)?;
        validate_id("group_id", &self.group_id)?;
        Sequence::new(self.last_sequence, MAX_SEQUENCE)?;
        Ok(())
    }
}

/// Generate a fresh message id from a [`NodeId`] + group + sequence.
/// The id is the full 32-byte BLAKE3 digest, rendered as 64 hex chars.
/// F-04 fix: the previous code truncated to 16 bytes, which has an
/// acceptable but non-zero collision probability for long-running
/// services.
pub fn next_group_message_id(sender: &NodeId, group_id: &str, sequence: u32) -> String {
    let mut h = blake3::Hasher::new();
    h.update(&sender.as_bytes());
    h.update(group_id.as_bytes());
    h.update(&sequence.to_le_bytes());
    hex::encode(h.finalize().as_bytes())
}

/// Build a fresh [`MessageAttachment`] from a [`ContentHash`] + name.
pub fn attachment_from_hash(
    attachment_id: String,
    file_type: AttachmentKind,
    blob: &ContentHash,
    file_name: impl Into<String>,
    file_size: u64,
) -> MessageAttachment {
    MessageAttachment {
        attachment_id,
        file_type,
        blob_hash: blob.as_hex().to_string(),
        file_name: file_name.into(),
        file_size,
        thumbnail_hash: None,
    }
}

// ── Forward `Validate` to each record's own `validate()` method. ────────
impl Validate for GroupChat {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}
impl Validate for GroupMessage {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}
impl Validate for MessageAttachment {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}
impl Validate for GroupMember {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}
impl Validate for GroupInvitation {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}
impl Validate for DirectChat {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}
impl Validate for DirectMessage {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}
impl Validate for MessageReceipt {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}
impl Validate for UserSequence {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}
impl Validate for GroupSequence {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}

/// Test-only helper kept for backward compatibility with the prior
/// tests: legacy callers passed `&str` file types and the function
/// accepted them via `from_strict`. Returns an error instead of
/// silently coercing invalid input.
pub fn attachment_from_hash_str(
    attachment_id: String,
    file_type: &str,
    blob: &ContentHash,
    file_name: impl Into<String>,
    file_size: u64,
) -> Result<MessageAttachment> {
    let ft = AttachmentKind::from_strict(file_type)?;
    Ok(attachment_from_hash(
        attachment_id,
        ft,
        blob,
        file_name,
        file_size,
    ))
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn good_group_msg(seq: u32) -> GroupMessage {
        GroupMessage {
            message_id: "m1".into(),
            group_id: "g1".into(),
            sender_id: "alice".into(),
            sender_name: "Alice".into(),
            content: "ping".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            mentions: vec![],
            timestamp: 2_000,
            is_edited: false,
            edited_at: None,
            sequence: seq,
            integrity_hash: None,
        }
    }

    #[test]
    fn direct_chat_id_is_symmetric() {
        assert_eq!(
            DirectChat::chat_id_for("alice", "bob"),
            DirectChat::chat_id_for("bob", "alice")
        );
        assert_ne!(
            DirectChat::chat_id_for("alice", "bob"),
            DirectChat::chat_id_for("alice", "eve")
        );
    }

    #[test]
    fn direct_message_integrity_roundtrip() {
        let mut msg = DirectMessage {
            message_id: "m1".into(),
            chat_id: DirectChat::chat_id_for("a", "b"),
            sender_id: "a".into(),
            receiver_id: "b".into(),
            content: "hello".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_000,
            integrity_hash: None,
            is_edited: false,
            edited_at: None,
        };
        msg.stamp_integrity_hash();
        assert!(msg.validate().is_ok());
        assert_eq!(msg.verify_integrity_outcome(), VerifyOutcome::Valid);
        msg.content = "tampered".into();
        assert_eq!(msg.verify_integrity_outcome(), VerifyOutcome::Mismatch);
        // No hash → Missing.
        let mut bare = msg.clone();
        bare.integrity_hash = None;
        assert_eq!(bare.verify_integrity_outcome(), VerifyOutcome::Missing);
    }

    #[test]
    fn group_message_integrity_roundtrip() {
        let mut msg = good_group_msg(7);
        assert!(msg.validate().is_ok());
        msg.stamp_integrity_hash();
        assert_eq!(msg.verify_integrity_outcome(), VerifyOutcome::Valid);
        // Edit must re-stamp the hash.
        msg.is_edited = true;
        msg.edited_at = Some(2_500);
        let stale_hash = msg.integrity_hash.clone().unwrap();
        assert_eq!(msg.verify_integrity_outcome(), VerifyOutcome::Mismatch);
        msg.stamp_integrity_hash();
        assert_ne!(stale_hash, msg.integrity_hash.clone().unwrap());
        assert_eq!(msg.verify_integrity_outcome(), VerifyOutcome::Valid);
    }

    #[test]
    fn validate_rejects_empty_and_oversize() {
        let mut msg = good_group_msg(1);
        msg.sender_id = "".into();
        assert!(msg.validate().is_err());
        msg.sender_id = "alice".into();
        msg.content = "x".repeat(invariants::MAX_CONTENT_LEN + 1);
        assert!(msg.validate().is_err());
        msg.content = "ok".into();
        assert!(msg.validate().is_ok());

        // edited_at without is_edited is rejected.
        msg.edited_at = Some(99);
        assert!(msg.validate().is_err());
        msg.edited_at = None;
        msg.is_edited = true;
        assert!(msg.validate().is_err()); // edited_at = None while is_edited
        msg.edited_at = Some(1_500); // earlier than timestamp
        assert!(msg.validate().is_err());
        msg.edited_at = Some(3_000);
        assert!(msg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_bad_sequence() {
        let mut msg = good_group_msg(MAX_SEQUENCE);
        assert!(msg.validate().is_err());
        msg.sequence = MAX_SEQUENCE - 1;
        assert!(msg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_bad_message_type_field_absent() {
        // Sanity: MessageType is an enum, so a bogus wire value would
        // fail to deserialise in the first place.
        let json = r#"{"message_type":"bogus"}"#;
        let r: std::result::Result<MessageType, _> = serde_json::from_str(json);
        assert!(r.is_err());
    }

    #[test]
    fn group_message_serializes_in_snake_case() {
        let msg = good_group_msg(1);
        let v = serde_json::to_value(&msg).unwrap();
        assert!(v.get("sender_id").is_some());
        assert!(v.get("senderId").is_none());
        assert!(v.get("is_edited").is_some());
        assert_eq!(v.get("message_type").unwrap(), "text");
    }

    #[test]
    fn attachment_from_hash_populates_blob_hash() {
        let blob = ContentHash::from_bytes(b"hello");
        let att = attachment_from_hash("a1".into(), AttachmentKind::Image, &blob, "hello.png", 5);
        assert_eq!(att.blob_hash, blob.as_hex());
        assert_eq!(att.file_size, 5);
        assert!(att.validate().is_ok());

        // Strict variant rejects unknown kind.
        let bad = attachment_from_hash_str("a2".into(), "weird", &blob, "x", 1);
        assert!(bad.is_err());
    }

    #[test]
    fn attachment_validates_hash_lengths() {
        let blob = ContentHash::from_bytes(b"hello");
        let mut att =
            attachment_from_hash("a1".into(), AttachmentKind::Image, &blob, "hello.png", 5);
        att.blob_hash = "short".into();
        assert!(att.validate().is_err());
        att.blob_hash = blob.as_hex().to_string();
        att.thumbnail_hash = Some("nope".into());
        assert!(att.validate().is_err());
        att.thumbnail_hash = None;
        assert!(att.validate().is_ok());
    }

    #[test]
    fn group_message_id_is_stable_per_sequence() {
        let node = NodeId::random();
        let m1 = next_group_message_id(&node, "g1", 1);
        let m2 = next_group_message_id(&node, "g1", 2);
        assert_ne!(m1, m2);
        let m1_again = next_group_message_id(&node, "g1", 1);
        assert_eq!(m1, m1_again);
        // Full BLAKE3 digest (64 hex chars), not the old 16-byte truncation.
        assert_eq!(m1.len(), 64);
    }

    #[test]
    fn invitation_validates_temporal_ordering() {
        let mut inv = GroupInvitation {
            invitation_id: "i1".into(),
            group_id: "g".into(),
            group_name: "G".into(),
            inviter_id: "a".into(),
            inviter_name: "Alice".into(),
            invitee_id: "b".into(),
            status: InvitationStatus::Pending,
            created_at: 100,
            expires_at: 200,
        };
        assert!(inv.validate().is_ok());
        inv.expires_at = 50;
        assert!(inv.validate().is_err());
    }

    #[test]
    fn member_validates_temporal_ordering() {
        let mut m = GroupMember {
            agent_id: "a".into(),
            agent_name: "A".into(),
            role: MemberRole::Owner,
            joined_at: 100,
            last_seen: 50,
            is_online: false,
            nickname: None,
        };
        assert!(m.validate().is_err());
        m.last_seen = 100;
        assert!(m.validate().is_ok());
    }

    #[test]
    fn admin_role_predicate() {
        assert!(MemberRole::Owner.can_administer());
        assert!(MemberRole::Admin.can_administer());
        assert!(!MemberRole::Member.can_administer());
    }

    #[test]
    fn invitation_status_terminal_predicate() {
        assert!(!InvitationStatus::Pending.is_terminal());
        assert!(InvitationStatus::Accepted.is_terminal());
        assert!(InvitationStatus::Rejected.is_terminal());
        assert!(InvitationStatus::Expired.is_terminal());
    }

    // ───────────────────── Property-based tests ────────────────────────────

    proptest! {
        /// Fuzz: any valid GroupMessage must validate.
        #[test]
        fn prop_group_message_validates(
            content in "[a-zA-Z0-9 ,.!?]{1,128}",
            seq in 0u32..MAX_SEQUENCE,
        ) {
            let msg = good_group_msg(seq);
            let mut m = msg;
            m.content = content;
            prop_assert!(m.validate().is_ok());
        }

        /// Fuzz: a single-character content change must invalidate the
        /// precomputed hash (avoids regressions where a field stops
        /// being covered by the digest).
        #[test]
        fn prop_edit_bypasses_when_hash_not_restyled(
            content in "[a-zA-Z0-9 ]{1,32}",
            seq in 0u32..MAX_SEQUENCE,
        ) {
            let mut m = good_group_msg(seq);
            m.content = content.clone();
            m.stamp_integrity_hash();
            prop_assert_eq!(m.verify_integrity_outcome(), VerifyOutcome::Valid);
            // Flip the last character.
            let mut bad = content;
            if let Some(c) = bad.pop() {
                bad.push(if c == 'a' { 'b' } else { 'a' });
            }
            m.content = bad;
            prop_assert_eq!(m.verify_integrity_outcome(), VerifyOutcome::Mismatch);
        }
    }
}
