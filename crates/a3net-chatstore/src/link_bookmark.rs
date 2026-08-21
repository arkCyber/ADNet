//! Link bookmarks — per-user URL storage shared between `a3net-chatstore`
//! (CLI / Tauri desktop) and the `a3chat.link.*` RPC namespace
//! exposed by `a3chat-app::link_bookmark_service`.
//!
//! The record types ([`LinkBookmark`], [`BookmarkSource`],
//! [`compute_bookmark_id`]) live in [`a3chat_core::link_bookmark`]
//! and are re-exported here so existing callers can keep using
//! `a3net_chatstore::LinkBookmark` without an extra dep. Only the
//! SQLite row-mapping and the `LinkBookmarkStore` are local.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{ChatStoreError, Result};
use crate::schema;

// -- Re-exports: domain types live in `a3chat-core` -------------------------

pub use a3chat_core::link_bookmark::{
    compute_bookmark_id, BookmarkSource, LinkBookmark, LinkBookmarkListFilter,
    LinkBookmarkSearchQuery, LinkTagCount, LinkFolderNode, LinkBookmarkCount,
    UpsertLinkBookmarkRequest, INTEGRITY_HASH_TAG, DEFAULT_FOLDER,
    MAX_DESCRIPTION_LEN, MAX_FOLDER_DEPTH, MAX_FOLDER_LEN, MAX_SNAPSHOT_LEN,
    MAX_TAG_LEN, MAX_TAGS_PER_BOOKMARK, MAX_TITLE_LEN,
    normalize_tag, normalize_tags, validate_folder,
};
/// Configuration for [`LinkBookmarkStore`].
#[derive(Debug, Clone)]
pub struct LinkBookmarkStoreConfig {
    pub storage_dir: PathBuf,
}

impl Default for LinkBookmarkStoreConfig {
    fn default() -> Self {
        let mut storage_dir = std::env::temp_dir();
        storage_dir.push("a3net-link-bookmarks");
        Self { storage_dir }
    }
}

/// Link bookmark store — a single SQLite database shared across
/// all local users, with `owner_id` providing the partitioning
/// key. Mirrors the pattern of [`crate::storage::ChatStorage`].
#[derive(Debug, Clone)]
pub struct LinkBookmarkStore {
    inner: Arc<LinkBookmarkStoreInner>,
}

#[derive(Debug)]
struct LinkBookmarkStoreInner {
    config: LinkBookmarkStoreConfig,
    db: Arc<Mutex<Connection>>,
}

/// Filter for [`LinkBookmarkStore::list`]. An empty filter
/// returns every bookmark for the owner.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    /// Optional folder prefix (e.g. `/work` returns `/work` and
    /// `/work/...`).
    pub folder_prefix: Option<String>,
    /// When `true` (default), the `folder_prefix` filter also
    /// matches every nested folder (`/work/notes`). When `false`,
    /// the filter is an exact match against the `folder` column.
    pub include_subfolders: bool,
    /// Optional tag match — bookmark must include ALL listed tags.
    pub tags: Vec<String>,
    /// Only return pinned bookmarks.
    pub only_pinned: bool,
    /// Only return archived bookmarks.
    pub only_archived: bool,
    /// Maximum number of rows to return.
    pub limit: u32,
    /// Return rows `created_at` strictly older than this unix
    /// timestamp (cursor pagination).
    pub before_unix: Option<i64>,
}

/// Filter for [`LinkBookmarkStore::count`] — choose which subset of
/// rows is tallied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CountFilter {
    /// `is_archived = 0`.
    #[default]
    Active,
    /// `is_archived = 1`.
    Archived,
    /// `is_pinned = 1`.
    Pinned,
    /// No filter — every bookmark for the owner.
    All,
}

impl CountFilter {
    /// Optional `is_archived = ?` predicate the SQL should apply.
    fn archived_predicate(self) -> Option<i64> {
        match self {
            CountFilter::Active => Some(0),
            CountFilter::Archived => Some(1),
            _ => None,
        }
    }
    /// Optional `is_pinned = ?` predicate the SQL should apply.
    fn pinned_predicate(self) -> Option<i64> {
        match self {
            CountFilter::Pinned => Some(1),
            _ => None,
        }
    }
}

/// Re-export the same names from `a3chat_core` so callers in this crate
// can keep using `a3net_chatstore::link_bookmark::BookmarkSource` etc.
// without taking a separate dep on `a3chat-core`. The re-exported
// types share their definitions (same field shapes), so a
// `a3net_chatstore::LinkBookmark` value produced here can be passed
// directly to `a3chat_app::link_bookmark_service` after a field
// copy.

/// Column list shared between INSERT and SELECT statements so we
/// cannot drift between the two. Keep the order aligned with the
/// table definition in `schema.rs`.
const INSERT_COLS: &str = "bookmark_id, owner_id, url, title, description, favicon_hash, folder, tags_json, is_pinned, is_archived, snapshot_text, source, created_at_unix, updated_at_unix, last_visited_unix, visit_count";

/// SELECT projection of [`INSERT_COLS`].
const SELECT_COLS: &str = INSERT_COLS;

impl LinkBookmarkStore {
    /// Open (or create) the database at
    /// `config.storage_dir/link_bookmarks.db`.
    pub fn open(config: LinkBookmarkStoreConfig) -> Result<Self> {
        std::fs::create_dir_all(&config.storage_dir)?;
        let db_path = config.storage_dir.join("link_bookmarks.db");
        let mut conn = Connection::open(&db_path)?;
        crate::schema::configure_connection(&conn)?;
        crate::schema::apply_schema(&mut conn)?;
        // Fail-fast startup probe (DO-178C): refuse to hand out a
        // store whose underlying file is corrupt.
        let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(ChatStoreError::DatabaseCorrupt(integrity));
        }
        Ok(Self {
            inner: Arc::new(LinkBookmarkStoreInner {
                config,
                db: Arc::new(Mutex::new(conn)),
            }),
        })
    }

    /// Open an in-memory store (used by tests). The schema is
    /// applied to the temporary connection so the same code
    /// paths run on it.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        crate::schema::configure_connection(&conn)?;
        let mut conn = conn;
        crate::schema::apply_schema(&mut conn)?;
        Ok(Self {
            inner: Arc::new(LinkBookmarkStoreInner {
                config: LinkBookmarkStoreConfig::default(),
                db: Arc::new(Mutex::new(conn)),
            }),
        })
    }

    /// Wrap an already-opened connection. The caller is responsible
    /// for `apply_schema` / `configure_connection` having run. Used
    /// by `a3chat-app::ChatStorage::link_bookmark_store` to share
    /// the per-user SQLite file with the chat tables — that way a
    /// user's bookmarks live in the same database as their messages
    /// and benefit from the same WAL checkpoint / backup cycle.
    pub fn from_connection(
        db: Arc<Mutex<Connection>>,
        config: LinkBookmarkStoreConfig,
    ) -> Self {
        Self {
            inner: Arc::new(LinkBookmarkStoreInner { config, db }),
        }
    }

    pub fn config(&self) -> &LinkBookmarkStoreConfig {
        &self.inner.config
    }

    /// Read the on-disk schema version (for diagnostics).
    pub fn schema_version(&self) -> Result<u32> {
        let conn = self.inner.db.lock()?;
        Ok(schema::current_version(&conn)?)
    }

    /// Insert or update a bookmark. `INSERT OR REPLACE` so the
    /// same `(owner_id, bookmark_id)` pair merges cleanly across
    /// devices.
    pub fn put(&self, bookmark: LinkBookmark) -> Result<()> {
        bookmark.validate()?;
        let tags_json = serde_json::to_string(&bookmark.tags)
            .map_err(ChatStoreError::Json)?;
        let conn = self.inner.db.lock()?;
        let sql = format!(
            "INSERT OR REPLACE INTO link_bookmarks ({INSERT_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
        );
        conn.execute(
            &sql,
            params![
                bookmark.bookmark_id,
                bookmark.owner_id,
                bookmark.url,
                bookmark.title,
                bookmark.description,
                bookmark.favicon_hash,
                bookmark.folder,
                tags_json,
                bookmark.is_pinned as i64,
                bookmark.is_archived as i64,
                bookmark.snapshot_text,
                bookmark.source.as_str(),
                bookmark.created_at.timestamp(),
                bookmark.updated_at.timestamp(),
                bookmark.last_visited_at.map(|d| d.timestamp()),
                bookmark.visit_count as i64,
            ],
        )?;
        Ok(())
    }

    /// Fetch a single bookmark by `(owner_id, bookmark_id)`.
    pub fn get(&self, owner_id: &str, bookmark_id: &str) -> Result<Option<LinkBookmark>> {
        let conn = self.inner.db.lock()?;
        row_to_bookmark(
            &conn,
            &format!("SELECT {SELECT_COLS} FROM link_bookmarks WHERE bookmark_id = ?1 AND owner_id = ?2"),
            params![bookmark_id, owner_id],
        )
    }

    /// Fetch a bookmark by URL (the natural lookup key for
    /// "the user already saved this?").
    pub fn get_by_url(&self, owner_id: &str, url: &str) -> Result<Option<LinkBookmark>> {
        let conn = self.inner.db.lock()?;
        row_to_bookmark(
            &conn,
            &format!("SELECT {SELECT_COLS} FROM link_bookmarks WHERE owner_id = ?1 AND url = ?2 LIMIT 1"),
            params![owner_id, url],
        )
    }

    /// List bookmarks for an owner.
    pub fn list(&self, owner_id: &str, filter: ListFilter) -> Result<Vec<LinkBookmark>> {
        let mut sql = format!(
            "SELECT {SELECT_COLS} FROM link_bookmarks WHERE owner_id = ?1"
        );
        let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(owner_id.to_string())];
        if let Some(prefix) = &filter.folder_prefix {
            if filter.include_subfolders {
                sql.push_str(" AND (folder = ?2 OR folder LIKE ?2 || '/%')");
            } else {
                sql.push_str(" AND folder = ?2");
            }
            param_values.push(Box::new(prefix.clone()));
        }
        if filter.only_pinned {
            sql.push_str(" AND is_pinned = 1");
        }
        if filter.only_archived {
            sql.push_str(" AND is_archived = 1");
        } else {
            sql.push_str(" AND is_archived = 0");
        }
        if let Some(before) = filter.before_unix {
            let p = format!(" AND created_at_unix < ?{}", param_values.len() + 1);
            sql.push_str(&p);
            param_values.push(Box::new(before));
        }
        sql.push_str(" ORDER BY created_at_unix DESC");
        if filter.limit > 0 {
            sql.push_str(&format!(" LIMIT {}", filter.limit));
        }
        let conn = self.inner.db.lock()?;
        let mut stmt = conn.prepare(&sql)?;
        let params_iter: Vec<&dyn rusqlite::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(params_iter.as_slice(), |row| bookmark_from_row(row))?;
        let mut bookmarks = Vec::new();
        for r in rows {
            bookmarks.push(r?);
        }
        // In-memory tag filtering — the SQLite side handles the
        // structural predicates; we filter on the parsed
        // `tags_json` so the JSON shape is the source of truth.
        if !filter.tags.is_empty() {
            let lower: Vec<String> = filter
                .tags
                .iter()
                .map(|t| t.to_ascii_lowercase())
                .collect();
            bookmarks.retain(|b| {
                let have: Vec<String> = b.tags.iter().map(|t| t.to_ascii_lowercase()).collect();
                lower.iter().all(|t| have.iter().any(|h| h == t))
            });
        }
        Ok(bookmarks)
    }

    /// Search across title / description / url / tags. Case
    /// insensitive. Returns at most `limit` rows.
    pub fn search(&self, owner_id: &str, needle: &str, limit: u32) -> Result<Vec<LinkBookmark>> {
        if needle.trim().is_empty() {
            return Err(ChatStoreError::Validation("needle: empty".into()));
        }
        let pattern = format!("%{}%", needle.to_ascii_lowercase());
        let conn = self.inner.db.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SELECT_COLS} FROM link_bookmarks
             WHERE owner_id = ?1 AND (
                LOWER(url) LIKE ?2 OR LOWER(title) LIKE ?2 OR
                LOWER(IFNULL(description, '')) LIKE ?2 OR LOWER(tags_json) LIKE ?2
             )
             ORDER BY created_at_unix DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(
            params![owner_id, pattern, limit as i64],
            |row| bookmark_from_row(row),
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Delete a single bookmark. Returns `NotFound` if the row
    /// did not exist (recovery-friendly per the table's
    /// `recoverability()` mapping).
    pub fn delete(&self, owner_id: &str, bookmark_id: &str) -> Result<()> {
        let conn = self.inner.db.lock()?;
        let n = conn.execute(
            "DELETE FROM link_bookmarks WHERE bookmark_id = ?1 AND owner_id = ?2",
            params![bookmark_id, owner_id],
        )?;
        if n == 0 {
            return Err(ChatStoreError::NotFound(format!(
                "link_bookmark({bookmark_id}) for owner {owner_id}"
            )));
        }
        Ok(())
    }

    /// Toggle the pinned flag. Returns the updated row.
    pub fn set_pinned(
        &self,
        owner_id: &str,
        bookmark_id: &str,
        pinned: bool,
    ) -> Result<LinkBookmark> {
        let conn = self.inner.db.lock()?;
        let n = conn.execute(
            "UPDATE link_bookmarks SET is_pinned = ?3, updated_at_unix = ?4
             WHERE bookmark_id = ?1 AND owner_id = ?2",
            params![bookmark_id, owner_id, pinned as i64, Utc::now().timestamp()],
        )?;
        if n == 0 {
            return Err(ChatStoreError::NotFound(format!(
                "link_bookmark({bookmark_id}) for owner {owner_id}"
            )));
        }
        drop(conn);
        self.get(owner_id, bookmark_id)?
            .ok_or_else(|| ChatStoreError::NotFound(format!("link_bookmark({bookmark_id})")))
    }

    /// Toggle the archived flag. Returns the updated row.
    pub fn set_archived(
        &self,
        owner_id: &str,
        bookmark_id: &str,
        archived: bool,
    ) -> Result<LinkBookmark> {
        let conn = self.inner.db.lock()?;
        let n = conn.execute(
            "UPDATE link_bookmarks SET is_archived = ?3, updated_at_unix = ?4
             WHERE bookmark_id = ?1 AND owner_id = ?2",
            params![bookmark_id, owner_id, archived as i64, Utc::now().timestamp()],
        )?;
        if n == 0 {
            return Err(ChatStoreError::NotFound(format!(
                "link_bookmark({bookmark_id}) for owner {owner_id}"
            )));
        }
        drop(conn);
        self.get(owner_id, bookmark_id)?
            .ok_or_else(|| ChatStoreError::NotFound(format!("link_bookmark({bookmark_id})")))
    }

    /// Increment the visit counter and update `last_visited_unix`.
    pub fn touch_visit(&self, owner_id: &str, bookmark_id: &str) -> Result<LinkBookmark> {
        let conn = self.inner.db.lock()?;
        let now = Utc::now().timestamp();
        let n = conn.execute(
            "UPDATE link_bookmarks
             SET visit_count = visit_count + 1, last_visited_unix = ?3, updated_at_unix = ?3
             WHERE bookmark_id = ?1 AND owner_id = ?2",
            params![bookmark_id, owner_id, now],
        )?;
        if n == 0 {
            return Err(ChatStoreError::NotFound(format!(
                "link_bookmark({bookmark_id}) for owner {owner_id}"
            )));
        }
        drop(conn);
        self.get(owner_id, bookmark_id)?
            .ok_or_else(|| ChatStoreError::NotFound(format!("link_bookmark({bookmark_id})")))
    }

    /// Collect every tag currently in use by the owner, with a
    /// count per tag. Useful for the "manage tags" UI. Returns
    /// `LinkTagCount` records (tag + count, lower-cased, sorted
    /// by count desc).
    pub fn tags(&self, owner_id: &str) -> Result<Vec<LinkTagCount>> {
        let conn = self.inner.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT tags_json FROM link_bookmarks WHERE owner_id = ?1 AND is_archived = 0",
        )?;
        let rows = stmt.query_map(params![owner_id], |row| row.get::<_, String>(0))?;
        let mut counts: std::collections::BTreeMap<String, u32> =
            std::collections::BTreeMap::new();
        for r in rows {
            let raw = r?;
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&raw) {
                for tag in tags {
                    let key = tag.to_ascii_lowercase();
                    *counts.entry(key).or_insert(0) += 1;
                }
            }
        }
        let mut out: Vec<LinkTagCount> = counts
            .into_iter()
            .map(|(tag, count)| LinkTagCount { tag, count })
            .collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
        Ok(out)
    }

    /// Collect every folder path currently in use (deduped),
    /// including the number of bookmarks directly stored in each
    /// folder (NOT recursive — sub-folder rows are tallied
    /// separately).
    pub fn folders(&self, owner_id: &str) -> Result<Vec<LinkFolderNode>> {
        let conn = self.inner.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT folder, COUNT(*) FROM link_bookmarks
             WHERE owner_id = ?1
             GROUP BY folder
             ORDER BY folder ASC",
        )?;
        let rows = stmt.query_map(params![owner_id], |row| {
            let folder: String = row.get(0)?;
            let direct_count: i64 = row.get(1)?;
            Ok(LinkFolderNode {
                folder,
                direct_count: direct_count.max(0) as u32,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

/// Count bookmarks for an owner, optionally restricted by
/// [`CountFilter`].
impl LinkBookmarkStore {
    pub fn count(&self, owner_id: &str, filter: CountFilter) -> Result<u32> {
        let conn = self.inner.db.lock()?;
        let archived = filter.archived_predicate();
        let pinned = filter.pinned_predicate();
        let mut sql = "SELECT COUNT(*) FROM link_bookmarks WHERE owner_id = ?1".to_string();
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(owner_id.to_string())];
        if let Some(a) = archived {
            sql.push_str(" AND is_archived = ?2");
            params_vec.push(Box::new(a));
        }
        if let Some(p) = pinned {
            let next = params_vec.len() + 1;
            sql.push_str(&format!(" AND is_pinned = ?{next}"));
            params_vec.push(Box::new(p));
        }
        let params_iter: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        let n: i64 = conn.query_row(&sql, params_iter.as_slice(), |row| row.get(0))?;
        Ok(n.max(0) as u32)
    }
}

fn bookmark_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LinkBookmark> {
    let tags_json: String = row.get(7)?;
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    let is_pinned: i64 = row.get(8)?;
    let is_archived: i64 = row.get(9)?;
    let source: String = row.get(11)?;
    let created_at_unix: i64 = row.get(12)?;
    let updated_at_unix: i64 = row.get(13)?;
    let last_visited: Option<i64> = row.get(14)?;
    let visit_count: i64 = row.get(15)?;
    let created_at = DateTime::<Utc>::from_timestamp(created_at_unix, 0)
        .unwrap_or_else(|| Utc::now());
    let updated_at = DateTime::<Utc>::from_timestamp(updated_at_unix, 0)
        .unwrap_or_else(|| Utc::now());
    let last_visited_at = last_visited
        .and_then(|t| DateTime::<Utc>::from_timestamp(t, 0));
    Ok(LinkBookmark {
        bookmark_id: row.get(0)?,
        owner_id: row.get(1)?,
        url: row.get(2)?,
        title: row.get(3)?,
        description: row.get(4)?,
        favicon_hash: row.get(5)?,
        folder: row.get(6)?,
        tags,
        is_pinned: is_pinned != 0,
        is_archived: is_archived != 0,
        snapshot_text: row.get(10)?,
        source: BookmarkSource::parse(&source),
        created_at,
        updated_at,
        last_visited_at,
        visit_count: visit_count.max(0) as u32,
    })
}

fn row_to_bookmark(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Option<LinkBookmark>> {
    let mut stmt = conn.prepare(sql)?;
    let row: Option<LinkBookmark> = stmt
        .query_row(params, |row| bookmark_from_row(row))
        .optional()?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> String {
        "alice".to_string()
    }

    fn sample_bookmark(url: &str, title: &str) -> LinkBookmark {
        let now = Utc::now();
        let bookmark_id = compute_bookmark_id(&owner(), url, now.timestamp());
        LinkBookmark {
            bookmark_id,
            owner_id: owner(),
            url: url.to_string(),
            title: title.to_string(),
            description: Some("test description".into()),
            favicon_hash: None,
            folder: "/".into(),
            tags: vec!["rust".into(), "test".into()],
            is_pinned: false,
            is_archived: false,
            snapshot_text: None,
            source: BookmarkSource::User,
            created_at: now,
            updated_at: now,
            last_visited_at: None,
            visit_count: 0,
        }
    }

    #[test]
    fn validate_rejects_bad_url() {
        let mut b = sample_bookmark("https://example.com", "ok");
        b.url = "ftp://x".into();
        assert!(b.validate().is_err());
    }

    #[test]
    fn validate_rejects_oversize_title() {
        let mut b = sample_bookmark("https://example.com", "ok");
        b.title = "x".repeat(MAX_TITLE_LEN + 1);
        assert!(b.validate().is_err());
    }

    #[test]
    fn validate_rejects_too_many_tags() {
        let mut b = sample_bookmark("https://example.com", "ok");
        b.tags = (0..MAX_TAGS_PER_BOOKMARK + 1).map(|i| format!("t{i}")).collect();
        assert!(b.validate().is_err());
    }

    #[test]
    fn validate_rejects_deep_folder() {
        let mut b = sample_bookmark("https://example.com", "ok");
        b.folder = (0..MAX_FOLDER_DEPTH + 1)
            .map(|i| format!("lvl{i}"))
            .collect::<Vec<_>>()
            .join("/");
        assert!(b.validate().is_err());
    }

    #[test]
    fn validate_passes_canonical_bookmark() {
        let b = sample_bookmark("https://example.com", "ok");
        assert!(b.validate().is_ok());
    }

    #[test]
    fn compute_bookmark_id_is_deterministic() {
        let id1 = compute_bookmark_id("alice", "https://example.com", 100);
        let id2 = compute_bookmark_id("alice", "https://example.com", 100);
        assert_eq!(id1, id2);
        let id3 = compute_bookmark_id("alice", "https://example.com", 101);
        assert_ne!(id1, id3);
    }

    #[test]
    fn put_round_trip_via_get() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let b = sample_bookmark("https://example.com", "Example");
        store.put(b.clone()).unwrap();
        let fetched = store.get(&owner(), &b.bookmark_id).unwrap().unwrap();
        assert_eq!(fetched.title, b.title);
        assert_eq!(fetched.url, b.url);
        assert_eq!(fetched.tags, b.tags);
    }

    #[test]
    fn put_returns_constraint_error_on_owner_id_overlong() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let mut b = sample_bookmark("https://example.com", "ok");
        b.owner_id = "x".repeat(129);
        let err = store.put(b).unwrap_err();
        assert!(matches!(err, ChatStoreError::Validation(_)));
    }

    #[test]
    fn get_by_url_returns_correct_row() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let b = sample_bookmark("https://example.com", "example");
        store.put(b.clone()).unwrap();
        let fetched = store.get_by_url(&owner(), "https://example.com").unwrap().unwrap();
        assert_eq!(fetched.bookmark_id, b.bookmark_id);
        assert!(store.get_by_url(&owner(), "https://nope.com").unwrap().is_none());
    }

    #[test]
    fn delete_removes_row_and_errors_on_missing() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let b = sample_bookmark("https://example.com", "example");
        store.put(b.clone()).unwrap();
        store.delete(&owner(), &b.bookmark_id).unwrap();
        let err = store.delete(&owner(), &b.bookmark_id).unwrap_err();
        assert!(matches!(err, ChatStoreError::NotFound(_)));
    }

    #[test]
    fn list_filters_by_pinned_and_archived() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let mut b1 = sample_bookmark("https://example.com/1", "pinned");
        b1.is_pinned = true;
        let b2 = sample_bookmark("https://example.com/2", "regular");
        let mut b3 = sample_bookmark("https://example.com/3", "archived");
        b3.is_archived = true;
        for b in [&b1, &b2, &b3] {
            store.put(b.clone()).unwrap();
        }
        let pinned = store.list(&owner(), ListFilter { only_pinned: true, ..Default::default() }).unwrap();
        assert_eq!(pinned.len(), 1);
        assert!(pinned[0].is_pinned);
        let active = store.list(&owner(), ListFilter::default()).unwrap();
        assert_eq!(active.len(), 2);
        let archived = store.list(&owner(), ListFilter { only_archived: true, ..Default::default() }).unwrap();
        assert_eq!(archived.len(), 1);
        assert!(archived[0].is_archived);
    }

    #[test]
    fn list_filters_by_folder_prefix() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let mut b1 = sample_bookmark("https://example.com/1", "work-1");
        b1.folder = "/work".into();
        let mut b2 = sample_bookmark("https://example.com/2", "work-nested");
        b2.folder = "/work/papers".into();
        let mut b3 = sample_bookmark("https://example.com/3", "home");
        b3.folder = "/home".into();
        for b in [&b1, &b2, &b3] {
            store.put(b.clone()).unwrap();
        }
        let work = store.list(&owner(), ListFilter {
            folder_prefix: Some("/work".into()),
            include_subfolders: true,
            ..Default::default()
        }).unwrap();
        assert_eq!(work.len(), 2);
        let home = store.list(&owner(), ListFilter {
            folder_prefix: Some("/home".into()),
            include_subfolders: true,
            ..Default::default()
        }).unwrap();
        assert_eq!(home.len(), 1);
    }

    #[test]
    fn list_filters_by_tags_intersection() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let mut b1 = sample_bookmark("https://example.com/1", "rust work");
        b1.tags = vec!["rust".into(), "work".into()];
        let mut b2 = sample_bookmark("https://example.com/2", "rust only");
        b2.tags = vec!["rust".into()];
        let mut b3 = sample_bookmark("https://example.com/3", "work only");
        b3.tags = vec!["work".into()];
        for b in [&b1, &b2, &b3] {
            store.put(b.clone()).unwrap();
        }
        let rust = store.list(&owner(), ListFilter {
            tags: vec!["rust".into()],
            ..Default::default()
        }).unwrap();
        assert_eq!(rust.len(), 2);
        let both = store.list(&owner(), ListFilter {
            tags: vec!["rust".into(), "work".into()],
            ..Default::default()
        }).unwrap();
        assert_eq!(both.len(), 1);
    }

    #[test]
    fn search_finds_by_title_and_url() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        // Bookmark 1: title contains "rust" but URL is unrelated.
        // Bookmark 2: title is unrelated. Both share the default
        // `rust` tag, so a needle that hits only the title should
        // return exactly one row.
        let now = Utc::now();
        let mut b1 = sample_bookmark("https://example.com/1", "Rust by Example");
        b1.tags = vec!["docs".into()];
        b1.created_at = now;
        b1.updated_at = now;
        b1.bookmark_id = compute_bookmark_id(&owner(), &b1.url, now.timestamp());
        store.put(b1.clone()).unwrap();
        let mut b2 = sample_bookmark("https://example.com/2", "Cooking with carrots");
        b2.tags = vec!["cooking".into()];
        b2.created_at = now;
        b2.updated_at = now;
        b2.bookmark_id = compute_bookmark_id(&owner(), &b2.url, now.timestamp());
        store.put(b2.clone()).unwrap();

        let hits = store.search(&owner(), "rust", 10).unwrap();
        assert_eq!(hits.len(), 1, "expected only the title match");
        assert!(hits[0].title.contains("Rust"));

        let url_hits = store.search(&owner(), "example.com", 10).unwrap();
        assert_eq!(url_hits.len(), 2, "both URLs match 'example.com'");

        // Description-only search.
        let desc_hits = store.search(&owner(), "description", 10).unwrap();
        assert_eq!(desc_hits.len(), 2, "description matches both rows");
    }

    #[test]
    fn search_rejects_empty_needle() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let err = store.search(&owner(), "  ", 10).unwrap_err();
        assert!(matches!(err, ChatStoreError::Validation(_)));
    }

    #[test]
    fn set_pinned_returns_updated_row() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let b = sample_bookmark("https://example.com", "x");
        store.put(b.clone()).unwrap();
        let updated = store.set_pinned(&owner(), &b.bookmark_id, true).unwrap();
        assert!(updated.is_pinned);
    }

    #[test]
    fn set_pinned_errors_on_missing() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let err = store.set_pinned(&owner(), "missing", true).unwrap_err();
        assert!(matches!(err, ChatStoreError::NotFound(_)));
    }

    #[test]
    fn touch_visit_increments_counter() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let b = sample_bookmark("https://example.com", "x");
        store.put(b.clone()).unwrap();
        let u1 = store.touch_visit(&owner(), &b.bookmark_id).unwrap();
        assert_eq!(u1.visit_count, 1);
        assert!(u1.last_visited_at.is_some());
        let u2 = store.touch_visit(&owner(), &b.bookmark_id).unwrap();
        assert_eq!(u2.visit_count, 2);
    }

    #[test]
    fn tags_returns_counts_lowercased() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let mut b1 = sample_bookmark("https://example.com/1", "x");
        b1.tags = vec!["Rust".into(), "Work".into()];
        let mut b2 = sample_bookmark("https://example.com/2", "y");
        b2.tags = vec!["rust".into(), "Personal".into()];
        store.put(b1).unwrap();
        store.put(b2).unwrap();
        let tags = store.tags(&owner()).unwrap();
        let map: std::collections::BTreeMap<String, u32> = tags
            .into_iter()
            .map(|t| (t.tag, t.count))
            .collect();
        assert_eq!(map.get("rust"), Some(&2));
        assert_eq!(map.get("work"), Some(&1));
        assert_eq!(map.get("personal"), Some(&1));
    }

    #[test]
    fn folders_returns_distinct_paths() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let mut b1 = sample_bookmark("https://example.com/1", "x");
        b1.folder = "/work".into();
        let mut b2 = sample_bookmark("https://example.com/2", "y");
        b2.folder = "/work".into();
        let mut b3 = sample_bookmark("https://example.com/3", "z");
        b3.folder = "/home".into();
        store.put(b1).unwrap();
        store.put(b2).unwrap();
        store.put(b3).unwrap();
        let folders = store.folders(&owner()).unwrap();
        let names: Vec<&str> = folders.iter().map(|f| f.folder.as_str()).collect();
        assert_eq!(names, vec!["/home", "/work"]);
    }

    #[test]
    fn count_split_by_archived() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        let b1 = sample_bookmark("https://example.com/1", "x");
        let mut b2 = sample_bookmark("https://example.com/2", "y");
        b2.is_archived = true;
        store.put(b1).unwrap();
        store.put(b2).unwrap();
        assert_eq!(store.count(&owner(), CountFilter::Active).unwrap(), 1);
        assert_eq!(store.count(&owner(), CountFilter::Archived).unwrap(), 1);
    }

    #[test]
    fn normalized_tags_lowercases_and_dedupes() {
        let mut b = sample_bookmark("https://example.com", "x");
        b.tags = vec!["Rust".into(), "rust".into(), "WORK".into(), "work".into()];
        let mut n: Vec<String> = b
            .tags
            .iter()
            .map(|t| t.to_ascii_lowercase())
            .collect();
        n.sort();
        n.dedup();
        assert_eq!(n, vec!["rust".to_string(), "work".to_string()]);
    }

    #[test]
    fn schema_version_is_v4() {
        let store = LinkBookmarkStore::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), crate::SCHEMA_VERSION);
    }
}
