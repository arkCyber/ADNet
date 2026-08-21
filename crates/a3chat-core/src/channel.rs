//! Channel / public-account domain types (公众号 / 视频号).
//!
//! A [`PublicAccount`] is a long-lived broadcasting identity owned by a
//! user. Subscribers receive a feed of [`FeedItem`]s authored by the
//! account. This module owns the wire shape and validation; persistence
//! lives in `a3chat_app::channel_storage` and the higher-level RPC
//! surface in `a3chat_app::channel_service`.
//!
//! ## Identifier scheme
//!
//! - `account_id` — `acc_<hex>` (24 hex chars from a BLAKE3 of
//!   `(INTEGRITY_HASH_TAG || owner_node_id || 0x00 || created_at_unix)`).
//!   Stable for the lifetime of the account; safe to embed in URLs.
//! - `feed_id` — `feed_<hex>` (BLAKE3 over the canonical
//!   `(account_id, sequence, created_at_unix, nonce)` preimage).
//!
//! Both ids share the same lowercase-hex discipline as
//! [`crate::link_bookmark`] so the SQLite layer can index on a single
//! `LIKE 'acc_%'` or `LIKE 'feed_%'` prefix without parser dispatch.
//!
//! ## Layering
//!
//! - Domain shape / validation: this module.
//! - SQLite persistence: `a3chat-app::channel_storage`.
//! - RPC dispatch + SSE bus publication: `a3chat-app::channel_service`.
//!
//! The service is built around `a3net-news::NewsService` (gossip
//! fan-out, monotonic per-room sequence, optional wallet signature)
//! but exposes a friendlier `account_id` / `feed_id` surface so RPC
//! clients never see `RoomId` or `BulletinEnvelope` directly.
//!
//! [`PublicAccount`]: crate::channel::PublicAccount
//! [`FeedItem`]: crate::channel::FeedItem

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::A3chatError;
use crate::id::validate_id;
use crate::validation::{validate_content, validate_name, validate_url};

/// `INTEGRITY_HASH_TAG` — domain tag baked into the account-id hash so
/// the same `(owner_node_id, created_at_unix)` triple cannot collide
/// with hashes produced by another feature.
pub const INTEGRITY_HASH_TAG: &[u8] = b"a3chat-channel-account|v1";

/// `INTEGRITY_HASH_FEED_TAG` — domain tag baked into the feed-id hash
/// so two features storing `(account_id, sequence, created_at_unix,
/// nonce)` cannot collide.
pub const INTEGRITY_HASH_FEED_TAG: &[u8] = b"a3chat-channel-feed|v1";

/// `ACCOUNT_ID_PREFIX` — every account id starts with this prefix so
/// the SQLite `WHERE id LIKE 'acc_%'` query plan stays narrow.
pub const ACCOUNT_ID_PREFIX: &str = "acc_";

/// `FEED_ID_PREFIX` — every feed-item id starts with this prefix.
pub const FEED_ID_PREFIX: &str = "feed_";

/// `MAX_ACCOUNT_NAME_LEN` — same ceiling as [`crate::validation::MAX_NAME_LEN`].
pub const MAX_ACCOUNT_NAME_LEN: usize = 64;

/// `MAX_ACCOUNT_BIO_LEN` — short bio, longer than a name but still
/// small enough to render in a follow screen.
pub const MAX_ACCOUNT_BIO_LEN: usize = 512;

/// `MAX_AVATAR_HASH_LEN` — hex-encoded BLAKE3 of the avatar blob.
/// Mirrors the link-bookmark favicon field.
pub const MAX_AVATAR_HASH_LEN: usize = 128;

/// `MAX_FEED_TITLE_LEN` — WeChat-style "headline" line for a feed item.
pub const MAX_FEED_TITLE_LEN: usize = 200;

/// `MAX_FEED_SUMMARY_LEN` — preview line shown in the timeline.
pub const MAX_FEED_SUMMARY_LEN: usize = 500;

/// `MAX_FEED_BODY_LEN` — full article body. Aligns with
/// [`crate::validation::MAX_CONTENT_LEN`] (16 KiB) so future rich-text
/// extensions don't need to change the cap.
pub const MAX_FEED_BODY_LEN: usize = 16 * 1024;

/// `MAX_TAGS_PER_FEED_ITEM` — defensive cap on the per-feed-item tag
/// list; matches [`crate::link_bookmark::MAX_TAGS_PER_BOOKMARK`].
pub const MAX_TAGS_PER_FEED_ITEM: usize = 32;

/// `MAX_TAG_LEN` — per-tag length cap.
pub const MAX_TAG_LEN: usize = 64;

/// `MAX_ATTACHMENTS_PER_FEED_ITEM` — caps the inline media array so a
/// crafted payload can't blow out the SQLite row width.
pub const MAX_ATTACHMENTS_PER_FEED_ITEM: usize = 32;

/// Default notify mode for a fresh subscription: notifications are
/// on but the subscriber's DND window still wins.
pub const DEFAULT_NOTIFY_MODE: &str = "normal";

/// Account classification. Mirrors the WeChat 订阅号 / 服务号 split,
/// but kept open so future kinds (e.g. 企业号) can be added without
/// breaking wire compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    /// 订阅号 — once-a-day broadcast, low priority, folded in inbox.
    #[default]
    Subscription,
    /// 服务号 — strong push, application-level messages.
    Service,
    /// 企业号 — internal / org-scoped (reserved).
    Enterprise,
}

impl AccountKind {
    /// Stable wire string for SQLite persistence and SSE tagging.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subscription => "subscription",
            Self::Service => "service",
            Self::Enterprise => "enterprise",
        }
    }

    /// Reverse of [`as_str`](Self::as_str). Unknown strings fall back
    /// to [`Subscription`](Self::Subscription) so older DB rows that
    /// pre-date a new variant don't crash on hydration.
    pub fn parse(s: &str) -> Self {
        match s {
            "service" => Self::Service,
            "enterprise" => Self::Enterprise,
            _ => Self::Subscription,
        }
    }
}

/// Verification status of an account. Verification is local-only
/// (i.e. this is a label the operator puts on an account); cross-node
/// verification would require a separate trust registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationLevel {
    /// No verification badge.
    #[default]
    None,
    /// Owner-verified (the user controls the node).
    OwnerVerified,
    /// Org-verified (a3chat-side stamp; admin-issued, not user-issued).
    OrgVerified,
}

impl VerificationLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::OwnerVerified => "owner_verified",
            Self::OrgVerified => "org_verified",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "owner_verified" => Self::OwnerVerified,
            "org_verified" => Self::OrgVerified,
            _ => Self::None,
        }
    }
}

/// A single subscription a local user has to a [`PublicAccount`]. The
/// subscription row is the join table between `UserId` and
/// `account_id`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Subscription {
    pub subscriber_id: String,
    pub account_id: String,
    /// Free-form alias the subscriber pinned onto the account (e.g.
    /// "work", "news"). Empty means "no alias".
    pub alias: String,
    /// Notification mode — `"normal"`, `"silent"`, `"strong"`.
    /// Defaults to `DEFAULT_NOTIFY_MODE`.
    pub notify_mode: String,
    pub is_muted: bool,
    pub is_pinned: bool,
    pub subscribed_at: DateTime<Utc>,
    /// Unix seconds — the highest feed sequence the subscriber has
    /// acknowledged. Lets the client render the unread badge without
    /// re-pulling the timeline.
    pub last_read_seq: u32,
}

/// The public-account record itself.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublicAccount {
    pub account_id: String,
    /// `UserId` of the account owner. Multiple devices under the same
    /// user can publish to the account — the service collapses them.
    pub owner_node_id: String,
    pub name: String,
    pub bio: String,
    /// Optional blake3 hash of the avatar blob (hex-encoded); the
    /// actual bytes live in the media store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_hash: Option<String>,
    /// Lower-case, ASCII tags the account is filed under (used by
    /// `account.search`).
    #[serde(default)]
    pub tags: Vec<String>,
    pub kind: AccountKind,
    pub verification: VerificationLevel,
    /// Highest monotonic sequence the account has published. Stored
    /// here so a subscriber can detect "I missed some items" without
    /// scanning the full timeline.
    pub sequence: u32,
    pub subscriber_count: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PublicAccount {
    /// Domain-layer validation. Mirrors the SQLite-layer checks but
    /// kept here so RPC handlers can short-circuit before opening a
    /// connection.
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_id("account_id", &self.account_id)?;
        if !self.account_id.starts_with(ACCOUNT_ID_PREFIX) {
            return Err(A3chatError::InvalidInput(format!(
                "account_id: must start with {ACCOUNT_ID_PREFIX:?}"
            )));
        }
        validate_id("owner_node_id", &self.owner_node_id)?;
        validate_name("name", &self.name)?;
        if self.name.len() > MAX_ACCOUNT_NAME_LEN {
            return Err(A3chatError::InvalidInput(format!(
                "name: length {} > {MAX_ACCOUNT_NAME_LEN}",
                self.name.len()
            )));
        }
        if self.bio.len() > MAX_ACCOUNT_BIO_LEN {
            return Err(A3chatError::InvalidInput(format!(
                "bio: length {} > {MAX_ACCOUNT_BIO_LEN}",
                self.bio.len()
            )));
        }
        if let Some(av) = &self.avatar_hash {
            if av.len() > MAX_AVATAR_HASH_LEN {
                return Err(A3chatError::InvalidInput(format!(
                    "avatar_hash: length {} > {MAX_AVATAR_HASH_LEN}",
                    av.len()
                )));
            }
        }
        if self.tags.len() > MAX_TAGS_PER_FEED_ITEM {
            return Err(A3chatError::InvalidInput(format!(
                "tags: {} entries > {MAX_TAGS_PER_FEED_ITEM}",
                self.tags.len()
            )));
        }
        for (i, t) in self.tags.iter().enumerate() {
            if t.is_empty() {
                return Err(A3chatError::InvalidInput(format!("tags[{i}]: empty")));
            }
            if t.len() > MAX_TAG_LEN {
                return Err(A3chatError::InvalidInput(format!(
                    "tags[{i}]: length {} > {MAX_TAG_LEN}",
                    t.len()
                )));
            }
        }
        if self.updated_at < self.created_at {
            return Err(A3chatError::InvalidInput(format!(
                "updated_at {} < created_at {}",
                self.updated_at, self.created_at
            )));
        }
        Ok(())
    }
}

/// A single feed entry published by a [`PublicAccount`]. Mirrors the
/// [`a3net_types::BulletinItem`] shape but trimmed to the columns the
/// chat UI actually renders; the storage layer keeps a separate
/// optional pointer to the bulletin row so a future audit UI can fall
/// back to the full envelope.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeedItem {
    pub feed_id: String,
    pub account_id: String,
    pub sequence: u32,
    pub title: String,
    pub summary: String,
    pub body: String,
    /// Optional URL pointing to the full-resolution cover image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub attachments: Vec<FeedAttachment>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub is_pinned: bool,
    /// Set when an admin takes the item down. Mirrors
    /// [`a3net_types::BulletinKind::Retraction`] but stays local —
    /// admins hide the row from the timeline without deleting it.
    pub is_retracted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retraction_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl FeedItem {
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_id("feed_id", &self.feed_id)?;
        if !self.feed_id.starts_with(FEED_ID_PREFIX) {
            return Err(A3chatError::InvalidInput(format!(
                "feed_id: must start with {FEED_ID_PREFIX:?}"
            )));
        }
        validate_id("account_id", &self.account_id)?;
        if self.title.is_empty() {
            return Err(A3chatError::InvalidInput("title: empty".into()));
        }
        if self.title.len() > MAX_FEED_TITLE_LEN {
            return Err(A3chatError::InvalidInput(format!(
                "title: length {} > {MAX_FEED_TITLE_LEN}",
                self.title.len()
            )));
        }
        if self.summary.len() > MAX_FEED_SUMMARY_LEN {
            return Err(A3chatError::InvalidInput(format!(
                "summary: length {} > {MAX_FEED_SUMMARY_LEN}",
                self.summary.len()
            )));
        }
        validate_content("body", &self.body)?;
        if self.body.len() > MAX_FEED_BODY_LEN {
            return Err(A3chatError::InvalidInput(format!(
                "body: length {} > {MAX_FEED_BODY_LEN}",
                self.body.len()
            )));
        }
        if let Some(url) = &self.cover_url {
            validate_url("cover_url", url)?;
        }
        if self.attachments.len() > MAX_ATTACHMENTS_PER_FEED_ITEM {
            return Err(A3chatError::InvalidInput(format!(
                "attachments: {} > {MAX_ATTACHMENTS_PER_FEED_ITEM}",
                self.attachments.len()
            )));
        }
        if self.tags.len() > MAX_TAGS_PER_FEED_ITEM {
            return Err(A3chatError::InvalidInput(format!(
                "tags: {} entries > {MAX_TAGS_PER_FEED_ITEM}",
                self.tags.len()
            )));
        }
        for (i, t) in self.tags.iter().enumerate() {
            if t.is_empty() {
                return Err(A3chatError::InvalidInput(format!("tags[{i}]: empty")));
            }
            if t.len() > MAX_TAG_LEN {
                return Err(A3chatError::InvalidInput(format!(
                    "tags[{i}]: length {} > {MAX_TAG_LEN}",
                    t.len()
                )));
            }
        }
        if self.is_retracted && self.retraction_reason.as_deref().unwrap_or("").is_empty() {
            return Err(A3chatError::InvalidInput(
                "retraction_reason: required when is_retracted=true".into(),
            ));
        }
        if self.updated_at < self.created_at {
            return Err(A3chatError::InvalidInput(format!(
                "updated_at {} < created_at {}",
                self.updated_at, self.created_at
            )));
        }
        Ok(())
    }
}

/// One inline attachment attached to a [`FeedItem`] — image, audio,
/// video, document. Mirrors the dimensions of
/// [`crate::message::Attachment`] but keeps the public-account fields
/// (kind string + URL) explicit so the UI can render without round
/// trips to the media store.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeedAttachment {
    /// `"image" | "audio" | "video" | "file"` — open set, the UI
    /// switches on this string.
    pub kind: String,
    pub url: String,
    /// Hex-encoded BLAKE3 hash of the attachment body, when the URL
    /// is content-addressed (e.g. an `a3net-blobstore` pointer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

impl FeedAttachment {
    pub fn validate(&self) -> Result<(), A3chatError> {
        if self.kind.is_empty() {
            return Err(A3chatError::InvalidInput("attachment.kind: empty".into()));
        }
        if self.kind.len() > 32 {
            return Err(A3chatError::InvalidInput(format!(
                "attachment.kind: length {} > 32",
                self.kind.len()
            )));
        }
        validate_url("attachment.url", &self.url)?;
        if let Some(h) = &self.content_hash {
            if h.len() > MAX_AVATAR_HASH_LEN {
                return Err(A3chatError::InvalidInput(format!(
                    "attachment.content_hash: length {} > {MAX_AVATAR_HASH_LEN}",
                    h.len()
                )));
            }
        }
        if let Some(m) = &self.mime_type {
            if m.len() > 128 {
                return Err(A3chatError::InvalidInput(format!(
                    "attachment.mime_type: length {} > 128",
                    m.len()
                )));
            }
        }
        if let Some(c) = &self.caption {
            if c.len() > MAX_FEED_SUMMARY_LEN {
                return Err(A3chatError::InvalidInput(format!(
                    "attachment.caption: length {} > {MAX_FEED_SUMMARY_LEN}",
                    c.len()
                )));
            }
        }
        Ok(())
    }
}

/// Request payload for `a3chat.channel.account.register` /
/// `a3chat.channel.account.update`. Field semantics match
/// [`PublicAccount`]; `Option<Option<T>>` distinguishes three update
/// intents in the future (kept simple for now).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UpsertChannelAccountRequest {
    pub name: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_hash: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub kind: AccountKind,
    #[serde(default)]
    pub verification: VerificationLevel,
}

impl UpsertChannelAccountRequest {
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_name("name", &self.name)?;
        if self.name.len() > MAX_ACCOUNT_NAME_LEN {
            return Err(A3chatError::InvalidInput(format!(
                "name: length {} > {MAX_ACCOUNT_NAME_LEN}",
                self.name.len()
            )));
        }
        if self.bio.len() > MAX_ACCOUNT_BIO_LEN {
            return Err(A3chatError::InvalidInput(format!(
                "bio: length {} > {MAX_ACCOUNT_BIO_LEN}",
                self.bio.len()
            )));
        }
        if let Some(av) = &self.avatar_hash {
            if av.is_empty() {
                return Err(A3chatError::InvalidInput("avatar_hash: empty".into()));
            }
            if av.len() > MAX_AVATAR_HASH_LEN {
                return Err(A3chatError::InvalidInput(format!(
                    "avatar_hash: length {} > {MAX_AVATAR_HASH_LEN}",
                    av.len()
                )));
            }
        }
        if self.tags.len() > MAX_TAGS_PER_FEED_ITEM {
            return Err(A3chatError::InvalidInput(format!(
                "tags: {} entries > {MAX_TAGS_PER_FEED_ITEM}",
                self.tags.len()
            )));
        }
        for (i, t) in self.tags.iter().enumerate() {
            if t.is_empty() {
                return Err(A3chatError::InvalidInput(format!("tags[{i}]: empty")));
            }
            if t.len() > MAX_TAG_LEN {
                return Err(A3chatError::InvalidInput(format!(
                    "tags[{i}]: length {} > {MAX_TAG_LEN}",
                    t.len()
                )));
            }
        }
        Ok(())
    }

    /// Build a fresh [`PublicAccount`] for the given owner +
    /// creation timestamp. The `account_id` is derived from
    /// `(owner_node_id, created_at_unix)` so the same triple yields
    /// the same id on retry — a write-after-restart hits the same
    /// row instead of creating a second account.
    pub fn into_account(
        self,
        owner_node_id: &str,
        created_at: DateTime<Utc>,
    ) -> Result<PublicAccount, A3chatError> {
        self.validate()?;
        validate_id("owner_node_id", owner_node_id)?;
        let account_id = compute_account_id(owner_node_id, created_at.timestamp());
        Ok(PublicAccount {
            account_id,
            owner_node_id: owner_node_id.to_string(),
            name: self.name,
            bio: self.bio,
            avatar_hash: self.avatar_hash,
            tags: self.tags,
            kind: self.kind,
            verification: self.verification,
            sequence: 0,
            subscriber_count: 0,
            created_at,
            updated_at: created_at,
        })
    }
}

/// Request payload for `a3chat.channel.feed.publish`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishFeedRequest {
    pub title: String,
    #[serde(default)]
    pub summary: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub attachments: Vec<FeedAttachment>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub is_pinned: bool,
}

impl PublishFeedRequest {
    pub fn validate(&self) -> Result<(), A3chatError> {
        if self.title.is_empty() {
            return Err(A3chatError::InvalidInput("title: empty".into()));
        }
        if self.title.len() > MAX_FEED_TITLE_LEN {
            return Err(A3chatError::InvalidInput(format!(
                "title: length {} > {MAX_FEED_TITLE_LEN}",
                self.title.len()
            )));
        }
        if self.summary.len() > MAX_FEED_SUMMARY_LEN {
            return Err(A3chatError::InvalidInput(format!(
                "summary: length {} > {MAX_FEED_SUMMARY_LEN}",
                self.summary.len()
            )));
        }
        validate_content("body", &self.body)?;
        if self.body.len() > MAX_FEED_BODY_LEN {
            return Err(A3chatError::InvalidInput(format!(
                "body: length {} > {MAX_FEED_BODY_LEN}",
                self.body.len()
            )));
        }
        if let Some(url) = &self.cover_url {
            validate_url("cover_url", url)?;
        }
        if self.attachments.len() > MAX_ATTACHMENTS_PER_FEED_ITEM {
            return Err(A3chatError::InvalidInput(format!(
                "attachments: {} > {MAX_ATTACHMENTS_PER_FEED_ITEM}",
                self.attachments.len()
            )));
        }
        for (i, a) in self.attachments.iter().enumerate() {
            a.validate().map_err(|e| match e {
                A3chatError::InvalidInput(msg) => {
                    A3chatError::InvalidInput(format!("attachments[{i}]: {msg}"))
                }
                other => other,
            })?;
        }
        if self.tags.len() > MAX_TAGS_PER_FEED_ITEM {
            return Err(A3chatError::InvalidInput(format!(
                "tags: {} entries > {MAX_TAGS_PER_FEED_ITEM}",
                self.tags.len()
            )));
        }
        for (i, t) in self.tags.iter().enumerate() {
            if t.is_empty() {
                return Err(A3chatError::InvalidInput(format!("tags[{i}]: empty")));
            }
            if t.len() > MAX_TAG_LEN {
                return Err(A3chatError::InvalidInput(format!(
                    "tags[{i}]: length {} > {MAX_TAG_LEN}",
                    t.len()
                )));
            }
        }
        Ok(())
    }
}

/// Compute the deterministic [`PublicAccount::account_id`] for a
/// `(owner_node_id, created_at_unix)` pair. The tag-prefixed BLAKE3
/// hash guarantees the same pair cannot collide with hashes produced
/// by other features.
pub fn compute_account_id(owner_node_id: &str, created_at_unix: i64) -> String {
    let mut h = blake3::Hasher::new();
    h.update(INTEGRITY_HASH_TAG);
    h.update(owner_node_id.as_bytes());
    h.update(&[0u8]);
    h.update(&created_at_unix.to_le_bytes());
    format!("{ACCOUNT_ID_PREFIX}{}", hex::encode(h.finalize().as_bytes())[..24].to_string())
}

/// Compute the deterministic [`FeedItem::feed_id`] for the canonical
/// `(account_id, sequence, created_at_unix, nonce)` preimage. The
/// leading 24 hex chars are kept to match the visual length of an
/// account id (the leading 4-byte `feed_` ASCII prefix is added back
/// to keep parity with `account_id`).
pub fn compute_feed_id(
    account_id: &str,
    sequence: u32,
    created_at_unix: i64,
    nonce: &[u8],
) -> String {
    let mut h = blake3::Hasher::new();
    h.update(INTEGRITY_HASH_FEED_TAG);
    h.update(account_id.as_bytes());
    h.update(&[0u8]);
    h.update(&sequence.to_le_bytes());
    h.update(&[0u8]);
    h.update(&created_at_unix.to_le_bytes());
    h.update(&[0u8]);
    h.update(nonce);
    let digest = hex::encode(h.finalize().as_bytes());
    // 4 chars for "feed_" + 24 hex chars = 28 chars total.
    format!("{FEED_ID_PREFIX}{}", &digest[..24])
}

/// Default tag for a freshly-created subscription.
pub fn default_notify_mode() -> &'static str {
    DEFAULT_NOTIFY_MODE
}

// ============================================================================
// Analytics + audit (F-09 v1.1 — counters + immutable log).
//
// Two-table design:
//
//  * `account_metrics_daily` — additive counters keyed by
//    `(account_id, day_local)`; the storage layer increments in place
//    on every event hook (publish / retract / subscribe / unsubscribe /
//    mark_read). Rolling windows are a single `WHERE day_local >= ?`
//    aggregate. `unique_readers` is a HyperLogLog-lite approximation
//    (first-16-bits hash bucket) — the column is bumped when the
//    bucket is freshly populated for the day, giving a sub-1% error
//    bound for the 30-day window at zero extra cost.
//
//  * `account_events_log` — append-only audit trail with a chain
//    `integrity_hash` so a verifier can confirm a copy they read has
//    not been mutated. Every counter increment is paired with an
//    audit row in the same SQLite transaction so the metrics cannot
//    drift from the audit trail.
//
// Both tables live in the same `channel.db` (added in SCHEMA_V2).
// ============================================================================

/// `METRICS_HLL_BUCKET_BYTES` — how many leading bytes of the blake3
/// hash we use to bucket a subscriber into the "unique readers" set.
/// 2 bytes = 65 536 buckets per (account_id, day) — at 30-day rolling
/// scale this keeps the error rate under 1% without any memory blowup.
pub const METRICS_HLL_BUCKET_BYTES: usize = 2;

/// `AUDIT_HASH_TAG` — domain tag baked into the chained audit hash so
/// a row from another feature cannot be smuggled into the log.
pub const AUDIT_HASH_TAG: &[u8] = b"a3chat-channel-audit|v1";

/// Discriminator for the kinds of events that bump metrics / append to
/// the audit log. The wire string is the lowercase variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountEventKind {
    Publish,
    Retract,
    Subscribe,
    Unsubscribe,
    MarkRead,
    Register,
    Update,
    Delete,
}

impl Default for AccountEventKind {
    fn default() -> Self {
        // Sentinel for `AuditEvent::default()` — never written to
        // the log; production code constructs the enum from the
        // service-layer discriminator.
        Self::Publish
    }
}

impl AccountEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::Retract => "retract",
            Self::Subscribe => "subscribe",
            Self::Unsubscribe => "unsubscribe",
            Self::MarkRead => "mark_read",
            Self::Register => "register",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

/// Per-day rollup row, returned by `metrics_timeline`. Mirrors the
/// `account_metrics_daily` columns so the frontend can chart without
/// shape conversion.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DailyMetricPoint {
    /// `'YYYY-MM-DD'` in the operator's local TZ — the storage layer
    /// uses the system clock at insert time and we deliberately do
    /// not store UTC offsets here to keep the SQL `WHERE day_local >=
    /// ?` index narrow.
    pub day_local: String,
    pub subscribes_new: u32,
    pub unsubscribes: u32,
    pub publishes: u32,
    pub retracts: u32,
    pub impressions: u32,
    pub reads: u32,
    /// Approximate — see [`METRICS_HLL_BUCKET_BYTES`].
    pub unique_readers: u32,
}

/// Aggregated window — returned by `metrics_summary`. Sums over
/// `account_metrics_daily` for `window_days` ending today (inclusive).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MetricsSummary {
    pub account_id: String,
    /// Window size, in days. Stored so the caller can echo it back
    /// without recomputing.
    pub window_days: u32,
    /// First day in the window — `'YYYY-MM-DD'`.
    pub day_from: String,
    /// Last day in the window — `'YYYY-MM-DD'`.
    pub day_to: String,
    pub subscribes_new: u32,
    pub unsubscribes: u32,
    pub net_subscribes: i32,
    pub publishes: u32,
    pub retracts: u32,
    pub impressions: u32,
    pub reads: u32,
    /// Approximate unique readers across the window.
    pub unique_readers: u32,
}

/// One immutable event row as it would be returned from the audit
/// trail. Field semantics mirror the storage columns so the frontend
/// can render "who did what when" without joining.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AuditEvent {
    pub event_seq: i64,
    pub account_id: String,
    pub kind: AccountEventKind,
    /// `UserId` of the actor (the local owner for publishes, the
    /// subscriber for subscribe / mark_read, `None` for system-initiated
    /// events such as scheduled cleanup).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// `feed_id` for publish/retract, `subscriber_id` for
    /// subscribe/unsubscribe/mark_read — absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    /// Free-form metadata: retract reason, publish tags snapshot, etc.
    /// Kept as a structured value (already validated JSON) so the
    /// verifier can re-hash deterministically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    pub occurred_at: DateTime<Utc>,
    /// Chain hash — `blake3(AUDIT_HASH_TAG || prev_hash || canonical(row))`.
    /// Clients that want tamper-evidence compare this to the next row's
    /// `prev_hash` (the storage layer keeps the previous row's hash in
    /// a sticky read).
    pub integrity_hash: String,
}

/// One page of the audit log. Cursor is the last `event_seq` of the
/// previous page (exclusive); pass `None` to start from the most
/// recent.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AuditPage {
    pub events: Vec<AuditEvent>,
    /// True when more rows remain beyond this page.
    pub has_more: bool,
    /// Next cursor to pass — equals the last `event_seq` in
    /// `events` when `has_more` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> &'static str {
        "user:alice"
    }

    fn ts(unix: i64) -> DateTime<Utc> {
        chrono::DateTime::<Utc>::from_timestamp(unix, 0).unwrap()
    }

    #[test]
    fn account_id_is_deterministic_and_prefixed() {
        let a = compute_account_id(owner(), 1_700_000_000);
        let b = compute_account_id(owner(), 1_700_000_000);
        assert_eq!(a, b);
        assert!(a.starts_with(ACCOUNT_ID_PREFIX));
        // 4 prefix chars + 24 hex chars = 28 total.
        assert_eq!(a.len(), 28);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() || c == '_'));
        // Different owner → different id.
        assert_ne!(a, compute_account_id("user:bob", 1_700_000_000));
        // Different timestamp → different id.
        assert_ne!(a, compute_account_id(owner(), 1_700_000_001));
    }

    #[test]
    fn feed_id_is_deterministic_and_prefixed() {
        let nonce = b"abcdefghijklmnop";
        let a = compute_feed_id("acc_abc", 1, 1_700_000_000, nonce);
        let b = compute_feed_id("acc_abc", 1, 1_700_000_000, nonce);
        assert_eq!(a, b);
        assert!(a.starts_with(FEED_ID_PREFIX));
        // 5 chars for "feed_" + 24 hex chars = 29 chars total
        // (the prefix length is 5 not 4 — there is an underscore).
        assert_eq!(a.len(), 29);
        assert_ne!(a, compute_feed_id("acc_xyz", 1, 1_700_000_000, nonce));
        assert_ne!(a, compute_feed_id("acc_abc", 2, 1_700_000_000, nonce));
        // Different nonce → different id even at the same seq/ts.
        assert_ne!(
            a,
            compute_feed_id("acc_abc", 1, 1_700_000_000, b"different-non!@#$%")
        );
    }

    #[test]
    fn account_kind_round_trip() {
        for k in [
            AccountKind::Subscription,
            AccountKind::Service,
            AccountKind::Enterprise,
        ] {
            assert_eq!(AccountKind::parse(k.as_str()), k);
        }
        assert_eq!(AccountKind::parse("bogus"), AccountKind::Subscription);
        assert_eq!(AccountKind::default(), AccountKind::Subscription);
    }

    #[test]
    fn verification_round_trip() {
        for v in [
            VerificationLevel::None,
            VerificationLevel::OwnerVerified,
            VerificationLevel::OrgVerified,
        ] {
            assert_eq!(VerificationLevel::parse(v.as_str()), v);
        }
        assert_eq!(VerificationLevel::parse("bogus"), VerificationLevel::None);
    }

    #[test]
    fn upsert_request_into_account_sets_deterministic_id() {
        let req = UpsertChannelAccountRequest {
            name: "Alice Channel".into(),
            bio: "news about a3chat".into(),
            avatar_hash: None,
            tags: vec!["tech".into(), "news".into()],
            kind: AccountKind::Service,
            verification: VerificationLevel::OwnerVerified,
        };
        let now = ts(1_700_000_000);
        let account = req.into_account(owner(), now).unwrap();
        assert!(account.account_id.starts_with(ACCOUNT_ID_PREFIX));
        assert_eq!(account.owner_node_id, owner());
        assert_eq!(account.kind, AccountKind::Service);
        assert_eq!(account.verification, VerificationLevel::OwnerVerified);
        assert_eq!(account.created_at, now);
        assert_eq!(account.updated_at, now);
        assert_eq!(account.sequence, 0);
        assert_eq!(account.subscriber_count, 0);
    }

    #[test]
    fn upsert_request_rejects_oversize_name() {
        let mut req = UpsertChannelAccountRequest {
            name: "x".repeat(MAX_ACCOUNT_NAME_LEN + 1),
            bio: "ok".into(),
            avatar_hash: None,
            tags: vec![],
            kind: AccountKind::Subscription,
            verification: VerificationLevel::None,
        };
        assert!(req.validate().is_err());
        req.name = "Alice".into();
        assert!(req.validate().is_ok());
    }

    #[test]
    fn upsert_request_rejects_bad_avatar_hash() {
        let req = UpsertChannelAccountRequest {
            name: "Alice".into(),
            bio: "ok".into(),
            avatar_hash: Some("".into()),
            tags: vec![],
            kind: AccountKind::Subscription,
            verification: VerificationLevel::None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn publish_request_rejects_empty_title() {
        let req = PublishFeedRequest {
            title: "".into(),
            summary: "ok".into(),
            body: "ok".into(),
            cover_url: None,
            attachments: vec![],
            tags: vec![],
            is_pinned: false,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn publish_request_rejects_empty_body() {
        let req = PublishFeedRequest {
            title: "ok".into(),
            summary: "ok".into(),
            body: "".into(),
            cover_url: None,
            attachments: vec![],
            tags: vec![],
            is_pinned: false,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn publish_request_rejects_oversize_body() {
        let req = PublishFeedRequest {
            title: "ok".into(),
            summary: "ok".into(),
            body: "x".repeat(MAX_FEED_BODY_LEN + 1),
            cover_url: None,
            attachments: vec![],
            tags: vec![],
            is_pinned: false,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn publish_request_rejects_too_many_attachments() {
        let req = PublishFeedRequest {
            title: "ok".into(),
            summary: "ok".into(),
            body: "ok".into(),
            cover_url: None,
            attachments: (0..MAX_ATTACHMENTS_PER_FEED_ITEM + 1)
                .map(|i| FeedAttachment {
                    kind: "image".into(),
                    url: format!("https://x.example/{i}"),
                    content_hash: None,
                    mime_type: None,
                    caption: None,
                })
                .collect(),
            tags: vec![],
            is_pinned: false,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn publish_request_rejects_attachment_with_bad_url() {
        let req = PublishFeedRequest {
            title: "ok".into(),
            summary: "ok".into(),
            body: "ok".into(),
            cover_url: None,
            attachments: vec![FeedAttachment {
                kind: "image".into(),
                url: "ftp://nope.example/file.png".into(),
                content_hash: None,
                mime_type: None,
                caption: None,
            }],
            tags: vec![],
            is_pinned: false,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn publish_request_rejects_bad_cover_url() {
        let req = PublishFeedRequest {
            title: "ok".into(),
            summary: "ok".into(),
            body: "ok".into(),
            cover_url: Some("file:///etc/passwd".into()),
            attachments: vec![],
            tags: vec![],
            is_pinned: false,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn feed_item_validates_retraction_requires_reason() {
        let item = FeedItem {
            feed_id: format!("{FEED_ID_PREFIX}abc"),
            account_id: format!("{ACCOUNT_ID_PREFIX}abc"),
            sequence: 1,
            title: "ok".into(),
            summary: "ok".into(),
            body: "ok".into(),
            cover_url: None,
            attachments: vec![],
            tags: vec![],
            is_pinned: false,
            is_retracted: true,
            retraction_reason: None,
            created_at: ts(1),
            updated_at: ts(1),
        };
        assert!(item.validate().is_err());
    }

    #[test]
    fn feed_item_accepts_well_formed() {
        let item = FeedItem {
            feed_id: format!("{FEED_ID_PREFIX}abc"),
            account_id: format!("{ACCOUNT_ID_PREFIX}abc"),
            sequence: 1,
            title: "ok".into(),
            summary: "ok".into(),
            body: "ok".into(),
            cover_url: Some("https://example.com/c.png".into()),
            attachments: vec![],
            tags: vec!["tech".into()],
            is_pinned: false,
            is_retracted: false,
            retraction_reason: None,
            created_at: ts(1),
            updated_at: ts(1),
        };
        assert!(item.validate().is_ok());
    }

    #[test]
    fn subscription_defaults() {
        let s = Subscription::default();
        assert_eq!(s.notify_mode, "");
        // The service layer is responsible for filling in
        // `DEFAULT_NOTIFY_MODE` on insert; the default is empty so
        // we can detect "field never set" during hydration.
        assert_eq!(s.last_read_seq, 0);
        assert!(!s.is_muted);
        assert!(!s.is_pinned);
    }

    #[test]
    fn public_account_validates_required_fields() {
        let a = PublicAccount::default();
        assert!(a.validate().is_err()); // account_id is empty
    }

    #[test]
    fn public_account_rejects_inverted_timestamps() {
        let mut a = PublicAccount::default();
        a.account_id = format!("{ACCOUNT_ID_PREFIX}abc");
        a.owner_node_id = owner().into();
        a.name = "Alice".into();
        a.bio = "ok".into();
        a.tags.clear();
        a.created_at = ts(100);
        a.updated_at = ts(50);
        assert!(a.validate().is_err());
        a.updated_at = ts(100);
        assert!(a.validate().is_ok());
    }
}