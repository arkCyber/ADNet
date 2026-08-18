//! SQLite-backed [`MailboxStore`] implementation.
//!
//! ## Design
//!
//! - **Single file**: each mailbox server instance is backed by one
//!   SQLite database (or `:memory:` for tests).
//! - **Per-recipient connection pool**: connections are opened lazily
//!   on first use and cached in a `DashMap<UserId,
//!   Arc<tokio::sync::Mutex<Connection>>>`. This mirrors the
//!   `a3chat-app` pattern exactly.
//! - **WAL mode**: `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL`.
//! - **Atomic transactions**: every mutating operation runs inside
//!   `Connection::transaction()`. The `enqueue` path holds the lock for
//!   the idempotency check + insert, guaranteeing that the unique
//!   constraint never fires for a genuine duplicate.
//! - **Idempotency**: `(sender_id, recipient_id, msg_id)` is UNIQUE.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::error::{MailboxError, MailboxResult};
use crate::storage::{
    EnqueueOutcome, MailboxStore, QuotaUsage, StoredEnvelope, Watermark,
};

/// Maximum prepared statement cache per connection.
const STMT_CACHE_SIZE: usize = 64;

/// SQLite store for the mailbox.
///
/// Implements [`MailboxStore`] with a single SQLite database file.
/// Connections are pooled per-recipient for concurrent multi-user access.
#[derive(Debug)]
pub struct SqliteStore {
    /// Path to the SQLite database file. `:memory:` is also accepted.
    path: String,
    /// Per-recipient connections. Opened lazily on first access, kept
    /// open until the store is dropped.
    connections: DashMap<String, Arc<Mutex<rusqlite::Connection>>>,
}

impl SqliteStore {
    /// Open (or create) a SQLite mailbox store at `path`.
    ///
    /// - A real filesystem path: creates the database file if absent.
    /// - `:memory:`: each connection is a separate in-memory database.
    pub fn open<P: AsRef<Path>>(path: P) -> MailboxResult<Self> {
        let path = path
            .as_ref()
            .to_str()
            .ok_or_else(|| MailboxError::Config("invalid sqlite path".into()))?
            .to_string();
        Ok(Self { path, connections: DashMap::new() })
    }

    /// Open a store backed by a temporary file. The file is deleted when
    /// the store is dropped.
    pub fn open_temp() -> std::io::Result<(Self, tempfile::TempPath)> {
        let tmp = tempfile::Builder::new()
            .prefix("a3net-mailbox-")
            .suffix(".db")
            .rand_bytes(8)
            .tempfile()?;
        let path = tmp.path().to_path_buf();
        let store =
            Self::open(&path).map_err(std::io::Error::other)?;
        Ok((store, tmp.into_temp_path()))
    }

    /// Configure a fresh SQLite connection: WAL mode, schema, cache.
    fn configure(path: &str) -> Result<rusqlite::Connection, rusqlite::Error> {
        let conn = rusqlite::Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;
             PRAGMA cache_size=-64000;",
        )?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS envelopes (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                sender_id       TEXT NOT NULL,
                recipient_id    TEXT NOT NULL,
                msg_id          TEXT NOT NULL,
                ciphertext      BLOB NOT NULL,
                sender_signature BLOB NOT NULL,
                sequence        INTEGER NOT NULL,
                queued_at       TEXT NOT NULL,
                expires_at      TEXT NOT NULL,
                UNIQUE(sender_id, recipient_id, msg_id)
            );
            CREATE INDEX IF NOT EXISTS idx_recipient_sequence
                ON envelopes(recipient_id, sequence ASC);
            CREATE INDEX IF NOT EXISTS idx_expires_at
                ON envelopes(expires_at);",
        )?;
        conn.set_prepared_statement_cache_capacity(STMT_CACHE_SIZE);
        Ok(conn)
    }

    /// Get or open the pooled connection for a recipient.
    fn get_conn(&self, recipient_id: &str) -> Arc<Mutex<rusqlite::Connection>> {
        if let Some(c) = self.connections.get(recipient_id) {
            return c.clone();
        }
        let conn =
            Self::configure(&self.path).expect("SqliteStore::open validated the path");
        let arc = Arc::new(Mutex::new(conn));
        self.connections.insert(recipient_id.to_string(), arc.clone());
        arc
    }

    /// Run a read-only blocking operation on the per-recipient connection.
    async fn read<F, T>(&self, rid: String, f: F) -> MailboxResult<T>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T, MailboxError> + Send + 'static,
        T: Send + 'static,
    {
        let arc = self.get_conn(&rid);
        tokio::task::spawn_blocking(move || {
            let guard = arc.blocking_lock();
            f(&guard)
        })
        .await
        .map_err(|e| MailboxError::Internal(format!("{e}")))?
    }

    /// Run a write (transaction) operation on the per-recipient connection.
    async fn write<F, T>(&self, rid: String, f: F) -> MailboxResult<T>
    where
        F: FnOnce(&rusqlite::Transaction) -> Result<T, MailboxError> + Send + 'static,
        T: Send + 'static,
    {
        let arc = self.get_conn(&rid);
        tokio::task::spawn_blocking(move || {
            let guard = arc.blocking_lock();
            let tx =
                guard.unchecked_transaction().map_err(|e| MailboxError::Storage(e.to_string()))?;
            let result = f(&tx);
            if result.is_ok() {
                tx.commit().map_err(|e| MailboxError::Storage(e.to_string()))?;
            }
            // tx dropped here on Err → implicit rollback
            result
        })
        .await
        .map_err(|e| MailboxError::Internal(format!("{e}")))?
    }

    fn row_to_envelope(row: &rusqlite::Row<'_>) -> Result<StoredEnvelope, rusqlite::Error> {
        Ok(StoredEnvelope {
            sender_id: row.get("sender_id")?,
            recipient_id: row.get("recipient_id")?,
            msg_id: row.get("msg_id")?,
            ciphertext: row.get("ciphertext")?,
            sender_signature: row.get("sender_signature")?,
            sequence: row.get::<_, i64>("sequence")? as u64,
            queued_at: DateTime::parse_from_rfc3339(&row.get::<_, String>("queued_at")?)
                .unwrap_or_else(|_| Utc::now().into())
                .with_timezone(&Utc),
            expires_at: DateTime::parse_from_rfc3339(&row.get::<_, String>("expires_at")?)
                .unwrap_or_else(|_| Utc::now().into())
                .with_timezone(&Utc),
        })
    }

    fn row_to_tuple(
        row: &rusqlite::Row<'_>,
    ) -> Result<(i64, String, String), rusqlite::Error> {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    }
}

#[async_trait]
impl MailboxStore for SqliteStore {
    async fn enqueue(&self, env: &StoredEnvelope) -> MailboxResult<EnqueueOutcome> {
        let sender_id = env.sender_id.clone();
        let recipient_id = env.recipient_id.clone();
        let msg_id = env.msg_id.clone();
        let ciphertext = env.ciphertext.clone();
        let sig = env.sender_signature.clone();
        let queued_at = env.queued_at;
        let expires_at = env.expires_at;

        self.write(recipient_id.clone(), move |tx| {
            // Step 1: idempotency check (EXISTS probe avoids NoRows error).
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM envelopes
                        WHERE sender_id = ?1 AND recipient_id = ?2 AND msg_id = ?3)",
                    rusqlite::params![&sender_id, &recipient_id, &msg_id],
                    |r| r.get(0),
                )
                .map_err(|e| MailboxError::Storage(e.to_string()))?;

            if exists {
                // Return the original stored outcome.
                let (seq, qat, eat) = tx
                    .query_row(
                        "SELECT sequence, queued_at, expires_at FROM envelopes
                         WHERE sender_id = ?1 AND recipient_id = ?2 AND msg_id = ?3",
                        rusqlite::params![&sender_id, &recipient_id, &msg_id],
                        Self::row_to_tuple,
                    )
                    .map_err(|e| MailboxError::Storage(e.to_string()))?;
                let qat_dt = DateTime::parse_from_rfc3339(&qat)
                    .unwrap_or_else(|_| Utc::now().into())
                    .with_timezone(&Utc);
                let eat_dt = DateTime::parse_from_rfc3339(&eat)
                    .unwrap_or_else(|_| Utc::now().into())
                    .with_timezone(&Utc);
                return Ok(EnqueueOutcome {
                    msg_id: msg_id.clone(),
                    sequence: seq as u64,
                    queued_at: qat_dt,
                    expires_at: eat_dt,
                    duplicate: true,
                });
            }

            // Step 2: assign sequence = MAX + 1.
            let max_seq: Option<i64> = tx
                .query_row(
                    "SELECT MAX(sequence) FROM envelopes WHERE recipient_id = ?",
                    [&recipient_id],
                    |r| r.get(0),
                )
                .ok();
            let seq = max_seq.unwrap_or(0) + 1;

            // Step 3: insert.
            tx.execute(
                "INSERT INTO envelopes
                 (sender_id, recipient_id, msg_id, ciphertext, sender_signature,
                  sequence, queued_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    &sender_id,
                    &recipient_id,
                    &msg_id,
                    &ciphertext,
                    &sig,
                    seq,
                    queued_at.to_rfc3339(),
                    expires_at.to_rfc3339(),
                ],
            )
            .map_err(|e| MailboxError::Storage(e.to_string()))?;

            debug!(recipient_id=%recipient_id, msg_id=%msg_id, sequence=%seq, "envelope enqueued");
            Ok(EnqueueOutcome { msg_id, sequence: seq as u64, queued_at, expires_at, duplicate: false })
        })
        .await
    }

    async fn pull(
        &self,
        recipient_id: &str,
        since: Watermark,
        limit: usize,
    ) -> MailboxResult<Vec<StoredEnvelope>> {
        let rid = recipient_id.to_string();
        self.read(rid.clone(), move |conn| {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, sender_id, recipient_id, msg_id,
                            ciphertext, sender_signature, sequence,
                            queued_at, expires_at
                     FROM envelopes
                     WHERE recipient_id = ? AND sequence > ?
                     ORDER BY sequence ASC LIMIT ?",
                )
                .map_err(|e| MailboxError::Storage(e.to_string()))?;

            let rows = stmt
                .query_map(
                    rusqlite::params![&rid, since as i64, limit as i64],
                    Self::row_to_envelope,
                )
                .map_err(|e| MailboxError::Storage(e.to_string()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| MailboxError::Storage(e.to_string()))?;

            Ok(rows)
        })
        .await
    }

    async fn ack(&self, recipient_id: &str, msg_ids: &[String]) -> MailboxResult<usize> {
        if msg_ids.is_empty() {
            return Ok(0);
        }
        let rid = recipient_id.to_string();
        let ids = msg_ids.to_vec();

        self.write(rid.clone(), move |tx| {
            let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "DELETE FROM envelopes WHERE recipient_id = ? AND msg_id IN ({})",
                placeholders
            );
            let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(rid.clone())];
            for id in &ids {
                params.push(Box::new(id.clone()));
            }
            let params_refs: Vec<&dyn rusqlite::ToSql> =
                params.iter().map(|b| b.as_ref()).collect();
            let deleted = tx
                .execute(&sql, params_refs.as_slice())
                .map_err(|e| MailboxError::Storage(e.to_string()))?;
            debug!(recipient_id=%rid, removed=%deleted, "ack completed");
            Ok(deleted)
        })
        .await
    }

    async fn purge_expired(&self) -> MailboxResult<u64> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = Self::configure(&path).map_err(|e| MailboxError::Storage(e.to_string()))?;
            let now = Utc::now().to_rfc3339();
            let n = conn
                .execute("DELETE FROM envelopes WHERE expires_at <= ?", [&now])
                .map_err(|e| MailboxError::Storage(e.to_string()))?;
            info!(removed=%n, "purge_expired completed");
            Ok(n as u64)
        })
        .await
        .map_err(|e| MailboxError::Internal(format!("{e}")))?
    }

    async fn quota_usage(&self, recipient_id: &str) -> MailboxResult<QuotaUsage> {
        let rid = recipient_id.to_string();
        self.read(rid.clone(), move |conn| {
            let now = Utc::now().to_rfc3339();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM envelopes WHERE recipient_id = ? AND expires_at > ?",
                    rusqlite::params![&rid, &now],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let total_bytes: i64 = conn
                .query_row(
                    "SELECT COALESCE(SUM(LENGTH(ciphertext) + LENGTH(sender_signature) + 236), 0)
                     FROM envelopes WHERE recipient_id = ? AND expires_at > ?",
                    rusqlite::params![&rid, &now],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let high_watermark: Option<i64> = conn
                .query_row(
                    "SELECT MAX(sequence) FROM envelopes WHERE recipient_id = ?",
                    [&rid],
                    |r| r.get(0),
                )
                .ok();
            Ok(QuotaUsage {
                message_count: count as usize,
                total_bytes: total_bytes as u64,
                high_watermark: high_watermark.unwrap_or(0) as u64,
            })
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_env(sender: &str, recipient: &str, msg_id: &str, ct: &[u8]) -> StoredEnvelope {
        StoredEnvelope {
            sender_id: sender.to_string(),
            recipient_id: recipient.to_string(),
            msg_id: msg_id.to_string(),
            ciphertext: ct.to_vec(),
            sender_signature: vec![0xab; 65],
            sequence: 0,
            queued_at: Utc::now(),
            expires_at: Utc::now() + Duration::days(7),
        }
    }

    const ALICE: &str = "0x0000000000000000000000000000000000000001";
    const BOB: &str = "0x0000000000000000000000000000000000000002";
    const CAROL: &str = "0x0000000000000000000000000000000000000003";

    #[tokio::test]
    async fn sequences_are_dense_and_monotonic() {
        let (s, _tmp) = SqliteStore::open_temp().unwrap();
        for i in 1..=10 {
            let e = make_env(
                ALICE, BOB,
                &format!("m{i}aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                b"x",
            );
            let out = s.enqueue(&e).await.unwrap();
            assert_eq!(out.sequence, i as u64);
            assert!(!out.duplicate);
        }
    }

    #[tokio::test]
    async fn duplicate_returns_original() {
        let (s, _tmp) = SqliteStore::open_temp().unwrap();
        let e = make_env(
            ALICE, BOB,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            b"dup",
        );
        let first = s.enqueue(&e).await.unwrap();
        assert!(!first.duplicate);
        let second = s.enqueue(&e).await.unwrap();
        assert!(second.duplicate);
        assert_eq!(first.sequence, second.sequence);
    }

    #[tokio::test]
    async fn pull_respects_watermark() {
        let (s, _tmp) = SqliteStore::open_temp().unwrap();
        for i in 1..=5 {
            let e = make_env(
                ALICE, BOB,
                &format!("pw{i}bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                b"x",
            );
            s.enqueue(&e).await.unwrap();
        }
        let out = s.pull(BOB, 2, 100).await.unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].sequence, 3);
        assert_eq!(out[2].sequence, 5);
    }

    #[tokio::test]
    async fn pull_respects_limit() {
        let (s, _tmp) = SqliteStore::open_temp().unwrap();
        for i in 1..=10 {
            let e = make_env(
                ALICE, BOB,
                &format!("pl{i}cccccccccccccccccccccccccccccccccccccccccccccccccccccc"),
                b"x",
            );
            s.enqueue(&e).await.unwrap();
        }
        let out = s.pull(BOB, 0, 3).await.unwrap();
        assert_eq!(out.len(), 3);
    }

    #[tokio::test]
    async fn ack_removes_envelopes() {
        let (s, _tmp) = SqliteStore::open_temp().unwrap();
        let mut ids = Vec::new();
        for i in 1..=3 {
            let e = make_env(
                ALICE, BOB,
                &format!("a{i}ddddddddddddddddddddddddddddddddddddddddddddddddddd"),
                b"x",
            );
            ids.push(s.enqueue(&e).await.unwrap().msg_id);
        }
        let removed = s.ack(BOB, &[ids[0].clone(), ids[1].clone()]).await.unwrap();
        assert_eq!(removed, 2);
        let remaining = s.pull(BOB, 0, 100).await.unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[tokio::test]
    async fn purge_removes_expired_only() {
        let (s, _tmp) = SqliteStore::open_temp().unwrap();
        let mut live = make_env(ALICE, BOB,
            "live1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", b"x");
        live.expires_at = Utc::now() + Duration::hours(1);
        let mut dead = make_env(ALICE, BOB,
            "dead1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", b"x");
        dead.expires_at = Utc::now() - Duration::seconds(1);
        s.enqueue(&live).await.unwrap();
        s.enqueue(&dead).await.unwrap();
        let removed = s.purge_expired().await.unwrap();
        assert_eq!(removed, 1);
        let remaining = s.pull(BOB, 0, 100).await.unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[tokio::test]
    async fn quota_usage_is_accurate() {
        let (s, _tmp) = SqliteStore::open_temp().unwrap();
        let e = make_env(ALICE, BOB,
            "q1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", b"hello world");
        s.enqueue(&e).await.unwrap();
        let u = s.quota_usage(BOB).await.unwrap();
        assert_eq!(u.message_count, 1);
        assert!(u.total_bytes > 0);
        assert_eq!(u.high_watermark, 1);
        let empty = s.quota_usage("nope").await.unwrap();
        assert_eq!(empty.message_count, 0);
    }

    #[tokio::test]
    async fn recipients_are_isolated() {
        let (s, _tmp) = SqliteStore::open_temp().unwrap();
        s.enqueue(&make_env(ALICE, BOB,
            "r1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", b"x")).await.unwrap();
        s.enqueue(&make_env(ALICE, CAROL,
            "r2aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", b"x")).await.unwrap();
        let bobs = s.pull(BOB, 0, 100).await.unwrap();
        let carols = s.pull(CAROL, 0, 100).await.unwrap();
        assert_eq!(bobs.len(), 1);
        assert_eq!(carols.len(), 1);
    }

    #[tokio::test]
    async fn sequences_are_unique() {
        let (s, _tmp) = SqliteStore::open_temp().unwrap();
        let mut seqs = Vec::new();
        for i in 0..20 {
            let e = make_env(
                ALICE, BOB,
                &format!("sq{i}eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
                format!("msg {i}").as_bytes(),
            );
            seqs.push(s.enqueue(&e).await.unwrap().sequence);
        }
        let unique: std::collections::HashSet<_> = seqs.iter().collect();
        assert_eq!(unique.len(), seqs.len(), "all sequences must be unique: {seqs:?}");
        let mut sorted = seqs.clone();
        sorted.sort();
        assert_eq!(sorted, (1u64..=20).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn ack_unknown_recipient_returns_zero() {
        let (s, _tmp) = SqliteStore::open_temp().unwrap();
        let removed = s.ack(
            "nope",
            &["xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".to_string()],
        ).await.unwrap();
        assert_eq!(removed, 0);
    }
}
