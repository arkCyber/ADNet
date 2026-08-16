//! Canonical hub-server storage layer.
//!
//! Ported from
//! `Exodus/src-backup/exodus-hub-server/src/manager.rs`
//! (exodus-hub-server historical reference). The data
//! model is identical (users, conversations, group_members, messages,
//! sender_sequences, user_sequences, pending_messages, message
//! receipts) but the API is shaped for the rest of the A3Net stack:
//!
//! - every record is the typed record from [`a3net_types::group_chat`]
//!   where one already exists (users / conversations use locally
//!   defined types because the original carried extra fields like
//!   `username` and `chat_type`);
//! - the sync response supports zstd+bincode compression so history
//!   transfer stays compact;
//! - errors are unified under [`crate::error::ChatStoreError`] so
//!   callers can pattern-match across the whole crate.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, info};
use uuid::Uuid;

pub use a3net_types::group_chat::MessageReceipt;

use crate::error::{ChatStoreError, Result};
use crate::schema;

/// Re-export the cyclic sequence ceiling from `a3net_types`, so the
/// hub layer can talk about its own ceiling without an extra import
/// in every call site.
pub use a3net_types::group_chat::MAX_SEQUENCE;
/// Backwards-compat alias — `MAX_SEQUENCE` was the original constant
/// name in the `exodus-hub-server` codebase.
#[allow(dead_code)]
pub const HUB_MAX_SEQUENCE: u32 = MAX_SEQUENCE;

/// Domain-separated tag baked into the integrity hash so a
/// historical hash from a previous schema version cannot collide
/// with a current one.
const INTEGRITY_HASH_TAG: &[u8] = b"a3net-chatstore/v1";

/// Generate a 12-digit numeric user id (matches the original hub
/// server's `generate_12digit_id`).
pub fn generate_12digit_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let id: u64 = rng.gen_range(100_000_000_000..=999_999_999_999);
    id.to_string()
}

/// SHA-256 integrity hash covering `(sender, receiver, content,
/// sequence, timestamp)`. Matches the hub server's
/// `generate_integrity_hash` so historical messages verify, plus a
/// domain-separated prefix so cyclic-sequence wrap-arounds stay
/// distinguishable.
pub fn generate_integrity_hash(
    sender_id: &str,
    receiver_id: Option<&str>,
    content: &str,
    sequence: u32,
    timestamp: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(INTEGRITY_HASH_TAG);
    hasher.update(sender_id.as_bytes());
    if let Some(rid) = receiver_id {
        hasher.update(rid.as_bytes());
    }
    hasher.update(content.as_bytes());
    hasher.update(sequence.to_string().as_bytes());
    hasher.update(timestamp.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Chat type — stored in the conversations table as `"one_on_one"` or
/// `"group"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatType {
    OneOnOne,
    Group,
}

impl ChatType {
    fn as_str(self) -> &'static str {
        match self {
            ChatType::OneOnOne => "one_on_one",
            ChatType::Group => "group",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "one_on_one" => ChatType::OneOnOne,
            _ => ChatType::Group,
        }
    }
}

/// User record — id, username, display name, lifecycle timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct User {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub last_seen: Option<DateTime<Utc>>,
}

/// Conversation / chat metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Conversation {
    pub id: String,
    pub chat_type: ChatType,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: u32,
    pub last_sequence: u32,
}

/// Roster entry — one row per `(conversation, user)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GroupMember {
    pub id: String,
    pub conversation_id: String,
    pub user_id: String,
    pub joined_at: DateTime<Utc>,
    pub role: String,
}

/// Single chat message (hub-canonical). `receiver_id` is `None` for
/// group messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub receiver_id: Option<String>,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub sequence: Option<u32>,
    pub reply_to: Option<String>,
    pub integrity_hash: Option<String>,
    /// `true` after [`ImManager::edit_message`] modified the row.
    /// Defaults to `false` so legacy / cross-crate payloads
    /// deserialize cleanly.
    #[serde(default)]
    pub is_edited: bool,
    /// RFC3339 timestamp of the most recent edit, if any.
    #[serde(default)]
    pub edited_at: Option<String>,
}

// `MessageReceipt` is re-exported from `a3net_types::group_chat` above
// (kept here so the rest of the file can refer to it unqualified).

/// Per-(receiver, sender) "last seen sequence" record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UserSequence {
    pub id: String,
    pub user_id: String,
    pub sender_id: String,
    pub last_sequence: u32,
    pub updated_at: DateTime<Utc>,
}

/// Per-sender "next sequence" record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SenderSequence {
    pub id: String,
    pub sender_id: String,
    pub last_sequence: u32,
    pub updated_at: DateTime<Utc>,
}

/// A pending message queued for an offline user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PendingMessage {
    pub id: String,
    pub message_id: String,
    pub receiver_id: String,
    pub conversation_id: String,
    pub created_at: DateTime<Utc>,
}

/// Sync request — historical messages newer than `after_sequence`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SyncRequest {
    pub user_id: String,
    pub conversation_id: String,
    pub after_sequence: Option<u32>,
    pub limit: usize,
}

/// Sync response — message slice + has_more + last_sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SyncResponse {
    pub messages: Vec<Message>,
    pub has_more: bool,
    pub last_sequence: u32,
}

/// Canonical hub-server storage. One SQLite file, async wrapper
/// around a synchronous connection (the original used
/// `tokio::sync::Mutex` for the same reason — SQLite is synchronous
/// and we want the executor to be able to schedule other work).
#[derive(Debug)]
pub struct ImManager {
    #[allow(dead_code)]
    db_path: PathBuf,
    conn: Arc<TokioMutex<Connection>>,
}

impl ImManager {
    /// Open (or create) the hub database at `db_path`. Applies the
    /// schema if absent.
    ///
    /// DO-178C startup contract (mirrors [`ChatStorage::new`]):
    /// - WAL + foreign keys + synchronous=NORMAL are enabled.
    /// - `PRAGMA integrity_check` runs to completion before the
    ///   manager is handed back. A corrupt DB returns
    ///   [`ChatStoreError::DatabaseCorrupt`] at open time rather
    ///   than as a cryptic mid-write error.
    pub fn new(db_path: PathBuf) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(&db_path)?;

        schema::configure_connection(&conn)?;
        schema::apply_schema(&mut conn)?;

        let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(ChatStoreError::DatabaseCorrupt(integrity));
        }

        info!(path = %db_path.display(), "hub database opened");
        Ok(Self {
            db_path,
            conn: Arc::new(TokioMutex::new(conn)),
        })
    }

    /// Underlying path (mostly for diagnostics).
    pub fn db_path(&self) -> &PathBuf {
        &self.db_path
    }

    /// Run `PRAGMA integrity_check` on the hub database. Returns
    /// `Ok(())` when healthy.
    pub async fn check_integrity(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        let result: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if result == "ok" {
            Ok(())
        } else {
            Err(ChatStoreError::DatabaseCorrupt(result))
        }
    }

    /// Run `VACUUM` to rebuild the on-disk file. Expensive.
    pub async fn vacuum(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch("VACUUM")?;
        Ok(())
    }

    /// Read the on-disk schema version (for diagnostics).
    pub async fn schema_version(&self) -> Result<u32> {
        let conn = self.conn.lock().await;
        Ok(schema::current_version(&conn)?)
    }

    // ------------------------------------------------------------------
    // Users
    // ------------------------------------------------------------------

    /// Create a new user. Returns [`ChatStoreError::Constraint`] if
    /// the username is already taken.
    pub async fn create_user(&self, username: &str, display_name: &str) -> Result<User> {
        validate_user_fields(username, display_name)?;
        debug!("creating user: {username}");
        let id = generate_12digit_id();
        let created_at = Utc::now();
        let conn = self.conn.lock().await;

        // Use an upsert-style probe first so we can return a typed
        // `Constraint` error rather than swallowing the SQLite
        // `UNIQUE` failure.
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM users WHERE username = ?1",
                params![username],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if existing.is_some() {
            return Err(ChatStoreError::Constraint(format!(
                "username {username} already exists"
            )));
        }

        conn.execute(
            "INSERT INTO users (id, username, display_name, created_at, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                username,
                display_name,
                created_at.to_rfc3339(),
                None::<String>
            ],
        )?;
        Ok(User {
            id,
            username: username.to_string(),
            display_name: display_name.to_string(),
            created_at,
            last_seen: None,
        })
    }

    /// Fetch a user by id.
    pub async fn get_user(&self, user_id: &str) -> Result<Option<User>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, username, display_name, created_at, last_seen FROM users WHERE id = ?1",
        )?;
        let user = stmt
            .query_row(params![user_id], |row| {
                let last_seen_str: Option<String> = row.get(4)?;
                let last_seen = last_seen_str
                    .filter(|s| !s.is_empty())
                    .map(parse_dt)
                    .transpose()?;
                Ok(User {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    display_name: row.get(2)?,
                    created_at: parse_dt(row.get::<_, String>(3)?)?,
                    last_seen,
                })
            })
            .optional()?;
        Ok(user)
    }

    /// Fetch a user by username (the unique key).
    pub async fn get_user_by_username(&self, username: &str) -> Result<Option<User>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, username, display_name, created_at, last_seen FROM users WHERE username = ?1",
        )?;
        let user = stmt
            .query_row(params![username], |row| {
                let last_seen_str: Option<String> = row.get(4)?;
                let last_seen = last_seen_str
                    .filter(|s| !s.is_empty())
                    .map(parse_dt)
                    .transpose()?;
                Ok(User {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    display_name: row.get(2)?,
                    created_at: parse_dt(row.get::<_, String>(3)?)?,
                    last_seen,
                })
            })
            .optional()?;
        Ok(user)
    }

    /// Update the user's `last_seen` timestamp to "now".
    pub async fn touch_user(&self, user_id: &str) -> Result<()> {
        a3net_types::invariants::validate_id("user_id", user_id)?;
        let conn = self.conn.lock().await;
        let now = Utc::now().to_rfc3339();
        let changed = conn.execute(
            "UPDATE users SET last_seen = ?1 WHERE id = ?2",
            params![now, user_id],
        )?;
        if changed == 0 {
            return Err(ChatStoreError::NotFound(format!("user {user_id}")));
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Conversations
    // ------------------------------------------------------------------

    /// Create a new conversation (1-to-1 or group).
    pub async fn create_conversation(
        &self,
        chat_type: ChatType,
        title: &str,
    ) -> Result<Conversation> {
        a3net_types::invariants::validate_name("title", title)?;
        debug!("creating conversation: {title} ({chat_type:?})");
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO conversations
             (id, chat_type, title, created_at, updated_at, message_count, last_sequence)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0)",
            params![
                id,
                chat_type.as_str(),
                title,
                now.to_rfc3339(),
                now.to_rfc3339()
            ],
        )?;
        Ok(Conversation {
            id,
            chat_type,
            title: title.to_string(),
            created_at: now,
            updated_at: now,
            message_count: 0,
            last_sequence: 0,
        })
    }

    /// Fetch a conversation by id.
    pub async fn get_conversation(&self, conversation_id: &str) -> Result<Option<Conversation>> {
        a3net_types::invariants::validate_id("conversation_id", conversation_id)?;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, chat_type, title, created_at, updated_at, message_count, last_sequence
             FROM conversations WHERE id = ?1",
        )?;
        let conv = stmt
            .query_row(params![conversation_id], row_to_conversation)
            .optional()?;
        Ok(conv)
    }

    /// All conversations visible to `user_id` (every 1-to-1 chat
    /// plus every group the user is a member of).
    pub async fn list_user_conversations(&self, user_id: &str) -> Result<Vec<Conversation>> {
        a3net_types::invariants::validate_id("user_id", user_id)?;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT c.id, c.chat_type, c.title, c.created_at, c.updated_at,
                    c.message_count, c.last_sequence
             FROM conversations c
             LEFT JOIN group_members gm
                    ON c.id = gm.conversation_id AND gm.user_id = ?1
             WHERE c.chat_type = 'one_on_one' OR gm.user_id IS NOT NULL
             ORDER BY c.updated_at DESC",
        )?;
        let rows = stmt.query_map(params![user_id], row_to_conversation)?;
        let convs = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(convs)
    }

    // ------------------------------------------------------------------
    // Group membership
    // ------------------------------------------------------------------

    /// Add a user to a group conversation. Returns the persisted
    /// `GroupMember` record. The `INSERT OR IGNORE` semantics mean a
    /// second call with the same `(conversation_id, user_id)` pair
    /// returns the **existing** record — callers can detect this by
    /// comparing the returned `joined_at` to "now".
    pub async fn add_group_member(
        &self,
        conversation_id: &str,
        user_id: &str,
        role: &str,
    ) -> Result<GroupMember> {
        a3net_types::invariants::validate_id("conversation_id", conversation_id)?;
        a3net_types::invariants::validate_id("user_id", user_id)?;
        a3net_types::invariants::validate_id("role", role)?;
        let conn = self.conn.lock().await;

        // Detect "already a member" so we can return the existing row.
        let existing = conn
            .query_row(
                "SELECT id, joined_at FROM group_members
                 WHERE conversation_id = ?1 AND user_id = ?2",
                params![conversation_id, user_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((id, joined_at)) = existing {
            return Ok(GroupMember {
                id,
                conversation_id: conversation_id.to_string(),
                user_id: user_id.to_string(),
                joined_at: parse_dt(joined_at)?,
                role: role.to_string(),
            });
        }

        let id = Uuid::new_v4().to_string();
        let joined_at = Utc::now();
        conn.execute(
            "INSERT INTO group_members
             (id, conversation_id, user_id, joined_at, role)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, conversation_id, user_id, joined_at.to_rfc3339(), role],
        )?;
        Ok(GroupMember {
            id,
            conversation_id: conversation_id.to_string(),
            user_id: user_id.to_string(),
            joined_at,
            role: role.to_string(),
        })
    }

    /// Remove a user from a group conversation. Returns the number
    /// of rows deleted (0 if the user was not a member).
    pub async fn remove_group_member(&self, conversation_id: &str, user_id: &str) -> Result<usize> {
        a3net_types::invariants::validate_id("conversation_id", conversation_id)?;
        a3net_types::invariants::validate_id("user_id", user_id)?;
        let conn = self.conn.lock().await;
        let removed = conn.execute(
            "DELETE FROM group_members WHERE conversation_id = ?1 AND user_id = ?2",
            params![conversation_id, user_id],
        )?;
        Ok(removed)
    }

    /// All members of a group conversation, oldest-joined first.
    pub async fn get_group_members(&self, conversation_id: &str) -> Result<Vec<GroupMember>> {
        a3net_types::invariants::validate_id("conversation_id", conversation_id)?;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, user_id, joined_at, role
             FROM group_members WHERE conversation_id = ?1
             ORDER BY joined_at ASC",
        )?;
        let rows = stmt.query_map(params![conversation_id], |row| {
            Ok(GroupMember {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                user_id: row.get(2)?,
                joined_at: parse_dt(row.get::<_, String>(3)?)?,
                role: row.get(4)?,
            })
        })?;
        let members = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(members)
    }

    // ------------------------------------------------------------------
    // Messages
    // ------------------------------------------------------------------

    /// Send a new message. Generates the next per-sender sequence
    /// number, stamps the integrity hash, bumps the conversation
    /// metadata, and returns the persisted record.
    ///
    /// `content` must be non-empty and within
    /// [`a3net_types::invariants::MAX_CONTENT_LEN`]. For group
    /// conversations pass `receiver_id = None`.
    pub async fn send_message(
        &self,
        conversation_id: &str,
        sender_id: &str,
        receiver_id: Option<&str>,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<Message> {
        a3net_types::invariants::validate_id("conversation_id", conversation_id)?;
        a3net_types::invariants::validate_id("sender_id", sender_id)?;
        if let Some(rid) = receiver_id {
            a3net_types::invariants::validate_id("receiver_id", rid)?;
        }
        a3net_types::invariants::validate_content("content", content)?;
        if let Some(r) = reply_to {
            a3net_types::invariants::validate_id("reply_to", r)?;
        }

        debug!("sending message: conv={conversation_id}, sender={sender_id}");
        let conn = self.conn.lock().await;

        // Verify the conversation actually exists — without this
        // check the FK constraint would fail with an opaque
        // `Sqlite(ForeignKey)` error.
        let chat_type: Option<String> = conn
            .query_row(
                "SELECT chat_type FROM conversations WHERE id = ?1",
                params![conversation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(chat_type) = chat_type else {
            return Err(ChatStoreError::NotFound(format!(
                "conversation {conversation_id}"
            )));
        };
        // For group conversations, `receiver_id` must be `None` —
        // the receiver is the whole group. For 1-to-1 conversations
        // `None` is allowed and is treated as a self-message
        // (notes-to-self, /me commands, etc.) so the original hub
        // server's permissive behaviour is preserved.
        if chat_type == "group" && receiver_id.is_some() {
            return Err(ChatStoreError::Invalid(
                "group messages must have receiver_id=None".into(),
            ));
        }

        // Per-sender cyclic sequence number. Both reads and writes
        // happen under the same lock so two concurrent senders can't
        // collide on the next_seq.
        let last_seq: i64 = conn
            .query_row(
                "SELECT last_sequence FROM sender_sequences WHERE sender_id = ?1",
                params![sender_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let next_seq = if last_seq >= MAX_SEQUENCE as i64 {
            1
        } else {
            last_seq + 1
        };

        // Update (or create) the sender's sequence record.
        let now = Utc::now();
        conn.execute(
            "INSERT INTO sender_sequences (id, sender_id, last_sequence, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(sender_id) DO UPDATE SET
                last_sequence = excluded.last_sequence,
                updated_at = excluded.updated_at",
            params![
                Uuid::new_v4().to_string(),
                sender_id,
                next_seq,
                now.to_rfc3339(),
            ],
        )?;

        let id = Uuid::new_v4().to_string();
        let timestamp = Utc::now();
        let timestamp_str = timestamp.to_rfc3339();
        let integrity_hash = generate_integrity_hash(
            sender_id,
            receiver_id,
            content,
            next_seq as u32,
            &timestamp_str,
        );

        conn.execute(
            "INSERT INTO messages
             (id, conversation_id, sender_id, receiver_id, content, timestamp, sequence,
              reply_to, integrity_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                conversation_id,
                sender_id,
                receiver_id,
                content,
                timestamp_str,
                next_seq,
                reply_to,
                integrity_hash,
            ],
        )?;

        conn.execute(
            "UPDATE conversations
             SET updated_at = ?1, message_count = message_count + 1
             WHERE id = ?2",
            params![timestamp_str, conversation_id],
        )?;

        Ok(Message {
            id,
            conversation_id: conversation_id.to_string(),
            sender_id: sender_id.to_string(),
            receiver_id: receiver_id.map(|s| s.to_string()),
            content: content.to_string(),
            timestamp,
            sequence: Some(next_seq as u32),
            reply_to: reply_to.map(|s| s.to_string()),
            integrity_hash: Some(integrity_hash),
            is_edited: false,
            edited_at: None,
        })
    }

    /// All messages in a conversation, capped at `limit` rows.
    pub async fn get_messages(
        &self,
        conversation_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<Message>> {
        a3net_types::invariants::validate_id("conversation_id", conversation_id)?;
        let limit = limit.unwrap_or(100) as i64;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, sender_id, receiver_id, content, timestamp, sequence,
                    reply_to, integrity_hash, is_edited, edited_at
             FROM messages WHERE conversation_id = ?1
             ORDER BY timestamp ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![conversation_id, limit], row_to_message)?;
        let messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(messages)
    }

    /// Fetch a single message by id.
    pub async fn get_message_by_id(&self, message_id: &str) -> Result<Option<Message>> {
        a3net_types::invariants::validate_id("message_id", message_id)?;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, sender_id, receiver_id, content, timestamp, sequence,
                    reply_to, integrity_hash, is_edited, edited_at
             FROM messages WHERE id = ?1",
        )?;
        let message = stmt
            .query_row(params![message_id], row_to_message)
            .optional()?;
        Ok(message)
    }

    /// Mark an existing message as edited. The caller supplies the
    /// new content and the new `edited_at` timestamp; the function
    /// re-stamps the integrity hash so verification keeps passing.
    /// Returns the updated record, or `None` if the message does
    /// not exist.
    ///
    /// `sender_id` is used to re-fetch the sender's sequence number
    /// — required for the integrity hash to remain deterministic.
    pub async fn edit_message(
        &self,
        message_id: &str,
        new_content: &str,
        edited_at: DateTime<Utc>,
    ) -> Result<Option<Message>> {
        a3net_types::invariants::validate_id("message_id", message_id)?;
        a3net_types::invariants::validate_content("new_content", new_content)?;
        if edited_at.timestamp() < 0 {
            return Err(ChatStoreError::Invalid(
                "edited_at must be non-negative".into(),
            ));
        }

        let conn = self.conn.lock().await;

        // Look up the original message so we can re-stamp the hash.
        let original = conn
            .query_row(
                "SELECT conversation_id, sender_id, receiver_id, sequence
                 FROM messages WHERE id = ?1",
                params![message_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((_conversation_id, sender_id, receiver_id, sequence_opt)) = original else {
            return Ok(None);
        };
        let Some(seq) = sequence_opt else {
            return Err(ChatStoreError::Invalid(
                "cannot edit a message without a sequence number".into(),
            ));
        };

        let edited_at_str = edited_at.to_rfc3339();
        let new_hash = generate_integrity_hash(
            &sender_id,
            receiver_id.as_deref(),
            new_content,
            seq as u32,
            &edited_at_str,
        );
        // Re-stamp the integrity hash against the **edit** timestamp,
        // not the original timestamp, so an unmodified edit is still
        // detectable as `Mismatch` on verify.
        let updated = conn.execute(
            "UPDATE messages
             SET content = ?1, is_edited = 1, edited_at = ?2, integrity_hash = ?3
             WHERE id = ?4",
            params![new_content, edited_at_str, new_hash, message_id],
        )?;
        if updated == 0 {
            return Ok(None);
        }

        // Re-read and return the updated record.
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, sender_id, receiver_id, content, timestamp, sequence,
                    reply_to, integrity_hash, is_edited, edited_at
             FROM messages WHERE id = ?1",
        )?;
        let updated_msg = stmt
            .query_row(params![message_id], row_to_message)
            .optional()?;
        debug!(message_id, "edited message");
        Ok(updated_msg)
    }

    /// Hard-delete a message by id. Returns `true` if a row was
    /// removed. Receipts and pending-message rows are cascaded by
    /// FK.
    pub async fn delete_message(&self, message_id: &str) -> Result<bool> {
        a3net_types::invariants::validate_id("message_id", message_id)?;
        let conn = self.conn.lock().await;
        let removed = conn.execute("DELETE FROM messages WHERE id = ?1", params![message_id])?;
        debug!(message_id, removed, "deleted message");
        Ok(removed > 0)
    }

    /// Count messages in a conversation. Cheaper than fetching them
    /// all when the UI just wants "X messages" badges.
    pub async fn count_messages(&self, conversation_id: &str) -> Result<u32> {
        a3net_types::invariants::validate_id("conversation_id", conversation_id)?;
        let conn = self.conn.lock().await;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE conversation_id = ?1",
            params![conversation_id],
            |row| row.get(0),
        )?;
        Ok(n as u32)
    }

    /// Soft-cleanup — delete every message older than `cutoff` (Unix
    /// seconds) in a conversation. Useful for retention policies.
    /// Returns the number of rows deleted.
    pub async fn prune_messages_before(
        &self,
        conversation_id: &str,
        cutoff_timestamp: i64,
    ) -> Result<usize> {
        a3net_types::invariants::validate_id("conversation_id", conversation_id)?;
        if cutoff_timestamp < 0 {
            return Err(ChatStoreError::Invalid(format!(
                "negative cutoff {cutoff_timestamp}"
            )));
        }
        let conn = self.conn.lock().await;
        let cutoff_str = chrono::DateTime::<Utc>::from_timestamp(cutoff_timestamp, 0)
            .ok_or_else(|| ChatStoreError::Invalid(format!("bad cutoff {cutoff_timestamp}")))?
            .to_rfc3339();
        let removed = conn.execute(
            "DELETE FROM messages
             WHERE conversation_id = ?1 AND timestamp < ?2",
            params![conversation_id, cutoff_str],
        )?;
        debug!(
            conversation_id,
            cutoff_timestamp, removed, "pruned messages"
        );
        Ok(removed)
    }

    // ------------------------------------------------------------------
    // Pending (offline) messages
    // ------------------------------------------------------------------

    /// Queue a message for an offline receiver.
    pub async fn add_pending_message(
        &self,
        message_id: &str,
        receiver_id: &str,
        conversation_id: &str,
    ) -> Result<()> {
        a3net_types::invariants::validate_id("message_id", message_id)?;
        a3net_types::invariants::validate_id("receiver_id", receiver_id)?;
        a3net_types::invariants::validate_id("conversation_id", conversation_id)?;
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        let conn = self.conn.lock().await;
        // Use a FK-friendly probe so a missing message surfaces as a
        // clear `ForeignKey` error rather than a raw `SQLITE_CONSTRAINT`.
        let msg_exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM messages WHERE id = ?1",
                params![message_id],
                |row| row.get(0),
            )
            .optional()?;
        if msg_exists.is_none() {
            return Err(ChatStoreError::ForeignKey(format!(
                "message {message_id} does not exist"
            )));
        }
        conn.execute(
            "INSERT INTO pending_messages
             (id, message_id, receiver_id, conversation_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                message_id,
                receiver_id,
                conversation_id,
                created_at.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// All queued messages for `receiver_id`, oldest first.
    pub async fn get_pending_messages(&self, receiver_id: &str) -> Result<Vec<PendingMessage>> {
        a3net_types::invariants::validate_id("receiver_id", receiver_id)?;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, message_id, receiver_id, conversation_id, created_at
             FROM pending_messages WHERE receiver_id = ?1
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![receiver_id], |row| {
            Ok(PendingMessage {
                id: row.get(0)?,
                message_id: row.get(1)?,
                receiver_id: row.get(2)?,
                conversation_id: row.get(3)?,
                created_at: parse_dt(row.get::<_, String>(4)?)?,
            })
        })?;
        let pending = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(pending)
    }

    /// Delete every queued message for `receiver_id`. Returns the
    /// row count.
    pub async fn clear_pending_messages(&self, receiver_id: &str) -> Result<usize> {
        a3net_types::invariants::validate_id("receiver_id", receiver_id)?;
        let conn = self.conn.lock().await;
        let deleted = conn.execute(
            "DELETE FROM pending_messages WHERE receiver_id = ?1",
            params![receiver_id],
        )?;
        Ok(deleted)
    }

    // ------------------------------------------------------------------
    // Sequence tracking
    // ------------------------------------------------------------------

    /// Update the "last sequence observed by user" record for
    /// `(user_id, sender_id)`.
    pub async fn update_user_sequence(
        &self,
        user_id: &str,
        sender_id: &str,
        last_sequence: u32,
    ) -> Result<()> {
        a3net_types::invariants::validate_id("user_id", user_id)?;
        a3net_types::invariants::validate_id("sender_id", sender_id)?;
        let conn = self.conn.lock().await;
        let now = Utc::now();
        conn.execute(
            "INSERT INTO user_sequences (id, user_id, sender_id, last_sequence, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(user_id, sender_id) DO UPDATE SET
                last_sequence = excluded.last_sequence,
                updated_at = excluded.updated_at",
            params![
                Uuid::new_v4().to_string(),
                user_id,
                sender_id,
                last_sequence,
                now.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Fetch the `(user_id, sender_id)` last-seen sequence.
    pub async fn get_user_sequence(
        &self,
        user_id: &str,
        sender_id: &str,
    ) -> Result<Option<UserSequence>> {
        a3net_types::invariants::validate_id("user_id", user_id)?;
        a3net_types::invariants::validate_id("sender_id", sender_id)?;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, user_id, sender_id, last_sequence, updated_at
             FROM user_sequences WHERE user_id = ?1 AND sender_id = ?2",
        )?;
        let seq = stmt
            .query_row(params![user_id, sender_id], |row| {
                Ok(UserSequence {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    sender_id: row.get(2)?,
                    last_sequence: row.get(3)?,
                    updated_at: parse_dt(row.get::<_, String>(4)?)?,
                })
            })
            .optional()?;
        Ok(seq)
    }

    /// Fetch the sender's current sequence (the one a hub uses to
    /// stamp outgoing messages).
    pub async fn get_sender_sequence(&self, sender_id: &str) -> Result<Option<SenderSequence>> {
        a3net_types::invariants::validate_id("sender_id", sender_id)?;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, sender_id, last_sequence, updated_at
             FROM sender_sequences WHERE sender_id = ?1",
        )?;
        let seq = stmt
            .query_row(params![sender_id], |row| {
                Ok(SenderSequence {
                    id: row.get(0)?,
                    sender_id: row.get(1)?,
                    last_sequence: row.get(2)?,
                    updated_at: parse_dt(row.get::<_, String>(3)?)?,
                })
            })
            .optional()?;
        Ok(seq)
    }

    /// Detect whether the user has missed any messages from
    /// `sender_id`. Returns the inclusive `(start, end)` range or
    /// `None` if the user is fully caught up.
    ///
    /// `epoch` is bumped every time the sender's sequence wraps so
    /// the caller can disambiguate the two cases that produce
    /// `user_seq > current_seq`. **In-memory only** — pass it via
    /// the application state, not the database.
    pub async fn detect_missing_messages(
        &self,
        user_id: &str,
        sender_id: &str,
    ) -> Result<Option<(u32, u32)>> {
        a3net_types::invariants::validate_id("user_id", user_id)?;
        a3net_types::invariants::validate_id("sender_id", sender_id)?;
        let conn = self.conn.lock().await;

        let current_seq: i64 = conn
            .query_row(
                "SELECT last_sequence FROM sender_sequences WHERE sender_id = ?1",
                params![sender_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let user_seq: i64 = conn
            .query_row(
                "SELECT last_sequence FROM user_sequences
                 WHERE user_id = ?1 AND sender_id = ?2",
                params![user_id, sender_id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if current_seq == 0 || user_seq == current_seq {
            return Ok(None);
        }
        // current_seq != 0 && user_seq != current_seq
        let (start, end) = if current_seq > user_seq {
            (user_seq + 1, current_seq)
        } else {
            // Wrap-around case. Without an `epoch` counter we can't
            // tell whether the receiver simply saw the new range
            // `[1, current_seq]` or is fully caught up after the
            // wrap. Be conservative and return the entire upper
            // half — the caller can dedupe by `message_id`.
            (1, current_seq)
        };
        Ok(Some((start as u32, end as u32)))
    }

    /// Range query — every message from `sender_id` whose sequence
    /// falls in `[start_sequence, end_sequence]`.
    pub async fn get_messages_by_sequence_range(
        &self,
        sender_id: &str,
        start_sequence: u32,
        end_sequence: u32,
    ) -> Result<Vec<Message>> {
        a3net_types::invariants::validate_id("sender_id", sender_id)?;
        if start_sequence > end_sequence {
            return Err(ChatStoreError::Invalid(format!(
                "range {start_sequence}..={end_sequence} is empty"
            )));
        }
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, sender_id, receiver_id, content, timestamp, sequence,
                    reply_to, integrity_hash, is_edited, edited_at
             FROM messages WHERE sender_id = ?1 AND sequence >= ?2 AND sequence <= ?3
             ORDER BY sequence ASC",
        )?;
        let rows = stmt.query_map(
            params![sender_id, start_sequence, end_sequence],
            row_to_message,
        )?;
        let messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(messages)
    }

    // ------------------------------------------------------------------
    // Receipts (hub-canonical, FK-bound to `messages`)
    // ------------------------------------------------------------------

    /// Create a delivery receipt in the hub-canonical table.
    pub async fn create_message_receipt(
        &self,
        message_id: &str,
        receiver_id: &str,
        sequence: u32,
    ) -> Result<MessageReceipt> {
        a3net_types::invariants::validate_id("message_id", message_id)?;
        a3net_types::invariants::validate_id("receiver_id", receiver_id)?;
        let id = Uuid::new_v4().to_string();
        let received_at = Utc::now();
        let conn = self.conn.lock().await;
        // FK probe so the caller sees a typed `ForeignKey` error
        // rather than a raw `Sqlite` failure.
        let msg_exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM messages WHERE id = ?1",
                params![message_id],
                |row| row.get(0),
            )
            .optional()?;
        if msg_exists.is_none() {
            return Err(ChatStoreError::ForeignKey(format!(
                "message {message_id} does not exist"
            )));
        }
        let user_exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM users WHERE id = ?1",
                params![receiver_id],
                |row| row.get(0),
            )
            .optional()?;
        if user_exists.is_none() {
            return Err(ChatStoreError::ForeignKey(format!(
                "user {receiver_id} does not exist"
            )));
        }
        conn.execute(
            "INSERT INTO hub_message_receipts (id, message_id, receiver_id, sequence, received_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                message_id,
                receiver_id,
                sequence,
                received_at.to_rfc3339()
            ],
        )?;
        Ok(MessageReceipt {
            receipt_id: id,
            message_id: message_id.to_string(),
            receiver_id: receiver_id.to_string(),
            sequence,
            received_at: received_at.timestamp() as u64,
        })
    }

    /// All receipts for a single message, oldest first.
    pub async fn get_message_receipts(&self, message_id: &str) -> Result<Vec<MessageReceipt>> {
        a3net_types::invariants::validate_id("message_id", message_id)?;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, message_id, receiver_id, sequence, received_at
             FROM hub_message_receipts WHERE message_id = ?1
             ORDER BY received_at ASC",
        )?;
        let rows = stmt.query_map(params![message_id], |row| {
            let received_at = parse_dt(row.get::<_, String>(4)?)?;
            Ok(MessageReceipt {
                receipt_id: row.get(0)?,
                message_id: row.get(1)?,
                receiver_id: row.get(2)?,
                sequence: row.get(3)?,
                received_at: received_at.timestamp() as u64,
            })
        })?;
        let receipts = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(receipts)
    }

    // ------------------------------------------------------------------
    // Sync + compression
    // ------------------------------------------------------------------

    /// Paginated sync — returns a slice of messages with
    /// `sequence > after_sequence`, capped at `limit`. `has_more`
    /// is `true` if we filled the slice (caller should retry).
    pub async fn get_messages_for_sync(
        &self,
        conversation_id: &str,
        after_sequence: Option<u32>,
        limit: usize,
    ) -> Result<SyncResponse> {
        a3net_types::invariants::validate_id("conversation_id", conversation_id)?;
        if limit == 0 {
            return Err(ChatStoreError::Invalid("limit must be >= 1".into()));
        }
        let conn = self.conn.lock().await;
        let limit_i64 = limit as i64;

        let messages = if let Some(after) = after_sequence {
            let mut stmt = conn.prepare(
                "SELECT id, conversation_id, sender_id, receiver_id, content, timestamp, sequence,
                        reply_to, integrity_hash, is_edited, edited_at
                 FROM messages WHERE conversation_id = ?1 AND sequence > ?2
                 ORDER BY sequence ASC LIMIT ?3",
            )?;
            let rows =
                stmt.query_map(params![conversation_id, after, limit_i64], row_to_message)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, conversation_id, sender_id, receiver_id, content, timestamp, sequence,
                        reply_to, integrity_hash, is_edited, edited_at
                 FROM messages WHERE conversation_id = ?1
                 ORDER BY sequence ASC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![conversation_id, limit_i64], row_to_message)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        let has_more = messages.len() == limit;
        let last_sequence = messages.last().and_then(|m| m.sequence).unwrap_or(0);
        Ok(SyncResponse {
            messages,
            has_more,
            last_sequence,
        })
    }

    /// Compress a message slice into a zstd+bincode blob for
    /// bulk history transfer.
    pub fn compress_messages(messages: &[Message]) -> Result<Vec<u8>> {
        let serialized = bincode::serialize(messages)?;
        let compressed = zstd::encode_all(&serialized[..], 3)?;
        Ok(compressed)
    }

    /// Reverse [`ImManager::compress_messages`].
    pub fn decompress_messages(data: &[u8]) -> Result<Vec<Message>> {
        let decompressed = zstd::decode_all(data)?;
        let messages: Vec<Message> = bincode::deserialize(&decompressed)?;
        Ok(messages)
    }

    /// Convenience — fetch and compress in one shot.
    pub async fn get_compressed_messages_for_sync(
        &self,
        conversation_id: &str,
        after_sequence: Option<u32>,
        limit: usize,
    ) -> Result<Vec<u8>> {
        let resp = self
            .get_messages_for_sync(conversation_id, after_sequence, limit)
            .await?;
        Self::compress_messages(&resp.messages)
    }

    // ------------------------------------------------------------------
    // Integrity
    // ------------------------------------------------------------------

    /// Recompute the integrity hash for `message` and compare to the
    /// stored value. Returns `false` if the hash is missing or
    /// mismatched.
    ///
    /// **Edit semantics:** an edited message is hashed with its
    /// `edited_at` timestamp (not the original `timestamp`) because
    /// that's what [`Self::edit_message`] stamped into the row.
    /// This means an unmodified edit will fail verification — by
    /// design.
    pub fn verify_message_integrity(message: &Message) -> bool {
        match &message.integrity_hash {
            Some(stored) => {
                let hash_ts: String = if message.is_edited {
                    match &message.edited_at {
                        Some(s) => s.clone(),
                        None => return false, // edited=true but no ts ⇒ invalid
                    }
                } else {
                    message.timestamp.to_rfc3339()
                };
                let computed = generate_integrity_hash(
                    &message.sender_id,
                    message.receiver_id.as_deref(),
                    &message.content,
                    message.sequence.unwrap_or(0),
                    &hash_ts,
                );
                stored == &computed
            }
            None => false,
        }
    }
}

// ----------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------

fn parse_dt(s: String) -> std::result::Result<DateTime<Utc>, rusqlite::Error> {
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| rusqlite::Error::InvalidQuery)
}

fn validate_user_fields(username: &str, display_name: &str) -> Result<()> {
    use a3net_types::invariants::{validate_id, validate_name};
    validate_id("username", username)?;
    validate_name("display_name", display_name)?;
    Ok(())
}

fn row_to_conversation(row: &rusqlite::Row<'_>) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: row.get(0)?,
        chat_type: ChatType::parse(&row.get::<_, String>(1)?),
        title: row.get(2)?,
        created_at: parse_dt(row.get::<_, String>(3)?)?,
        updated_at: parse_dt(row.get::<_, String>(4)?)?,
        message_count: row.get::<_, i64>(5)? as u32,
        last_sequence: row.get::<_, i64>(6)? as u32,
    })
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let edited_at_str: Option<String> = row.get(10)?;
    Ok(Message {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        sender_id: row.get(2)?,
        receiver_id: row.get(3)?,
        content: row.get(4)?,
        timestamp: parse_dt(row.get::<_, String>(5)?)?,
        sequence: row.get::<_, Option<i64>>(6)?.map(|s| s as u32),
        reply_to: row.get(7)?,
        integrity_hash: row.get(8)?,
        is_edited: row.get::<_, i64>(9)? != 0,
        edited_at: edited_at_str,
    })
}
