//! Link bookmark / favorites domain types.
//!
//! A [`LinkBookmark`] is a user-saved URL with optional title,
//! description, favicon hash, folder path, tag list, pinning flag and
//! archive flag. Bookmarks are owned by a single user (`owner_id`)
//! and never shared cross-account in this release (cross-device sync
//! is flagged as a future feature — see
//! `a3chat-app/src/link_bookmark_service.rs`).
//!
//! The domain module owns the *shape* of the data and the validation
//! rules. Persistence lives in `a3net_chatstore::link_bookmark`; the
//! service that wires RPC + storage lives in
//! `a3chat_app::link_bookmark_service`.
//!
//! ## Identifier scheme
//!
//! A bookmark's `bookmark_id` is a 64-char lowercase hex BLAKE3 hash
//! over `(INTEGRITY_HASH_TAG || owner_id || 0x00 || url ||
//! 0x00 || created_at_unix_le)`. Two bookmarks sharing both `(owner_id,
//! url, created_at_unix)` are by construction identical, so
//! `INSERT OR REPLACE` merges cleanly across devices that share an
//! NTP-synced clock.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::A3chatError;
use crate::id::validate_id;
use crate::validation::validate_url;

/// `INTEGRITY_HASH_TAG` — domain tag baked into the bookmark-id hash
/// so the same `(owner_id, url, created_at_unix)` triple cannot
/// collide with a hash produced by another feature using the same
/// inputs.
pub const INTEGRITY_HASH_TAG: &[u8] = b"a3chat-link-bookmark|v1";

/// `MAX_TITLE_LEN` — same ceiling as `MAX_NAME_LEN` so a title
/// comfortably fits in any chat preview surface.
pub const MAX_TITLE_LEN: usize = 256;

/// `MAX_DESCRIPTION_LEN` — generous free-text cap; matches the
/// group `description` ceiling.
pub const MAX_DESCRIPTION_LEN: usize = 1024;

/// `MAX_SNAPSHOT_LEN` — captured page text for offline fallback.
pub const MAX_SNAPSHOT_LEN: usize = 64 * 1024;

/// `MAX_TAGS_PER_BOOKMARK` — defensive cap on the per-bookmark tag
/// list. Frontends (WeChat / Chrome / Raindrop) cap at ~20-100.
pub const MAX_TAGS_PER_BOOKMARK: usize = 32;

/// `MAX_TAG_LEN` — per-tag length cap (after lower-casing).
pub const MAX_TAG_LEN: usize = 64;

/// `MAX_FOLDER_DEPTH` — `/a/b/c/d` style paths with at most this many
/// nested folders. Keeps the SQLite `LIKE '/%/%/%/%'` scan bounded.
pub const MAX_FOLDER_DEPTH: usize = 8;

/// `MAX_FOLDER_LEN` — length cap for the entire path string.
pub const MAX_FOLDER_LEN: usize = 256;

/// `DEFAULT_FOLDER` — bookmarked links land here when the caller
/// doesn't pick a folder explicitly.
pub const DEFAULT_FOLDER: &str = "/";

/// Source / provenance of a bookmark row. The wire format is
/// `snake_case` so frontends can switch on `source` without parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BookmarkSource {
    /// Manually created by the user (drag-drop, paste, "save" button).
    #[default]
    User,
    /// Auto-captured by the chat scanner (a URL surfaced in a message
    /// the user opened; user did not explicitly save).
    AutoCapture,
    /// Imported from another browser / service (e.g. Chrome export).
    Import,
    /// Shared with the local user by a contact. Reserved — current
    /// release does not support cross-user sharing but the row tag is
    /// wired up so historic imports don't need a schema change.
    Shared,
}

impl BookmarkSource {
    /// Stable wire string for SQLite persistence and SSE tagging.
    pub fn as_str(&self) -> &'static str {
        match self {
            BookmarkSource::User => "user",
            BookmarkSource::AutoCapture => "auto_capture",
            BookmarkSource::Import => "import",
            BookmarkSource::Shared => "shared",
        }
    }

    /// Reverse of [`as_str`](Self::as_str). Unknown strings fall
    /// back to [`User`](Self::User) so older DB rows that pre-date
    /// a new variant don't crash on hydration.
    pub fn parse(s: &str) -> Self {
        match s {
            "auto_capture" => BookmarkSource::AutoCapture,
            "import" => BookmarkSource::Import,
            "shared" => BookmarkSource::Shared,
            _ => BookmarkSource::User,
        }
    }
}

/// The full bookmark record — what the UI renders, what the SQLite
/// row stores, and what SSE pushes to subscribers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct LinkBookmark {
    pub bookmark_id: String,
    pub owner_id: String,
    pub url: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional blake3 hash of the favicon, hex-encoded. The actual
    /// favicon bytes live in the media store; this column is a pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon_hash: Option<String>,
    /// Folder path — `/` for the root. Use `/` as the segment
    /// separator (matches WeChat's "收藏 / 文件传输助手" tree).
    pub folder: String,
    /// Normalized tags: ASCII lower-case, deduplicated, length-bounded.
    #[serde(default)]
    pub tags: Vec<String>,
    pub is_pinned: bool,
    pub is_archived: bool,
    /// Snapshot of the page text, captured at save time. None if the
    /// fetch failed (offline save, anti-bot block, etc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_text: Option<String>,
    pub source: BookmarkSource,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_visited_at: Option<DateTime<Utc>>,
    pub visit_count: u32,
}

impl LinkBookmark {
    /// Convenience constructor for unit tests that just need a
    /// well-formed `LinkBookmark` without having to thread every
    /// field through. Production code paths go through
    /// [`UpsertLinkBookmarkRequest::into_bookmark`] which performs
    /// validation and normalisation. The `new_for_test` name signals
    /// that this is intended for cross-crate test fixtures; callers
    /// should not rely on the exact ID format.
    pub fn new_for_test(
        owner_id: &str,
        url: &str,
        title: &str,
        created_at_unix: i64,
        updated_at_unix: i64,
    ) -> Self {
        let created_at = chrono::DateTime::<chrono::Utc>::from_timestamp(created_at_unix, 0)
            .unwrap_or_else(chrono::Utc::now);
        let updated_at = chrono::DateTime::<chrono::Utc>::from_timestamp(updated_at_unix, 0)
            .unwrap_or_else(chrono::Utc::now);
        // Deterministic ID — tests don't need cryptographic strength.
        let bookmark_id =
            format!("test-{owner_id}-{url}-{created_at_unix}").replace([':', '/'], "_");
        Self {
            bookmark_id,
            owner_id: owner_id.to_string(),
            url: url.to_string(),
            title: title.to_string(),
            description: None,
            favicon_hash: None,
            folder: DEFAULT_FOLDER.to_string(),
            tags: Vec::new(),
            is_pinned: false,
            is_archived: false,
            snapshot_text: None,
            source: BookmarkSource::User,
            created_at,
            updated_at,
            last_visited_at: None,
            visit_count: 0,
        }
    }

    /// Field-level validation. Mirrors the storage-layer checks but
    /// kept in `a3chat-core` so server code can short-circuit before
    /// the SQLite write.
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_id("owner_id", &self.owner_id)?;
        validate_url("url", &self.url)?;
        validate_id("bookmark_id", &self.bookmark_id)?;
        // Title is required (frontend fills with URL when the user
        // hasn't picked a title).
        if self.title.is_empty() {
            return Err(A3chatError::InvalidInput("title: empty".into()));
        }
        if self.title.len() > MAX_TITLE_LEN {
            return Err(A3chatError::InvalidInput(format!(
                "title: length {} > {MAX_TITLE_LEN}",
                self.title.len()
            )));
        }
        if let Some(d) = &self.description {
            if d.len() > MAX_DESCRIPTION_LEN {
                return Err(A3chatError::InvalidInput(format!(
                    "description: length {} > {MAX_DESCRIPTION_LEN}",
                    d.len()
                )));
            }
        }
        if let Some(s) = &self.snapshot_text {
            if s.len() > MAX_SNAPSHOT_LEN {
                return Err(A3chatError::InvalidInput(format!(
                    "snapshot_text: length {} > {MAX_SNAPSHOT_LEN}",
                    s.len()
                )));
            }
        }
        validate_folder("folder", &self.folder)?;
        if self.tags.len() > MAX_TAGS_PER_BOOKMARK {
            return Err(A3chatError::InvalidInput(format!(
                "tags: {} entries > {MAX_TAGS_PER_BOOKMARK}",
                self.tags.len()
            )));
        }
        for (i, t) in self.tags.iter().enumerate() {
            if t.is_empty() {
                return Err(A3chatError::InvalidInput(format!(
                    "tags[{i}]: empty"
                )));
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

/// Validate that `folder` is a `/`-prefixed absolute path with no
/// empty segments and at most [`MAX_FOLDER_DEPTH`] levels. The
/// string `/` (the root) is accepted as a single segment with zero
/// children.
pub fn validate_folder(field: &str, folder: &str) -> Result<(), A3chatError> {
    if folder.is_empty() {
        return Err(A3chatError::InvalidInput(format!("{field}: empty")));
    }
    if folder.len() > MAX_FOLDER_LEN {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: length {} > {MAX_FOLDER_LEN}",
            folder.len()
        )));
    }
    if !folder.starts_with('/') {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: must start with '/' (got {folder:?})"
        )));
    }
    if folder == "/" {
        return Ok(());
    }
    let depth = folder.chars().filter(|c| *c == '/').count();
    if depth > MAX_FOLDER_DEPTH {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: depth {depth} > {MAX_FOLDER_DEPTH}"
        )));
    }
    // No empty segments and no control characters.
    for seg in folder.split('/').skip(1) {
        if seg.is_empty() {
            return Err(A3chatError::InvalidInput(format!(
                "{field}: empty segment in {folder:?}"
            )));
        }
        if seg.chars().any(|c| c.is_control()) {
            return Err(A3chatError::InvalidInput(format!(
                "{field}: control character in segment {seg:?}"
            )));
        }
    }
    Ok(())
}

/// Normalize a single tag: trim, lower-case, reject empty / oversized.
///
/// Exposed so RPC handlers can normalize on the way in *and* tests
/// can exercise the rules directly without round-tripping through a
/// `LinkBookmark`.
pub fn normalize_tag(raw: &str) -> Result<String, A3chatError> {
    let t = raw.trim().to_ascii_lowercase();
    if t.is_empty() {
        return Err(A3chatError::InvalidInput("tag: empty".into()));
    }
    if t.len() > MAX_TAG_LEN {
        return Err(A3chatError::InvalidInput(format!(
            "tag: length {} > {MAX_TAG_LEN}",
            t.len()
        )));
    }
    if t.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(A3chatError::InvalidInput(format!(
            "tag: contains whitespace or control char: {t:?}"
        )));
    }
    Ok(t)
}

/// Normalize a list of tags: trim, lower-case, deduplicate, reject
/// anything that fails [`normalize_tag`]. Order is preserved
/// (first-seen wins).
pub fn normalize_tags<I, S>(raw: I) -> Result<Vec<String>, A3chatError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for r in raw {
        let n = normalize_tag(r.as_ref())?;
        if !out.iter().any(|existing| existing == &n) {
            out.push(n);
        }
    }
    if out.len() > MAX_TAGS_PER_BOOKMARK {
        return Err(A3chatError::InvalidInput(format!(
            "tags: {} entries > {MAX_TAGS_PER_BOOKMARK} (deduped)",
            out.len()
        )));
    }
    Ok(out)
}

/// Compute the deterministic [`LinkBookmark::bookmark_id`] for a
/// `(owner_id, url, created_at_unix)` triple. The tag-prefixed
/// BLAKE3 hash guarantees the same triple cannot collide with hashes
/// produced by other features.
pub fn compute_bookmark_id(
    owner_id: &str,
    url: &str,
    created_at_unix: i64,
) -> String {
    let mut h = blake3::Hasher::new();
    h.update(INTEGRITY_HASH_TAG);
    h.update(owner_id.as_bytes());
    h.update(&[0u8]);
    h.update(url.as_bytes());
    h.update(&[0u8]);
    h.update(&created_at_unix.to_le_bytes());
    hex::encode(h.finalize().as_bytes())
}

/// Request payload for `a3chat.link.bookmark.add` /
/// `a3chat.link.bookmark.update`.
///
/// `Option<Option<T>>` distinguishes three update intents:
/// - `None` → leave the field alone.
/// - `Some(None)` → clear the field (description, snapshot).
/// - `Some(Some(v))` → set the field to `v`.
///
/// Fields that don't change (`url`, `folder`, …) are taken from the
/// stored row, so the client can re-submit the entire list and the
/// service does a straight overwrite.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UpsertLinkBookmarkRequest {
    pub url: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon_hash: Option<String>,
    #[serde(default = "default_folder")]
    pub folder: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub is_pinned: bool,
    #[serde(default)]
    pub is_archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_text: Option<String>,
    #[serde(default)]
    pub source: BookmarkSource,
}

fn default_folder() -> String {
    DEFAULT_FOLDER.to_string()
}

impl UpsertLinkBookmarkRequest {
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_url("url", &self.url)?;
        if self.title.is_empty() {
            return Err(A3chatError::InvalidInput("title: empty".into()));
        }
        if self.title.len() > MAX_TITLE_LEN {
            return Err(A3chatError::InvalidInput(format!(
                "title: length {} > {MAX_TITLE_LEN}",
                self.title.len()
            )));
        }
        if let Some(d) = &self.description {
            if d.len() > MAX_DESCRIPTION_LEN {
                return Err(A3chatError::InvalidInput(format!(
                    "description: length {} > {MAX_DESCRIPTION_LEN}",
                    d.len()
                )));
            }
        }
        validate_folder("folder", &self.folder)?;
        let _ = normalize_tags(self.tags.iter())?;
        Ok(())
    }

    /// Build a fresh [`LinkBookmark`] for the given owner + creation
    /// timestamp. The bookmark_id is derived from the same triple
    /// (owner, url, created_at_unix) so identical saves dedupe.
    pub fn into_bookmark(
        self,
        owner_id: &str,
        created_at: DateTime<Utc>,
    ) -> Result<LinkBookmark, A3chatError> {
        validate_id("owner_id", owner_id)?;
        let now = Utc::now();
        let normalized_tags = normalize_tags(self.tags.iter())?;
        let bookmark_id = compute_bookmark_id(owner_id, &self.url, created_at.timestamp());
        Ok(LinkBookmark {
            bookmark_id,
            owner_id: owner_id.to_string(),
            url: self.url,
            title: self.title,
            description: self.description,
            favicon_hash: self.favicon_hash,
            folder: if self.folder.is_empty() {
                DEFAULT_FOLDER.to_string()
            } else {
                self.folder
            },
            tags: normalized_tags,
            is_pinned: self.is_pinned,
            is_archived: self.is_archived,
            snapshot_text: self.snapshot_text,
            source: self.source,
            created_at,
            updated_at: now,
            last_visited_at: None,
            visit_count: 0,
        })
    }
}

/// Query filter for `a3chat.link.bookmark.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LinkBookmarkListFilter {
    /// Folder path — exact match when set, otherwise root + children
    /// are returned when `include_subfolders = true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Include all bookmarks whose folder starts with `folder + '/'`.
    #[serde(default)]
    pub include_subfolders: bool,
    /// Restrict to rows whose tag set *contains* **all** of these
    /// tags (logical AND). Empty means "no tag filter".
    #[serde(default)]
    pub tags: Vec<String>,
    /// Restrict to rows with this exact `is_pinned` value. `None`
    /// means "no filter".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_pinned: Option<bool>,
    /// Restrict to rows with this exact `is_archived` value. `None`
    /// means "no filter" (clients typically pass `Some(false)` so
    /// archived rows are hidden by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_archived: Option<bool>,
    /// Max rows to return. Defaults to 200, capped at 1000 by the
    /// service layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Search query for `a3chat.link.bookmark.search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LinkBookmarkSearchQuery {
    /// Fuzzy needle. Service rejects empty / whitespace-only.
    pub needle: String,
    /// Optional folder scope — same semantics as
    /// [`LinkBookmarkListFilter::folder`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Max rows to return. Defaults to 50, capped at 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl LinkBookmarkSearchQuery {
    pub fn validate(&self) -> Result<(), A3chatError> {
        if self.needle.trim().is_empty() {
            return Err(A3chatError::InvalidInput("needle: empty".into()));
        }
        if self.needle.len() > 256 {
            return Err(A3chatError::InvalidInput(format!(
                "needle: length {} > 256",
                self.needle.len()
            )));
        }
        Ok(())
    }
}

/// Tag + count row returned by `a3chat.link.bookmark.tags`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LinkTagCount {
    pub tag: String,
    pub count: u32,
}

/// Folder node returned by `a3chat.link.bookmark.folders`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LinkFolderNode {
    pub folder: String,
    /// Number of direct children at this folder. Direct — not
    /// recursive. The UI uses this to render the "(n)" badge.
    pub direct_count: u32,
}

/// Aggregate count returned by `a3chat.link.bookmark.count`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct LinkBookmarkCount {
    pub total: u32,
    pub pinned: u32,
    pub archived: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> String {
        "user:alice".to_string()
    }

    fn sample_request(url: &str, title: &str) -> UpsertLinkBookmarkRequest {
        UpsertLinkBookmarkRequest {
            url: url.to_string(),
            title: title.to_string(),
            description: Some("note".into()),
            favicon_hash: None,
            folder: DEFAULT_FOLDER.to_string(),
            tags: vec!["Rust".into(), "Docs".into(), "rust".into()],
            is_pinned: false,
            is_archived: false,
            snapshot_text: None,
            source: BookmarkSource::User,
        }
    }

    #[test]
    fn compute_bookmark_id_is_deterministic() {
        let a = compute_bookmark_id("alice", "https://example.com", 1700000000);
        let b = compute_bookmark_id("alice", "https://example.com", 1700000000);
        assert_eq!(a, b);
        // Different owner → different hash.
        let c = compute_bookmark_id("bob", "https://example.com", 1700000000);
        assert_ne!(a, c);
        // Different timestamp → different hash.
        let d = compute_bookmark_id("alice", "https://example.com", 1700000001);
        assert_ne!(a, d);
        // Output is 64-char hex.
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn normalized_tags_lowercases_and_dedupes() {
        let out = normalize_tags(["Rust", "rust", " docs ", "DOCs", "tutorial"]).unwrap();
        assert_eq!(out, vec!["rust", "docs", "tutorial"]);
    }

    #[test]
    fn normalized_tags_rejects_empty() {
        let err = normalize_tags(["ok", "   "]).unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[test]
    fn upsert_request_into_bookmark_normalizes_tags() {
        let req = sample_request("https://example.com", "Example");
        let now = Utc::now();
        let bookmark = req
            .clone()
            .into_bookmark(&owner(), now)
            .expect("into_bookmark ok");
        assert_eq!(bookmark.owner_id, owner());
        assert_eq!(bookmark.tags, vec!["rust", "docs"]);
        assert_eq!(bookmark.folder, DEFAULT_FOLDER);
        assert_eq!(bookmark.source, BookmarkSource::User);
        assert_eq!(bookmark.bookmark_id.len(), 64);
        assert_eq!(bookmark.created_at, now);
    }

    #[test]
    fn upsert_request_rejects_empty_title() {
        let mut req = sample_request("https://example.com", "");
        let err = req.validate().unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
        req.title = "x".repeat(MAX_TITLE_LEN + 1);
        assert!(req.validate().is_err());
    }

    #[test]
    fn upsert_request_rejects_non_http_url() {
        let mut req = sample_request("ftp://example.com", "ok");
        assert!(req.validate().is_err());
        req.url = "https://example.com".to_string();
        assert!(req.validate().is_ok());
    }

    #[test]
    fn validate_folder_rejects_relative_paths() {
        assert!(validate_folder("folder", "foo/bar").is_err());
        assert!(validate_folder("folder", "").is_err());
        assert!(validate_folder("folder", "/").is_ok());
        assert!(validate_folder("folder", "/work").is_ok());
        assert!(validate_folder("folder", "/work/notes").is_ok());
        assert!(validate_folder("folder", "/work//notes").is_err());
    }

    #[test]
    fn validate_folder_rejects_too_deep() {
        let deep = format!("/{}", "a/".repeat(MAX_FOLDER_DEPTH + 1));
        assert!(validate_folder("folder", &deep).is_err());
    }

    #[test]
    fn search_query_rejects_empty_needle() {
        let q = LinkBookmarkSearchQuery {
            needle: "  ".to_string(),
            folder: None,
            limit: None,
        };
        assert!(q.validate().is_err());
        let ok_q = LinkBookmarkSearchQuery {
            needle: "rust".to_string(),
            folder: None,
            limit: Some(10),
        };
        assert!(ok_q.validate().is_ok());
    }

    #[test]
    fn bookmark_source_default_is_user() {
        assert_eq!(BookmarkSource::default(), BookmarkSource::User);
        assert_eq!(BookmarkSource::AutoCapture.as_str(), "auto_capture");
        assert_eq!(BookmarkSource::Import.as_str(), "import");
    }

    #[test]
    fn bookmark_source_round_trip() {
        for src in [
            BookmarkSource::User,
            BookmarkSource::AutoCapture,
            BookmarkSource::Import,
            BookmarkSource::Shared,
        ] {
            assert_eq!(BookmarkSource::parse(src.as_str()), src);
        }
        // Unknown / legacy values default to User.
        assert_eq!(BookmarkSource::parse("imported"), BookmarkSource::User);
        assert_eq!(BookmarkSource::parse("auto"), BookmarkSource::User);
    }
}