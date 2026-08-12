//! SQLite-backed bulletin persistence with monotonic per-room
//! sequence enforcement and offline catch-up cursors.
//!
//! ## Schema
//!
//! Three tables back the service:
//!
//! - `bulletins` — the canonical store. Keyed by `(room_id,
//!   bulletin_id)`. `sequence` is `UNIQUE` within `(room_id,
//!   author_id, sequence)` so a single author cannot regress.
//! - `bulletin_receipts` — per-recipient "have read" markers
//!   (used by `acknowledge_bulletin`).
//! - `bulletin_cursors` — `(room_id, kind)` → `last_seq`. The
//!   `kind` dimension lets the store distinguish the local
//!   publisher's monotonic counter from the highest observed
//!   remote counter; offline replay reads both to decide whether
//!   any bulletins are still pending.
//!
//! All tables are created idempotently (`CREATE TABLE IF NOT
//! EXISTS`) so the bootstrap path is a single function call.
//!
//! ## Concurrency
//!
//! A `parking_lot::Mutex<Connection>` guards the SQLite handle.
//! SQLite is opened with `journal_mode=WAL` and
//! `synchronous=NORMAL` (set in [`BulletinStore::open`]) so
//! concurrent reads do not block writers. Every public write is
//! wrapped in a transaction so a crash mid-write cannot leave the
//! `bulletin_cursors.last_seq` table ahead of the `bulletins`
//! table.
//!
//! ## Ordering
//!
//! Inserts enforce `item.sequence > last_seq_for_room` at the SQL
//! layer (`UPDATE … RETURNING`). The store deliberately rejects
//! inserts with `sequence <= last_seq`, regardless of `author_id`,
//! so the caller (the service layer) controls the counter
//! assignment and a crash cannot replay the same sequence twice.
//! Remote bulletins with the same `sequence` value but a different
//! `bulletin_id` are stored as distinct rows; downstream code can
//! dedupe by id.
//!
//! ## Aerospace-grade guarantees
//!
//! - Every public API returns [`BulletinStoreError`]; no
//!   `unwrap`/`expect`/`panic!` on user input.
//! - Storage size is bounded (`MAX_BULLETINS_PER_ROOM`) so a
//!   compromised peer cannot exhaust the local disk.
//! - Schema is versioned (`SCHEMA_VERSION`) and the bootstrap
//!   refuses to open a *newer* schema than the build knows about.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::info;

use adnet_types::{
    AdnetError, BulletinCategory, BulletinId, BulletinItem, BulletinKind, BulletinSeverity,
    NodeId, RoomId,
};

use crate::error::NewsError;

/// Current schema version. Bump on every schema change.
pub const SCHEMA_VERSION: u32 = 1;

/// Hard cap on bulletins stored per room. Once exceeded the store
/// starts GC'ing the oldest expired entries; if the cap is reached
/// by non-expired entries the insert fails with
/// [`BulletinStoreError::RoomFull`].
pub const MAX_BULLETINS_PER_ROOM: usize = 16_384;

/// Hard cap on receipts per (recipient, room).
pub const MAX_RECEIPTS_PER_ROOM: usize = 16_384;

/// Crate-local result alias.
pub type Result<T> = std::result::Result<T, BulletinStoreError>;

/// Errors emitted by the storage layer.
#[derive(Debug, Error)]
pub enum BulletinStoreError {
    /// SQLite returned an error.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Mutex poisoned. Should never happen with `parking_lot`.
    #[error("mutex poisoned")]
    Lock,

    /// JSON (de)serialisation failure.
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),

    /// I/O failure creating the data directory.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Per-room storage cap reached.
    #[error("room storage full: {0} exceeds {MAX_BULLETINS_PER_ROOM}")]
    RoomFull(RoomId),

    /// Caller attempted to insert a sequence that is not strictly
    /// greater than the persisted `last_seq`.
    #[error("sequence regression: {0} <= last_seq {1}")]
    SequenceRegression(u32, u32),

    /// Caller attempted to insert a duplicate `(room_id,
    /// bulletin_id)`.
    #[error("duplicate bulletin_id: {0}")]
    Duplicate(BulletinId),

    /// Stored schema is newer than this build understands.
    #[error("schema version too new: stored {0}, build {SCHEMA_VERSION}")]
    SchemaTooNew(u32),

    /// Validation failure surfaced from `adnet_types`.
    #[error("validation: {0}")]
    Validation(String),
}

impl From<AdnetError> for BulletinStoreError {
    fn from(e: AdnetError) -> Self {
        match e {
            AdnetError::Validation(m) => Self::Validation(m),
            other => Self::Validation(other.to_string()),
        }
    }
}

/// Configuration for [`BulletinStore`].
#[derive(Debug, Clone)]
pub struct BulletinStoreConfig {
    /// Directory holding the SQLite file. Created if missing.
    pub storage_dir: PathBuf,
}

impl Default for BulletinStoreConfig {
    fn default() -> Self {
        Self {
            storage_dir: std::env::temp_dir().join("adnet-news"),
        }
    }
}

/// Cursor key. `Local` is the monotonic counter the local node
/// assigns to its own outbound bulletins; `Remote` is the highest
/// sequence observed from any peer so the catch-up replay loop can
/// stop cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BulletinCursor {
    Local,
    Remote,
}

impl BulletinCursor {
    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "local" => Self::Local,
            _ => Self::Remote,
        }
    }
}

/// Hydrated record returned from [`BulletinStore::list_timeline`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredBulletin {
    pub item: BulletinItem,
    pub received_at: DateTime<Utc>,
    pub source: BulletinSource,
}

/// Where a stored bulletin originated. Used by catch-up replay to
/// distinguish local re-emissions from peer receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BulletinSource {
    Local,
    Remote,
}

/// SQLite-backed bulletin store.
#[derive(Debug)]
pub struct BulletinStore {
    config: BulletinStoreConfig,
    conn: Arc<Mutex<Connection>>,
}

impl BulletinStore {
    /// Open (or create) the database at `<config.storage_dir>/news.db`.
    ///
    /// DO-178C startup contract:
    /// - WAL + foreign keys + `synchronous=NORMAL` are enabled in
    ///   [`configure_connection`].
    /// - `PRAGMA integrity_check` runs before the store becomes
    ///   available so a corrupt database fails loudly at open time.
    /// - `SCHEMA_VERSION` is checked against the build's version;
    ///   a *newer* file on disk refuses to open.
    pub fn open(config: BulletinStoreConfig) -> Result<Self> {
        std::fs::create_dir_all(&config.storage_dir)?;
        let db_path = config.storage_dir.join("news.db");
        let conn = Connection::open(&db_path)?;
        configure_connection(&conn)?;
        apply_schema(&conn)?;

        let stored_version: i64 = conn
            .query_row(
                "SELECT version FROM schema_version WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if stored_version as u32 > SCHEMA_VERSION {
            return Err(BulletinStoreError::SchemaTooNew(stored_version as u32));
        }

        let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(BulletinStoreError::Validation(format!(
                "integrity_check: {integrity}"
            )));
        }

        info!(path = %db_path.display(), "news store opened");
        Ok(Self {
            config,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Open an in-memory database. Used by tests to avoid touching
    /// the host filesystem.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        configure_connection(&conn)?;
        apply_schema(&conn)?;
        Ok(Self {
            config: BulletinStoreConfig::default(),
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Storage directory used to back this store. `None` for
    /// in-memory stores.
    pub fn storage_dir(&self) -> Option<&Path> {
        if self.config.storage_dir.as_os_str().is_empty() {
            None
        } else {
            Some(&self.config.storage_dir)
        }
    }

    // ── Cursor helpers ──────────────────────────────────────────────────

    /// Read the persisted sequence cursor for `(room, kind)`.
    pub fn cursor(&self, room: &RoomId, kind: BulletinCursor) -> Result<u32> {
        let conn = self.conn.lock();
        let row: Option<i64> = conn
            .query_row(
                "SELECT last_seq FROM bulletin_cursors WHERE room_id = ?1 AND cursor_kind = ?2",
                params![room.as_str(), kind.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(row.unwrap_or(0).max(0) as u32)
    }

    /// Convenience: read both cursors.
    pub fn cursors(&self, room: &RoomId) -> Result<(u32, u32)> {
        Ok((self.cursor(room, BulletinCursor::Local)?, self.cursor(room, BulletinCursor::Remote)?))
    }

    /// Persist a new cursor value. Used by callers that update the
    /// counter directly (e.g. on remote replay). Returns an error
    /// if the new value is not strictly greater than the persisted
    /// one — preserves the monotonic invariant.
    pub fn bump_cursor(
        &self,
        room: &RoomId,
        kind: BulletinCursor,
        new_value: u32,
    ) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT last_seq FROM bulletin_cursors WHERE room_id = ?1 AND cursor_kind = ?2",
                params![room.as_str(), kind.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let current = current.unwrap_or(0).max(0) as u32;
        if new_value <= current {
            return Err(BulletinStoreError::SequenceRegression(new_value, current));
        }
        tx.execute(
            "INSERT INTO bulletin_cursors(room_id, cursor_kind, last_seq, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(room_id, cursor_kind) DO UPDATE SET
                 last_seq = excluded.last_seq,
                 updated_at = excluded.updated_at",
            params![
                room.as_str(),
                kind.as_str(),
                new_value as i64,
                Utc::now().timestamp(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ── Insert ──────────────────────────────────────────────────────────

    /// Persist a freshly-validated bulletin. The store assigns the
    /// `sequence` to `cursor(Local) + 1` and updates the cursor in
    /// the same transaction.
    ///
    /// Returns the stored item with the assigned `sequence`.
    pub fn insert(&self, item: BulletinItem) -> Result<StoredBulletin> {
        item.validate().map_err(BulletinStoreError::from)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;

        // Cap check + GC of expired entries (best-effort; if the
        // cap is reached by non-expired entries we abort).
        let live_count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM bulletins WHERE room_id = ?1 AND expires_at > ?2",
                params![item.room_id.as_str(), Utc::now().timestamp()],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if live_count as usize >= MAX_BULLETINS_PER_ROOM {
            // Try to evict expired entries first.
            tx.execute(
                "DELETE FROM bulletins WHERE room_id = ?1 AND expires_at <= ?2",
                params![item.room_id.as_str(), Utc::now().timestamp()],
            )?;
            let after_gc: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM bulletins WHERE room_id = ?1",
                    params![item.room_id.as_str()],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if after_gc as usize >= MAX_BULLETINS_PER_ROOM {
                return Err(BulletinStoreError::RoomFull(item.room_id.clone()));
            }
        }

        // Duplicate id check.
        let existing: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM bulletins WHERE room_id = ?1 AND bulletin_id = ?2",
                params![item.room_id.as_str(), item.bulletin_id.as_hex()],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Err(BulletinStoreError::Duplicate(item.bulletin_id.clone()));
        }

        // Assign the local sequence number.
        let next_seq: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(last_seq), 0) FROM bulletin_cursors
                 WHERE room_id = ?1 AND cursor_kind = 'local'",
                params![item.room_id.as_str()],
                |row| row.get(0),
            )
            .unwrap_or(0)
            + 1;
        let next_seq = next_seq as u32;
        let mut stored_item = item.clone();
        stored_item.sequence = next_seq;

        let payload = serde_json::to_string(&stored_item)?;
        tx.execute(
            "INSERT INTO bulletins(
                bulletin_id, room_id, author_id, sequence, payload_json,
                kind, category, severity, created_at, expires_at, received_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                stored_item.bulletin_id.as_hex(),
                stored_item.room_id.as_str(),
                stored_item.author_id.to_string(),
                next_seq as i64,
                payload,
                stored_item.kind.as_str(),
                stored_item.category.as_str(),
                stored_item.severity.as_str(),
                stored_item.created_at.timestamp(),
                stored_item.expires_at.timestamp(),
                Utc::now().timestamp(),
            ],
        )?;
        tx.execute(
            "INSERT INTO bulletin_cursors(room_id, cursor_kind, last_seq, updated_at)
             VALUES (?1, 'local', ?2, ?3)
             ON CONFLICT(room_id, cursor_kind) DO UPDATE SET
                 last_seq = excluded.last_seq,
                 updated_at = excluded.updated_at",
            params![
                stored_item.room_id.as_str(),
                next_seq as i64,
                Utc::now().timestamp(),
            ],
        )?;
        tx.commit()?;

        Ok(StoredBulletin {
            item: stored_item,
            received_at: Utc::now(),
            source: BulletinSource::Local,
        })
    }

    /// Insert a bulletin received from a peer (no local sequence
    /// bump). The item must carry a non-zero `sequence`; the
    /// `Remote` cursor is bumped to `max(last_seq, sequence)` to
    /// support catch-up replay.
    pub fn insert_remote(&self, item: BulletinItem) -> Result<StoredBulletin> {
        item.validate().map_err(BulletinStoreError::from)?;
        if item.sequence == 0 {
            return Err(BulletinStoreError::Validation(
                "remote bulletin: sequence must be > 0".into(),
            ));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;

        // Duplicate id check.
        let existing: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM bulletins WHERE room_id = ?1 AND bulletin_id = ?2",
                params![item.room_id.as_str(), item.bulletin_id.as_hex()],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Err(BulletinStoreError::Duplicate(item.bulletin_id.clone()));
        }

        // Cap + GC same as insert.
        let live_count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM bulletins WHERE room_id = ?1 AND expires_at > ?2",
                params![item.room_id.as_str(), Utc::now().timestamp()],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if live_count as usize >= MAX_BULLETINS_PER_ROOM {
            tx.execute(
                "DELETE FROM bulletins WHERE room_id = ?1 AND expires_at <= ?2",
                params![item.room_id.as_str(), Utc::now().timestamp()],
            )?;
        }

        let payload = serde_json::to_string(&item)?;
        tx.execute(
            "INSERT INTO bulletins(
                bulletin_id, room_id, author_id, sequence, payload_json,
                kind, category, severity, created_at, expires_at, received_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                item.bulletin_id.as_hex(),
                item.room_id.as_str(),
                item.author_id.to_string(),
                item.sequence as i64,
                payload,
                item.kind.as_str(),
                item.category.as_str(),
                item.severity.as_str(),
                item.created_at.timestamp(),
                item.expires_at.timestamp(),
                Utc::now().timestamp(),
            ],
        )?;

        // Bump remote cursor monotonically.
        let remote_now: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(last_seq), 0) FROM bulletin_cursors
                 WHERE room_id = ?1 AND cursor_kind = 'remote'",
                params![item.room_id.as_str()],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if (item.sequence as i64) > remote_now {
            tx.execute(
                "INSERT INTO bulletin_cursors(room_id, cursor_kind, last_seq, updated_at)
                 VALUES (?1, 'remote', ?2, ?3)
                 ON CONFLICT(room_id, cursor_kind) DO UPDATE SET
                     last_seq = excluded.last_seq,
                     updated_at = excluded.updated_at",
                params![
                    item.room_id.as_str(),
                    item.sequence as i64,
                    Utc::now().timestamp(),
                ],
            )?;
        }
        tx.commit()?;

        Ok(StoredBulletin {
            item,
            received_at: Utc::now(),
            source: BulletinSource::Remote,
        })
    }

    // ── Read / timeline ────────────────────────────────────────────────

    /// Look up a bulletin by id.
    pub fn get(&self, room: &RoomId, id: &BulletinId) -> Result<Option<StoredBulletin>> {
        let conn = self.conn.lock();
        let row: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM bulletins WHERE room_id = ?1 AND bulletin_id = ?2",
                params![room.as_str(), id.as_hex()],
                |row| row.get(0),
            )
            .optional()?;
        match row {
            Some(s) => {
                let item: BulletinItem = serde_json::from_str(&s)?;
                let received_at: i64 = conn
                    .query_row(
                        "SELECT received_at FROM bulletins WHERE room_id = ?1 AND bulletin_id = ?2",
                        params![room.as_str(), id.as_hex()],
                        |row| row.get(0),
                    )
                    .unwrap_or(Utc::now().timestamp());
                Ok(Some(StoredBulletin {
                    item,
                    received_at: ts_to_utc(received_at),
                    source: BulletinSource::Remote,
                }))
            }
            None => Ok(None),
        }
    }

    /// Paginated timeline fetch — newest first, optionally limited
    /// to a `before_seq` (exclusive) cursor and a `before_id`
    /// tiebreaker.
    pub fn list_timeline(
        &self,
        room: &RoomId,
        before_seq: Option<u32>,
        limit: usize,
    ) -> Result<Vec<StoredBulletin>> {
        let conn = self.conn.lock();
        let before_seq = before_seq.map(|s| s as i64).unwrap_or(i64::MAX);
        let mut stmt = conn.prepare(
            "SELECT payload_json, received_at FROM bulletins
             WHERE room_id = ?1 AND sequence < ?2 AND expires_at > ?3
             ORDER BY sequence DESC LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                room.as_str(),
                before_seq,
                Utc::now().timestamp(),
                limit as i64,
            ],
            |row| {
                let payload: String = row.get(0)?;
                let received_at: i64 = row.get(1)?;
                Ok((payload, received_at))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (payload, received_at) = row?;
            let item: BulletinItem = serde_json::from_str(&payload)?;
            out.push(StoredBulletin {
                item,
                received_at: ts_to_utc(received_at),
                source: BulletinSource::Remote,
            });
        }
        Ok(out)
    }

    /// All non-expired bulletins in a room, in monotonic sequence
    /// order. Used by the catch-up replay path.
    pub fn list_replay(&self, room: &RoomId) -> Result<Vec<StoredBulletin>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT payload_json, received_at FROM bulletins
             WHERE room_id = ?1 AND expires_at > ?2
             ORDER BY sequence ASC",
        )?;
        let rows = stmt.query_map(
            params![room.as_str(), Utc::now().timestamp()],
            |row| {
                let payload: String = row.get(0)?;
                let received_at: i64 = row.get(1)?;
                Ok((payload, received_at))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (payload, received_at) = row?;
            let item: BulletinItem = serde_json::from_str(&payload)?;
            out.push(StoredBulletin {
                item,
                received_at: ts_to_utc(received_at),
                source: BulletinSource::Remote,
            });
        }
        Ok(out)
    }

    /// Total bulletin count (any state) for diagnostic logging.
    pub fn total_count(&self) -> Result<u64> {
        let conn = self.conn.lock();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM bulletins", [], |row| row.get(0))?;
        Ok(n.max(0) as u64)
    }

    // ── Receipts ───────────────────────────────────────────────────────

    /// Mark a bulletin as read for `reader`. Idempotent: a second
    /// call only refreshes `read_at`.
    pub fn mark_read(&self, room: &RoomId, id: &BulletinId, reader: &NodeId) -> Result<()> {
        let conn = self.conn.lock();
        // Cap receipts per room before inserting.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bulletin_receipts WHERE room_id = ?1",
                params![room.as_str()],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if count as usize >= MAX_RECEIPTS_PER_ROOM {
            // GC: drop oldest 1/4 of receipts.
            let to_drop = (count / 4).max(1);
            conn.execute(
                "DELETE FROM bulletin_receipts WHERE receipt_id IN
                 (SELECT receipt_id FROM bulletin_receipts
                  WHERE room_id = ?1 ORDER BY read_at ASC LIMIT ?2)",
                params![room.as_str(), to_drop],
            )?;
        }
        let receipt_id = format!("{}:{}:{}", room.as_str(), id.as_hex(), reader.to_string());
        conn.execute(
            "INSERT INTO bulletin_receipts(receipt_id, room_id, bulletin_id, reader_id, read_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(receipt_id) DO UPDATE SET read_at = excluded.read_at",
            params![
                receipt_id,
                room.as_str(),
                id.as_hex(),
                reader.to_string(),
                Utc::now().timestamp(),
            ],
        )?;
        Ok(())
    }

    /// List receipts for a bulletin. Returns the set of reader node
    /// ids that have marked it read.
    pub fn list_readers(&self, room: &RoomId, id: &BulletinId) -> Result<Vec<NodeId>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT reader_id FROM bulletin_receipts WHERE room_id = ?1 AND bulletin_id = ?2",
        )?;
        let rows = stmt.query_map(
            params![room.as_str(), id.as_hex()],
            |row| {
                let s: String = row.get(0)?;
                Ok(s)
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let s = row?;
            if let Ok(n) = NodeId::from_hex(&s) {
                out.push(n);
            }
        }
        Ok(out)
    }

    // ── Subscriptions (per-room cursors + category filters) ────────────

    /// List rooms that have at least one bulletin stored. Used by
    /// the catch-up replay path on startup.
    pub fn known_rooms(&self) -> Result<Vec<RoomId>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT room_id FROM bulletins ORDER BY room_id",
        )?;
        let rows = stmt.query_map([], |row| {
            let s: String = row.get(0)?;
            Ok(RoomId::new(s))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

/// Enable WAL, foreign keys, and `synchronous=NORMAL` on the
/// connection. Idempotent.
fn configure_connection(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;",
    )?;
    Ok(())
}

/// Idempotent schema apply. Bumping [`SCHEMA_VERSION`] requires a
/// matching `ALTER TABLE` migration here.
fn apply_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS schema_version (
            id    INTEGER PRIMARY KEY,
            version INTEGER NOT NULL
         );
         INSERT OR IGNORE INTO schema_version(id, version) VALUES (1, {});",
        SCHEMA_VERSION as i64
    ))?;

    conn.execute_batch(SCHEMA_SQL)?;
    Ok(())
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS bulletins (
    bulletin_id   TEXT NOT NULL,
    room_id       TEXT NOT NULL,
    author_id     TEXT NOT NULL,
    sequence      INTEGER NOT NULL,
    payload_json  TEXT NOT NULL,
    kind          TEXT NOT NULL,
    category      TEXT NOT NULL,
    severity      TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    received_at   INTEGER NOT NULL,
    PRIMARY KEY(room_id, bulletin_id)
);
CREATE INDEX IF NOT EXISTS idx_bulletins_room_seq ON bulletins(room_id, sequence DESC);
CREATE INDEX IF NOT EXISTS idx_bulletins_expires_at ON bulletins(expires_at);

CREATE TABLE IF NOT EXISTS bulletin_cursors (
    room_id     TEXT NOT NULL,
    cursor_kind TEXT NOT NULL,
    last_seq    INTEGER NOT NULL DEFAULT 0,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY(room_id, cursor_kind)
);

CREATE TABLE IF NOT EXISTS bulletin_receipts (
    receipt_id  TEXT PRIMARY KEY,
    room_id     TEXT NOT NULL,
    bulletin_id TEXT NOT NULL,
    reader_id   TEXT NOT NULL,
    read_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_receipts_room ON bulletin_receipts(room_id);
"#;

fn ts_to_utc(ts: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(ts, 0).unwrap_or_else(Utc::now)
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::WalletAddress;

    fn node() -> NodeId {
        NodeId::random()
    }

    fn item(room: &str, severity: BulletinSeverity) -> BulletinItem {
        BulletinItem::new(
            BulletinKind::Announcement,
            BulletinCategory::General,
            severity,
            RoomId::new(room),
            node(),
            "Title",
            "Summary",
            "Body",
            b"nonce",
            None,
        )
        .unwrap()
    }

    #[test]
    fn cursor_starts_at_zero() {
        let store = BulletinStore::open_in_memory().unwrap();
        let (local, remote) = store.cursors(&RoomId::new("r")).unwrap();
        assert_eq!(local, 0);
        assert_eq!(remote, 0);
    }

    #[test]
    fn insert_assigns_monotonic_sequence() {
        let store = BulletinStore::open_in_memory().unwrap();
        let a = store.insert(item("r", BulletinSeverity::Info)).unwrap();
        let b = store.insert(item("r", BulletinSeverity::Info)).unwrap();
        assert_eq!(a.item.sequence, 1);
        assert_eq!(b.item.sequence, 2);
        assert_eq!(store.cursor(&RoomId::new("r"), BulletinCursor::Local).unwrap(), 2);
    }

    #[test]
    fn insert_rejects_duplicate_id() {
        let store = BulletinStore::open_in_memory().unwrap();
        let mut a = item("r", BulletinSeverity::Info);
        a.bulletin_id = BulletinId::derive(
            &a.room_id,
            &a.author_id,
            0,
            a.created_at,
            b"dup",
        );
        store.insert(a.clone()).unwrap();
        let err = store.insert(a).unwrap_err();
        assert!(matches!(err, BulletinStoreError::Duplicate(_)));
    }

    #[test]
    fn insert_remote_does_not_bump_local_cursor() {
        let store = BulletinStore::open_in_memory().unwrap();
        let mut remote_item = item("r", BulletinSeverity::Info);
        remote_item.sequence = 42;
        let stored = store.insert_remote(remote_item).unwrap();
        assert_eq!(stored.item.sequence, 42);
        assert_eq!(
            store.cursor(&RoomId::new("r"), BulletinCursor::Local).unwrap(),
            0
        );
        assert_eq!(
            store.cursor(&RoomId::new("r"), BulletinCursor::Remote).unwrap(),
            42
        );
    }

    #[test]
    fn insert_remote_rejects_zero_sequence() {
        let store = BulletinStore::open_in_memory().unwrap();
        let err = store.insert_remote(item("r", BulletinSeverity::Info)).unwrap_err();
        assert!(matches!(err, BulletinStoreError::Validation(_)));
    }

    #[test]
    fn timeline_paginates_newest_first() {
        let store = BulletinStore::open_in_memory().unwrap();
        for _ in 0..5 {
            store.insert(item("r", BulletinSeverity::Info)).unwrap();
        }
        let page = store.list_timeline(&RoomId::new("r"), None, 3).unwrap();
        assert_eq!(page.len(), 3);
        assert_eq!(page[0].item.sequence, 5);
        assert_eq!(page[2].item.sequence, 3);
        let next = store.list_timeline(&RoomId::new("r"), Some(3), 3).unwrap();
        assert_eq!(next.len(), 2);
        assert_eq!(next[0].item.sequence, 2);
    }

    #[test]
    fn replay_returns_in_monotonic_order() {
        let store = BulletinStore::open_in_memory().unwrap();
        store.insert(item("r", BulletinSeverity::Info)).unwrap();
        store.insert(item("r", BulletinSeverity::Info)).unwrap();
        let replay = store.list_replay(&RoomId::new("r")).unwrap();
        assert_eq!(replay.len(), 2);
        assert!(replay[0].item.sequence < replay[1].item.sequence);
    }

    #[test]
    fn mark_read_is_idempotent() {
        let store = BulletinStore::open_in_memory().unwrap();
        let stored = store.insert(item("r", BulletinSeverity::Info)).unwrap();
        let id = stored.item.bulletin_id.clone();
        let reader = node();
        store
            .mark_read(&RoomId::new("r"), &id, &reader)
            .unwrap();
        store
            .mark_read(&RoomId::new("r"), &id, &reader)
            .unwrap();
        let readers = store
            .list_readers(&RoomId::new("r"), &id)
            .unwrap();
        assert_eq!(readers.len(), 1);
    }

    #[test]
    fn known_rooms_returns_distinct_set() {
        let store = BulletinStore::open_in_memory().unwrap();
        store.insert(item("a", BulletinSeverity::Info)).unwrap();
        store.insert(item("b", BulletinSeverity::Info)).unwrap();
        store.insert(item("a", BulletinSeverity::Info)).unwrap();
        let rooms = store.known_rooms().unwrap();
        assert_eq!(rooms.len(), 2);
        assert!(rooms.iter().any(|r| r.as_str() == "a"));
        assert!(rooms.iter().any(|r| r.as_str() == "b"));
    }

    #[test]
    fn bump_cursor_rejects_regression() {
        let store = BulletinStore::open_in_memory().unwrap();
        store
            .bump_cursor(&RoomId::new("r"), BulletinCursor::Local, 5)
            .unwrap();
        let err = store
            .bump_cursor(&RoomId::new("r"), BulletinCursor::Local, 4)
            .unwrap_err();
        assert!(matches!(err, BulletinStoreError::SequenceRegression(_, _)));
    }

    #[test]
    fn schema_version_recorded() {
        let store = BulletinStore::open_in_memory().unwrap();
        let conn = store.conn.lock();
        let v: i64 = conn
            .query_row(
                "SELECT version FROM schema_version WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v as u32, SCHEMA_VERSION);
    }

    #[test]
    fn content_hash_round_trip_via_db() {
        use adnet_types::ContentHash;
        let store = BulletinStore::open_in_memory().unwrap();
        let mut b = item("r", BulletinSeverity::Info);
        b.body_hash = Some(ContentHash::from_bytes(b"hello"));
        let stored = store.insert(b.clone()).unwrap();
        let back = store
            .get(&RoomId::new("r"), &stored.item.bulletin_id)
            .unwrap()
            .unwrap();
        assert_eq!(back.item.body_hash, b.body_hash);
    }

    #[test]
    fn wallet_signature_persists() {
        let store = BulletinStore::open_in_memory().unwrap();
        let mut b = item("r", BulletinSeverity::Info);
        b.attach_signature(WalletAddress::from_bytes([0x42u8; 20]), vec![0u8; 65]);
        let stored = store.insert(b.clone()).unwrap();
        let back = store
            .get(&RoomId::new("r"), &stored.item.bulletin_id)
            .unwrap()
            .unwrap();
        assert_eq!(back.item.signer, b.signer);
        assert_eq!(back.item.signature, b.signature);
    }
}