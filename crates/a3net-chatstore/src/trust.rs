//! Per-user chat-trust table — the bridge between user-facing
//! trust decisions and the global [`a3net_reputation`] subsystem.
//!
//! The `chat_trust` table lives in the same SQLite file as the
//! rest of the chat store. It is **owned** by the chat layer (the
//! chat layer is the canonical source of user-attributed trust)
//! but every write also produces a
//! [`a3net_reputation::ReputationEvent::ChatTrustSet`] /
//! `ChatTrustReport` so the global PeerScore table can react.
//!
//! ## Why a separate crate's-perspective bridge?
//!
//! A reputation table that only knew about `NodeId` would miss the
//! "user A trusts user B" signal — which is the most actionable
//! signal we have. Putting the table in `a3net-chatstore` keeps the
//! canonical write path in the same transaction as the chat
//! activity that triggered the trust change, and reading from the
//! chat layer is a single SQL query away.

use std::sync::{Arc, Mutex as StdMutex};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::debug;

use crate::error::{ChatStoreError, Result};
/// Persistent record of a user's trust judgement of another user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTrustRecord {
    /// Local user issuing the judgement.
    pub owner_user_id: String,
    /// Peer being judged.
    pub target_user_id: String,
    /// Trust level in `[-3, +3]`. The numeric tag matches
    /// [`a3net_reputation::TrustLevel`].
    pub level: i8,
    /// Unix timestamp (seconds) of the last change.
    pub last_event_unix: i64,
    /// Number of trust-modifying events observed for this pair.
    pub event_count: u64,
    /// Optional free-form notes ("this is Bob's laptop, see DM
    /// from 2026-01-01").
    pub notes: Option<String>,
}

impl ChatTrustRecord {
    /// Construct a fresh record at the current time.
    pub fn new(owner_user_id: String, target_user_id: String, level: i8) -> Self {
        debug_assert!((-3..=3).contains(&level), "trust level out of range");
        Self {
            owner_user_id,
            target_user_id,
            level,
            last_event_unix: Utc::now().timestamp(),
            event_count: 1,
            notes: None,
        }
    }

    /// Translate to the [`a3net_reputation::TrustLevel`] enum.
    pub fn trust_level(&self) -> a3net_reputation::TrustLevel {
        a3net_reputation::TrustLevel::from_i8(self.level)
    }
}

/// Handle to the `chat_trust` table. Cheap to clone.
///
/// `db` is an `Arc<tokio::sync::Mutex<Connection>>` so it can share
/// the same per-user SQLite connection with the rest of the chat
/// storage layer (which uses `tokio::sync::Mutex` for the same
/// reason — to interleave with `spawn_blocking`).
#[derive(Debug, Clone)]
pub struct ChatTrustStore {
    db: Arc<Mutex<Connection>>,
    /// Optional reputation hook. When set, every `set` call emits a
    /// `ChatTrustSet` event into the global PeerScore so that the
    /// user's chat-side trust judgement immediately influences
    /// gossip / bitswap routing decisions. Held under a
    /// `StdMutex<Option<_>>` because we only mutate it during
    /// configuration (never across an `.await`).
    reputation: Arc<StdMutex<Option<a3net_reputation::ReputationReporter>>>,
}

impl ChatTrustStore {
    /// Construct from an existing chat-storage connection handle.
    /// No reputation reporter is attached by default — use
    /// [`with_reputation`](Self::with_reputation) to wire one in.
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self {
            db,
            reputation: Arc::new(StdMutex::new(None)),
        }
    }

    /// Install a reputation reporter. Subsequent `set` calls will
    /// feed a `ChatTrustSet` event into the PeerScore table. The
    /// reporter is stored behind a `Mutex<Option<_>>` so callers
    /// can swap it out at runtime (e.g. once persistence is
    /// available in tests).
    pub fn with_reputation(
        &self,
        reporter: a3net_reputation::ReputationReporter,
    ) -> &Self {
        *self.reputation.lock().expect("reputation mutex") = Some(reporter);
        self
    }

    /// Borrow the currently installed reporter, if any.
    pub fn reputation(&self) -> Option<a3net_reputation::ReputationReporter> {
        self.reputation.lock().expect("reputation mutex").clone()
    }

    /// Map a user-id string to a stable numeric id for reputation
    /// events. Hashes the id with `blake3` and truncates to 8 bytes
    /// — this is **stable across runs** for the same user-id but
    /// does not have to be globally unique because
    /// `ReputationEvent::ChatTrustSet` carries it as an opaque
    /// discriminator (the PeerScore table only keys on `peer`).
    fn user_to_u64(user_id: &str) -> u64 {
        let h = blake3::hash(user_id.as_bytes());
        let bytes = h.as_bytes();
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&bytes[..8]);
        u64::from_le_bytes(buf)
    }

    /// Look up the trust record for `(owner, target)`. Returns
    /// `None` if the pair has no recorded trust.
    pub async fn get(
        &self,
        owner_user_id: &str,
        target_user_id: &str,
    ) -> Result<Option<ChatTrustRecord>> {
        let conn = self.db.lock().await;
        let row = conn
            .query_row(
                "SELECT owner_user_id, target_user_id, level, last_event_unix, event_count, notes
                 FROM chat_trust WHERE owner_user_id = ?1 AND target_user_id = ?2",
                params![owner_user_id, target_user_id],
                |r| {
                    Ok(ChatTrustRecord {
                        owner_user_id: r.get(0)?,
                        target_user_id: r.get(1)?,
                        level: r.get(2)?,
                        last_event_unix: r.get(3)?,
                        event_count: r.get(4)?,
                        notes: r.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Upsert a trust record. The `event_count` is incremented on
    /// existing rows; the `last_event_unix` is always bumped to the
    /// current time.
    ///
    /// If a reputation reporter is attached via
    /// [`with_reputation`](Self::with_reputation), this call also
    /// emits a [`a3net_reputation::ReputationEvent::ChatTrustSet`]
    /// into the global PeerScore so that the user's chat-side
    /// trust judgement immediately affects routing / bandwidth
    /// decisions in gossip and bitswap. The `peer` field of the
    /// event is derived deterministically from `target_user_id`
    /// (`blake3` hash → first 32 bytes → `NodeId`); this is
    /// intentional because chat trust is a user-level decision
    /// that should apply uniformly across all transport identities
    /// owned by that user.
    pub async fn set(
        &self,
        owner_user_id: &str,
        target_user_id: &str,
        level: i8,
        notes: Option<String>,
    ) -> Result<ChatTrustRecord> {
        if !(-3..=3).contains(&level) {
            return Err(ChatStoreError::InvalidTrustLevel { level });
        }
        if owner_user_id.is_empty() || target_user_id.is_empty() {
            return Err(ChatStoreError::InvalidId(
                "chat_trust ids must be non-empty".into(),
            ));
        }
        if owner_user_id == target_user_id {
            return Err(ChatStoreError::InvalidId(
                "owner and target must differ".into(),
            ));
        }
        let now = Utc::now().timestamp();
        let conn = self.db.lock().await;
        // Use UPSERT semantics so the insert is idempotent on
        // restart. `event_count` is incremented atomically.
        conn.execute(
            "INSERT INTO chat_trust (owner_user_id, target_user_id, level, last_event_unix, event_count, notes)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)
             ON CONFLICT(owner_user_id, target_user_id) DO UPDATE SET
                 level = excluded.level,
                 last_event_unix = excluded.last_event_unix,
                 event_count = chat_trust.event_count + 1,
                 notes = excluded.notes",
            params![owner_user_id, target_user_id, level, now, notes],
        )?;
        drop(conn);
        debug!(owner = owner_user_id, target = target_user_id, level, "chat trust set");

        // Reputation hook: emit a ChatTrustSet event so the global
        // PeerScore can react. We map the user-id strings to
        // stable NodeId values (hash-derived) so the same user
        // produces the same score entry across devices / sessions.
        if let Some(rep) = self.reputation.lock().expect("reputation mutex").as_ref() {
            let by_user = Self::user_to_u64(owner_user_id);
            let target_node = Self::user_to_node(target_user_id);
            a3net_reputation::reporter::ChatSignal(rep).set_trust(
                target_node,
                by_user,
                level,
            );
        }

        Ok(ChatTrustRecord {
            owner_user_id: owner_user_id.to_string(),
            target_user_id: target_user_id.to_string(),
            level,
            last_event_unix: now,
            event_count: 1,
            notes,
        })
    }

    /// Map a chat user-id string to a deterministic `NodeId` for
    /// reputation bookkeeping. Pure function — same input ⇒ same
    /// output across processes.
    fn user_to_node(user_id: &str) -> a3net_types::NodeId {
        let h = blake3::hash(user_id.as_bytes());
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&h.as_bytes()[..32]);
        // `NodeId::from_bytes` validates length and falls back to a
        // sentinel on error; we already pinned length to 32 so
        // this is infallible in practice.
        a3net_types::NodeId::from_bytes(&bytes)
            .unwrap_or_else(|_| a3net_types::NodeId::from_bytes(&[0u8; 32]).expect("zero NodeId is valid"))
    }

    /// Delete a trust record. Used when a user "clears" their
    /// trust judgement.
    pub async fn clear(&self, owner_user_id: &str, target_user_id: &str) -> Result<bool> {
        let conn = self.db.lock().await;
        let n = conn.execute(
            "DELETE FROM chat_trust WHERE owner_user_id = ?1 AND target_user_id = ?2",
            params![owner_user_id, target_user_id],
        )?;
        Ok(n > 0)
    }

    /// List every trust record owned by `owner_user_id`, sorted by
    /// level descending (most-trusted first).
    pub async fn list_for_owner(&self, owner_user_id: &str) -> Result<Vec<ChatTrustRecord>> {
        let conn = self.db.lock().await;
        let mut stmt = conn.prepare(
            "SELECT owner_user_id, target_user_id, level, last_event_unix, event_count, notes
             FROM chat_trust WHERE owner_user_id = ?1 ORDER BY level DESC, last_event_unix DESC",
        )?;
        let rows = stmt
            .query_map(params![owner_user_id], |r| {
                Ok(ChatTrustRecord {
                    owner_user_id: r.get(0)?,
                    target_user_id: r.get(1)?,
                    level: r.get(2)?,
                    last_event_unix: r.get(3)?,
                    event_count: r.get(4)?,
                    notes: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Count of currently-blocked peers (level = -3). Used by the
    /// dashboard, not for routing decisions.
    pub async fn count_blocked(&self, owner_user_id: &str) -> Result<u64> {
        let conn = self.db.lock().await;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM chat_trust WHERE owner_user_id = ?1 AND level = -3",
            params![owner_user_id],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }
}

/// Apply the `chat_trust` schema to a raw connection. Exposed so
/// integration tests that build a `ChatTrustStore` against a
/// fresh SQLite file do not need to reach into the `schema`
/// module (which is `pub(super)`).
pub fn init_trust_schema(conn: &mut Connection) -> Result<()> {
    // The full chatstore schema is idempotent and includes the
    // `chat_trust` table; running it is the simplest, safest
    // path for tests.
    crate::schema::apply_schema(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use rusqlite::Connection;
    use tempfile::tempdir;

    async fn store_async() -> ChatTrustStore {
        let dir = tempdir().unwrap();
        let path = dir.path().join("chat.sqlite");
        let mut conn = Connection::open(&path).unwrap();
        // `init_trust_schema` runs the full chatstore schema
        // (which is idempotent and includes `chat_trust`).
        init_trust_schema(&mut conn).unwrap();
        // Leak the tempdir so the SQLite file (and its `-wal` /
        // `-shm` siblings) survive for the duration of the test.
        // The OS reaps the directory when the process exits.
        std::mem::forget(dir);
        ChatTrustStore::new(Arc::new(Mutex::new(conn)))
    }

    #[tokio::test]
    async fn set_then_get() {
        let s = store_async().await;
        s.set("alice", "bob", 2, Some("friend".into())).await.unwrap();
        let r = s.get("alice", "bob").await.unwrap().unwrap();
        assert_eq!(r.level, 2);
        assert_eq!(r.event_count, 1);
        assert_eq!(r.notes.as_deref(), Some("friend"));
    }

    #[tokio::test]
    async fn set_increments_event_count() {
        let s = store_async().await;
        s.set("alice", "bob", 1, None).await.unwrap();
        s.set("alice", "bob", -2, None).await.unwrap();
        s.set("alice", "bob", 3, None).await.unwrap();
        let r = s.get("alice", "bob").await.unwrap().unwrap();
        assert_eq!(r.level, 3);
        assert_eq!(r.event_count, 3);
    }

    #[tokio::test]
    async fn range_validation() {
        let s = store_async().await;
        assert!(matches!(
            s.set("alice", "bob", 4, None).await,
            Err(ChatStoreError::InvalidTrustLevel { level: 4 })
        ));
        assert!(matches!(
            s.set("alice", "bob", -99, None).await,
            Err(ChatStoreError::InvalidTrustLevel { level: -99 })
        ));
    }

    #[tokio::test]
    async fn self_trust_rejected() {
        let s = store_async().await;
        assert!(matches!(
            s.set("alice", "alice", 1, None).await,
            Err(ChatStoreError::InvalidId(_))
        ));
    }

    #[tokio::test]
    async fn list_orders_by_level_desc() {
        let s = store_async().await;
        s.set("alice", "bob", 1, None).await.unwrap();
        s.set("alice", "carol", 3, None).await.unwrap();
        s.set("alice", "dave", -2, None).await.unwrap();
        let list = s.list_for_owner("alice").await.unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].target_user_id, "carol");
        assert_eq!(list[1].target_user_id, "bob");
        assert_eq!(list[2].target_user_id, "dave");
    }

    #[tokio::test]
    async fn clear_removes_row() {
        let s = store_async().await;
        s.set("alice", "bob", 1, None).await.unwrap();
        assert!(s.clear("alice", "bob").await.unwrap());
        assert!(s.get("alice", "bob").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn count_blocked_filters_correctly() {
        let s = store_async().await;
        s.set("alice", "bob", -3, None).await.unwrap();
        s.set("alice", "carol", -3, None).await.unwrap();
        s.set("alice", "dave", 1, None).await.unwrap();
        assert_eq!(s.count_blocked("alice").await.unwrap(), 2);
    }
}
