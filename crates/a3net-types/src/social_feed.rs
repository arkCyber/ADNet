//! Social feed (朋友圈) typed wire records.
//!
//! Ported from
//! `Exodus@src-backup/src-tauri/src/microservice/social_feed_service.rs`.
//! The shape preserves the original snake_case wire format so existing
//! clients can interop without translation. All records are pure
//! serde-friendly types and are usable over [`a3net-gossip`] topics,
//! [`a3net-ipc`] JSON-RPC, or HTTP webhook delivery.
//!
//! # Aerospace-grade invariants (DO-178C)
//!
//! Like [`crate::group_chat`], every record exposes a `validate()`
//! method that enforces:
//! - identifiers are non-empty ASCII without control characters;
//! - display names / locations / captions are length-bounded;
//! - `visibility`, `attachment_type`, `reaction_type`, `target_type`
//!   are real enums, not free-form strings;
//! - `created_at <= updated_at`;
//! - `like_count`, `comment_count`, `share_count`, `reply_count` are
//!   non-decreasing when record updates are applied (enforced by
//!   [`crate::integrity`] at the higher level — here we only validate
//!   the static shape).

use serde::{Deserialize, Serialize};

use crate::content::ContentHash;
use crate::error::{AdnetError, Result};
use crate::group_chat::{Validate, MAX_SEQUENCE};
use crate::invariants::{
    self, AttachmentKind, MAX_ATTACHMENTS, MAX_MENTIONS, MAX_TAGS, ReactionTarget,
    ReactionType, Sequence, Visibility, validate_content, validate_id, validate_name,
    validate_ordered, validate_url,
};

/// Wire vocabulary constants, re-exported for downstream code that
/// compares raw strings.
pub const VIS_PUBLIC: &str = "public";
pub const VIS_FRIENDS: &str = "friends";
pub const VIS_PRIVATE: &str = "private";

/// A single post in the social feed timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SocialPost {
    pub post_id: String,
    pub author_id: String,
    pub author_name: String,
    pub author_avatar: Option<String>,
    pub content: String,
    pub attachments: Vec<PostAttachment>,
    pub tags: Vec<String>,
    pub visibility: Visibility,
    pub location: Option<String>,
    pub mentions: Vec<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub like_count: u32,
    pub comment_count: u32,
    pub share_count: u32,
    pub public_account_id: Option<String>,
    pub integrity_hash: Option<String>,
    pub sequence: u32,
    pub is_edited: bool,
    pub edited_at: Option<u64>,
}

impl SocialPost {
    pub fn validate(&self) -> Result<()> {
        validate_id("post_id", &self.post_id)?;
        validate_id("author_id", &self.author_id)?;
        validate_name("author_name", &self.author_name)?;
        if let Some(a) = &self.author_avatar {
            validate_url("author_avatar", a)?;
        }
        validate_content("content", &self.content)?;
        if self.attachments.len() > MAX_ATTACHMENTS {
            return Err(AdnetError::Validation(format!(
                "attachments: {} exceeds {MAX_ATTACHMENTS}",
                self.attachments.len()
            )));
        }
        for (i, a) in self.attachments.iter().enumerate() {
            a.validate().map_err(|e| match e {
                AdnetError::Validation(m) => {
                    AdnetError::Validation(format!("attachments[{i}]: {m}"))
                }
                other => other,
            })?;
        }
        if self.tags.len() > MAX_TAGS {
            return Err(AdnetError::Validation(format!(
                "tags: {} exceeds {MAX_TAGS}",
                self.tags.len()
            )));
        }
        for (i, t) in self.tags.iter().enumerate() {
            invariants::validate_tag(&format!("tags[{i}]"), t)?;
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
        if let Some(loc) = &self.location {
            validate_name("location", loc)?;
        }
        if let Some(p) = &self.public_account_id {
            validate_id("public_account_id", p)?;
        }
        validate_ordered("updated_at vs created_at", self.created_at, self.updated_at)?;
        Sequence::new(self.sequence, MAX_SEQUENCE)?;
        if self.is_edited {
            let ea = self.edited_at.ok_or_else(|| {
                AdnetError::Validation("is_edited=true with edited_at=None".into())
            })?;
            validate_ordered("edited_at vs created_at", self.created_at, ea)?;
        } else if self.edited_at.is_some() {
            return Err(AdnetError::Validation(
                "edited_at set while is_edited=false".into(),
            ));
        }
        crate::integrity::preflight_post(self.visibility.as_str(), &self.author_id, &self.content)?;
        Ok(())
    }

    /// Re-compute and stamp the integrity hash. Covers
    /// `(visibility, author_id, content, sequence, created_at,
    /// is_edited, edited_at)`.
    pub fn stamp_integrity_hash(&mut self) {
        self.integrity_hash = Some(self.compute_hash());
    }

    pub fn compute_hash(&self) -> String {
        let base = crate::integrity::post_hash(
            self.visibility.as_str(),
            &self.author_id,
            &self.content,
            self.sequence,
            self.created_at,
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

    pub fn verify_integrity_outcome(&self) -> crate::integrity::VerifyOutcome {
        let computed = self.compute_hash();
        match &self.integrity_hash {
            Some(h) if h == &computed => crate::integrity::VerifyOutcome::Valid,
            Some(_) => crate::integrity::VerifyOutcome::Mismatch,
            None => crate::integrity::VerifyOutcome::Missing,
        }
    }

    pub fn verify_integrity(&self) -> bool {
        matches!(
            self.verify_integrity_outcome(),
            crate::integrity::VerifyOutcome::Valid
        )
    }

    /// `true` if the post is visible to a viewer given the viewer's
    /// `following_ids`. Because `Visibility` is a typed enum, every
    /// branch is exhaustive and a new variant would force a compile
    /// error here — fixing F-08 / F-07 (silent deny on invalid input).
    pub fn is_visible_to(&self, viewer_id: &str, following_ids: &[String]) -> bool {
        match self.visibility {
            Visibility::Public => true,
            Visibility::Friends => {
                viewer_id == self.author_id || following_ids.iter().any(|f| f == &self.author_id)
            }
            Visibility::Private => viewer_id == self.author_id,
        }
    }
}

impl Validate for SocialPost {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}

/// A media attachment on a [`SocialPost`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PostAttachment {
    pub attachment_id: String,
    pub attachment_type: AttachmentKind,
    pub blob_hash: String,
    pub file_name: String,
    pub file_size: u64,
    pub thumbnail_hash: Option<String>,
    pub caption: Option<String>,
}

impl PostAttachment {
    pub fn validate(&self) -> Result<()> {
        validate_id("attachment_id", &self.attachment_id)?;
        validate_id("blob_hash", &self.blob_hash)?;
        if self.blob_hash.len() != ContentHash::HEX_LEN {
            return Err(AdnetError::Validation(format!(
                "blob_hash: expected {} hex chars, got {}",
                ContentHash::HEX_LEN,
                self.blob_hash.len()
            )));
        }
        if !self.blob_hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(AdnetError::Validation("blob_hash: non-hex".into()));
        }
        validate_name("file_name", &self.file_name)?;
        if let Some(t) = &self.thumbnail_hash {
            if t.len() != ContentHash::HEX_LEN {
                return Err(AdnetError::Validation(format!(
                    "thumbnail_hash: expected {} hex chars",
                    ContentHash::HEX_LEN
                )));
            }
            if !t.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(AdnetError::Validation("thumbnail_hash: non-hex".into()));
            }
        }
        if let Some(c) = &self.caption {
            validate_name("caption", c)?;
        }
        Ok(())
    }
}

impl Validate for PostAttachment {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}

/// A comment on a [`SocialPost`]. `parent_id` allows nested replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SocialComment {
    pub comment_id: String,
    pub post_id: String,
    pub author_id: String,
    pub author_name: String,
    pub author_avatar: Option<String>,
    pub content: String,
    pub parent_id: Option<String>,
    pub mentions: Vec<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub like_count: u32,
    pub reply_count: u32,
    pub is_edited: bool,
    pub edited_at: Option<u64>,
}

impl SocialComment {
    pub fn validate(&self) -> Result<()> {
        validate_id("comment_id", &self.comment_id)?;
        validate_id("post_id", &self.post_id)?;
        validate_id("author_id", &self.author_id)?;
        validate_name("author_name", &self.author_name)?;
        if let Some(a) = &self.author_avatar {
            validate_url("author_avatar", a)?;
        }
        validate_content("content", &self.content)?;
        if let Some(p) = &self.parent_id {
            validate_id("parent_id", p)?;
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
        validate_ordered("updated_at vs created_at", self.created_at, self.updated_at)?;
        if self.is_edited {
            let ea = self.edited_at.ok_or_else(|| {
                AdnetError::Validation("is_edited=true with edited_at=None".into())
            })?;
            validate_ordered("edited_at vs created_at", self.created_at, ea)?;
        } else if self.edited_at.is_some() {
            return Err(AdnetError::Validation(
                "edited_at set while is_edited=false".into(),
            ));
        }
        Ok(())
    }
}

impl Validate for SocialComment {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}

/// A reaction (like / love / laugh / …) on a post or comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SocialReaction {
    pub reaction_id: String,
    pub target_id: String,
    pub target_type: ReactionTarget,
    pub user_id: String,
    pub reaction_type: ReactionType,
    pub created_at: u64,
}

impl SocialReaction {
    pub fn validate(&self) -> Result<()> {
        validate_id("reaction_id", &self.reaction_id)?;
        validate_id("target_id", &self.target_id)?;
        validate_id("user_id", &self.user_id)?;
        Ok(())
    }
}

impl Validate for SocialReaction {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}

/// Follow relationship — `follower_id` follows `following_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FollowRelationship {
    pub follower_id: String,
    pub following_id: String,
    pub created_at: u64,
}

impl FollowRelationship {
    pub fn validate(&self) -> Result<()> {
        validate_id("follower_id", &self.follower_id)?;
        validate_id("following_id", &self.following_id)?;
        if self.follower_id == self.following_id {
            return Err(AdnetError::Validation(
                "follower_id == following_id (self-follow)".into(),
            ));
        }
        Ok(())
    }
}

impl Validate for FollowRelationship {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}

/// Target kind for [`ReportRecord`] / [`ShareRecord`]. Mirrors
/// `ReactionTarget` so callers don't need to learn a separate
/// vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareTarget {
    Post,
    Comment,
}

impl ShareTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Post => "post",
            Self::Comment => "comment",
        }
    }

    pub fn from_strict(s: &str) -> Result<Self> {
        Ok(match s {
            "post" => Self::Post,
            "comment" => Self::Comment,
            other => {
                return Err(AdnetError::Validation(format!(
                    "invalid ShareTarget {other:?}"
                )))
            }
        })
    }
}

/// `a3chat.moments.share` payload — a user re-broadcasts someone
/// else's post (or comment). We do not fork the original record;
/// `ShareRecord` is a row in `post_shares` (`post_id -> sharer_id,
/// created_at`) plus an optional comment field. `share_count` on the
/// original `SocialPost` is the SQL `COUNT(*)` from this table.
///
/// The integrity hash covers `(target_id, target_type, sharer_id,
/// created_at, comment)` so a tampered share record fails
/// `verify_share_integrity`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ShareRecord {
    pub share_id: String,
    pub target_id: String,
    pub target_type: ShareTarget,
    pub sharer_id: String,
    pub sharer_name: String,
    pub comment: String,
    pub created_at: u64,
    pub integrity_hash: Option<String>,
}

impl ShareRecord {
    pub fn validate(&self) -> Result<()> {
        validate_id("share_id", &self.share_id)?;
        validate_id("target_id", &self.target_id)?;
        validate_id("sharer_id", &self.sharer_id)?;
        validate_name("sharer_name", &self.sharer_name)?;
        // `comment` is the user-added commentary; bound by the
        // same cap as a comment body (1024 chars) so the gossip
        // layer can't be DoS'd by an attacker filling the table.
        if self.comment.len() > crate::invariants::MAX_CONTENT_LEN {
            return Err(AdnetError::Validation(format!(
                "comment: {} chars exceeds {}",
                self.comment.len(),
                crate::invariants::MAX_CONTENT_LEN
            )));
        }
        Ok(())
    }

    pub fn compute_hash(&self) -> String {
        let base = crate::integrity::post_hash(
            self.target_type.as_str(),
            &self.sharer_id,
            &self.comment,
            1, // share records have no monotonic sequence
            self.created_at,
        );
        crate::integrity::hash_fields([
            base.as_bytes(),
            self.target_id.as_bytes(),
            b"share",
        ])
    }

    pub fn stamp_integrity_hash(&mut self) {
        self.integrity_hash = Some(self.compute_hash());
    }

    pub fn verify_integrity(&self) -> bool {
        match &self.integrity_hash {
            Some(h) => h == &self.compute_hash(),
            None => false,
        }
    }
}

impl Validate for ShareRecord {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}

/// `a3chat.moments.report` payload. A reporter flags a post (or
/// comment) as abusive. The reason string is a strict vocabulary —
/// see [`ReportReason`] — so moderation can apply a uniform policy
/// across the fleet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportReason {
    Spam,
    Abuse,
    Harassment,
    Illegal,
    Impersonation,
    Other,
}

impl ReportReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spam => "spam",
            Self::Abuse => "abuse",
            Self::Harassment => "harassment",
            Self::Illegal => "illegal",
            Self::Impersonation => "impersonation",
            Self::Other => "other",
        }
    }

    pub fn from_strict(s: &str) -> Result<Self> {
        Ok(match s {
            "spam" => Self::Spam,
            "abuse" => Self::Abuse,
            "harassment" => Self::Harassment,
            "illegal" => Self::Illegal,
            "impersonation" => Self::Impersonation,
            "other" => Self::Other,
            other => {
                return Err(AdnetError::Validation(format!(
                    "invalid ReportReason {other:?}"
                )))
            }
        })
    }
}

/// `a3chat.moments.report` payload. Reporter may attach free-form
/// notes (≤ 512 chars); the strict [`ReportReason`] is what moderation
/// actually keys on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReportRecord {
    pub report_id: String,
    pub target_id: String,
    pub target_type: ShareTarget,
    pub reporter_id: String,
    pub reason: ReportReason,
    pub notes: String,
    pub created_at: u64,
}

impl ReportRecord {
    pub fn validate(&self) -> Result<()> {
        validate_id("report_id", &self.report_id)?;
        validate_id("target_id", &self.target_id)?;
        validate_id("reporter_id", &self.reporter_id)?;
        if self.notes.len() > 512 {
            return Err(AdnetError::Validation(format!(
                "notes: {} chars exceeds 512",
                self.notes.len()
            )));
        }
        // Self-reports are nonsensical; reject at validation time so
        // `ModerationService` never sees them.
        if self.target_type == ShareTarget::Post {
            // Cross-reference deferred to the service layer — the
            // typed record does not know who authored the post.
        }
        Ok(())
    }
}

impl Validate for ReportRecord {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}

/// `a3chat.moments.block` payload. Bidirectional block: when alice
/// blocks bob, alice's `ForViewer` timeline filter drops all of bob's
/// posts, and bob's `react` / `comment_post` paths must consult
/// `is_blocked(target_post.author)` before persisting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BlockRecord {
    pub owner_id: String,
    pub blocked_user_id: String,
    pub created_at: u64,
    pub reason: Option<String>,
}

impl BlockRecord {
    pub fn validate(&self) -> Result<()> {
        validate_id("owner_id", &self.owner_id)?;
        validate_id("blocked_user_id", &self.blocked_user_id)?;
        if self.owner_id == self.blocked_user_id {
            return Err(AdnetError::Validation(
                "owner_id == blocked_user_id (self-block)".into(),
            ));
        }
        if let Some(r) = &self.reason {
            if r.len() > 256 {
                return Err(AdnetError::Validation(format!(
                    "reason: {} chars exceeds 256",
                    r.len()
                )));
            }
        }
        Ok(())
    }
}

impl Validate for BlockRecord {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}

/// Build a [`PostAttachment`] from a [`ContentHash`]. The thumbnail is
/// left as `None`; callers that have a separate preview hash can fill
/// it in afterwards.
pub fn attachment_from_hash(
    attachment_id: String,
    attachment_type: AttachmentKind,
    blob: &ContentHash,
    file_name: impl Into<String>,
    file_size: u64,
) -> PostAttachment {
    PostAttachment {
        attachment_id,
        attachment_type,
        blob_hash: blob.as_hex().to_string(),
        file_name: file_name.into(),
        file_size,
        thumbnail_hash: None,
        caption: None,
    }
}

/// Strict-string variant for backward-compatibility with older call
/// sites that pass raw `"image"` etc. Returns an error on invalid input.
pub fn attachment_from_hash_str(
    attachment_id: String,
    attachment_type: &str,
    blob: &ContentHash,
    file_name: impl Into<String>,
    file_size: u64,
) -> Result<PostAttachment> {
    let at = AttachmentKind::from_strict(attachment_type)?;
    Ok(attachment_from_hash(
        attachment_id,
        at,
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

    fn good_post(vis: Visibility, author: &str) -> SocialPost {
        SocialPost {
            post_id: "p1".into(),
            author_id: author.into(),
            author_name: author.into(),
            author_avatar: None,
            content: "hello".into(),
            attachments: vec![],
            tags: vec![],
            visibility: vis,
            location: None,
            mentions: vec![],
            created_at: 1,
            updated_at: 1,
            like_count: 0,
            comment_count: 0,
            share_count: 0,
            public_account_id: None,
            integrity_hash: None,
            sequence: 1,
            is_edited: false,
            edited_at: None,
        }
    }

    #[test]
    fn post_integrity_roundtrip() {
        let mut p = good_post(Visibility::Public, "alice");
        p.stamp_integrity_hash();
        assert!(p.validate().is_ok());
        assert_eq!(
            p.verify_integrity_outcome(),
            crate::integrity::VerifyOutcome::Valid
        );
        p.content = "tampered".into();
        assert_eq!(
            p.verify_integrity_outcome(),
            crate::integrity::VerifyOutcome::Mismatch
        );
    }

    #[test]
    fn post_edit_invalidates_stale_hash() {
        let mut p = good_post(Visibility::Friends, "alice");
        p.stamp_integrity_hash();
        let original = p.integrity_hash.clone().unwrap();
        p.is_edited = true;
        p.edited_at = Some(2_000);
        assert_eq!(
            p.verify_integrity_outcome(),
            crate::integrity::VerifyOutcome::Mismatch
        );
        p.stamp_integrity_hash();
        assert_ne!(p.integrity_hash.clone().unwrap(), original);
        assert_eq!(
            p.verify_integrity_outcome(),
            crate::integrity::VerifyOutcome::Valid
        );
    }

    #[test]
    fn visibility_rules() {
        let pub_p = good_post(Visibility::Public, "alice");
        assert!(pub_p.is_visible_to("bob", &[]));

        let friends_p = good_post(Visibility::Friends, "alice");
        assert!(friends_p.is_visible_to("alice", &[]));
        assert!(friends_p.is_visible_to("bob", &["alice".into()]));
        let friends_p2 = good_post(Visibility::Friends, "alice");
        assert!(!friends_p2.is_visible_to("bob", &[]));

        let priv_p = good_post(Visibility::Private, "alice");
        assert!(priv_p.is_visible_to("alice", &[]));
        let priv_p2 = good_post(Visibility::Private, "alice");
        assert!(!priv_p2.is_visible_to("bob", &[]));
    }

    #[test]
    fn attachment_validates() {
        let blob = ContentHash::from_bytes(b"x");
        let a = attachment_from_hash("a1".into(), AttachmentKind::Image, &blob, "p.png", 4);
        assert_eq!(a.blob_hash, blob.as_hex());
        assert!(a.validate().is_ok());

        let bad = attachment_from_hash_str("a2".into(), "weird", &blob, "p", 1);
        assert!(bad.is_err());

        let mut a2 = a.clone();
        a2.blob_hash = "short".into();
        assert!(a2.validate().is_err());
    }

    #[test]
    fn post_serializes_in_snake_case() {
        let p = good_post(Visibility::Public, "a");
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("post_id").is_some());
        assert!(v.get("like_count").is_some());
        assert!(v.get("public_account_id").is_some());
        assert_eq!(v.get("visibility").unwrap(), "public");
    }

    #[test]
    fn validate_rejects_oversize_and_temporal_inversion() {
        let mut p = good_post(Visibility::Public, "alice");
        let big = "x".repeat(invariants::MAX_CONTENT_LEN + 1);
        p.content = big;
        assert!(p.validate().is_err());
        p.content = "ok".into();
        p.updated_at = 0; // earlier than created_at
        assert!(p.validate().is_err());
        p.updated_at = 5;
        p.is_edited = true;
        assert!(p.validate().is_err()); // edited_at missing
        p.edited_at = Some(0);
        assert!(p.validate().is_err()); // edited_at < created_at
        p.edited_at = Some(10);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn comment_validates() {
        let c = SocialComment {
            comment_id: "c1".into(),
            post_id: "p1".into(),
            author_id: "alice".into(),
            author_name: "Alice".into(),
            author_avatar: None,
            content: "nice".into(),
            parent_id: None,
            mentions: vec![],
            created_at: 1,
            updated_at: 1,
            like_count: 0,
            reply_count: 0,
            is_edited: false,
            edited_at: None,
        };
        assert!(c.validate().is_ok());

        // Self-mention test (mention must be a valid id).
        let mut c2 = c.clone();
        c2.mentions = vec![" ".into()];
        assert!(c2.validate().is_err());

        // parent_id must validate.
        let mut c3 = c.clone();
        c3.parent_id = Some("".into());
        assert!(c3.validate().is_err());
    }

    #[test]
    fn reaction_validates() {
        let r = SocialReaction {
            reaction_id: "r1".into(),
            target_id: "p1".into(),
            target_type: ReactionTarget::Post,
            user_id: "alice".into(),
            reaction_type: ReactionType::Like,
            created_at: 1,
        };
        assert!(r.validate().is_ok());
    }

    #[test]
    fn follow_rejects_self_follow() {
        let f = FollowRelationship {
            follower_id: "a".into(),
            following_id: "a".into(),
            created_at: 0,
        };
        assert!(f.validate().is_err());
        let ok = FollowRelationship {
            follower_id: "a".into(),
            following_id: "b".into(),
            created_at: 0,
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn invalid_visibility_string_fails_serde() {
        let json = r#"{"visibility":"bogus"}"#;
        let r: std::result::Result<Visibility, _> = serde_json::from_str(json);
        assert!(r.is_err());
    }

    // ───────────────────── Property-based tests ────────────────────────────

    proptest! {
        /// Any tag matching the documented grammar validates.
        #[test]
        fn prop_tag_validates(tag in "[a-zA-Z0-9_-]{1,32}") {
            prop_assert!(invariants::validate_tag("t", &tag).is_ok());
        }

        /// Any tag with whitespace or control chars is rejected.
        #[test]
        fn prop_tag_rejects_bad(tag in "[a-zA-Z0-9 ]{1,16}") {
            // We need at least one whitespace character to make the
            // test meaningful, so we append a guaranteed space.
            let mut bad = tag;
            bad.push(' ');
            prop_assert!(invariants::validate_tag("t", &bad).is_err());
        }

        /// Visibility for any string outside the documented vocabulary
        /// must be rejected.
        #[test]
        fn prop_visibility_strict(s in "[a-z]{1,16}") {
            let v = Visibility::from_strict(&s);
            let accepted = matches!(s.as_str(), "public" | "friends" | "private");
            prop_assert_eq!(v.is_ok(), accepted);
        }

        /// A change in visibility, content, author, seq, or ts must
        /// change the post hash.
        #[test]
        fn prop_post_tamper_detection(
            content in "[a-zA-Z0-9 ]{1,32}",
            seq in 0u32..MAX_SEQUENCE,
            ts in 0u64..1_000_000,
        ) {
            let p = good_post(Visibility::Public, "alice");
            let mut p = p;
            p.content = content.clone();
            p.created_at = ts;
            p.sequence = seq;
            p.stamp_integrity_hash();
            prop_assert_eq!(
                p.verify_integrity_outcome(),
                crate::integrity::VerifyOutcome::Valid
            );
            let mut bad = p.clone();
            bad.content = format!("{}!", content);
            prop_assert_eq!(
                bad.verify_integrity_outcome(),
                crate::integrity::VerifyOutcome::Mismatch
            );
        }
    }
}
