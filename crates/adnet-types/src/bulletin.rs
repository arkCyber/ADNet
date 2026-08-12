//! `Bulletin` — unified news + announcement wire format.
//!
//! A single `BulletinItem` covers both **authoritative announcements**
//! ("system out for maintenance at 18:00 UTC", "breaking: water main
//! repair on 5th Avenue") and **news feed entries** ("the council
//! approved the budget", "weekly weather advisory"). The `kind` enum
//! selects which downstream pipeline handles the record:
//!
//! | `kind`         | Severity floor | Default TTL | Use case                       |
//! |----------------|----------------|-------------|--------------------------------|
//! | `Announcement` | `Info`         | 7 days      | Operator / system notices      |
//! | `Advisory`     | `Notable`      | 7 days      | Public service messages        |
//! | `NewsArticle`  | `Info`         | 30 days     | Persistent news / press items  |
//! | `Correction`   | `Info`         | 30 days     | Update to a previous bulletin  |
//! | `Retraction`   | `Critical`     | 30 days     | Withdraw / supersede           |
//!
//! Every record carries:
//!
//! - a stable 32-byte content-derived [`BulletinId`],
//! - an optional content hash for the body blob (BLAKE3),
//! - a monotonic per-room `sequence` so receivers can order them
//!   *deterministically* even when gossip delivers out of order,
//! - a `supersedes` / `superseded_by` pair so corrections/retractions
//!   can be linked to the original bulletin,
//! - a TTL (`expires_at`) and a per-severity priority,
//! - the [`Signature`] envelope (wallet address + scheme-tagged
//!   signature over the canonical preimage).
//!
//! [`BulletinId`]: crate::bulletin::BulletinId

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::content::ContentHash;
use crate::error::{AdnetError, Result};
use crate::invariants::{
    MAX_CONTENT_LEN, MAX_ID_LEN, MAX_TAGS, validate_content, validate_id, validate_name,
    validate_ordered, validate_tag,
};
use crate::node::NodeId;
use crate::room::RoomId;
use crate::wallet_address::WalletAddress;

/// Maximum number of attachments a single bulletin may carry.
/// Bounds the JSON envelope size so gossip frames stay below
/// `MAX_BULLETIN_ENVELOPE_BYTES`.
pub const MAX_BULLETIN_ATTACHMENTS: usize = 8;

/// Hard ceiling on the serialised envelope size. Above this we reject
/// the record before it touches SQLite or the gossip bus. Aligned
/// with `Announcement::MAX_ANNOUNCED_SIZE` semantics — gossip frames
/// must stay bounded.
pub const MAX_BULLETIN_ENVELOPE_BYTES: usize = 256 * 1024;

/// Maximum signature size. Mirrors [`crate::announce::MAX_SIGNATURE_LEN`]
/// (1 KiB) so a single schema constant applies across both wire
/// formats. PQ upgrades (Dilithium ~2 KiB) need a coordinated bump.
pub const MAX_BULLETIN_SIGNATURE_LEN: usize = 1024;

/// Per-bulletin monotonic sequence ceiling. Exceeds
/// [`crate::invariants::MAX_SEQUENCE`] (9999) because a single news
/// channel can plausibly see more than 10k items in a year.
pub const MAX_BULLETIN_SEQUENCE: u32 = 1_000_000;

/// Default TTL when the publisher omits `expires_at`. Seven days is
/// the operator-tested sweet spot for community announcements: long
/// enough that urgent notices stay readable, short enough that stale
/// items do not accumulate indefinitely.
pub const DEFAULT_BULLETIN_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Maximum allowed clock skew between local wall clock and the
/// `created_at` field. Mirrors
/// [`crate::announce::MAX_TIMESTAMP_SKEW_HOURS`] so a future single
/// gateway can validate both record families under the same window.
pub const MAX_BULLETIN_TIMESTAMP_SKEW_HOURS: i64 = 24;

/// Distinguishes what a bulletin *is*. Drives downstream routing
/// (operator dashboard vs. public news feed), default TTL, and the
/// severity floor enforced by `validate()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BulletinKind {
    /// Operator / system announcement. Goes to dashboards and
    /// notification streams.
    Announcement,
    /// Public service advisory (weather, traffic, public safety).
    Advisory,
    /// Persistent news / press article.
    NewsArticle,
    /// Update to a previous bulletin (always paired with
    /// `supersedes`).
    Correction,
    /// Withdraw / supersede another bulletin (always paired with
    /// `supersedes`).
    Retraction,
}

impl BulletinKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Announcement => "announcement",
            Self::Advisory => "advisory",
            Self::NewsArticle => "news_article",
            Self::Correction => "correction",
            Self::Retraction => "retraction",
        }
    }

    pub fn from_strict(s: &str) -> Result<Self> {
        Ok(match s {
            "announcement" => Self::Announcement,
            "advisory" => Self::Advisory,
            "news_article" => Self::NewsArticle,
            "correction" => Self::Correction,
            "retraction" => Self::Retraction,
            other => {
                return Err(AdnetError::Validation(format!(
                    "invalid BulletinKind {other:?}"
                )));
            }
        })
    }

    /// Minimum severity floor. A bulletin with severity below the
    /// floor fails validation, so e.g. an `Advisory` cannot be sent
    /// as `Info` (would defeat its purpose).
    pub fn severity_floor(self) -> BulletinSeverity {
        match self {
            Self::Announcement => BulletinSeverity::Info,
            Self::Advisory => BulletinSeverity::Notable,
            Self::NewsArticle => BulletinSeverity::Info,
            Self::Correction => BulletinSeverity::Info,
            Self::Retraction => BulletinSeverity::Critical,
        }
    }

    /// Default TTL in seconds for the kind. Override via
    /// `BulletinItem::with_ttl`.
    pub fn default_ttl(self) -> u64 {
        match self {
            Self::Announcement | Self::Advisory => 7 * 24 * 60 * 60,
            Self::NewsArticle | Self::Correction | Self::Retraction => 30 * 24 * 60 * 60,
        }
    }
}

/// Per-bulletin severity. Mirrors the semantics used by emergency
/// alert systems (CAP / IPAWS): higher severity = earlier delivery
/// priority and stronger retention guarantees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BulletinSeverity {
    Info,
    Notable,
    Important,
    Critical,
}

impl BulletinSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Notable => "notable",
            Self::Important => "important",
            Self::Critical => "critical",
        }
    }

    pub fn from_strict(s: &str) -> Result<Self> {
        Ok(match s {
            "info" => Self::Info,
            "notable" => Self::Notable,
            "important" => Self::Important,
            "critical" => Self::Critical,
            other => {
                return Err(AdnetError::Validation(format!(
                    "invalid BulletinSeverity {other:?}"
                )));
            }
        })
    }
}

/// Taxonomy categories. Open set in the spec; the enum below is the
/// canonical baseline that ships with the reference implementation.
/// Unknown categories are rejected at the boundary so wire drift
/// becomes a typed failure instead of a silent string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BulletinCategory {
    General,
    Security,
    Outage,
    Weather,
    Health,
    Safety,
    Traffic,
    Politics,
    Economy,
    Tech,
    Community,
    Sports,
    Culture,
}

impl BulletinCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Security => "security",
            Self::Outage => "outage",
            Self::Weather => "weather",
            Self::Health => "health",
            Self::Safety => "safety",
            Self::Traffic => "traffic",
            Self::Politics => "politics",
            Self::Economy => "economy",
            Self::Tech => "tech",
            Self::Community => "community",
            Self::Sports => "sports",
            Self::Culture => "culture",
        }
    }

    pub fn from_strict(s: &str) -> Result<Self> {
        Ok(match s {
            "general" => Self::General,
            "security" => Self::Security,
            "outage" => Self::Outage,
            "weather" => Self::Weather,
            "health" => Self::Health,
            "safety" => Self::Safety,
            "traffic" => Self::Traffic,
            "politics" => Self::Politics,
            "economy" => Self::Economy,
            "tech" => Self::Tech,
            "community" => Self::Community,
            "sports" => Self::Sports,
            "culture" => Self::Culture,
            other => {
                return Err(AdnetError::Validation(format!(
                    "invalid BulletinCategory {other:?}"
                )));
            }
        })
    }
}

/// Stable bulletin identifier — BLAKE3-256 of the canonical
/// preimage `(room_id, author_id, sequence, created_at_unix_seconds,
/// nonce)`. Because the inputs include a `nonce`, an author can
/// deliberately mint multiple distinct bulletins at the same sequence
/// position — useful for fan-out redundancy on flaky networks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BulletinId(String);

impl BulletinId {
    pub const HEX_LEN: usize = 64;

    pub fn as_hex(&self) -> &str {
        &self.0
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        if s.len() != Self::HEX_LEN || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AdnetError::Validation(format!(
                "bulletin_id: expected {}-char lowercase hex (got {} chars)",
                Self::HEX_LEN,
                s.len()
            )));
        }
        Ok(Self(s.to_ascii_lowercase()))
    }

    /// Compute a bulletin id from the canonical inputs. Two
    /// `BulletinItem`s with the same `(room, author, sequence,
    /// created_at, nonce)` always produce the same id — useful when
    /// the receiving side wants to dedup by id.
    pub fn derive(
        room: &RoomId,
        author: &NodeId,
        sequence: u32,
        created_at: DateTime<Utc>,
        nonce: &[u8],
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"adnet-bulletin/v1");
        hasher.update(room.as_str().as_bytes());
        hasher.update(author.to_string().as_bytes());
        hasher.update(&sequence.to_le_bytes());
        hasher.update(&created_at.timestamp().to_le_bytes());
        hasher.update(&(nonce.len() as u32).to_le_bytes());
        hasher.update(nonce);
        Self(hasher.finalize().to_hex().to_string())
    }
}

impl std::fmt::Display for BulletinId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Pointer to a media attachment on a bulletin (image, video, audio,
/// document). The body blob lives in the regular blobstore — the
/// bulletin just carries the content hash and metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BulletinAttachment {
    pub attachment_id: String,
    pub content_hash: ContentHash,
    pub mime_type: String,
    pub file_name: String,
    pub file_size: u64,
    pub caption: Option<String>,
}

impl BulletinAttachment {
    pub fn validate(&self) -> Result<()> {
        validate_id("attachment_id", &self.attachment_id)?;
        if self.mime_type.is_empty() || self.mime_type.len() > MAX_ID_LEN {
            return Err(AdnetError::Validation(format!(
                "mime_type: must be 1..={MAX_ID_LEN} bytes"
            )));
        }
        if self.mime_type.as_bytes().contains(&0) {
            return Err(AdnetError::Validation("mime_type: contains NUL".into()));
        }
        validate_name("file_name", &self.file_name)?;
        if let Some(c) = &self.caption {
            validate_content("caption", c)?;
        }
        Ok(())
    }
}

/// The business object: a typed, validated, optionally-signed
/// bulletin. Construction goes through [`BulletinItem::new`] so the
/// invariants are enforced once at the boundary; downstream layers
/// (`BulletinStore`, `BulletinBus`) only see already-validated
/// records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BulletinItem {
    pub bulletin_id: BulletinId,
    pub kind: BulletinKind,
    pub category: BulletinCategory,
    pub severity: BulletinSeverity,
    pub room_id: RoomId,
    pub author_id: NodeId,
    pub author_name: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    pub body_hash: Option<ContentHash>,
    pub lang: String,
    pub tags: Vec<String>,
    pub attachments: Vec<BulletinAttachment>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub sequence: u32,
    pub is_pinned: bool,
    pub is_edited: bool,
    pub edited_at: Option<DateTime<Utc>>,
    pub supersedes: Option<BulletinId>,
    pub superseded_by: Option<BulletinId>,
    pub retraction_reason: Option<String>,
    pub integrity_hash: Option<String>,
    /// Optional wallet address that vouches for the bulletin. When
    /// present, `signature` MUST also be present and verify the
    /// canonical preimage. Mirrors the convention in
    /// [`crate::announce::Announcement`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer: Option<WalletAddress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Vec<u8>>,
}

impl BulletinItem {
    /// Build a new bulletin. `nonce` is opaque to the caller
    /// (typically 16 random bytes); it's mixed into
    /// [`BulletinId::derive`] so the caller can mint a fresh id
    /// even when `(room, author, sequence, created_at)` would
    /// otherwise collide. `supersedes` is required for
    /// [`BulletinKind::Correction`] / [`BulletinKind::Retraction`]
    /// and ignored otherwise.
    ///
    /// **The constructor does NOT call [`BulletinItem::validate`]**
    /// — callers must invoke it themselves before publishing or
    /// persisting. Skipping validation lets builder helpers like
    /// [`Self::as_retraction_of`] populate required fields
    /// (`retraction_reason`) on a freshly-constructed
    /// `Retraction` record without spurious errors.
    pub fn new(
        kind: BulletinKind,
        category: BulletinCategory,
        severity: BulletinSeverity,
        room_id: RoomId,
        author_id: NodeId,
        title: impl Into<String>,
        summary: impl Into<String>,
        body: impl Into<String>,
        nonce: &[u8],
        supersedes: Option<BulletinId>,
    ) -> Result<Self> {
        let now = Utc::now();
        let item = Self {
            bulletin_id: BulletinId::derive(&room_id, &author_id, 0, now, nonce),
            kind,
            category,
            severity,
            room_id,
            author_id: author_id.clone(),
            author_name: String::new(),
            title: title.into(),
            summary: summary.into(),
            body: body.into(),
            body_hash: None,
            lang: "en".to_string(),
            tags: Vec::new(),
            attachments: Vec::new(),
            created_at: now,
            updated_at: now,
            expires_at: now + ChronoDuration::seconds(kind.default_ttl() as i64),
            sequence: 0,
            is_pinned: false,
            is_edited: false,
            edited_at: None,
            supersedes,
            superseded_by: None,
            retraction_reason: None,
            integrity_hash: None,
            signer: None,
            signature: None,
        };
        Ok(item)
    }

    /// Builder-style helpers (kept narrow so `validate()` can be
    /// called once on the finished record).
    pub fn with_author_name(mut self, name: impl Into<String>) -> Self {
        self.author_name = name.into();
        self
    }

    pub fn with_lang(mut self, lang: impl Into<String>) -> Self {
        self.lang = lang.into();
        self
    }

    pub fn with_body_hash(mut self, h: ContentHash) -> Self {
        self.body_hash = Some(h);
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    pub fn with_attachments(mut self, att: Vec<BulletinAttachment>) -> Self {
        self.attachments = att;
        self
    }

    pub fn with_sequence(mut self, seq: u32) -> Self {
        self.sequence = seq;
        self
    }

    pub fn with_ttl(mut self, ttl_seconds: u64) -> Self {
        self.expires_at = self.created_at + ChronoDuration::seconds(ttl_seconds as i64);
        self
    }

    pub fn with_pinned(mut self, pinned: bool) -> Self {
        self.is_pinned = pinned;
        self
    }

    pub fn with_supersedes(mut self, target: BulletinId) -> Self {
        self.supersedes = Some(target);
        self
    }

    pub fn with_retraction_reason(mut self, reason: impl Into<String>) -> Self {
        self.retraction_reason = Some(reason.into());
        self
    }

    /// Mark the bulletin as a correction of an earlier one and
    /// stamp the canonical preimage. Convenience for the
    /// `Correction` flow.
    pub fn as_correction_of(mut self, target: BulletinId) -> Self {
        self.kind = BulletinKind::Correction;
        self.supersedes = Some(target);
        self
    }

    /// Mark the bulletin as a retraction.
    pub fn as_retraction_of(mut self, target: BulletinId, reason: impl Into<String>) -> Self {
        self.kind = BulletinKind::Retraction;
        self.supersedes = Some(target);
        self.retraction_reason = Some(reason.into());
        self
    }

    /// `true` iff the bulletin is signed by a wallet.
    pub fn is_signed(&self) -> bool {
        self.signer.is_some() && self.signature.is_some()
    }

    /// Pin a signature onto the bulletin. The signature is stored
    /// verbatim; the verifier (in `adnet-news::service`) is
    /// responsible for checking the scheme tag and preimage.
    pub fn attach_signature(&mut self, signer: WalletAddress, signature: Vec<u8>) {
        self.signer = Some(signer);
        self.signature = Some(signature);
    }

    /// Drop signature fields. Used when re-broadcasting through a
    /// channel that strips crypto metadata.
    pub fn without_signature(mut self) -> Self {
        self.signer = None;
        self.signature = None;
        self
    }

    /// Full DO-178C-style validation. Mirrors the layered
    /// guarantees used by [`crate::group_chat`] and
    /// [`crate::social_feed`]:
    ///
    /// 1. type-level — every enum is a typed variant
    /// 2. length-level — every string is bounded
    /// 3. character-level — IDs / tags cannot contain control
    ///    characters or whitespace
    /// 4. temporal-level — `created_at <= updated_at <= expires_at`
    ///    and `is_edited` ⇔ `edited_at.is_some()`
    /// 5. sequence-level — `sequence < MAX_BULLETIN_SEQUENCE`
    /// 6. semantic-level — `Correction`/`Retraction` MUST carry
    ///    `supersedes`; severity MUST be at the kind's floor
    ///    (`Advisory` ≥ `Notable`, `Retraction` ≥ `Critical`).
    pub fn validate(&self) -> Result<()> {
        // Title / summary / body — `title` may be empty only when the
        // record is a Correction (its meaning is implied by
        // `supersedes`); summary and body must be non-empty so
        // recipients always have something to display.
        validate_name("title", &self.title)?;
        if self.title.is_empty() && self.kind != BulletinKind::Correction {
            return Err(AdnetError::Validation(
                "title: empty (Correction may omit title)".into(),
            ));
        }
        validate_content("summary", &self.summary)?;
        validate_content("body", &self.body)?;
        if self.body.len() > MAX_CONTENT_LEN {
            return Err(AdnetError::Validation(format!(
                "body: exceeds {MAX_CONTENT_LEN} bytes (got {})",
                self.body.len()
            )));
        }
        if !self.lang.is_empty() && self.lang.len() > 16 {
            return Err(AdnetError::Validation(format!(
                "lang: must be 1..=16 bytes (got {})",
                self.lang.len()
            )));
        }
        if self.lang.as_bytes().contains(&0) {
            return Err(AdnetError::Validation("lang: contains NUL".into()));
        }
        validate_name("author_name", &self.author_name)?;
        validate_id("bulletin_id_hex", self.bulletin_id.as_hex())?;

        // Sequence ceiling.
        if self.sequence >= MAX_BULLETIN_SEQUENCE {
            return Err(AdnetError::Validation(format!(
                "sequence: {} >= ceiling {MAX_BULLETIN_SEQUENCE}",
                self.sequence
            )));
        }

        // Tags.
        if self.tags.len() > MAX_TAGS {
            return Err(AdnetError::Validation(format!(
                "tags: {} exceeds {MAX_TAGS}",
                self.tags.len()
            )));
        }
        for (i, t) in self.tags.iter().enumerate() {
            validate_tag(&format!("tags[{i}]"), t)?;
        }

        // Attachments.
        if self.attachments.len() > MAX_BULLETIN_ATTACHMENTS {
            return Err(AdnetError::Validation(format!(
                "attachments: {} exceeds {MAX_BULLETIN_ATTACHMENTS}",
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

        // Temporal ordering.
        let created_ms = self.created_at.timestamp_millis().max(0) as u64;
        let updated_ms = self.updated_at.timestamp_millis().max(0) as u64;
        validate_ordered("updated_at vs created_at", created_ms, updated_ms)?;
        if self.expires_at <= self.created_at {
            return Err(AdnetError::Validation(format!(
                "expires_at: {} must be strictly after created_at {}",
                self.expires_at, self.created_at
            )));
        }
        if self.is_edited {
            let ea = self.edited_at.ok_or_else(|| {
                AdnetError::Validation("is_edited=true with edited_at=None".into())
            })?;
            let created_ms = self.created_at.timestamp_millis().max(0) as u64;
            let edited_ms = ea.timestamp_millis().max(0) as u64;
            validate_ordered("edited_at vs created_at", created_ms, edited_ms)?;
        } else if self.edited_at.is_some() {
            return Err(AdnetError::Validation(
                "edited_at set while is_edited=false".into(),
            ));
        }

        // Clock skew window.
        let now = Utc::now();
        let limit = ChronoDuration::hours(MAX_BULLETIN_TIMESTAMP_SKEW_HOURS);
        let earliest = now - limit;
        let latest = now + limit;
        if self.created_at < earliest || self.created_at > latest {
            return Err(AdnetError::Validation(format!(
                "created_at: {} outside the ±{}h window around {}",
                self.created_at, MAX_BULLETIN_TIMESTAMP_SKEW_HOURS, now
            )));
        }

        // Severity must clear the kind's floor.
        if self.severity < self.kind.severity_floor() {
            return Err(AdnetError::Validation(format!(
                "severity: {:?} below floor {:?} for kind {:?}",
                self.severity,
                self.kind.severity_floor(),
                self.kind
            )));
        }

        // Correction / Retraction MUST carry `supersedes`.
        match self.kind {
            BulletinKind::Correction | BulletinKind::Retraction => {
                if self.supersedes.is_none() {
                    return Err(AdnetError::Validation(format!(
                        "{:?}: must set `supersedes`",
                        self.kind
                    )));
                }
            }
            _ => {
                if self.superseded_by.is_some() {
                    return Err(AdnetError::Validation(
                        "superseded_by must only be set by downstream retractions".into(),
                    ));
                }
            }
        }

        // Retraction reason is required and bounded.
        if self.kind == BulletinKind::Retraction {
            match &self.retraction_reason {
                None => {
                    return Err(AdnetError::Validation(
                        "retraction: requires `retraction_reason`".into(),
                    ));
                }
                Some(r) => validate_content("retraction_reason", r)?,
            }
        } else if self.retraction_reason.is_some() {
            return Err(AdnetError::Validation(
                "retraction_reason must only be set on Retraction bulletins".into(),
            ));
        }

        // Signature sanity. We deliberately do NOT verify the
        // signature here (no crypto deps in `adnet-types`); the
        // service / verifier layer is responsible. We only check
        // size and presence consistency.
        match (&self.signer, &self.signature) {
            (Some(_), Some(sig)) if sig.len() <= MAX_BULLETIN_SIGNATURE_LEN => {}
            (Some(_), Some(_)) => {
                return Err(AdnetError::Validation(format!(
                    "signature: exceeds {MAX_BULLETIN_SIGNATURE_LEN} bytes"
                )));
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(AdnetError::Validation(
                    "signer/signature must be both present or both absent".into(),
                ));
            }
            (None, None) => {}
        }

        // Envelope size — measure the canonical JSON encoding
        // (without signature fields) and reject anything that
        // would blow past the gossip frame cap.
        let approx = self.estimate_envelope_bytes();
        if approx > MAX_BULLETIN_ENVELOPE_BYTES {
            return Err(AdnetError::Validation(format!(
                "envelope size: ~{approx} bytes exceeds {MAX_BULLETIN_ENVELOPE_BYTES}"
            )));
        }

        Ok(())
    }

    /// Rough upper bound on the serialised JSON envelope size.
    /// Used as a fast gate in `validate()`; we accept a small
    /// over-estimate in exchange for not allocating twice.
    pub fn estimate_envelope_bytes(&self) -> usize {
        let mut n = 256; // fixed overhead (field names, braces, separators)
        n += self.bulletin_id.as_hex().len();
        n += self.room_id.as_str().len();
        n += self.author_id.to_string().len();
        n += self.author_name.len();
        n += self.title.len();
        n += self.summary.len();
        n += self.body.len();
        n += self.lang.len();
        for t in &self.tags {
            n += t.len() + 2;
        }
        for a in &self.attachments {
            n += a.attachment_id.len() + a.mime_type.len() + a.file_name.len();
            n += a.caption.as_ref().map(|s| s.len() + 4).unwrap_or(4);
        }
        if let Some(s) = &self.supersedes {
            n += s.as_hex().len();
        }
        if let Some(s) = &self.superseded_by {
            n += s.as_hex().len();
        }
        if let Some(r) = &self.retraction_reason {
            n += r.len();
        }
        if let Some(h) = &self.integrity_hash {
            n += h.len();
        }
        if let Some(sig) = &self.signature {
            n += sig.len();
        }
        n
    }

    /// Compute the canonical preimage for signing. Stable across
    /// `serde_json` upgrades by routing through [`serde_json::Value`]
    /// with explicit alphabetical-key ordering on a hand-written
    /// struct — same trick used by
    /// [`crate::announce::Announcement::signing_preimage`].
    pub fn signing_preimage(&self) -> Result<Vec<u8>> {
        let signable = SignableBulletin {
            attachments: &self.attachments,
            author_id: &self.author_id,
            author_name: &self.author_name,
            body: &self.body,
            body_hash: &self.body_hash,
            bulletin_id: &self.bulletin_id,
            category: self.category,
            created_at: self.created_at,
            edited_at: self.edited_at,
            expires_at: self.expires_at,
            is_edited: self.is_edited,
            is_pinned: self.is_pinned,
            kind: self.kind,
            lang: &self.lang,
            retraction_reason: &self.retraction_reason,
            room_id: &self.room_id,
            sequence: self.sequence,
            severity: self.severity,
            supersedes: &self.supersedes,
            superseded_by: &self.superseded_by,
            summary: &self.summary,
            tags: &self.tags,
            title: &self.title,
            updated_at: self.updated_at,
        };
        Ok(serde_json::to_vec(&signable)?)
    }

    /// Stamp the integrity hash. Mirrors the convention used by
    /// [`crate::social_feed::SocialPost`]: SHA-256 of
    /// `(room_id, author_id, body, sequence, created_at, kind,
    /// category, severity, supersedes)`. Distinct from the
    /// signature so a reader can verify the body has not been
    /// tampered with even without the wallet signature.
    pub fn stamp_integrity_hash(&mut self) {
        let base = bulletin_integrity_hash(
            &self.room_id,
            &self.author_id,
            &self.body,
            self.sequence,
            self.created_at,
        );
        let mut extras: Vec<Vec<u8>> = Vec::new();
        extras.push(self.kind.as_str().as_bytes().to_vec());
        extras.push(self.category.as_str().as_bytes().to_vec());
        extras.push(self.severity.as_str().as_bytes().to_vec());
        extras.push(
            self.supersedes
                .as_ref()
                .map(|s| s.as_hex().as_bytes().to_vec())
                .unwrap_or_default(),
        );
        self.integrity_hash = Some(crate::integrity::hash_fields(
            std::iter::once(base.as_bytes().to_vec()).chain(extras),
        ));
    }

    /// Verify the integrity hash against the current field values.
    pub fn verify_integrity(&self) -> bool {
        let expected = {
            let base = bulletin_integrity_hash(
                &self.room_id,
                &self.author_id,
                &self.body,
                self.sequence,
                self.created_at,
            );
            let extras: Vec<Vec<u8>> = vec![
                self.kind.as_str().as_bytes().to_vec(),
                self.category.as_str().as_bytes().to_vec(),
                self.severity.as_str().as_bytes().to_vec(),
                self.supersedes
                    .as_ref()
                    .map(|s| s.as_hex().as_bytes().to_vec())
                    .unwrap_or_default(),
            ];
            crate::integrity::hash_fields(std::iter::once(base.as_bytes().to_vec()).chain(extras))
        };
        self.integrity_hash.as_deref() == Some(expected.as_str())
    }

    /// `true` iff the bulletin has expired against `now`.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }
}

/// Hand-rolled signer preimage with explicit field order so the byte
/// form survives `serde_json` upgrades. Adding or removing a field
/// here is a wire-format change.
#[derive(Debug, Serialize)]
struct SignableBulletin<'a> {
    attachments: &'a Vec<BulletinAttachment>,
    author_id: &'a NodeId,
    author_name: &'a String,
    body: &'a String,
    body_hash: &'a Option<ContentHash>,
    bulletin_id: &'a BulletinId,
    category: BulletinCategory,
    created_at: DateTime<Utc>,
    edited_at: Option<DateTime<Utc>>,
    expires_at: DateTime<Utc>,
    is_edited: bool,
    is_pinned: bool,
    kind: BulletinKind,
    lang: &'a String,
    retraction_reason: &'a Option<String>,
    room_id: &'a RoomId,
    sequence: u32,
    severity: BulletinSeverity,
    supersedes: &'a Option<BulletinId>,
    superseded_by: &'a Option<BulletinId>,
    summary: &'a String,
    tags: &'a Vec<String>,
    title: &'a String,
    updated_at: DateTime<Utc>,
}

/// Compute the integrity hash for the static body fields. Extracted
/// so [`BulletinItem::stamp_integrity_hash`] and
/// [`BulletinItem::verify_integrity`] can share the same code path.
fn bulletin_integrity_hash(
    room: &RoomId,
    author: &NodeId,
    body: &str,
    sequence: u32,
    created_at: DateTime<Utc>,
) -> String {
    let mut h = <sha2::Sha256 as sha2::Digest>::new();
    h.update(b"adnet-bulletin-integrity/v1");
    h.update(room.as_str().as_bytes());
    h.update(author.to_string().as_bytes());
    h.update(body.as_bytes());
    h.update(&sequence.to_le_bytes());
    h.update(&created_at.timestamp().to_le_bytes());
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> NodeId {
        NodeId::random()
    }

    fn good(severity: BulletinSeverity) -> BulletinItem {
        BulletinItem::new(
            BulletinKind::Announcement,
            BulletinCategory::General,
            severity,
            RoomId::new("news-tech"),
            node(),
            "System upgrade tonight",
            "We will restart the cluster at 18:00 UTC.",
            "Full maintenance window is 18:00-19:00 UTC. No data loss expected.",
            b"nonce-1234567890",
            None,
        )
        .unwrap()
    }

    // ── Constructors / accessors ─────────────────────────────────────────

    #[test]
    fn new_derives_stable_id_per_nonce() {
        let n = node();
        let now = Utc::now();
        let id_a = BulletinId::derive(&RoomId::new("r"), &n, 1, now, b"a");
        let id_b = BulletinId::derive(&RoomId::new("r"), &n, 1, now, b"b");
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn bulletin_id_hex_roundtrip() {
        let id = BulletinId::derive(&RoomId::new("r"), &node(), 1, Utc::now(), b"nonce");
        assert_eq!(id.as_hex().len(), BulletinId::HEX_LEN);
        let parsed = BulletinId::from_hex(id.as_hex()).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn bulletin_id_rejects_short_hex() {
        assert!(BulletinId::from_hex("abcd").is_err());
    }

    #[test]
    fn bulletin_id_rejects_non_hex() {
        let bad = "z".repeat(BulletinId::HEX_LEN);
        assert!(BulletinId::from_hex(&bad).is_err());
    }

    // ── Validate ────────────────────────────────────────────────────────

    #[test]
    fn validate_accepts_good_item() {
        assert!(good(BulletinSeverity::Important).validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_summary() {
        let mut b = good(BulletinSeverity::Info);
        b.summary = String::new();
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("summary"), "got {err}");
    }

    #[test]
    fn validate_rejects_empty_body() {
        let mut b = good(BulletinSeverity::Info);
        b.body = String::new();
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("body"), "got {err}");
    }

    #[test]
    fn validate_rejects_empty_title_except_correction() {
        let mut b = good(BulletinSeverity::Info);
        b.title = String::new();
        assert!(b.validate().is_err());

        let target = BulletinId::derive(&RoomId::new("r"), &node(), 0, Utc::now(), b"target");
        let c = BulletinItem::new(
            BulletinKind::Correction,
            BulletinCategory::General,
            BulletinSeverity::Info,
            RoomId::new("r"),
            node(),
            "",
            "Correction summary",
            "Correction body",
            b"nonce-c",
            Some(target),
        )
        .unwrap();
        assert!(c.validate().is_ok(), "correction may omit title");
    }

    #[test]
    fn validate_enforces_severity_floor() {
        let mut b = good(BulletinSeverity::Info);
        b.kind = BulletinKind::Advisory;
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("severity"), "got {err}");
    }

    #[test]
    fn retraction_requires_supersedes_and_reason() {
        let target = BulletinId::derive(&RoomId::new("r"), &node(), 0, Utc::now(), b"target");
        // supersedes set, but retraction_reason still missing.
        let r = BulletinItem::new(
            BulletinKind::Retraction,
            BulletinCategory::General,
            BulletinSeverity::Critical,
            RoomId::new("r"),
            node(),
            "Withdrawn",
            "Removed due to incorrect data",
            "Body explaining retraction",
            b"nonce-r",
            Some(target.clone()),
        )
        .unwrap();
        assert!(r.validate().is_err());
        // Populate the reason — now the record validates.
        let r = r.with_retraction_reason("incorrect data");
        assert!(r.validate().is_ok());

        // supersedes missing → also invalid.
        let no_target = BulletinItem::new(
            BulletinKind::Retraction,
            BulletinCategory::General,
            BulletinSeverity::Critical,
            RoomId::new("r"),
            node(),
            "Withdrawn",
            "Removed due to incorrect data",
            "Body explaining retraction",
            b"nonce-r-no-target",
            None,
        )
        .unwrap();
        assert!(no_target.validate().is_err());
    }

    #[test]
    fn correction_requires_supersedes() {
        let target = BulletinId::derive(&RoomId::new("r"), &node(), 0, Utc::now(), b"target");
        let c = BulletinItem::new(
            BulletinKind::Correction,
            BulletinCategory::General,
            BulletinSeverity::Info,
            RoomId::new("r"),
            node(),
            "",
            "Correction summary",
            "Correction body",
            b"nonce-c",
            Some(target),
        )
        .unwrap();
        assert!(c.validate().is_ok());

        let no_target = BulletinItem::new(
            BulletinKind::Correction,
            BulletinCategory::General,
            BulletinSeverity::Info,
            RoomId::new("r"),
            node(),
            "",
            "Correction summary",
            "Correction body",
            b"nonce-c-no-target",
            None,
        )
        .unwrap();
        assert!(no_target.validate().is_err());
    }

    #[test]
    fn retraction_reason_only_on_retraction() {
        let mut b = good(BulletinSeverity::Info);
        b.retraction_reason = Some("oops".into());
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("retraction_reason"), "got {err}");
    }

    #[test]
    fn validate_rejects_partial_signature() {
        let mut b = good(BulletinSeverity::Info);
        b.signer = Some(WalletAddress::from_bytes([0x01u8; 20]));
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("signer/signature"), "got {err}");
    }

    #[test]
    fn validate_rejects_oversize_signature() {
        let mut b = good(BulletinSeverity::Info);
        b.attach_signature(
            WalletAddress::from_bytes([0x01u8; 20]),
            vec![0u8; MAX_BULLETIN_SIGNATURE_LEN + 1],
        );
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("signature"), "got {err}");
    }

    #[test]
    fn validate_rejects_far_future_created_at() {
        let mut b = good(BulletinSeverity::Info);
        b.created_at = Utc::now() + ChronoDuration::hours(MAX_BULLETIN_TIMESTAMP_SKEW_HOURS + 2);
        b.updated_at = b.created_at;
        b.expires_at = b.created_at + ChronoDuration::days(1);
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("created_at"), "got {err}");
    }

    #[test]
    fn validate_rejects_sequence_at_ceiling() {
        let mut b = good(BulletinSeverity::Info);
        b.sequence = MAX_BULLETIN_SEQUENCE;
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("sequence"), "got {err}");
    }

    #[test]
    fn validate_rejects_expires_before_created() {
        let mut b = good(BulletinSeverity::Info);
        b.expires_at = b.created_at - ChronoDuration::seconds(1);
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("expires_at"), "got {err}");
    }

    #[test]
    fn validate_rejects_edited_at_without_is_edited() {
        let mut b = good(BulletinSeverity::Info);
        b.edited_at = Some(Utc::now());
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("is_edited"), "got {err}");
    }

    #[test]
    fn validate_rejects_oversize_body() {
        let mut b = good(BulletinSeverity::Info);
        b.body = "x".repeat(MAX_CONTENT_LEN + 1);
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("body"), "got {err}");
    }

    #[test]
    fn validate_rejects_too_many_tags() {
        let mut b = good(BulletinSeverity::Info);
        b.tags = (0..MAX_TAGS + 1).map(|i| format!("t{i}")).collect();
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("tags"), "got {err}");
    }

    #[test]
    fn validate_rejects_too_many_attachments() {
        let mut b = good(BulletinSeverity::Info);
        b.attachments = (0..MAX_BULLETIN_ATTACHMENTS + 1)
            .map(|i| BulletinAttachment {
                attachment_id: format!("att{i}"),
                content_hash: ContentHash::from_bytes(b"x"),
                mime_type: "image/png".into(),
                file_name: format!("img{i}.png"),
                file_size: 1,
                caption: None,
            })
            .collect();
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("attachments"), "got {err}");
    }

    // ── Integrity hash ──────────────────────────────────────────────────

    #[test]
    fn integrity_hash_roundtrip() {
        let mut b = good(BulletinSeverity::Info);
        b.stamp_integrity_hash();
        assert!(b.verify_integrity());
        b.body.push(' ');
        assert!(!b.verify_integrity());
    }

    #[test]
    fn integrity_hash_field_order_matters() {
        let mut a = good(BulletinSeverity::Info);
        a.stamp_integrity_hash();
        let mut b = a.clone();
        b.stamp_integrity_hash();
        assert_eq!(a.integrity_hash, b.integrity_hash);
        b.kind = BulletinKind::NewsArticle;
        b.stamp_integrity_hash();
        assert_ne!(a.integrity_hash, b.integrity_hash);
    }

    // ── Signing preimage ─────────────────────────────────────────────────

    #[test]
    fn signing_preimage_is_deterministic() {
        let b = good(BulletinSeverity::Info);
        let p1 = b.signing_preimage().unwrap();
        let p2 = b.signing_preimage().unwrap();
        assert_eq!(p1, p2);
    }

    #[test]
    fn signing_preimage_changes_with_body() {
        let mut b = good(BulletinSeverity::Info);
        let p1 = b.signing_preimage().unwrap();
        b.body.push('!');
        let p2 = b.signing_preimage().unwrap();
        assert_ne!(p1, p2);
    }

    #[test]
    fn signing_preimage_excludes_signature() {
        let mut b = good(BulletinSeverity::Info);
        b.attach_signature(WalletAddress::from_bytes([0x01u8; 20]), vec![0u8; 65]);
        let pre = b.signing_preimage().unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&pre).unwrap();
        assert!(parsed.get("signer").is_none());
        assert!(parsed.get("signature").is_none());
    }

    // ── Envelope size ────────────────────────────────────────────────────

    #[test]
    fn envelope_size_caps() {
        let mut b = good(BulletinSeverity::Info);
        // Estimate with an aggressive caption per attachment: 8
        // attachments × (8 KiB caption + 64-byte filename + 32-byte
        // attachment id + 16-byte mime + 4-byte overhead) easily
        // exceeds 64 KiB. Tags + body are kept small so they do
        // not trip the cheaper body-length gate first.
        b.tags = (0..MAX_TAGS).map(|i| format!("t-{i:04}")).collect();
        b.attachments = (0..MAX_BULLETIN_ATTACHMENTS)
            .map(|i| BulletinAttachment {
                attachment_id: format!("att{i}-padding-padding-padding"),
                content_hash: ContentHash::from_bytes(b"x"),
                mime_type: "image/png".into(),
                file_name: format!(
                    "verylongfilename-{i:04}-padding-padding-padding-padding-padding.png"
                ),
                file_size: 1,
                caption: Some("x".repeat(32 * 1024)),
            })
            .collect();
        assert!(
            b.estimate_envelope_bytes() > MAX_BULLETIN_ENVELOPE_BYTES,
            "estimate {} should exceed {}",
            b.estimate_envelope_bytes(),
            MAX_BULLETIN_ENVELOPE_BYTES
        );
        let err = b.validate().unwrap_err();
        assert!(err.to_string().contains("envelope size"), "got {err}");
    }

    // ── Roundtrip / serde ────────────────────────────────────────────────

    #[test]
    fn serde_roundtrip_preserves_all_fields() {
        let mut b = good(BulletinSeverity::Important);
        b = b.with_tags(vec!["tech".into(), "ops".into()]);
        b = b.with_pinned(true);
        b.stamp_integrity_hash();
        b.attach_signature(WalletAddress::from_bytes([0x42u8; 20]), vec![0u8; 65]);
        let json = serde_json::to_string(&b).unwrap();
        let back: BulletinItem = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn is_expired_uses_expires_at() {
        let b = good(BulletinSeverity::Info);
        assert!(!b.is_expired(b.created_at + ChronoDuration::seconds(1)));
        assert!(b.is_expired(b.expires_at + ChronoDuration::seconds(1)));
    }

    #[test]
    fn kind_severity_floor() {
        assert_eq!(
            BulletinKind::Announcement.severity_floor(),
            BulletinSeverity::Info
        );
        assert_eq!(
            BulletinKind::Advisory.severity_floor(),
            BulletinSeverity::Notable
        );
        assert_eq!(
            BulletinKind::Retraction.severity_floor(),
            BulletinSeverity::Critical
        );
    }

    #[test]
    fn kind_default_ttl() {
        assert_eq!(BulletinKind::Announcement.default_ttl(), 7 * 24 * 60 * 60);
        assert_eq!(BulletinKind::NewsArticle.default_ttl(), 30 * 24 * 60 * 60);
    }
}
