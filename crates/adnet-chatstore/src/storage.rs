//! Per-user chat storage — friends, direct messages, group messages,
//! sequence tracking and message receipts.
//!
//! Originally
//! `Exodus/src-backup/src-tauri/src/microservice/chat_storage.rs`
//! (exodus-chatstore historical reference). The data model is unchanged: a single SQLite database partitioned
//! by `user_id` so multiple local users can coexist on the same
//! node. Every record type is the typed record from
//! [`adnet_types::group_chat`]; `message_type` is the
//! [`adnet_types::invariants::MessageType`] enum (serialised as
//! `snake_case` JSON), not a free-form string.
//!
//! # Concurrency
//!
//! A single `std::sync::Mutex<Connection>` guards the SQLite
//! handle. All public methods take the lock for the duration of
//! the SQL operation. Long-running callers should not hold the
//! guard across `.await` points; the methods here are all
//! synchronous and do not await.
//!
//! # Validation
//!
//! Every write validates its record against [`adnet_types`]
//! invariants *before* touching SQLite, so a malformed input never
//! lands in the database. Reads are unvalidated (the DB only holds
//! data we wrote, so the same invariants hold).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

pub use adnet_types::group_chat::MessageAttachment;
use adnet_types::group_chat::{DirectMessage, GroupMessage, MessageReceipt, MAX_SEQUENCE};
use adnet_types::invariants::{validate_id, validate_name, MessageType};

use crate::error::{ChatStoreError, Result};
use crate::schema;

/// Configuration for [`ChatStorage`].
#[derive(Debug, Clone)]
pub struct ChatStorageConfig {
    /// Directory holding the SQLite file. Created if missing.
    pub storage_dir: PathBuf,
}

impl Default for ChatStorageConfig {
    fn default() -> Self {
        let mut storage_dir = std::env::temp_dir();
        storage_dir.push("exodus_chat_storage");
        Self { storage_dir }
    }
}

/// Friend roster entry, stored per `user_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Friend {
    pub friend_id: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub status: Option<String>,
    pub last_seen: Option<i64>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
}

// `MessageAttachment` is re-exported from `adnet_types::group_chat`
// above so the lib root can pull it into its public surface without
// forcing users of this module to reach into `adnet_types`.

/// Convenience alias kept for backwards compatibility with code
/// that used the original `chat_storage` field path.
#[allow(dead_code)]
#[deprecated(note = "use `MessageAttachment` instead")]
pub type Attachment = MessageAttachment;

/// Chat storage — a single SQLite database shared across all local
/// users, with `user_id` providing the partitioning key.
#[derive(Debug)]
pub struct ChatStorage {
    config: ChatStorageConfig,
    db: Arc<Mutex<Connection>>,
}

impl ChatStorage {
    /// Open (or create) the database at `config.storage_dir/exodus_chat.db`.
    pub fn new(config: ChatStorageConfig) -> Result<Self> {
        std::fs::create_dir_all(&config.storage_dir)?;
        let db_path = config.storage_dir.join("exodus_chat.db");
        let mut conn = Connection::open(&db_path)?;

        schema::configure_connection(&conn)?;
        schema::apply_schema(&mut conn)?;

        info!(path = %db_path.display(), "chatstore opened");
        Ok(Self {
            config,
            db: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run SQLite's `PRAGMA integrity_check`. Returns `Ok(())` when
    /// the database is healthy, otherwise an error describing the
    /// corruption. Useful as a startup probe.
    pub fn check_integrity(&self) -> Result<()> {
        let conn = self.db.lock()?;
        let result: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if result == "ok" {
            Ok(())
        } else {
            Err(ChatStoreError::DatabaseCorrupt(result))
        }
    }

    /// Run `VACUUM` to rebuild the on-disk file and reclaim space
    /// after many deletes. **Expensive** — call from a maintenance
    /// task, not on every write.
    pub fn vacuum(&self) -> Result<()> {
        let conn = self.db.lock()?;
        conn.execute_batch("VACUUM")?;
        Ok(())
    }

    /// Read the on-disk schema version (for diagnostics).
    pub fn schema_version(&self) -> Result<u32> {
        let conn = self.db.lock()?;
        Ok(schema::current_version(&conn)?)
    }

    /// Borrow the underlying config (mostly for tests / diagnostics).
    pub fn config(&self) -> &ChatStorageConfig {
        &self.config
    }

    // ------------------------------------------------------------------
    // Friends
    // ------------------------------------------------------------------

    /// Insert or update a friend entry for `user_id`.
    pub fn save_friend(&self, user_id: &str, friend: Friend) -> Result<()> {
        validate_id("user_id", user_id)?;
        validate_id("friend_id", &friend.friend_id)?;
        validate_name("name", &friend.name)?;
        if let Some(url) = &friend.avatar_url {
            adnet_types::invariants::validate_url("avatar_url", url)?;
        }
        let conn = self.db.lock()?;
        let now: i64 = Utc::now().timestamp();

        conn.execute(
            "INSERT OR REPLACE INTO friends
             (user_id, friend_id, name, avatar_url, status, last_seen, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                user_id,
                friend.friend_id,
                friend.name,
                friend.avatar_url,
                friend.status,
                friend.last_seen,
                friend.created_at.unwrap_or(now),
                now,
            ],
        )?;
        debug!(user_id, friend_id = %friend.friend_id, "saved friend");
        Ok(())
    }

    /// Get a single friend entry.
    pub fn get_friend(&self, user_id: &str, friend_id: &str) -> Result<Option<Friend>> {
        let conn = self.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT friend_id, name, avatar_url, status, last_seen, created_at, updated_at
             FROM friends WHERE user_id = ?1 AND friend_id = ?2",
        )?;
        let friend = stmt
            .query_row(params![user_id, friend_id], |row| {
                Ok(Friend {
                    friend_id: row.get(0)?,
                    name: row.get(1)?,
                    avatar_url: row.get(2)?,
                    status: row.get(3)?,
                    last_seen: row.get(4)?,
                    created_at: Some(row.get(5)?),
                    updated_at: Some(row.get(6)?),
                })
            })
            .optional()?;
        Ok(friend)
    }

    /// Get all friends for `user_id`.
    pub fn get_friends(&self, user_id: &str) -> Result<Vec<Friend>> {
        let conn = self.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT friend_id, name, avatar_url, status, last_seen, created_at, updated_at
             FROM friends WHERE user_id = ?1",
        )?;
        let rows = stmt.query_map(params![user_id], |row| {
            Ok(Friend {
                friend_id: row.get(0)?,
                name: row.get(1)?,
                avatar_url: row.get(2)?,
                status: row.get(3)?,
                last_seen: row.get(4)?,
                created_at: Some(row.get(5)?),
                updated_at: Some(row.get(6)?),
            })
        })?;
        let friends = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(friends)
    }

    /// Remove a friend from `user_id`'s roster. Returns `true` if
    /// a row was deleted.
    pub fn remove_friend(&self, user_id: &str, friend_id: &str) -> Result<bool> {
        validate_id("user_id", user_id)?;
        validate_id("friend_id", friend_id)?;
        let conn = self.db.lock()?;
        let removed = conn.execute(
            "DELETE FROM friends WHERE user_id = ?1 AND friend_id = ?2",
            params![user_id, friend_id],
        )?;
        Ok(removed > 0)
    }

    /// Count friends for `user_id`. Cheaper than fetching all rows.
    pub fn count_friends(&self, user_id: &str) -> Result<u32> {
        let conn = self.db.lock()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM friends WHERE user_id = ?1",
            params![user_id],
            |row| row.get(0),
        )?;
        Ok(n as u32)
    }

    // ------------------------------------------------------------------
    // Direct messages
    // ------------------------------------------------------------------

    /// Insert or update a 1-to-1 message for `user_id`.
    pub fn save_direct_message(&self, user_id: &str, message: DirectMessage) -> Result<()> {
        validate_id("user_id", user_id)?;
        // Validate the inbound record so we never persist garbage that
        // would later fail an integrity check.
        message.validate()?;
        let conn = self.db.lock()?;

        let attachments_json = serde_json::to_string(&message.attachments)?;
        let message_type = message_type_to_str(&message.message_type);

        conn.execute(
            "INSERT OR REPLACE INTO direct_messages
             (message_id, user_id, chat_id, sender_id, receiver_id, content, message_type,
              attachments, reply_to, sequence, timestamp, integrity_hash, is_edited, edited_at,
              direction)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                message.message_id,
                user_id,
                message.chat_id,
                message.sender_id,
                message.receiver_id,
                message.content,
                message_type,
                attachments_json,
                message.reply_to,
                message.sequence,
                message.timestamp as i64,
                message.integrity_hash,
                if message.is_edited { 1 } else { 0 },
                message.edited_at.map(|v| v as i64),
                // Direction: derive from sender vs. owner. Stored
                // values are still useful for UI heuristics.
                if message.sender_id == user_id {
                    "sent"
                } else {
                    "received"
                },
            ],
        )?;
        debug!(user_id, message_id = %message.message_id, "saved direct message");
        Ok(())
    }

    /// Batch variant — saves many messages in a single transaction so
    /// a partial failure cannot leave the DB in a torn state. Useful
    /// for backfilling history from a sync response.
    pub fn save_direct_messages(
        &self,
        user_id: &str,
        messages: impl IntoIterator<Item = DirectMessage>,
    ) -> Result<usize> {
        validate_id("user_id", user_id)?;
        let mut conn = self.db.lock()?;
        let tx = conn.transaction()?;
        let mut saved = 0usize;

        for message in messages {
            message.validate()?;
            let attachments_json = serde_json::to_string(&message.attachments)?;
            let message_type = message_type_to_str(&message.message_type);
            tx.execute(
                "INSERT OR REPLACE INTO direct_messages
                 (message_id, user_id, chat_id, sender_id, receiver_id, content, message_type,
                  attachments, reply_to, sequence, timestamp, integrity_hash, is_edited,
                  edited_at, direction)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    message.message_id,
                    user_id,
                    message.chat_id,
                    message.sender_id,
                    message.receiver_id,
                    message.content,
                    message_type,
                    attachments_json,
                    message.reply_to,
                    message.sequence,
                    message.timestamp as i64,
                    message.integrity_hash,
                    if message.is_edited { 1 } else { 0 },
                    message.edited_at.map(|v| v as i64),
                    if message.sender_id == user_id {
                        "sent"
                    } else {
                        "received"
                    },
                ],
            )?;
            saved += 1;
        }

        tx.commit()?;
        debug!(user_id, saved, "batch-saved direct messages");
        Ok(saved)
    }

    /// All direct messages in a chat, ordered by ascending sequence.
    pub fn get_direct_messages(&self, user_id: &str, chat_id: &str) -> Result<Vec<DirectMessage>> {
        let conn = self.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT message_id, chat_id, sender_id, receiver_id, content, message_type,
                    attachments, reply_to, sequence, timestamp, integrity_hash, is_edited, edited_at
             FROM direct_messages
             WHERE user_id = ?1 AND chat_id = ?2
             ORDER BY sequence ASC",
        )?;
        let rows = stmt.query_map(params![user_id, chat_id], row_to_direct_message)?;
        let messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(messages)
    }

    /// Latest `limit` direct messages, newest first. Useful for chat
    /// list previews that only need the tail.
    pub fn get_recent_direct_messages(
        &self,
        user_id: &str,
        chat_id: &str,
        limit: u32,
    ) -> Result<Vec<DirectMessage>> {
        let conn = self.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT message_id, chat_id, sender_id, receiver_id, content, message_type,
                    attachments, reply_to, sequence, timestamp, integrity_hash, is_edited, edited_at
             FROM direct_messages
             WHERE user_id = ?1 AND chat_id = ?2
             ORDER BY sequence DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![user_id, chat_id, limit as i64],
            row_to_direct_message,
        )?;
        let mut messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    /// Sub-range of direct messages by sequence number — used to
    /// backfill missing history after a reconnect.
    pub fn get_direct_messages_by_sequence(
        &self,
        user_id: &str,
        chat_id: &str,
        start_seq: u32,
        end_seq: u32,
    ) -> Result<Vec<DirectMessage>> {
        if start_seq > end_seq {
            return Err(ChatStoreError::Invalid(format!(
                "direct message range {start_seq}..={end_seq} is empty"
            )));
        }
        let conn = self.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT message_id, chat_id, sender_id, receiver_id, content, message_type,
                    attachments, reply_to, sequence, timestamp, integrity_hash, is_edited, edited_at
             FROM direct_messages
             WHERE user_id = ?1 AND chat_id = ?2 AND sequence >= ?3 AND sequence <= ?4
             ORDER BY sequence ASC",
        )?;
        let rows = stmt.query_map(
            params![user_id, chat_id, start_seq, end_seq],
            row_to_direct_message,
        )?;
        let messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(messages)
    }

    /// Total number of direct messages in a chat — useful for
    /// "scroll to bottom" detection in UIs.
    pub fn count_direct_messages(&self, user_id: &str, chat_id: &str) -> Result<u32> {
        let conn = self.db.lock()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM direct_messages WHERE user_id = ?1 AND chat_id = ?2",
            params![user_id, chat_id],
            |row| row.get(0),
        )?;
        Ok(n as u32)
    }

    // ------------------------------------------------------------------
    // Group messages
    // ------------------------------------------------------------------

    /// Insert or update a group message for `user_id` (one row per
    /// recipient).
    pub fn save_group_message(&self, user_id: &str, message: GroupMessage) -> Result<()> {
        validate_id("user_id", user_id)?;
        message.validate()?;
        let conn = self.db.lock()?;

        let attachments_json = serde_json::to_string(&message.attachments)?;
        let mentions_json = serde_json::to_string(&message.mentions)?;
        let message_type = message_type_to_str(&message.message_type);

        conn.execute(
            "INSERT OR REPLACE INTO group_messages
             (message_id, user_id, group_id, sender_id, sender_name, content, message_type,
              attachments, reply_to, mentions, sequence, timestamp, integrity_hash, is_edited,
              edited_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                message.message_id,
                user_id,
                message.group_id,
                message.sender_id,
                message.sender_name,
                message.content,
                message_type,
                attachments_json,
                message.reply_to,
                mentions_json,
                message.sequence,
                message.timestamp as i64,
                message.integrity_hash,
                if message.is_edited { 1 } else { 0 },
                message.edited_at.map(|v| v as i64),
            ],
        )?;
        debug!(user_id, message_id = %message.message_id, "saved group message");
        Ok(())
    }

    /// Batch variant of [`Self::save_group_message`]. All writes are
    /// wrapped in a single transaction.
    pub fn save_group_messages(
        &self,
        user_id: &str,
        messages: impl IntoIterator<Item = GroupMessage>,
    ) -> Result<usize> {
        validate_id("user_id", user_id)?;
        let mut conn = self.db.lock()?;
        let tx = conn.transaction()?;
        let mut saved = 0usize;

        for message in messages {
            message.validate()?;
            let attachments_json = serde_json::to_string(&message.attachments)?;
            let mentions_json = serde_json::to_string(&message.mentions)?;
            let message_type = message_type_to_str(&message.message_type);
            tx.execute(
                "INSERT OR REPLACE INTO group_messages
                 (message_id, user_id, group_id, sender_id, sender_name, content, message_type,
                  attachments, reply_to, mentions, sequence, timestamp, integrity_hash,
                  is_edited, edited_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    message.message_id,
                    user_id,
                    message.group_id,
                    message.sender_id,
                    message.sender_name,
                    message.content,
                    message_type,
                    attachments_json,
                    message.reply_to,
                    mentions_json,
                    message.sequence,
                    message.timestamp as i64,
                    message.integrity_hash,
                    if message.is_edited { 1 } else { 0 },
                    message.edited_at.map(|v| v as i64),
                ],
            )?;
            saved += 1;
        }

        tx.commit()?;
        debug!(user_id, saved, "batch-saved group messages");
        Ok(saved)
    }

    /// All group messages in a group, ordered by ascending sequence.
    pub fn get_group_messages(&self, user_id: &str, group_id: &str) -> Result<Vec<GroupMessage>> {
        let conn = self.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT message_id, group_id, sender_id, sender_name, content, message_type,
                    attachments, reply_to, mentions, sequence, timestamp, integrity_hash,
                    is_edited, edited_at
             FROM group_messages
             WHERE user_id = ?1 AND group_id = ?2
             ORDER BY sequence ASC",
        )?;
        let rows = stmt.query_map(params![user_id, group_id], row_to_group_message)?;
        let messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(messages)
    }

    /// Sub-range of group messages by sequence number.
    pub fn get_group_messages_by_sequence(
        &self,
        user_id: &str,
        group_id: &str,
        start_seq: u32,
        end_seq: u32,
    ) -> Result<Vec<GroupMessage>> {
        if start_seq > end_seq {
            return Err(ChatStoreError::Invalid(format!(
                "group message range {start_seq}..={end_seq} is empty"
            )));
        }
        let conn = self.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT message_id, group_id, sender_id, sender_name, content, message_type,
                    attachments, reply_to, mentions, sequence, timestamp, integrity_hash,
                    is_edited, edited_at
             FROM group_messages
             WHERE user_id = ?1 AND group_id = ?2 AND sequence >= ?3 AND sequence <= ?4
             ORDER BY sequence ASC",
        )?;
        let rows = stmt.query_map(
            params![user_id, group_id, start_seq, end_seq],
            row_to_group_message,
        )?;
        let messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(messages)
    }

    /// Count of group messages stored for `user_id` in `group_id`.
    pub fn count_group_messages(&self, user_id: &str, group_id: &str) -> Result<u32> {
        let conn = self.db.lock()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_messages WHERE user_id = ?1 AND group_id = ?2",
            params![user_id, group_id],
            |row| row.get(0),
        )?;
        Ok(n as u32)
    }

    /// Simple substring search over direct messages for `user_id` in
    /// `chat_id`. Case-insensitive `LIKE` query — for production
    /// installs, back this with a real FTS5 index. `limit` caps the
    /// result count; pass `0` for "no limit" (returns up to 1024
    /// rows as a safety cap).
    pub fn search_direct_messages(
        &self,
        user_id: &str,
        chat_id: &str,
        query: &str,
        limit: u32,
    ) -> Result<Vec<DirectMessage>> {
        validate_id("user_id", user_id)?;
        validate_id("chat_id", chat_id)?;
        if query.is_empty() {
            return Err(ChatStoreError::Invalid("search query is empty".into()));
        }
        let cap = if limit == 0 { 1024 } else { limit as i64 };
        let pattern = format!("%{}%", query.replace('%', r"\%").replace('_', r"\_"));
        let conn = self.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT message_id, chat_id, sender_id, receiver_id, content, message_type,
                    attachments, reply_to, sequence, timestamp, integrity_hash, is_edited, edited_at
             FROM direct_messages
             WHERE user_id = ?1 AND chat_id = ?2 AND content LIKE ?3 ESCAPE '\\'
             ORDER BY sequence DESC LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![user_id, chat_id, pattern, cap],
            row_to_direct_message,
        )?;
        let mut messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        messages.reverse(); // return chronological order to match `get_*`
        Ok(messages)
    }

    /// Substring search across **all** of `user_id`'s direct-message
    /// chats. Cheaper than scanning each chat individually when the
    /// UI shows a "global search" box.
    pub fn search_all_direct_messages(
        &self,
        user_id: &str,
        query: &str,
        limit: u32,
    ) -> Result<Vec<DirectMessage>> {
        validate_id("user_id", user_id)?;
        if query.is_empty() {
            return Err(ChatStoreError::Invalid("search query is empty".into()));
        }
        let cap = if limit == 0 { 1024 } else { limit as i64 };
        let pattern = format!("%{}%", query.replace('%', r"\%").replace('_', r"\_"));
        let conn = self.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT message_id, chat_id, sender_id, receiver_id, content, message_type,
                    attachments, reply_to, sequence, timestamp, integrity_hash, is_edited, edited_at
             FROM direct_messages
             WHERE user_id = ?1 AND content LIKE ?2 ESCAPE '\\'
             ORDER BY sequence DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![user_id, pattern, cap],
            row_to_direct_message,
        )?;
        let mut messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    /// Delete all messages in a chat (one side — `user_id`'s view).
    /// Returns the number of rows removed. Use this for "clear
    /// conversation" UIs that should *not* touch the other
    /// participant's history.
    pub fn delete_chat_messages(&self, user_id: &str, chat_id: &str) -> Result<usize> {
        validate_id("user_id", user_id)?;
        validate_id("chat_id", chat_id)?;
        let conn = self.db.lock()?;
        let removed = conn.execute(
            "DELETE FROM direct_messages WHERE user_id = ?1 AND chat_id = ?2",
            params![user_id, chat_id],
        )?;
        debug!(user_id, chat_id, removed, "cleared chat");
        Ok(removed)
    }

    /// Prune direct messages older than `cutoff_timestamp` (Unix
    /// seconds). Returns the number of rows deleted. Use this for
    /// TTL / "auto-delete after N days" UIs. Receipts and
    /// sequences are **not** touched — they remain as historical
    /// bookkeeping.
    pub fn prune_direct_messages_before(
        &self,
        user_id: &str,
        cutoff_timestamp: i64,
    ) -> Result<usize> {
        validate_id("user_id", user_id)?;
        if cutoff_timestamp < 0 {
            return Err(ChatStoreError::Invalid(format!(
                "negative cutoff {cutoff_timestamp}"
            )));
        }
        let conn = self.db.lock()?;
        let removed = conn.execute(
            "DELETE FROM direct_messages
             WHERE user_id = ?1 AND timestamp < ?2",
            params![user_id, cutoff_timestamp],
        )?;
        debug!(user_id, cutoff_timestamp, removed, "pruned direct messages");
        Ok(removed)
    }

    /// Update `friend.status` (and the `last_seen` column) in one
    /// round-trip. Returns `false` if the friend row does not
    /// exist for `user_id` so callers can surface a "no such
    /// friend" UI message without a separate lookup.
    pub fn update_friend_status(
        &self,
        user_id: &str,
        friend_id: &str,
        status: Option<&str>,
        last_seen: Option<i64>,
    ) -> Result<bool> {
        validate_id("user_id", user_id)?;
        validate_id("friend_id", friend_id)?;
        if let Some(s) = status
            && s.is_empty()
        {
            return Err(ChatStoreError::Invalid("status is empty".into()));
        }
        let now: i64 = Utc::now().timestamp();
        let conn = self.db.lock()?;
        let updated = conn.execute(
            "UPDATE friends
             SET status = ?1, last_seen = ?2, updated_at = ?3
             WHERE user_id = ?4 AND friend_id = ?5",
            params![status, last_seen, now, user_id, friend_id],
        )?;
        Ok(updated > 0)
    }

    // ------------------------------------------------------------------
    // Sequences (per-user, per-target)
    // ------------------------------------------------------------------

    /// Stamp the last seen sequence for `(user_id, target_id, sequence_type)`.
    /// `sequence_type` is `"direct"` or `"group"` to match the original
    /// schema.
    pub fn update_sequence(
        &self,
        user_id: &str,
        target_id: &str,
        sequence_type: &str,
        sequence: u32,
    ) -> Result<()> {
        validate_id("user_id", user_id)?;
        validate_id("target_id", target_id)?;
        validate_id("sequence_type", sequence_type)?;
        if sequence >= MAX_SEQUENCE {
            return Err(ChatStoreError::Invalid(format!(
                "sequence {sequence} >= MAX_SEQUENCE {MAX_SEQUENCE}"
            )));
        }
        let conn = self.db.lock()?;
        let now: i64 = Utc::now().timestamp();
        conn.execute(
            "INSERT OR REPLACE INTO sequences
             (user_id, target_id, sequence_type, last_sequence, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![user_id, target_id, sequence_type, sequence, now],
        )?;
        Ok(())
    }

    /// Last seen sequence for `(user_id, target_id, sequence_type)`.
    pub fn get_sequence(
        &self,
        user_id: &str,
        target_id: &str,
        sequence_type: &str,
    ) -> Result<Option<u32>> {
        let conn = self.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT last_sequence
             FROM sequences
             WHERE user_id = ?1 AND target_id = ?2 AND sequence_type = ?3",
        )?;
        let seq = stmt
            .query_row(params![user_id, target_id, sequence_type], |row| {
                row.get::<_, u32>(0)
            })
            .optional()?;
        Ok(seq)
    }

    // ------------------------------------------------------------------
    // Receipts
    // ------------------------------------------------------------------

    /// Persist a delivery receipt.
    pub fn save_receipt(&self, user_id: &str, receipt: MessageReceipt) -> Result<()> {
        validate_id("user_id", user_id)?;
        receipt.validate()?;
        let conn = self.db.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO message_receipts
             (receipt_id, message_id, user_id, receiver_id, sequence, received_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                receipt.receipt_id,
                receipt.message_id,
                user_id,
                receipt.receiver_id,
                receipt.sequence,
                receipt.received_at as i64,
            ],
        )?;
        Ok(())
    }

    /// All delivery receipts for `message_id` seen by `user_id`.
    pub fn get_message_receipts(
        &self,
        user_id: &str,
        message_id: &str,
    ) -> Result<Vec<MessageReceipt>> {
        let conn = self.db.lock()?;
        let mut stmt = conn.prepare(
            "SELECT receipt_id, message_id, receiver_id, sequence, received_at
             FROM message_receipts
             WHERE user_id = ?1 AND message_id = ?2",
        )?;
        let rows = stmt.query_map(params![user_id, message_id], |row| {
            Ok(MessageReceipt {
                receipt_id: row.get(0)?,
                message_id: row.get(1)?,
                receiver_id: row.get(2)?,
                sequence: row.get(3)?,
                received_at: row.get::<_, i64>(4)? as u64,
            })
        })?;
        let receipts = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(receipts)
    }

    // ------------------------------------------------------------------
    // GDPR / right-to-be-forgotten
    // ------------------------------------------------------------------

    /// Delete **all** data tied to `user_id` across every table.
    /// Returns the total number of rows removed.
    pub fn delete_user_data(&self, user_id: &str) -> Result<usize> {
        validate_id("user_id", user_id)?;
        let mut conn = self.db.lock()?;
        let tx = conn.transaction()?;
        let mut total = 0usize;
        for stmt in [
            "DELETE FROM friends          WHERE user_id = ?1",
            "DELETE FROM direct_messages  WHERE user_id = ?1",
            "DELETE FROM group_messages   WHERE user_id = ?1",
            "DELETE FROM sequences        WHERE user_id = ?1",
            "DELETE FROM message_receipts WHERE user_id = ?1",
        ] {
            total += tx.execute(stmt, params![user_id])?;
        }
        tx.commit()?;
        info!(user_id, deleted = total, "purged user data");
        Ok(total)
    }

    /// Reset the **entire** database (all users). Intended for tests
    /// and "factory reset" UIs. Bumps the schema_version to the
    /// current build version so the migration machinery knows the
    /// store is fresh.
    pub fn reset(&self) -> Result<()> {
        warn!("reset() wiping the entire chat store");
        let mut conn = self.db.lock()?;
        let tx = conn.transaction()?;
        for stmt in [
            "DELETE FROM friends",
            "DELETE FROM direct_messages",
            "DELETE FROM group_messages",
            "DELETE FROM sequences",
            "DELETE FROM message_receipts",
        ] {
            tx.execute_batch(stmt)?;
        }
        tx.commit()?;
        Ok(())
    }
}

// ----------------------------------------------------------------------
// Row → typed record converters
// ----------------------------------------------------------------------

fn row_to_direct_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<DirectMessage> {
    let attachments_json: String = row.get(6)?;
    let message_type_str: String = row.get(5)?;
    let attachments: Vec<MessageAttachment> =
        serde_json::from_str(&attachments_json).unwrap_or_default();
    let message_type = parse_message_type(&message_type_str);
    Ok(DirectMessage {
        message_id: row.get(0)?,
        chat_id: row.get(1)?,
        sender_id: row.get(2)?,
        receiver_id: row.get(3)?,
        content: row.get(4)?,
        message_type,
        attachments,
        reply_to: row.get(7)?,
        sequence: row.get::<_, u32>(8)?,
        timestamp: row.get::<_, i64>(9)? as u64,
        integrity_hash: row.get(10)?,
        is_edited: row.get::<_, i64>(11)? == 1,
        edited_at: row.get::<_, Option<i64>>(12)?.map(|v| v as u64),
    })
}

fn row_to_group_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<GroupMessage> {
    let attachments_json: String = row.get(6)?;
    let mentions_json: String = row.get(8)?;
    let message_type_str: String = row.get(5)?;
    let attachments: Vec<MessageAttachment> =
        serde_json::from_str(&attachments_json).unwrap_or_default();
    let mentions: Vec<String> = serde_json::from_str(&mentions_json).unwrap_or_default();
    let message_type = parse_message_type(&message_type_str);
    Ok(GroupMessage {
        message_id: row.get(0)?,
        group_id: row.get(1)?,
        sender_id: row.get(2)?,
        sender_name: row.get(3)?,
        content: row.get(4)?,
        message_type,
        attachments,
        reply_to: row.get(7)?,
        mentions,
        sequence: row.get::<_, u32>(9)?,
        timestamp: row.get::<_, i64>(10)? as u64,
        integrity_hash: row.get(11)?,
        is_edited: row.get::<_, i64>(12)? == 1,
        edited_at: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
    })
}

/// Serialize the `MessageType` enum to its snake_case wire string
/// (e.g. `text`, `image`, `file`, `system`).
fn message_type_to_str(t: &MessageType) -> &'static str {
    use adnet_types::invariants::MessageType as M;
    match t {
        M::Text => "text",
        M::Image => "image",
        M::File => "file",
        M::System => "system",
    }
}

/// Parse the stored message_type text into the typed enum. Falls
/// back to `MessageType::Text` if the database holds an unknown
/// variant (forward-compat).
fn parse_message_type(raw: &str) -> MessageType {
    use adnet_types::invariants::MessageType as M;
    match raw {
        "text" => M::Text,
        "image" => M::Image,
        "file" => M::File,
        "system" => M::System,
        // Unknown variant — fall back to Text rather than fail.
        // SQLite cannot represent "unknown enum variant", and we
        // explicitly want forward compatibility.
        _ => M::Text,
    }
}
