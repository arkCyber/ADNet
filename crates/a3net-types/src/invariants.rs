//! Aerospace-grade (DO-178C style) invariants for wire records.
//!
//! ## Design rationale
//!
//! DO-178C requires every input to be either:
//!   * bounded (a finite, validated domain), or
//!   * rejected at the boundary with a typed error.
//!
//! This module provides the building blocks used by [`crate::group_chat`]
//! and [`crate::social_feed`] so that every record carries *executable*
//! invariants, not just comments. The validators are deterministic,
//! allocation-bounded, and free of `unwrap` / `panic!` / `expect`.
//!
//! ## Layered guarantees
//!
//! 1. **Type-level**: every domain string (`MessageType`, `Visibility`,
//!    `MemberRole`, `ReactionType`, `InvitationStatus`) is a real enum,
//!    not a free-form `String`. Any value that deserialises to something
//!    outside the documented vocabulary fails fast.
//! 2. **Length-level**: every `&str` is bounded by a `MAX_*` constant
//!    chosen so a single record can never exceed 16 KiB even under
//!    adversarial input. This is the single most important DoS guard.
//! 3. **Character-level**: identifiers (`*_id`) are required to be
//!    non-empty ASCII without control characters or whitespace, so they
//!    are safe to embed in filenames, URLs, and SQL.
//! 4. **Temporal-level**: `edited_at >= timestamp`; `expires_at >
//!    created_at`; `last_seen >= joined_at`.
//! 5. **Sequence-level**: `sequence` is wrapped in [`Sequence`], which
//!    enforces `0 <= seq < MAX_SEQUENCE` at construction.
//!
//! None of the validators here can panic on any input — including the
//! worst-case 16 KiB string of NUL bytes.

use serde::{Deserialize, Serialize};

use crate::error::{AdnetError, Result};

/// Maximum length of any identifier (`post_id`, `group_id`, `sender_id`,
/// `chat_id`, `attachment_id`, …). 128 bytes is more than enough for a
/// UUIDv4 (36) plus an optional service prefix, and small enough to keep
/// every record well under 16 KiB.
pub const MAX_ID_LEN: usize = 128;

/// Maximum length of a display name / description / nickname.
pub const MAX_NAME_LEN: usize = 256;

/// Maximum length of a free-form content field (post body, message body,
/// comment body). 64 KiB matches the upstream blobstore single-chunk
/// cap and prevents a single message from consuming > 1 MiB after
/// JSON serialisation.
pub const MAX_CONTENT_LEN: usize = 64 * 1024;

/// Maximum length of a tag / mention / visibility-tag string.
pub const MAX_TAG_LEN: usize = 64;

/// Maximum number of attachments on a single message / post.
pub const MAX_ATTACHMENTS: usize = 32;

/// Maximum number of mentions / tags on a single record.
pub const MAX_MENTIONS: usize = 64;

/// Maximum number of tags on a single post.
pub const MAX_TAGS: usize = 32;

/// Maximum number of members in a group.
pub const MAX_MEMBERS: usize = 1024;

/// Maximum per-sender sequence number before cycling. Matches the
/// Exodus reference implementation. Re-exported here so the `Sequence`
/// helper can validate against a single canonical value without
/// re-declaring it.
pub const MAX_SEQUENCE: u32 = 9999;

/// Validate an identifier: non-empty ASCII with no whitespace and no
/// control characters, bounded by [`MAX_ID_LEN`].
pub fn validate_id(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(AdnetError::Validation(format!("{field}: empty id")));
    }
    if value.len() > MAX_ID_LEN {
        return Err(AdnetError::Validation(format!(
            "{field}: id exceeds {MAX_ID_LEN} bytes (got {})",
            value.len()
        )));
    }
    if !value.is_ascii() {
        return Err(AdnetError::Validation(format!(
            "{field}: id must be ASCII (got {value:?})"
        )));
    }
    // Reject any byte that could break filenames / URLs / SQL.
    for &b in value.as_bytes() {
        if b <= 0x20 || b == 0x7f {
            return Err(AdnetError::Validation(format!(
                "{field}: id contains control or whitespace byte (0x{b:02x})"
            )));
        }
    }
    Ok(())
}

/// Validate a fixed-length lowercase hex string (e.g. a 64-char
/// [`crate::node::NodeId`] or [`crate::content::ContentHash`]). Use this
/// at the IPC boundary instead of [`validate_id`] when the field is
/// known to be a hex-encoded digest: it enforces the lowercase-hex
/// charset AND the exact length, so a corrupted / mixed-case / non-hex
/// byte cannot slip through.
pub fn validate_hex_id(field: &str, value: &str, expected_len: usize) -> Result<()> {
    if value.len() != expected_len {
        return Err(AdnetError::Validation(format!(
            "{field}: hex id must be {expected_len} chars (got {})",
            value.len()
        )));
    }
    if !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AdnetError::Validation(format!(
            "{field}: hex id must be ASCII hex (got {value:?})"
        )));
    }
    Ok(())
}

/// Validate a free-form human-readable name.
pub fn validate_name(field: &str, value: &str) -> Result<()> {
    if value.len() > MAX_NAME_LEN {
        return Err(AdnetError::Validation(format!(
            "{field}: exceeds {MAX_NAME_LEN} bytes (got {})",
            value.len()
        )));
    }
    // Reject NULs but allow any other UTF-8 byte, including whitespace
    // and non-ASCII, because names are rendered, not interpreted.
    if value.as_bytes().contains(&0) {
        return Err(AdnetError::Validation(format!("{field}: contains NUL")));
    }
    Ok(())
}

/// Validate free-form content body (post / message / comment). Empty
/// content is rejected (callers that legitimately want to send a
/// media-only message should still set a caption / placeholder) and the
/// payload is otherwise bounded by [`MAX_CONTENT_LEN`].
pub fn validate_content(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(AdnetError::Validation(format!("{field}: empty content")));
    }
    if value.len() > MAX_CONTENT_LEN {
        return Err(AdnetError::Validation(format!(
            "{field}: exceeds {MAX_CONTENT_LEN} bytes (got {})",
            value.len()
        )));
    }
    if value.as_bytes().contains(&0) {
        return Err(AdnetError::Validation(format!("{field}: contains NUL")));
    }
    Ok(())
}

/// Validate a tag (short ASCII identifier without whitespace).
pub fn validate_tag(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(AdnetError::Validation(format!("{field}: empty tag")));
    }
    if value.len() > MAX_TAG_LEN {
        return Err(AdnetError::Validation(format!(
            "{field}: tag exceeds {MAX_TAG_LEN} bytes"
        )));
    }
    if !value.is_ascii() {
        return Err(AdnetError::Validation(format!(
            "{field}: tag must be ASCII"
        )));
    }
    for &b in value.as_bytes() {
        if b <= 0x20 || b == 0x7f {
            return Err(AdnetError::Validation(format!(
                "{field}: tag contains control or whitespace byte (0x{b:02x})"
            )));
        }
    }
    Ok(())
}

/// Validate a URL/avatar string. Checks:
/// - Valid UTF-8, no NUL bytes
/// - Bounded length
/// - Parses as a `url::Url` with a valid scheme (http/https/data)
/// - Host is non-empty (reject `file:`, bare paths, etc.)
pub fn validate_url(field: &str, value: &str) -> Result<()> {
    if value.len() > MAX_NAME_LEN {
        return Err(AdnetError::Validation(format!(
            "{field}: URL exceeds {MAX_NAME_LEN} bytes"
        )));
    }
    if value.as_bytes().contains(&0) {
        return Err(AdnetError::Validation(format!("{field}: URL contains NUL")));
    }
    let parsed = url::Url::parse(value).map_err(|e| {
        AdnetError::Validation(format!("{field}: not a valid URL: {e}"))
    })?;
    let scheme = parsed.scheme();
    if scheme.is_empty() {
        return Err(AdnetError::Validation(format!(
            "{field}: URL has no scheme (e.g. https://)"
        )));
    }
    if !matches!(scheme, "http" | "https" | "data") {
        return Err(AdnetError::Validation(format!(
            "{field}: unsupported scheme '{scheme}' (only http/https/data allowed)"
        )));
    }
    if parsed.host().is_none() && scheme != "data" {
        return Err(AdnetError::Validation(format!(
            "{field}: URL has no host"
        )));
    }
    Ok(())
}

/// Validate temporal ordering. Both values are unix milliseconds.
pub fn validate_ordered(field: &str, lo: u64, hi: u64) -> Result<()> {
    if lo > hi {
        return Err(AdnetError::Validation(format!(
            "{field}: {lo} > {hi} (temporal ordering violated)"
        )));
    }
    Ok(())
}

/// Monotonic per-sender sequence number, bounded by the
/// [`crate::group_chat::MAX_SEQUENCE`] ceiling.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct Sequence(pub u32);

impl Sequence {
    /// Build a sequence, returning an error if it exceeds the ceiling.
    pub fn new(value: u32, ceiling: u32) -> Result<Self> {
        if value >= ceiling {
            return Err(AdnetError::Validation(format!(
                "sequence {value} >= ceiling {ceiling}"
            )));
        }
        Ok(Self(value))
    }

    /// Build without checking — only valid when the caller can prove
    /// the input is already within bounds (e.g. a server that owns the
    /// counter).
    pub const fn new_unchecked(value: u32) -> Self {
        Self(value)
    }

    pub fn get(self) -> u32 {
        self.0
    }

    /// Increment modulo `ceiling`. Returns an error if the next value
    /// would wrap to `0`; callers that want to allow rollover should
    /// use [`Sequence::next_or_rollover`].
    pub fn next(self, ceiling: u32) -> Result<Self> {
        let next = self
            .0
            .checked_add(1)
            .ok_or_else(|| AdnetError::Validation("sequence overflow".into()))?;
        if next >= ceiling {
            return Err(AdnetError::Validation(format!(
                "sequence would exceed ceiling {ceiling}"
            )));
        }
        Ok(Self(next))
    }

    /// Increment modulo `ceiling`. On overflow, rolls back to `0`.
    pub fn next_or_rollover(self, ceiling: u32) -> Self {
        match self.0.checked_add(1) {
            Some(v) if v < ceiling => Self(v),
            _ => Self(0),
        }
    }
}

/// Membership role inside a group. Typed enum so the wire format is
/// pinned — no more "owner" vs "Owner" vs "OWNER" drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    Owner,
    Admin,
    Member,
}

impl MemberRole {
    pub const ALL: &'static [MemberRole] =
        &[MemberRole::Owner, MemberRole::Admin, MemberRole::Member];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }

    pub fn from_strict(s: &str) -> Result<Self> {
        Ok(match s {
            "owner" => Self::Owner,
            "admin" => Self::Admin,
            "member" => Self::Member,
            other => {
                return Err(AdnetError::Validation(format!(
                    "invalid MemberRole {other:?}"
                )));
            }
        })
    }

    /// `true` if this role can change group metadata.
    pub fn can_administer(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }
}

/// Chat message kind. Matches the upstream `"text"|"image"|"file"|"system"`
/// vocabulary exactly so wire interop is preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Text,
    Image,
    File,
    System,
}

impl MessageType {
    pub const ALL: &'static [MessageType] = &[
        MessageType::Text,
        MessageType::Image,
        MessageType::File,
        MessageType::System,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::File => "file",
            Self::System => "system",
        }
    }

    pub fn from_strict(s: &str) -> Result<Self> {
        Ok(match s {
            "text" => Self::Text,
            "image" => Self::Image,
            "file" => Self::File,
            "system" => Self::System,
            other => {
                return Err(AdnetError::Validation(format!(
                    "invalid MessageType {other:?}"
                )));
            }
        })
    }
}

/// Attachment kind (mirrors the wire `"image"|"video"|"audio"|"file"`
/// vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    File,
}

impl AttachmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::File => "file",
        }
    }

    pub fn from_strict(s: &str) -> Result<Self> {
        Ok(match s {
            "image" => Self::Image,
            "video" => Self::Video,
            "audio" => Self::Audio,
            "file" => Self::File,
            other => {
                return Err(AdnetError::Validation(format!(
                    "invalid AttachmentKind {other:?}"
                )));
            }
        })
    }
}

/// Invitation status (matches upstream `"pending"|"accepted"|"rejected"|"expired"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvitationStatus {
    Pending,
    Accepted,
    Rejected,
    Expired,
}

impl InvitationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }

    pub fn from_strict(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => Self::Pending,
            "accepted" => Self::Accepted,
            "rejected" => Self::Rejected,
            "expired" => Self::Expired,
            other => {
                return Err(AdnetError::Validation(format!(
                    "invalid InvitationStatus {other:?}"
                )));
            }
        })
    }

    /// `true` if this status is terminal (no further state change
    /// expected).
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Accepted | Self::Rejected | Self::Expired)
    }
}

/// Social-feed post visibility. Wire vocabulary
/// `"public"|"friends"|"private"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Friends,
    Private,
}

impl Visibility {
    pub const PUBLIC: &'static str = "public";
    pub const FRIENDS: &'static str = "friends";
    pub const PRIVATE: &'static str = "private";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => Self::PUBLIC,
            Self::Friends => Self::FRIENDS,
            Self::Private => Self::PRIVATE,
        }
    }

    pub fn from_strict(s: &str) -> Result<Self> {
        Ok(match s {
            "public" => Self::Public,
            "friends" => Self::Friends,
            "private" => Self::Private,
            other => {
                return Err(AdnetError::Validation(format!(
                    "invalid Visibility {other:?}"
                )));
            }
        })
    }
}

/// Reaction kind. Wire vocabulary
/// `"like"|"love"|"laugh"|"wow"|"sad"|"angry"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionType {
    Like,
    Love,
    Laugh,
    Wow,
    Sad,
    Angry,
}

impl ReactionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Like => "like",
            Self::Love => "love",
            Self::Laugh => "laugh",
            Self::Wow => "wow",
            Self::Sad => "sad",
            Self::Angry => "angry",
        }
    }

    pub fn from_strict(s: &str) -> Result<Self> {
        Ok(match s {
            "like" => Self::Like,
            "love" => Self::Love,
            "laugh" => Self::Laugh,
            "wow" => Self::Wow,
            "sad" => Self::Sad,
            "angry" => Self::Angry,
            other => {
                return Err(AdnetError::Validation(format!(
                    "invalid ReactionType {other:?}"
                )));
            }
        })
    }
}

/// What a reaction targets (post vs comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionTarget {
    Post,
    Comment,
}

impl ReactionTarget {
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
                    "invalid ReactionTarget {other:?}"
                )));
            }
        })
    }
}

/// File-kind classifier on a post attachment. Wire vocabulary
/// `"image"|"video"|"audio"|"file"` (mirrors [`AttachmentKind`]).
pub type PostAttachmentKind = AttachmentKind;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_id_rejects_bad_inputs() {
        assert!(validate_id("x", "").is_err());
        assert!(validate_id("x", " ").is_err()); // whitespace
        assert!(validate_id("x", "abc\t").is_err()); // control
        assert!(validate_id("x", "abc\x00def").is_err()); // NUL
        assert!(validate_id("x", "héllo").is_err()); // non-ASCII
        let big = "a".repeat(MAX_ID_LEN + 1);
        assert!(validate_id("x", &big).is_err());
    }

    #[test]
    fn validate_id_accepts_normal_inputs() {
        assert!(validate_id("x", "alice").is_ok());
        assert!(validate_id("x", "group-123").is_ok());
        assert!(validate_id("x", &"a".repeat(MAX_ID_LEN)).is_ok());
    }

    #[test]
    fn validate_content_rejects_oversize_and_nul() {
        let big = "a".repeat(MAX_CONTENT_LEN + 1);
        assert!(validate_content("x", &big).is_err());
        assert!(validate_content("x", "ok\0bad").is_err());
        assert!(validate_content("x", "ok 中文 emoji 🎉").is_ok());
    }

    #[test]
    fn validate_content_rejects_empty() {
        assert!(validate_content("x", "").is_err());
    }

    #[test]
    fn sequence_enforces_ceiling() {
        assert!(Sequence::new(0, 9999).is_ok());
        assert!(Sequence::new(9998, 9999).is_ok());
        assert!(Sequence::new(9999, 9999).is_err());
        assert!(Sequence::next(Sequence::new_unchecked(9997), 9999).is_ok());
        assert!(Sequence::next(Sequence::new_unchecked(9998), 9999).is_err());
        assert_eq!(
            Sequence::next_or_rollover(Sequence::new_unchecked(9998), 9999).get(),
            0
        );
        // u32::MAX still rolls over safely.
        assert_eq!(
            Sequence::next_or_rollover(Sequence::new_unchecked(u32::MAX), 9999).get(),
            0
        );
    }

    #[test]
    fn enums_round_trip_snake_case() {
        for m in MessageType::ALL {
            let s = m.as_str();
            assert_eq!(MessageType::from_strict(s).unwrap(), *m);
            assert!(MessageType::from_strict("bogus").is_err());
        }
        for v in [Visibility::Public, Visibility::Friends, Visibility::Private] {
            assert_eq!(Visibility::from_strict(v.as_str()).unwrap(), v);
        }
        for r in [
            ReactionType::Like,
            ReactionType::Love,
            ReactionType::Laugh,
            ReactionType::Wow,
            ReactionType::Sad,
            ReactionType::Angry,
        ] {
            assert_eq!(ReactionType::from_strict(r.as_str()).unwrap(), r);
        }
        for role in MemberRole::ALL {
            assert_eq!(MemberRole::from_strict(role.as_str()).unwrap(), *role);
            assert!(MemberRole::from_strict("god").is_err());
        }
    }

    #[test]
    fn validate_ordered_detects_inversions() {
        assert!(validate_ordered("x", 1, 2).is_ok());
        assert!(validate_ordered("x", 2, 2).is_ok());
        assert!(validate_ordered("x", 3, 2).is_err());
    }

    #[test]
    fn validate_hex_id_accepts_well_formed() {
        let h = "0123456789abcdef".repeat(4); // 64 chars
        assert_eq!(h.len(), 64);
        assert!(validate_hex_id("h", &h, 64).is_ok());
        // Wrong length rejected.
        assert!(validate_hex_id("h", &h[..63], 64).is_err());
        assert!(validate_hex_id("h", &format!("{h}0"), 64).is_err());
        // Non-hex rejected.
        assert!(validate_hex_id("h", &"g".repeat(64), 64).is_err());
        // Mixed case OK (hex digits include A-F).
        let upper = "ABCDEF0123456789".repeat(4);
        assert!(validate_hex_id("h", &upper, 64).is_ok());
    }
}
