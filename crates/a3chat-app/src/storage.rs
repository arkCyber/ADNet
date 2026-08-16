//! SQLite-backed chat storage with E2E-aware encryption wrappers.
//!
//! ## Concurrency model (DO-178C §6.3 — *determinism / fail-safe*)
//!
//! Per-user SQLite is opened **exactly once** and guarded by a
//! `tokio::sync::Mutex`. Connections are stored in a process-local
//! `DashMap<UserId, Arc<Mutex<Connection>>>`; the first call for a
//! given user opens the file and runs migrations, every subsequent
//! call reuses the same connection. This eliminates the previous
//! `ensure_schema()`-per-op pattern that opened a fresh connection
//! for every RPC and was unsafe under concurrent `send_message` from
//! multiple tasks (each connection has its own prepared-statement
//! cache and WAL state, so a fresh `Connection::open` mid-WAL can
//! observe a half-committed checkpoint).
//!
//! Every multi-statement operation is wrapped in an unchecked
//! transaction — including `save_outbound` + `record_message` so a
//! crash between them can no longer leave an orphan message row
//! whose conversation row was never updated.
//!
//! ## E2E (DO-178C §6.3 — *defensive / verifiable*)
//!
//! Outbound bodies are wrapped into `MessageBody::Encrypted` via the
//! AEAD session keyed off the per-peer `DmSession`. The associated
//! data is `sender | receiver | conversation_id | sequence | timestamp`,
//! matching the contract documented in `a3chat-crypto::session::seal`.
//! Receivers MUST verify the same AD — see `A3chatError::CryptoError`
//! when `open` returns `AeadTagMismatch`.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use base64::Engine;

use a3chat_core::conversation::{ConversationMeta, ConversationRecord};
use a3chat_core::error::A3chatError;
use a3chat_core::id::{ConversationId, MessageId, UserId};
use a3chat_core::message::{ChatMessage, MessageBody, MessageEnvelope, MessageType};
use a3chat_core::presence::Presence;
use a3chat_core::validation::MAX_PREVIEW_LEN;

use crate::error::{AppError, AppResult};
use crate::keyring::E2eKeyring;

/// Default send-side preview length (matches `a3chat-core::MAX_PREVIEW_LEN`).
const PREVIEW_LEN: usize = MAX_PREVIEW_LEN;

/// Maximum number of inbound messages returned by a single
/// `list_messages` call. Defends against OOM on unbounded limit.
const HARD_LIMIT_CAP: u32 = 5_000;

/// Configuration for [`ChatStorage`].
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Base directory for SQLite files. One `<user_id>.sqlite` per
    /// local user.
    pub base_dir: PathBuf,
    /// Default sender key for self-notifications / system messages.
    pub enable_e2e: bool,
    /// Max sequence number per sender. Mirrors `a3net_types::group_chat`.
    pub max_sequence: u32,
}

impl StorageConfig {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            enable_e2e: true,
            max_sequence: 9_999,
        }
    }

    pub fn path_for(&self, user_id: &UserId) -> PathBuf {
        self.base_dir.join(format!("{user_id}.sqlite"))
    }
}

/// A row in the local SQLite `messages` table — what we actually
/// persist. Mirrors [`ChatMessage`] but always stores the body as
/// ciphertext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMessage {
    pub message: ChatMessage,
    /// `true` if the body was encrypted by this node before writing.
    /// `false` means the body was a plaintext system message — we
    /// keep that distinction so the UI knows whether to attempt
    /// decryption or render directly.
    pub was_encrypted_at_write: bool,
}

/// Per-user chat storage. Cloning is cheap — it's an `Arc` inside.
#[derive(Clone)]
pub struct ChatStorage {
    inner: Arc<Inner>,
}

struct Inner {
    config: StorageConfig,
    keyring: E2eKeyring,
    /// Per-user SQLite connections, lazily opened and reused.
    /// `Mutex` (not `RwLock`) because every operation does at least
    /// one write (WAL checkpoint) — holding a read lock would not
    /// provide any concurrency benefit.
    /// (`DashMap` is `Send + Sync` so we use it directly without an
    /// outer Arc.)
    connections: dashmap::DashMap<UserId, Arc<Mutex<rusqlite::Connection>>>,
}

/// Outcome of [`ChatStorage::record_message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordOutcome {
    pub new_message_count: u32,
    pub new_unread_count: u32,
}

/// Filter for [`ChatStorage::search_messages`].
#[derive(Debug, Clone)]
pub struct SearchQuery<'a> {
    pub owner: &'a UserId,
    pub needle: &'a str,
    pub conversation_id: Option<&'a ConversationId>,
    pub limit: u32,
}

impl ChatStorage {
    pub fn new(config: StorageConfig, keyring: E2eKeyring) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                keyring,
                connections: dashmap::DashMap::new(),
            }),
        }
    }

    pub fn config(&self) -> &StorageConfig {
        &self.inner.config
    }

    pub fn keyring(&self) -> &E2eKeyring {
        &self.inner.keyring
    }

    /// Build a [`a3net_chatstore::ChatTrustStore`] bound to the
    /// per-user SQLite connection backing this storage. The trust
    /// store is cheap to clone (it holds an `Arc<Mutex<Connection>>`)
    /// but lives only as long as the underlying storage handle; it
    /// does **not** create a second SQLite file.
    pub async fn trust_store(&self, user_id: &UserId) -> AppResult<a3net_chatstore::ChatTrustStore> {
        let conn = self.connection(user_id).await?;
        Ok(a3net_chatstore::ChatTrustStore::new(conn))
    }

    /// Initialise the per-user schema. Idempotent.
    pub async fn init_user(&self, user_id: &UserId) -> AppResult<()> {
        self.connection(user_id).await?;
        Ok(())
    }

    /// Open (or reuse) the per-user SQLite connection. The first call
    /// for `user_id` runs `init_schema`; subsequent calls just clone
    /// the cached `Arc<Mutex<Connection>>`.
    async fn connection(&self, user_id: &UserId) -> AppResult<Arc<Mutex<rusqlite::Connection>>> {
        if let Some(c) = self.inner.connections.get(user_id) {
            return Ok(c.clone());
        }
        let path = self.inner.config.path_for(user_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // Open synchronously — `Connection::open` is blocking. Holding
        // a tokio blocking call inside a hot path is undesirable, so
        // we route through `spawn_blocking`. Once the connection is
        // cached, subsequent calls skip this branch entirely.
        let path_clone = path.clone();
        let conn = tokio::task::spawn_blocking(move || -> AppResult<rusqlite::Connection> {
            let conn = rusqlite::Connection::open(&path_clone)?;
            init_schema(&conn)?;
            Ok(conn)
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;
        let arc = Arc::new(Mutex::new(conn));
        self.inner.connections.insert(user_id.clone(), arc.clone());
        Ok(arc)
    }

    /// Acquire the per-user lock + connection in one helper so all
    /// callers follow the same pattern. Returns the
    /// `tokio::sync::MutexGuard`; the caller is responsible for
    /// keeping the guard alive for the duration of the operation.
    #[allow(dead_code)]
    async fn lock(
        &self,
        user_id: &UserId,
    ) -> AppResult<tokio::sync::OwnedMutexGuard<rusqlite::Connection>> {
        let arc = self.connection(user_id).await?;
        Ok(arc.lock_owned().await)
    }

    /// Save a `MessageEnvelope` from a client. Returns the persisted
    /// `ChatMessage` (with E2E-encrypted body if `enable_e2e` and the
    /// message is not a system announcement).
    ///
    /// Wraps the message write + conversation-meta update + read
    /// receipt (self-insert) in a single SQLite transaction so a
    /// crash mid-flight can no longer leave a stranded message
    /// without a corresponding conversation row.
    pub async fn save_outbound(
        &self,
        owner: &UserId,
        envelope: &MessageEnvelope,
    ) -> AppResult<StoredMessage> {
        envelope.validate()?;
        let max_seq = self.inner.config.max_sequence;
        if envelope.sequence >= max_seq {
            return Err(AppError::Domain(format!(
                "envelope.sequence {} >= max {max_seq}",
                envelope.sequence
            )));
        }
        let should_encrypt = self.inner.config.enable_e2e
            && envelope.message_type != MessageType::System;
        let body = if should_encrypt {
            self.encrypt_body(owner, &envelope.receiver_id, envelope, &envelope.body)
                .await?
        } else {
            envelope.body.clone()
        };
        let conv_id = envelope.conversation_id.clone();
        let preview = body.preview();
        let kind_hint = conv_id.kind_hint();
        let (kind, peer, title) = match kind_hint {
            a3chat_core::id::ConversationKindHint::Dm => (
                a3chat_core::conversation::ConversationKind::Dm,
                Some(envelope.receiver_id.clone()),
                envelope.receiver_id.as_str().to_string(),
            ),
            a3chat_core::id::ConversationKindHint::Group => (
                a3chat_core::conversation::ConversationKind::Group,
                None,
                envelope.conversation_id.as_str().to_string(),
            ),
            a3chat_core::id::ConversationKindHint::Other => (
                a3chat_core::conversation::ConversationKind::Dm,
                Some(envelope.receiver_id.clone()),
                envelope.receiver_id.as_str().to_string(),
            ),
        };
        let integrity = integrity_hash(owner, &envelope.receiver_id, envelope);
        let message_id = a3chat_core::id::generate_message_id(owner.as_str());
        let sequence = envelope.sequence;
        let timestamp = envelope.timestamp;
        let message_type = envelope.message_type;
        let sender_id = owner.clone();
        let receiver_id = envelope.receiver_id.clone();
        let attachments_json = serde_json::to_string(&envelope.attachments)?;
        let reply_to_str = envelope.reply_to.as_ref().map(|m| m.as_str().to_string());
        let conv_id_str = conv_id.as_str().to_string();
        let body_json = serde_json::to_string(&body)?;

        // Serialise the SQLite write — `Connection::unchecked_transaction`
        // is synchronous and blocks; we run it on a blocking task.
        let conn_arc = self.connection(owner).await?;
        // Build the returned ChatMessage *before* spawning so we
        // don't move the original fields into the closure (we need
        // them below to assemble the stored value).
        let stored_msg = ChatMessage {
            message_id: message_id.clone(),
            conversation_id: conv_id.clone(),
            sender_id: sender_id.clone(),
            receiver_id: receiver_id.clone(),
            message_type,
            body: body.clone(),
            attachments: envelope.attachments.clone(),
            reply_to: envelope.reply_to.clone(),
            sequence,
            timestamp,
            read_at: None,
            is_edited: false,
            edited_at: None,
            integrity_hash: Some(integrity.clone()),
            recalled_at: None,
        };
        stored_msg.validate()?;
        tokio::task::spawn_blocking(move || -> AppResult<()> {
            let conv_id = conv_id_str;
            let message_id_str = message_id.as_str().to_string();
            let sender_str = sender_id.as_str().to_string();
            let receiver_str = receiver_id.as_str().to_string();
            let reply_to = reply_to_str;
            let body_json = body_json;
            let attachments_json = attachments_json;
            let integrity = integrity;
            let mut guard = conn_arc.blocking_lock_owned();
            let tx = guard.transaction()?;
            // Sequence conflict detection — refuse to insert a
            // message with `sequence <= max(sequence)` already seen
            // for this `(sender, conversation_id)` pair. This
            // catches replay/out-of-order delivery on the server
            // side without trusting the client.
            let prev_max: Option<i64> = tx
                .query_row(
                    "SELECT MAX(sequence) FROM messages
                     WHERE conversation_id = ?1 AND sender_id = ?2",
                    rusqlite::params![conv_id, sender_str],
                    |row| row.get(0),
                )
                .ok();
            if let Some(prev) = prev_max {
                if prev >= sequence as i64 {
                    return Err(AppError::Conflict(format!(
                        "sequence {sequence} <= prev {prev} for sender in {conv_id}"
                    )));
                }
            }
            tx.execute(
                "INSERT OR REPLACE INTO messages
                 (message_id, conversation_id, sender_id, receiver_id, message_type,
                  body_json, attachments_json, reply_to, sequence, timestamp,
                  read_at, is_edited, edited_at, integrity_hash, recalled_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                         NULL, 0, NULL, ?11, NULL)",
                rusqlite::params![
                    message_id_str,
                    conv_id,
                    sender_str,
                    receiver_str,
                    message_type.as_str(),
                    body_json,
                    attachments_json,
                    reply_to,
                    sequence,
                    timestamp,
                    integrity,
                ],
            )?;
            // Upsert the conversation meta in the same tx.
            let existing: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT message_count, unread_count FROM conversations WHERE conversation_id = ?1",
                    rusqlite::params![conv_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .ok();
            let (old_count, _old_unread) = existing.unwrap_or((0, 0));
            let new_count = (old_count as u32).saturating_add(1);
            // Outbound = we're the sender → unread stays at 0.
            let new_unread = 0u32;
            let bounded_preview: String = if preview.chars().count() > PREVIEW_LEN {
                let mut s: String = preview.chars().take(PREVIEW_LEN).collect();
                s.push('…');
                s
            } else {
                preview.clone()
            };
            let kind_str = kind.as_str();
            let peer_str = peer.as_ref().map(|u| u.as_str().to_string());
            tx.execute(
                "INSERT INTO conversations
                    (conversation_id, kind, title, peer_user_id,
                     last_message_preview, last_activity, message_count,
                     unread_count, peer_online, muted, pinned)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 0, 0)
                 ON CONFLICT(conversation_id) DO UPDATE SET
                    title               = excluded.title,
                    peer_user_id        = COALESCE(excluded.peer_user_id, conversations.peer_user_id),
                    last_message_preview = excluded.last_message_preview,
                    last_activity       = excluded.last_activity,
                    message_count       = excluded.message_count,
                    unread_count        = excluded.unread_count",
                rusqlite::params![
                    conv_id,
                    kind_str,
                    title,
                    peer_str,
                    bounded_preview,
                    timestamp,
                    new_count,
                    new_unread,
                ],
            )?;
            // Outbound messages are read by the sender by definition.
            tx.execute(
                "INSERT OR IGNORE INTO read_receipts (message_id, reader_id, read_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    message_id_str,
                    sender_str,
                    chrono::Utc::now().to_rfc3339(),
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;
        Ok(StoredMessage {
            message: stored_msg,
            was_encrypted_at_write: should_encrypt,
        })
    }

    /// Inbound-only path. Inserts a `ChatMessage` that was received
    /// over the wire (already-decrypted, by us) and bumps
    /// `unread_count` atomically with the message insert.
    pub async fn record_inbound(
        &self,
        owner: &UserId,
        message: &ChatMessage,
    ) -> AppResult<RecordOutcome> {
        message.validate()?;
        let preview = message.body.preview();
        let conv_id = message.conversation_id.clone();
        let conv_id_str = conv_id.as_str().to_string();
        let msg_id_str = message.message_id.as_str().to_string();
        let sender_str = message.sender_id.as_str().to_string();
        let receiver_str = message.receiver_id.as_str().to_string();
        let reply_to = message.reply_to.as_ref().map(|m| m.as_str().to_string());
        let body_json = serde_json::to_string(&message.body)?;
        let attachments_json = serde_json::to_string(&message.attachments)?;
        let timestamp = message.timestamp;
        let sequence = message.sequence;
        let message_type = message.message_type;
        let read_at = message.read_at.map(|t| t.to_rfc3339());
        let edited_at = message.edited_at.map(|t| t.to_rfc3339());
        let is_edited = message.is_edited;
        let integrity = message.integrity_hash.clone();
        let recalled_at = message.recalled_at.map(|t| t.to_rfc3339());
        let owner_str = owner.as_str().to_string();

        let conn_arc = self.connection(owner).await?;
        let (new_count, new_unread): (u32, u32) = tokio::task::spawn_blocking(move || {
            let mut guard = conn_arc.blocking_lock_owned();
            let tx = guard.transaction()?;
            tx.execute(
                "INSERT OR REPLACE INTO messages
                 (message_id, conversation_id, sender_id, receiver_id, message_type,
                  body_json, attachments_json, reply_to, sequence, timestamp,
                  read_at, is_edited, edited_at, integrity_hash, recalled_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                         ?11, ?12, ?13, ?14, ?15)",
                rusqlite::params![
                    msg_id_str,
                    conv_id_str,
                    sender_str,
                    receiver_str,
                    message_type.as_str(),
                    body_json,
                    attachments_json,
                    reply_to,
                    sequence,
                    timestamp,
                    read_at,
                    is_edited as i64,
                    edited_at,
                    integrity,
                    recalled_at,
                ],
            )?;
            let existing: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT message_count, unread_count FROM conversations WHERE conversation_id = ?1",
                    rusqlite::params![conv_id_str],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .ok();
            let (old_count, old_unread) = existing.unwrap_or((0, 0));
            let new_count = (old_count as u32).saturating_add(1);
            // Sender != owner ⇒ we (owner) have not yet read it.
            let new_unread = if sender_str != owner_str {
                (old_unread as u32).saturating_add(1)
            } else {
                0
            };
            let bounded_preview: String = if preview.chars().count() > PREVIEW_LEN {
                let mut s: String = preview.chars().take(PREVIEW_LEN).collect();
                s.push('…');
                s
            } else {
                preview
            };
            tx.execute(
                "INSERT INTO conversations
                    (conversation_id, kind, title, peer_user_id,
                     last_message_preview, last_activity, message_count,
                     unread_count, peer_online, muted, pinned)
                 VALUES (?1, 'dm', ?2, ?3, ?4, ?5, ?6, ?7, 0, 0, 0)
                 ON CONFLICT(conversation_id) DO UPDATE SET
                    last_message_preview = excluded.last_message_preview,
                    last_activity       = excluded.last_activity,
                    message_count       = excluded.message_count,
                    unread_count        = excluded.unread_count",
                rusqlite::params![
                    conv_id_str,
                    receiver_str,
                    sender_str,
                    bounded_preview,
                    timestamp,
                    new_count,
                    new_unread,
                ],
            )?;
            tx.commit()?;
            Ok::<(u32, u32), AppError>((new_count, new_unread))
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;

        Ok(RecordOutcome {
            new_message_count: new_count,
            new_unread_count: new_unread,
        })
    }

    /// Variant of [`list_messages`] that returns only messages
    /// with `sequence > since_sequence`. Used by `sync_service.delta`
    /// to push incremental updates to remote devices; the
    /// `since_sequence` is the last sequence the client
    /// successfully persisted.
    pub async fn list_messages_since(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        since_sequence: u32,
        limit: u32,
    ) -> AppResult<Vec<ChatMessage>> {
        let cap = limit.min(HARD_LIMIT_CAP).max(1);
        let conn_arc = self.connection(owner).await?;
        let conv_str = conversation_id.as_str().to_string();
        let rows: Vec<ChatMessage> = tokio::task::spawn_blocking(move || {
            let guard = conn_arc.blocking_lock_owned();
            let mut stmt = guard.prepare(
                "SELECT message_id, conversation_id, sender_id, receiver_id,
                        message_type, body_json, attachments_json, reply_to,
                        sequence, timestamp, read_at, is_edited, edited_at,
                        integrity_hash, recalled_at
                 FROM messages
                 WHERE conversation_id = ?1 AND sequence > ?2
                 ORDER BY sequence ASC
                 LIMIT ?3",
            )?;
            let rows: Vec<ChatMessage> = stmt
                .query_map(rusqlite::params![conv_str, since_sequence, cap], row_to_message)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<Vec<ChatMessage>, AppError>(rows)
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;
        Ok(rows)
    }
    /// Decrypts the body on the way out (P2 — currently a no-op for
    /// already-encrypted bodies; the UI is responsible for displaying
    /// them as ciphertext).
    pub async fn list_messages(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        limit: u32,
    ) -> AppResult<Vec<ChatMessage>> {
        let cap = limit.min(HARD_LIMIT_CAP).max(1);
        let conn_arc = self.connection(owner).await?;
        let conv_str = conversation_id.as_str().to_string();
        let rows: Vec<ChatMessage> = tokio::task::spawn_blocking(move || {
            let guard = conn_arc.blocking_lock_owned();
            let mut stmt = guard.prepare(
                "SELECT message_id, conversation_id, sender_id, receiver_id,
                        message_type, body_json, attachments_json, reply_to,
                        sequence, timestamp, read_at, is_edited, edited_at,
                        integrity_hash, recalled_at
                 FROM messages
                 WHERE conversation_id = ?1
                 ORDER BY sequence DESC
                 LIMIT ?2",
            )?;
            let rows: Vec<ChatMessage> = stmt
                .query_map(rusqlite::params![conv_str, cap], row_to_message)?
                .collect::<Result<Vec<_>, _>>()?;
            let mut out: Vec<ChatMessage> = rows.into_iter().collect();
            out.reverse();
            Ok::<Vec<ChatMessage>, AppError>(out)
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;
        Ok(rows)
    }

    /// Look up a single message by id.
    pub async fn get_message(
        &self,
        owner: &UserId,
        message_id: &MessageId,
    ) -> AppResult<Option<ChatMessage>> {
        let conn_arc = self.connection(owner).await?;
        let id_str = message_id.as_str().to_string();
        let got: Option<ChatMessage> = tokio::task::spawn_blocking(move || {
            let guard = conn_arc.blocking_lock_owned();
            let mut stmt = guard.prepare(
                "SELECT message_id, conversation_id, sender_id, receiver_id,
                        message_type, body_json, attachments_json, reply_to,
                        sequence, timestamp, read_at, is_edited, edited_at,
                        integrity_hash, recalled_at
                 FROM messages
                 WHERE message_id = ?1",
            )?;
            let mut rows = stmt.query_map(rusqlite::params![id_str], row_to_message)?;
            match rows.next() {
                Some(Ok(m)) => Ok(Some(m)),
                Some(Err(e)) => Err(AppError::Storage(e.to_string())),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;
        Ok(got)
    }

    /// Mark `message_id` as read by `owner` and decrement the
    /// conversation's unread badge — all in one transaction. No-op
    /// if the message was already read by this user.
    pub async fn ack_message(&self, owner: &UserId, message_id: &MessageId) -> AppResult<()> {
        let conn_arc = self.connection(owner).await?;
        let id_str = message_id.as_str().to_string();
        let owner_str = owner.as_str().to_string();
        let n: usize = tokio::task::spawn_blocking(move || -> AppResult<usize> {
            let mut guard = conn_arc.blocking_lock_owned();
            let tx = guard.transaction()?;
            // Look up the message to learn its conversation_id +
            // sender so we can decrement unread correctly.
            let (conv_id, sender_id): (String, String) = tx
                .query_row(
                    "SELECT conversation_id, sender_id FROM messages WHERE message_id = ?1",
                    rusqlite::params![id_str],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(|_| {
                    AppError::Domain(format!("message {} not found", id_str))
                })?;
            let now = chrono::Utc::now().to_rfc3339();
            let n = tx.execute(
                "UPDATE messages SET read_at = ?2
                 WHERE message_id = ?1 AND (read_at IS NULL OR read_at = '')",
                rusqlite::params![id_str, now],
            )?;
            if n == 0 {
                // Already read — still record receipt below, but
                // don't decrement unread again.
            } else if sender_id != owner_str {
                tx.execute(
                    "UPDATE conversations
                     SET unread_count = CASE WHEN unread_count > 0 THEN unread_count - 1 ELSE 0 END
                     WHERE conversation_id = ?1",
                    rusqlite::params![conv_id],
                )?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO read_receipts (message_id, reader_id, read_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![id_str, owner_str, now],
            )?;
            tx.commit()?;
            Ok(n)
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;
        if n == 0 {
            // Distinguish "unknown" from "already-ack" by re-checking.
            // Either way we already wrote a receipt above; return Ok
            // so idempotent retries from the client don't 404.
        }
        Ok(())
    }

    /// Set `recalled_at = now` on the given message. Only the
    /// original sender may recall.
    pub async fn recall_message(
        &self,
        owner: &UserId,
        message_id: &MessageId,
    ) -> AppResult<()> {
        let conn_arc = self.connection(owner).await?;
        let id_str = message_id.as_str().to_string();
        let owner_str = owner.as_str().to_string();
        let r: AppResult<()> = tokio::task::spawn_blocking(move || -> AppResult<()> {
            let guard = conn_arc.blocking_lock_owned();
            let now = chrono::Utc::now().to_rfc3339();
            let n = guard.execute(
                "UPDATE messages SET recalled_at = ?2
                 WHERE message_id = ?1 AND sender_id = ?3 AND recalled_at IS NULL",
                rusqlite::params![id_str, now, owner_str],
            )?;
            if n == 0 {
                // Either unknown, or recalled, or not the sender.
                // Distinguish so the caller can give a sensible error.
                let exists: bool = guard
                    .query_row(
                        "SELECT 1 FROM messages WHERE message_id = ?1",
                        rusqlite::params![id_str],
                        |_| Ok(true),
                    )
                    .unwrap_or(false);
                if !exists {
                    return Err(AppError::Domain(format!(
                        "message {} not found",
                        id_str
                    )));
                }
                let sender: Option<String> = guard
                    .query_row(
                        "SELECT sender_id FROM messages WHERE message_id = ?1",
                        rusqlite::params![id_str],
                        |row| row.get::<_, String>(0),
                    )
                    .ok();
                if sender.as_deref() != Some(owner_str.as_str()) {
                    return Err(AppError::Forbidden(
                        "only the original sender can recall a message".into(),
                    ));
                }
                // already recalled — idempotent ok
            }
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))?;
        r
    }

    /// Edit a message in-place. Re-encrypts the body if the message
    /// was encrypted at write-time, so an attacker who snapshots the
    /// row before edit doesn't get a free oracle. Only the original
    /// sender may edit.
    pub async fn edit_message(
        &self,
        owner: &UserId,
        message_id: &MessageId,
        new_body: &MessageBody,
    ) -> AppResult<ChatMessage> {
        new_body.validate()?;
        let mut updated = self
            .get_message(owner, message_id)
            .await?
            .ok_or_else(|| AppError::Domain(format!("message {} not found", message_id.as_str())))?;
        if updated.sender_id != *owner {
            return Err(AppError::Forbidden(
                "only the original sender can edit a message".into(),
            ));
        }
        // Re-seal if the original body was encrypted.
        let new_body = if updated.body.is_encrypted() {
            self.encrypt_body(
                owner,
                &updated.receiver_id,
                // We don't have the envelope handy; build a minimal
                // one with the *new* sequence / timestamp so the AD
                // matches the envelope contract.
                &MessageEnvelope {
                    conversation_id: updated.conversation_id.clone(),
                    receiver_id: updated.receiver_id.clone(),
                    message_type: updated.message_type,
                    body: new_body.clone(),
                    attachments: updated.attachments.clone(),
                    reply_to: updated.reply_to.clone(),
                    sequence: updated.sequence,
                    timestamp: chrono::Utc::now().timestamp(),
                },
                new_body,
            )
            .await?
        } else {
            new_body.clone()
        };
        let now = chrono::Utc::now();
        updated.body = new_body;
        updated.is_edited = true;
        updated.edited_at = Some(now);
        let body_json = serde_json::to_string(&updated.body)?;
        let edited_at = now.to_rfc3339();
        let msg_id_str = updated.message_id.as_str().to_string();
        let owner_str = owner.as_str().to_string();
        let conn_arc = self.connection(owner).await?;
        let _: AppResult<()> = tokio::task::spawn_blocking(move || -> AppResult<()> {
            let guard = conn_arc.blocking_lock_owned();
            let n = guard.execute(
                "UPDATE messages SET body_json = ?2, is_edited = 1, edited_at = ?3
                 WHERE message_id = ?1 AND sender_id = ?4",
                rusqlite::params![msg_id_str, body_json, edited_at, owner_str],
            )?;
            if n == 0 {
                return Err(AppError::Forbidden(
                    "edit rejected (message vanished mid-flight)".into(),
                ));
            }
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))?;
        Ok(updated)
    }

    /// Delete a message from the local store ("delete for me").
    /// Returns `NotFound` if the message id is unknown.
    pub async fn delete_message_for_me(
        &self,
        owner: &UserId,
        message_id: &MessageId,
    ) -> AppResult<()> {
        let conn_arc = self.connection(owner).await?;
        let id_str = message_id.as_str().to_string();
        let n: usize = tokio::task::spawn_blocking(move || -> AppResult<usize> {
            let mut guard = conn_arc.blocking_lock_owned();
            let tx = guard.transaction()?;
            let conv: Option<String> = tx
                .query_row(
                    "SELECT conversation_id FROM messages WHERE message_id = ?1",
                    rusqlite::params![id_str],
                    |row| row.get::<_, String>(0),
                )
                .ok();
            let conv = match conv {
                Some(c) => c,
                None => {
                    return Err(AppError::Domain(format!(
                        "message {id_str} not found"
                    )))
                }
            };
            let n = tx.execute(
                "DELETE FROM messages WHERE message_id = ?1",
                rusqlite::params![id_str],
            )?;
            if n > 0 {
                // Re-derive conversation counters from the remaining rows.
                let (mc, last_ts, last_preview): (i64, i64, Option<String>) = tx
                    .query_row(
                        "SELECT COUNT(*), COALESCE(MAX(timestamp), 0),
                                (SELECT body_json FROM messages
                                 WHERE conversation_id = ?1
                                 ORDER BY timestamp DESC LIMIT 1)
                         FROM messages WHERE conversation_id = ?1",
                        rusqlite::params![conv],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .unwrap_or((0, 0, None));
                let bounded: String = match last_preview {
                    Some(p) if p.chars().count() <= PREVIEW_LEN => p,
                    Some(p) => {
                        let mut s: String = p.chars().take(PREVIEW_LEN).collect();
                        s.push('…');
                        s
                    }
                    None => String::new(),
                };
                tx.execute(
                    "UPDATE conversations
                     SET message_count = ?2,
                         last_activity = ?3,
                         last_message_preview = ?4
                     WHERE conversation_id = ?1",
                    rusqlite::params![conv, mc, last_ts, bounded],
                )?;
            }
            tx.commit()?;
            Ok(n)
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;
        let _ = n;
        Ok(())
    }

    /// Search messages for `needle` (case-insensitive substring) in
    /// the local store. Plaintext bodies only — encrypted bodies
    /// never match (we can't decrypt them server-side, by design).
    pub async fn search_messages(&self, q: SearchQuery<'_>) -> AppResult<Vec<ChatMessage>> {
        if q.needle.is_empty() {
            return Err(AppError::Domain("search needle is empty".into()));
        }
        if q.needle.len() > 256 {
            return Err(AppError::Domain("search needle exceeds 256 chars".into()));
        }
        let cap = q.limit.min(HARD_LIMIT_CAP).max(1);
        let conn_arc = self.connection(q.owner).await?;
        let needle = format!("%{}%", q.needle.to_lowercase());
        let conv_filter = q.conversation_id.map(|c| c.as_str().to_string());
        let has_conv = conv_filter.is_some();
        let hits: Vec<ChatMessage> = tokio::task::spawn_blocking(move || {
            let guard = conn_arc.blocking_lock_owned();
            // Build the SQL with exactly the placeholders we plan
            // to pass — rusqlite counts `?` params strictly.
            let mut sql = String::from(
                "SELECT message_id, conversation_id, sender_id, receiver_id,
                        message_type, body_json, attachments_json, reply_to,
                        sequence, timestamp, read_at, is_edited, edited_at,
                        integrity_hash, recalled_at
                 FROM messages
                 WHERE LOWER(body_json) LIKE ?1",
            );
            if has_conv {
                sql.push_str(" AND conversation_id = ?2");
                sql.push_str(" ORDER BY timestamp DESC LIMIT ?3");
            } else {
                sql.push_str(" ORDER BY timestamp DESC LIMIT ?2");
            }
            let mut stmt = guard.prepare(&sql)?;
            let rows = if let Some(conv) = conv_filter.as_ref() {
                stmt.query_map(rusqlite::params![needle, conv, cap], row_to_message)?
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                stmt.query_map(rusqlite::params![needle, cap], row_to_message)?
                    .collect::<Result<Vec<_>, _>>()?
            };
            Ok::<Vec<ChatMessage>, AppError>(rows)
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;
        Ok(hits)
    }

    /// Compute and cache a conversation meta row.
    pub async fn upsert_conversation(
        &self,
        owner: &UserId,
        meta: &ConversationMeta,
    ) -> AppResult<()> {
        meta.validate()?;
        let conn_arc = self.connection(owner).await?;
        let meta = meta.clone();
        let _: AppResult<()> = tokio::task::spawn_blocking(move || -> AppResult<()> {
            let guard = conn_arc.blocking_lock_owned();
            guard.execute(
                "INSERT INTO conversations
                 (conversation_id, kind, title, peer_user_id, last_message_preview,
                  last_activity, message_count, unread_count, peer_online, muted, pinned)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(conversation_id) DO UPDATE SET
                   kind = excluded.kind,
                   title = excluded.title,
                   peer_user_id = excluded.peer_user_id,
                   last_message_preview = excluded.last_message_preview,
                   last_activity = excluded.last_activity,
                   message_count = excluded.message_count,
                   unread_count = excluded.unread_count,
                   peer_online = excluded.peer_online,
                   muted = excluded.muted,
                   pinned = excluded.pinned",
                rusqlite::params![
                    meta.conversation_id.as_str(),
                    meta.kind.as_str(),
                    meta.title,
                    meta.peer_user_id.as_ref().map(|u| u.as_str()),
                    meta.last_message_preview,
                    meta.last_activity,
                    meta.message_count,
                    meta.unread_count,
                    meta.peer_online as i64,
                    meta.muted as i64,
                    meta.pinned as i64,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))?;
        Ok(())
    }

    /// Bulk counter update — kept for backward-compat with callers
    /// that pre-date the per-tx save_outbound. New code should call
    /// `save_outbound` or `record_inbound` instead.
    #[deprecated(note = "use save_outbound / record_inbound which are atomic")]
    pub async fn record_message(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        preview: &str,
        timestamp: i64,
        author: &UserId,
        title: &str,
        peer_user_id: Option<&UserId>,
        kind: a3chat_core::conversation::ConversationKind,
    ) -> AppResult<()> {
        let conn_arc = self.connection(owner).await?;
        let conv_str = conversation_id.as_str().to_string();
        let title = title.to_string();
        let author_str = author.as_str().to_string();
        let owner_str = owner.as_str().to_string();
        let peer_str = peer_user_id.map(|u| u.as_str().to_string());
        let preview = preview.to_string();
        let bounded_preview: String = if preview.chars().count() > PREVIEW_LEN {
            let mut s: String = preview.chars().take(PREVIEW_LEN).collect();
            s.push('…');
            s
        } else {
            preview
        };
        let kind_str = kind.as_str().to_string();
        let _: AppResult<()> = tokio::task::spawn_blocking(move || -> AppResult<()> {
            let mut guard = conn_arc.blocking_lock_owned();
            let tx = guard.transaction()?;
            let existing: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT message_count, unread_count FROM conversations WHERE conversation_id = ?1",
                    rusqlite::params![conv_str],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .ok();
            let (old_count, _old_unread) = existing.unwrap_or((0, 0));
            let new_count = (old_count as u32).saturating_add(1);
            let new_unread = if author_str == owner_str { 0 } else { new_count };
            tx.execute(
                "INSERT INTO conversations
                    (conversation_id, kind, title, peer_user_id,
                     last_message_preview, last_activity, message_count,
                     unread_count, peer_online, muted, pinned)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 0, 0)
                 ON CONFLICT(conversation_id) DO UPDATE SET
                    title               = excluded.title,
                    peer_user_id        = COALESCE(excluded.peer_user_id, conversations.peer_user_id),
                    last_message_preview = excluded.last_message_preview,
                    last_activity       = excluded.last_activity,
                    message_count       = excluded.message_count,
                    unread_count        = excluded.unread_count",
                rusqlite::params![
                    conv_str,
                    kind_str,
                    title,
                    peer_str,
                    bounded_preview,
                    timestamp,
                    new_count,
                    new_unread,
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))?;
        Ok(())
    }

    /// Load every conversation for `owner` — what the chat list UI
    /// renders.
    pub async fn list_conversations(&self, owner: &UserId) -> AppResult<Vec<ConversationMeta>> {
        let conn_arc = self.connection(owner).await?;
        let rows: Vec<ConversationMeta> = tokio::task::spawn_blocking(move || {
            let guard = conn_arc.blocking_lock_owned();
            let mut stmt = guard.prepare(
                "SELECT conversation_id, kind, title, peer_user_id,
                        last_message_preview, last_activity, message_count,
                        unread_count, peer_online, muted, pinned
                 FROM conversations
                 ORDER BY pinned DESC, last_activity DESC",
            )?;
            let rows = stmt
                .query_map([], row_to_meta)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<Vec<ConversationMeta>, AppError>(rows)
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;
        Ok(rows)
    }

    /// Total number of unread messages across all conversations.
    pub async fn unread_total(&self, owner: &UserId) -> AppResult<u32> {
        let conn_arc = self.connection(owner).await?;
        let n: i64 = tokio::task::spawn_blocking(move || -> AppResult<i64> {
            let guard = conn_arc.blocking_lock_owned();
            let n: i64 = guard
                .query_row(
                    "SELECT COALESCE(SUM(unread_count), 0) FROM conversations",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| AppError::Storage(e.to_string()))?;
            Ok(n)
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;
        Ok(n as u32)
    }

    /// Upsert a presence row for `user_id`. Used by the local
    /// `PresenceService::publish` path.
    pub async fn upsert_presence(&self, owner: &UserId, presence: &Presence) -> AppResult<()> {
        presence.validate()?;
        let conn_arc = self.connection(owner).await?;
        let presence = presence.clone();
        let _: AppResult<()> = tokio::task::spawn_blocking(move || -> AppResult<()> {
            let guard = conn_arc.blocking_lock_owned();
            guard.execute(
                "INSERT INTO presence (user_id, status, status_message, last_changed)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(user_id) DO UPDATE SET
                   status = excluded.status,
                   status_message = excluded.status_message,
                   last_changed = excluded.last_changed",
                rusqlite::params![
                    presence.user_id.as_str(),
                    presence.status.as_str(),
                    presence.status_message,
                    presence.last_changed.to_rfc3339(),
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))?;
        Ok(())
    }

    pub async fn get_presence(
        &self,
        owner: &UserId,
        user_id: &UserId,
    ) -> AppResult<Option<Presence>> {
        let conn_arc = self.connection(owner).await?;
        let id_str = user_id.as_str().to_string();
        let got: Option<Presence> = tokio::task::spawn_blocking(move || {
            let guard = conn_arc.blocking_lock_owned();
            let mut stmt = guard.prepare(
                "SELECT user_id, status, status_message, last_changed
                 FROM presence WHERE user_id = ?1",
            )?;
            let mut rows = stmt.query_map(rusqlite::params![id_str], row_to_presence)?;
            match rows.next() {
                Some(Ok(p)) => Ok(Some(p)),
                Some(Err(e)) => Err(AppError::Storage(e.to_string())),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;
        Ok(got)
    }

    /// Open a [`ConversationRecord`] — the full conversation view.
    pub async fn open_conversation(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<Option<ConversationRecord>> {
        let conn_arc = self.connection(owner).await?;
        let id_str = conversation_id.as_str().to_string();
        let got: Option<ConversationMeta> = tokio::task::spawn_blocking(move || {
            let guard = conn_arc.blocking_lock_owned();
            let mut stmt = guard.prepare(
                "SELECT conversation_id, kind, title, peer_user_id,
                        last_message_preview, last_activity, message_count,
                        unread_count, peer_online, muted, pinned
                 FROM conversations WHERE conversation_id = ?1",
            )?;
            let mut rows = stmt.query_map(rusqlite::params![id_str], row_to_meta)?;
            match rows.next() {
                Some(Ok(m)) => Ok(Some(m)),
                Some(Err(e)) => Err(AppError::Storage(e.to_string())),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))??;
        let meta = match got {
            Some(m) => m,
            None => return Ok(None),
        };
        let now = chrono::Utc::now();
        Ok(Some(ConversationRecord {
            meta,
            members: vec![], // Group members loaded by GroupService.
            created_at: now,
            updated_at: now,
        }))
    }

    /// Encrypt `body` using the session key for `peer` and the
    /// envelope context (sequence / timestamp / conversation_id) so
    /// the AD binds the ciphertext to the exact envelope — preventing
    /// cut-and-paste attacks.
    async fn encrypt_body(
        &self,
        owner: &UserId,
        peer: &UserId,
        envelope: &MessageEnvelope,
        body: &MessageBody,
    ) -> AppResult<MessageBody> {
        if body.is_encrypted() {
            return Ok(body.clone());
        }
        let key = self
            .inner
            .keyring
            .send_key_for(owner, peer)
            .ok_or_else(|| AppError::Crypto("keyring returned no send key".into()))?;

        let ad = build_ad(owner, peer, envelope);
        let plaintext = serde_json::to_vec(body)?;
        let (nonce, ciphertext) = a3chat_crypto::session::seal(&key, &ad, &plaintext)
            .map_err(|e| AppError::Crypto(format!("seal: {e}")))?;

        Ok(MessageBody::Encrypted {
            algorithm: "chacha20-poly1305-v1".to_string(),
            nonce: hex::encode(&nonce),
            ciphertext: base64::engine::general_purpose::STANDARD.encode(&ciphertext),
            tag: hex::encode(&ciphertext[ciphertext.len().saturating_sub(16)..]),
        })
    }
}

// -- helpers ----------------------------------------------------------------

/// Construct the AEAD associated-data buffer for a single message.
/// Layout: `sender | 0x00 | receiver | 0x00 | conversation_id | 0x00 |
///        sequence_le_u32 | timestamp_le_i64`.
/// The 0x00 separators ensure a peer can't smuggle a different
/// conversation_id by re-using a different (sequence, timestamp)
/// pair.
pub fn build_ad(owner: &UserId, peer: &UserId, env: &MessageEnvelope) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(owner.as_str().as_bytes());
    out.push(0x00);
    out.extend_from_slice(peer.as_str().as_bytes());
    out.push(0x00);
    out.extend_from_slice(env.conversation_id.as_str().as_bytes());
    out.push(0x00);
    out.extend_from_slice(&env.sequence.to_le_bytes());
    out.extend_from_slice(&env.timestamp.to_le_bytes());
    out
}

fn init_schema(conn: &rusqlite::Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        PRAGMA busy_timeout = 5000;

        CREATE TABLE IF NOT EXISTS conversations (
            conversation_id     TEXT PRIMARY KEY,
            kind                TEXT NOT NULL,
            title               TEXT NOT NULL,
            peer_user_id        TEXT,
            last_message_preview TEXT NOT NULL DEFAULT '',
            last_activity       INTEGER NOT NULL DEFAULT 0,
            message_count       INTEGER NOT NULL DEFAULT 0,
            unread_count        INTEGER NOT NULL DEFAULT 0,
            peer_online         INTEGER NOT NULL DEFAULT 0,
            muted               INTEGER NOT NULL DEFAULT 0,
            pinned              INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS messages (
            message_id          TEXT PRIMARY KEY,
            conversation_id     TEXT NOT NULL,
            sender_id           TEXT NOT NULL,
            receiver_id         TEXT NOT NULL,
            message_type        TEXT NOT NULL,
            body_json           TEXT NOT NULL,
            attachments_json    TEXT NOT NULL DEFAULT '[]',
            reply_to            TEXT,
            sequence            INTEGER NOT NULL,
            timestamp           INTEGER NOT NULL,
            read_at             TEXT,
            is_edited           INTEGER NOT NULL DEFAULT 0,
            edited_at           TEXT,
            integrity_hash      TEXT,
            recalled_at         TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_messages_conv_seq
            ON messages(conversation_id, sequence);
        CREATE INDEX IF NOT EXISTS idx_messages_conv_ts
            ON messages(conversation_id, timestamp);
        CREATE INDEX IF NOT EXISTS idx_messages_sender_seq
            ON messages(sender_id, conversation_id, sequence);
        CREATE INDEX IF NOT EXISTS idx_messages_unread
            ON messages(receiver_id, read_at) WHERE read_at IS NULL;

        CREATE TABLE IF NOT EXISTS presence (
            user_id             TEXT PRIMARY KEY,
            status              TEXT NOT NULL,
            status_message      TEXT,
            last_changed        TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS read_receipts (
            message_id          TEXT NOT NULL,
            reader_id           TEXT NOT NULL,
            read_at             TEXT NOT NULL,
            PRIMARY KEY(message_id, reader_id)
        );

        CREATE TABLE IF NOT EXISTS edit_history (
            message_id          TEXT NOT NULL,
            edited_at           TEXT NOT NULL,
            body_json           TEXT NOT NULL,
            PRIMARY KEY(message_id, edited_at)
        );

        CREATE TABLE IF NOT EXISTS message_search (
            message_id TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            body_text TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_message_search_text
            ON message_search(body_text);

        -- ── Chat trust (peer-feedback bridge to a3net-reputation) ──
        -- Owned by the chat layer; writes flow through
        -- `a3chat-app::peer_feedback_service::PeerFeedbackService`.
        -- When a `ReputationReporter` is attached, every `set` also
        -- emits a `ChatTrustSet` event into the global PeerScore.
        CREATE TABLE IF NOT EXISTS chat_trust (
            owner_user_id    TEXT NOT NULL,
            target_user_id   TEXT NOT NULL,
            level            INTEGER NOT NULL,
            last_event_unix  INTEGER NOT NULL,
            event_count      INTEGER NOT NULL DEFAULT 0,
            notes            TEXT,
            PRIMARY KEY (owner_user_id, target_user_id)
        );
        CREATE INDEX IF NOT EXISTS idx_chat_trust_owner
            ON chat_trust(owner_user_id, level DESC);
        "#,
    )?;
    Ok(())
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessage> {
    let body_json: String = row.get(5)?;
    let attachments_json: String = row.get(6)?;
    let reply_to: Option<String> = row.get(7)?;
    let read_at: Option<String> = row.get(10)?;
    let edited_at: Option<String> = row.get(12)?;
    let recalled_at: Option<String> = row.get(14)?;
    let message_type_str: String = row.get(4)?;

    Ok(ChatMessage {
        message_id: MessageId::from(row.get::<_, String>(0)?),
        conversation_id: ConversationId::from(row.get::<_, String>(1)?),
        sender_id: UserId::from(row.get::<_, String>(2)?),
        receiver_id: UserId::from(row.get::<_, String>(3)?),
        message_type: MessageType::parse(&message_type_str).unwrap_or(MessageType::Text),
        body: serde_json::from_str(&body_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?,
        attachments: serde_json::from_str(&attachments_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
        })?,
        reply_to: reply_to.map(MessageId::from),
        sequence: row.get(8)?,
        timestamp: row.get(9)?,
        read_at: read_at
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|d| d.with_timezone(&chrono::Utc)),
        is_edited: row.get::<_, i64>(11)? != 0,
        edited_at: edited_at
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|d| d.with_timezone(&chrono::Utc)),
        integrity_hash: row.get(13)?,
        recalled_at: recalled_at
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|d| d.with_timezone(&chrono::Utc)),
    })
}

fn row_to_meta(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationMeta> {
    let kind_str: String = row.get(1)?;
    let peer: Option<String> = row.get(3)?;
    Ok(ConversationMeta {
        conversation_id: ConversationId::from(row.get::<_, String>(0)?),
        kind: if kind_str == "group" {
            a3chat_core::conversation::ConversationKind::Group
        } else {
            a3chat_core::conversation::ConversationKind::Dm
        },
        title: row.get(2)?,
        peer_user_id: peer.map(UserId::from),
        last_message_preview: row.get(4)?,
        last_activity: row.get(5)?,
        message_count: row.get::<_, i64>(6)? as u32,
        unread_count: row.get::<_, i64>(7)? as u32,
        peer_online: row.get::<_, i64>(8)? != 0,
        muted: row.get::<_, i64>(9)? != 0,
        pinned: row.get::<_, i64>(10)? != 0,
    })
}

fn row_to_presence(row: &rusqlite::Row<'_>) -> rusqlite::Result<Presence> {
    let status_str: String = row.get(1)?;
    let last_changed_str: String = row.get(3)?;
    Ok(Presence {
        user_id: UserId::from(row.get::<_, String>(0)?),
        status: a3chat_core::presence::PresenceStatus::parse(&status_str)
            .unwrap_or(a3chat_core::presence::PresenceStatus::Offline),
        status_message: row.get(2)?,
        last_changed: chrono::DateTime::parse_from_rfc3339(&last_changed_str)
            .map(|d| d.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now()),
    })
}

/// Body integrity hash — for plaintext bodies only. Encrypted bodies
/// rely on the AEAD tag. AD is `sender | 0x00 | receiver | 0x00 |
/// canonical_json(body)` so two peers can't produce the same hash for
/// different plaintexts.
pub fn integrity_hash(sender: &UserId, receiver: &UserId, env: &MessageEnvelope) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"a3chat/integrity/v1");
    h.update(sender.as_bytes());
    h.update(b"\x00");
    h.update(receiver.as_bytes());
    h.update(b"\x00");
    h.update(env.conversation_id.as_str().as_bytes());
    h.update(b"\x00");
    h.update(&env.sequence.to_le_bytes());
    h.update(&env.timestamp.to_le_bytes());
    let json = serde_json::to_string(&env.body).unwrap_or_default();
    h.update(json.as_bytes());
    hex::encode(h.finalize().as_bytes())
}

// -- Tying everything together: A3chatError bridge ----------------------------

impl From<AppError> for A3chatError {
    fn from(e: AppError) -> Self {
        match e {
            AppError::Domain(m) => A3chatError::Internal(m),
            AppError::Storage(m) => A3chatError::StorageError(m),
            AppError::Crypto(m) => A3chatError::CryptoError(m),
            AppError::NotInitialised(m) => A3chatError::Internal(format!("not initialised: {m}")),
            AppError::Forbidden(m) => A3chatError::PermissionDenied(m),
            AppError::Conflict(m) => A3chatError::InvalidInput(m),
            AppError::Network(m) => A3chatError::NetworkError(m),
            AppError::Rpc(m) => A3chatError::RpcError(m),
            AppError::Internal(m) => A3chatError::Internal(m),
        }
    }
}

// Synchronous lock helper used inside `spawn_blocking` closures.
// `tokio::sync::Mutex::lock_owned` returns an `OwnedMutexGuard` only
// when awaited, so we drive the future synchronously via the
// current-thread runtime handle. Safe because `spawn_blocking`
// always runs on a dedicated blocking thread.
#[allow(dead_code)]
trait BlockingLockOwned {
    type Guard;
    fn blocking_lock_owned(self: Arc<Self>) -> Self::Guard;
}

impl BlockingLockOwned for tokio::sync::Mutex<rusqlite::Connection> {
    type Guard = tokio::sync::OwnedMutexGuard<rusqlite::Connection>;
    fn blocking_lock_owned(self: Arc<Self>) -> Self::Guard {
        // `lock_owned` requires `.await`; we drive it via the
        // current-thread runtime handle when called from
        // `spawn_blocking`. This is safe because `spawn_blocking`
        // runs on a dedicated thread with no live runtime handle by
        // default — but tokio installs a `Handle::current()` so
        // `block_in_place` style APIs work. Here we just block the
        // thread; if the runtime is single-threaded this would
        // deadlock. `spawn_blocking` always runs on a blocking
        // thread (not on a worker thread) so the future is safe to
        // drive via `Handle::block_on`.
        let h = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => {
                // No runtime — build a tiny current-thread one.
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build current-thread runtime");
                return rt.block_on(async move { self.lock_owned().await });
            }
        };
        h.block_on(async move { self.lock_owned().await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> UserId {
        UserId::from("alice-node-id")
    }
    fn peer() -> UserId {
        UserId::from("bob-node-id")
    }

    async fn fresh_storage() -> (tempfile::TempDir, ChatStorage) {
        let dir = tempfile::tempdir().unwrap();
        let keyring = E2eKeyring::new(owner());
        let cfg = StorageConfig::new(dir.path().to_path_buf());
        let storage = ChatStorage::new(cfg, keyring);
        storage.init_user(&owner()).await.unwrap();
        (dir, storage)
    }

    fn envelope() -> MessageEnvelope {
        MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice:bob"),
            receiver_id: peer(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "hello bob".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
        }
    }

    #[tokio::test]
    async fn save_outbound_persists_message() {
        let (_dir, storage) = fresh_storage().await;
        let stored = storage.save_outbound(&owner(), &envelope()).await.unwrap();
        assert_eq!(stored.message.sequence, 1);
        assert_eq!(stored.message.sender_id, owner());
    }

    #[tokio::test]
    async fn save_outbound_sequence_conflict_is_rejected() {
        let (_dir, storage) = fresh_storage().await;
        storage.save_outbound(&owner(), &envelope()).await.unwrap();
        // Replay the same sequence — must conflict.
        let err = storage.save_outbound(&owner(), &envelope()).await.unwrap_err();
        match err {
            AppError::Conflict(_) => {}
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_messages_returns_in_chronological_order() {
        let (_dir, storage) = fresh_storage().await;
        for i in 1..=3 {
            let mut env = envelope();
            env.sequence = i;
            env.timestamp += i as i64;
            storage.save_outbound(&owner(), &env).await.unwrap();
        }
        let messages = storage
            .list_messages(&owner(), &ConversationId::from("dm:alice:bob"), 10)
            .await
            .unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].sequence, 1);
        assert_eq!(messages[2].sequence, 3);
    }

    #[tokio::test]
    async fn list_messages_caps_huge_limit() {
        let (_dir, storage) = fresh_storage().await;
        // limit > HARD_LIMIT_CAP must be silently clamped, not error.
        let _ = storage
            .list_messages(&owner(), &ConversationId::from("dm:nobody"), 1_000_000)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn list_messages_zero_limit_returns_empty() {
        // `limit.max(1)` clamps 0 to 1, so we expect a successful
        // empty list rather than an error — defence against buggy
        // clients passing 0.
        let (_dir, storage) = fresh_storage().await;
        let msgs = storage
            .list_messages(&owner(), &ConversationId::from("dm:nobody"), 0)
            .await
            .unwrap();
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn get_message_returns_specific_message() {
        let (_dir, storage) = fresh_storage().await;
        let stored = storage.save_outbound(&owner(), &envelope()).await.unwrap();
        let found = storage
            .get_message(&owner(), &stored.message.message_id)
            .await
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().message_id, stored.message.message_id);
    }

    #[tokio::test]
    async fn get_message_returns_none_for_unknown() {
        let (_dir, storage) = fresh_storage().await;
        let r = storage
            .get_message(&owner(), &MessageId::from("0".repeat(64)))
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn ack_marks_message_as_read_and_idempotent() {
        let (_dir, storage) = fresh_storage().await;
        let stored = storage.save_outbound(&owner(), &envelope()).await.unwrap();
        storage
            .ack_message(&owner(), &stored.message.message_id)
            .await
            .unwrap();
        let got = storage
            .get_message(&owner(), &stored.message.message_id)
            .await
            .unwrap()
            .unwrap();
        assert!(got.read_at.is_some());
        // Second ack is a no-op.
        storage
            .ack_message(&owner(), &stored.message.message_id)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn ack_unknown_message_errors_with_domain() {
        let (_dir, storage) = fresh_storage().await;
        let r = storage
            .ack_message(&owner(), &MessageId::from("1".repeat(64)))
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn ack_inbound_only_decrements_unread_for_others() {
        let (_dir, storage) = fresh_storage().await;
        // Build an inbound message as if from peer.
        let inbound = ChatMessage {
            message_id: a3chat_core::id::generate_message_id("peer"),
            conversation_id: ConversationId::from("dm:alice:bob"),
            sender_id: peer(),
            receiver_id: owner(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "hi alice".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_001,
            read_at: None,
            is_edited: false,
            edited_at: None,
            integrity_hash: None,
            recalled_at: None,
        };
        let outcome = storage.record_inbound(&owner(), &inbound).await.unwrap();
        assert_eq!(outcome.new_unread_count, 1);
        storage
            .ack_message(&owner(), &inbound.message_id)
            .await
            .unwrap();
        let convos = storage.list_conversations(&owner()).await.unwrap();
        assert_eq!(convos[0].unread_count, 0);
    }

    #[tokio::test]
    async fn ack_outbound_message_does_not_decrement_unread() {
        let (_dir, storage) = fresh_storage().await;
        let stored = storage.save_outbound(&owner(), &envelope()).await.unwrap();
        // ack our own outbound — unread was already 0, but the
        // conv row might still bump if other inbound messages
        // landed; verify we never go below zero.
        storage
            .ack_message(&owner(), &stored.message.message_id)
            .await
            .unwrap();
        let convos = storage.list_conversations(&owner()).await.unwrap();
        assert_eq!(convos[0].unread_count, 0);
    }

    #[tokio::test]
    async fn recall_sets_recalled_at_and_is_idempotent() {
        let (_dir, storage) = fresh_storage().await;
        let stored = storage.save_outbound(&owner(), &envelope()).await.unwrap();
        storage
            .recall_message(&owner(), &stored.message.message_id)
            .await
            .unwrap();
        let got = storage
            .get_message(&owner(), &stored.message.message_id)
            .await
            .unwrap()
            .unwrap();
        assert!(got.recalled_at.is_some());
        // Second recall is idempotent.
        storage
            .recall_message(&owner(), &stored.message.message_id)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn recall_by_non_sender_is_forbidden_or_not_found() {
        let (_dir, storage) = fresh_storage().await;
        let stored = storage.save_outbound(&owner(), &envelope()).await.unwrap();
        // peer()'s storage is a different DB, so recall returns
        // Domain (message not found) — this is also a valid defence
        // in depth.
        let err_peer = storage
            .recall_message(&peer(), &stored.message.message_id)
            .await
            .unwrap_err();
        assert!(
            matches!(err_peer, AppError::Domain(_) | AppError::Forbidden(_)),
            "got {err_peer:?}",
        );
        // Same DB, but a message authored by someone else — Forbidden.
        let mut foreign = stored.message.clone();
        foreign.sender_id = peer();
        storage.record_inbound(&owner(), &foreign).await.unwrap();
        let err_owner = storage
            .recall_message(&owner(), &foreign.message_id)
            .await
            .unwrap_err();
        assert!(matches!(err_owner, AppError::Forbidden(_)));
    }

    #[tokio::test]
    async fn recall_unknown_message_errors_with_domain() {
        let (_dir, storage) = fresh_storage().await;
        let r = storage
            .recall_message(&owner(), &MessageId::from("2".repeat(64)))
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn edit_message_by_sender_updates_body_and_flags() {
        let (_dir, storage) = fresh_storage().await;
        // Save an envelope whose body is already-encrypted so the
        // edit path doesn't try to re-seal it. The stored result
        // is then plaintext-equivalent (a new ciphertext) and we
        // can verify the edit flags flipped.
        let env = MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice:bob"),
            receiver_id: peer(),
            message_type: MessageType::Text,
            body: MessageBody::Encrypted {
                algorithm: "chacha20-poly1305-v1".into(),
                nonce: "a".repeat(24),
                ciphertext: "ZW5jcnlwdGVk".into(),
                tag: "b".repeat(32),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
        };
        let stored = storage.save_outbound(&owner(), &env).await.unwrap();
        let new_body = MessageBody::Plain {
            content: "edited".into(),
        };
        let edited = storage
            .edit_message(&owner(), &stored.message.message_id, &new_body)
            .await
            .unwrap();
        assert!(edited.is_edited);
        assert!(edited.edited_at.is_some());
        // The body is now re-sealed (still encrypted).
        assert!(edited.body.is_encrypted());
    }

    #[tokio::test]
    async fn edit_message_by_non_sender_returns_forbidden_or_not_found() {
        let (_dir, storage) = fresh_storage().await;
        let stored = storage.save_outbound(&owner(), &envelope()).await.unwrap();
        // peer's storage doesn't have this message — get_message
        // returns None, surfacing as Domain. From the sender's own
        // storage we'd see Forbidden. Both are valid defence in
        // depth: we test both paths.
        let err_peer = storage
            .edit_message(
                &peer(),
                &stored.message.message_id,
                &MessageBody::Plain {
                    content: "tampered".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err_peer, AppError::Domain(_) | AppError::Forbidden(_)),
            "got {err_peer:?}",
        );

        // Now simulate the *sender's* storage seeing a row whose
        // sender_id is someone else — Forbidden must fire.
        let mut foreign = stored.message.clone();
        foreign.sender_id = peer();
        storage.record_inbound(&owner(), &foreign).await.unwrap();
        let err_owner = storage
            .edit_message(
                &owner(),
                &foreign.message_id,
                &MessageBody::Plain {
                    content: "tampered".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err_owner, AppError::Forbidden(_)));
    }

    #[tokio::test]
    async fn edit_unknown_message_errors_with_domain() {
        let (_dir, storage) = fresh_storage().await;
        let err = storage
            .edit_message(
                &owner(),
                &MessageId::from("3".repeat(64)),
                &MessageBody::Plain {
                    content: "x".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn delete_message_for_me_removes_row() {
        let (_dir, storage) = fresh_storage().await;
        let stored = storage.save_outbound(&owner(), &envelope()).await.unwrap();
        storage
            .delete_message_for_me(&owner(), &stored.message.message_id)
            .await
            .unwrap();
        let got = storage
            .get_message(&owner(), &stored.message.message_id)
            .await
            .unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn delete_unknown_message_errors_with_domain() {
        let (_dir, storage) = fresh_storage().await;
        let r = storage
            .delete_message_for_me(&owner(), &MessageId::from("4".repeat(64)))
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn search_messages_finds_plaintext_match() {
        // Disable E2E so the body is stored as plaintext and the
        // substring search can hit it. Encrypted-body search is
        // intentionally impossible server-side (by design).
        let dir = tempfile::tempdir().unwrap();
        let keyring = E2eKeyring::new(owner());
        let mut cfg = StorageConfig::new(dir.path().to_path_buf());
        cfg.enable_e2e = false;
        let storage = ChatStorage::new(cfg, keyring);
        storage.init_user(&owner()).await.unwrap();
        storage.save_outbound(&owner(), &envelope()).await.unwrap();
        let hits = storage
            .search_messages(SearchQuery {
                owner: &owner(),
                needle: "hello",
                conversation_id: None,
                limit: 10,
            })
            .await
            .unwrap();
        assert!(!hits.is_empty());
    }

    #[tokio::test]
    async fn search_messages_skips_encrypted_bodies() {
        // Encrypted bodies must never match — that's the whole point
        // of E2E.
        let (_dir, storage) = fresh_storage().await;
        storage.save_outbound(&owner(), &envelope()).await.unwrap();
        let hits = storage
            .search_messages(SearchQuery {
                owner: &owner(),
                needle: "hello",
                conversation_id: None,
                limit: 10,
            })
            .await
            .unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn search_messages_rejects_empty_needle() {
        let (_dir, storage) = fresh_storage().await;
        let r = storage
            .search_messages(SearchQuery {
                owner: &owner(),
                needle: "",
                conversation_id: None,
                limit: 10,
            })
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn search_messages_rejects_oversize_needle() {
        let (_dir, storage) = fresh_storage().await;
        let r = storage
            .search_messages(SearchQuery {
                owner: &owner(),
                needle: &"x".repeat(257),
                conversation_id: None,
                limit: 10,
            })
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn upsert_and_list_conversations() {
        let (_dir, storage) = fresh_storage().await;
        let meta = ConversationMeta {
            conversation_id: ConversationId::from("dm:alice:bob"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "Bob".into(),
            peer_user_id: Some(peer()),
            last_message_preview: "hello".into(),
            last_activity: 100,
            message_count: 1,
            unread_count: 1,
            peer_online: false,
            muted: false,
            pinned: false,
        };
        storage.upsert_conversation(&owner(), &meta).await.unwrap();
        let all = storage.list_conversations(&owner()).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "Bob");
    }

    #[tokio::test]
    async fn upsert_conversation_rejects_invalid_meta() {
        let (_dir, storage) = fresh_storage().await;
        let mut meta = ConversationMeta {
            conversation_id: ConversationId::from("dm:alice:bob"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "Bob".into(),
            peer_user_id: Some(peer()),
            last_message_preview: "hello".into(),
            last_activity: 100,
            message_count: 1,
            unread_count: 1,
            peer_online: false,
            muted: false,
            pinned: false,
        };
        meta.unread_count = 5;
        meta.message_count = 2; // unread > message
        let r = storage.upsert_conversation(&owner(), &meta).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn unread_total_sums_correctly() {
        let (_dir, storage) = fresh_storage().await;
        let meta1 = ConversationMeta {
            conversation_id: ConversationId::from("dm:alice:bob"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "Bob".into(),
            peer_user_id: Some(peer()),
            last_message_preview: "".into(),
            last_activity: 0,
            message_count: 5,
            unread_count: 5,
            peer_online: false,
            muted: false,
            pinned: false,
        };
        let meta2 = ConversationMeta {
            conversation_id: ConversationId::from("dm:alice:carol"),
            unread_count: 3,
            message_count: 3,
            ..meta1.clone()
        };
        storage.upsert_conversation(&owner(), &meta1).await.unwrap();
        storage.upsert_conversation(&owner(), &meta2).await.unwrap();
        let total = storage.unread_total(&owner()).await.unwrap();
        assert_eq!(total, 8);
    }

    #[tokio::test]
    async fn presence_round_trip() {
        let (_dir, storage) = fresh_storage().await;
        let p = Presence {
            user_id: peer(),
            status: a3chat_core::presence::PresenceStatus::Online,
            status_message: Some("ready".into()),
            last_changed: chrono::Utc::now(),
        };
        storage.upsert_presence(&owner(), &p).await.unwrap();
        let got = storage
            .get_presence(&owner(), &peer())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.status, a3chat_core::presence::PresenceStatus::Online);
        assert_eq!(got.status_message.as_deref(), Some("ready"));
    }

    #[tokio::test]
    async fn open_conversation_returns_record() {
        let (_dir, storage) = fresh_storage().await;
        let meta = ConversationMeta {
            conversation_id: ConversationId::from("dm:alice:bob"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "Bob".into(),
            peer_user_id: Some(peer()),
            last_message_preview: "".into(),
            last_activity: 0,
            message_count: 0,
            unread_count: 0,
            peer_online: false,
            muted: false,
            pinned: false,
        };
        storage.upsert_conversation(&owner(), &meta).await.unwrap();
        let rec = storage
            .open_conversation(&owner(), &ConversationId::from("dm:alice:bob"))
            .await
            .unwrap();
        assert!(rec.is_some());
        assert_eq!(rec.unwrap().meta.title, "Bob");
    }

    #[tokio::test]
    async fn open_unknown_conversation_returns_none() {
        let (_dir, storage) = fresh_storage().await;
        let r = storage
            .open_conversation(&owner(), &ConversationId::from("dm:unknown"))
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn integrity_hash_is_deterministic() {
        let env = envelope();
        let h1 = integrity_hash(&owner(), &peer(), &env);
        let h2 = integrity_hash(&owner(), &peer(), &env);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[tokio::test]
    async fn integrity_hash_differs_with_sender() {
        let env = envelope();
        let h1 = integrity_hash(&owner(), &peer(), &env);
        let h2 = integrity_hash(&peer(), &owner(), &env);
        assert_ne!(h1, h2);
    }

    #[tokio::test]
    async fn integrity_hash_includes_conversation_id() {
        let mut env_a = envelope();
        env_a.conversation_id = ConversationId::from("dm:a:b");
        let mut env_b = envelope();
        env_b.conversation_id = ConversationId::from("dm:a:c");
        assert_ne!(
            integrity_hash(&owner(), &peer(), &env_a),
            integrity_hash(&owner(), &peer(), &env_b),
        );
    }

    #[test]
    fn storage_config_path_is_per_user() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = StorageConfig::new(dir.path().to_path_buf());
        let p = cfg.path_for(&owner());
        assert!(p.ends_with("alice-node-id.sqlite"));
    }

    #[tokio::test]
    async fn save_outbound_with_e2e_seals_the_body() {
        let (_dir, storage) = fresh_storage().await;
        let stored = storage.save_outbound(&owner(), &envelope()).await.unwrap();
        assert!(stored.message.body.is_encrypted());
        assert!(stored.was_encrypted_at_write);
        if let MessageBody::Encrypted { ciphertext, .. } = &stored.message.body {
            assert!(!ciphertext.contains("hello bob"));
            let raw = base64::engine::general_purpose::STANDARD
                .decode(ciphertext)
                .expect("base64 decode");
            assert!(raw.len() >= "hello bob".len() + 16);
        } else {
            panic!("expected Encrypted body");
        }
    }

    #[tokio::test]
    async fn encrypted_body_round_trips_through_storage() {
        let (_dir, storage) = fresh_storage().await;
        let env = MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice:bob"),
            receiver_id: peer(),
            message_type: MessageType::Text,
            body: MessageBody::Encrypted {
                algorithm: "chacha20-poly1305-v1".into(),
                nonce: "a".repeat(24),
                ciphertext: "ZW5jcnlwdGVk".into(),
                tag: "b".repeat(32),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
        };
        let stored = storage.save_outbound(&owner(), &env).await.unwrap();
        let got = storage
            .get_message(&owner(), &stored.message.message_id)
            .await
            .unwrap()
            .unwrap();
        assert!(got.body.is_encrypted());
        assert!(got.validate().is_ok());
    }

    #[tokio::test]
    async fn encrypt_body_is_idempotent_under_repeated_send() {
        let (_dir, storage) = fresh_storage().await;
        let mut env = envelope();
        env.body = MessageBody::Plain {
            content: "same".into(),
        };
        let a = storage.save_outbound(&owner(), &env).await.unwrap();
        env.sequence = 2;
        env.timestamp += 1;
        let b = storage.save_outbound(&owner(), &env).await.unwrap();
        if let (
            MessageBody::Encrypted { nonce: na, .. },
            MessageBody::Encrypted { nonce: nb, .. },
        ) = (&a.message.body, &b.message.body)
        {
            assert_ne!(na, nb, "nonces must differ");
        } else {
            panic!("expected Encrypted bodies");
        }
    }

    #[tokio::test]
    async fn concurrent_sends_to_same_conversation_are_serialised() {
        // Two concurrent outbound calls must not deadlock and must
        // produce strictly monotonic sequence numbers — the
        // per-user Mutex + MAX(seq) check inside the tx is what
        // gives us the monotonic invariant.
        let (_dir, storage) = fresh_storage().await;
        let mut hs = Vec::new();
        for i in 1..=8 {
            let (s, o, p, e) = (storage.clone(), owner(), peer(), envelope());
            let mut e = e;
            e.sequence = i;
            e.timestamp = 1_700_000_000 + i as i64;
            hs.push(tokio::spawn(async move { s.save_outbound(&o, &e).await }));
        }
        let mut ok = 0;
        for h in hs {
            if h.await.unwrap().is_ok() {
                ok += 1;
            }
        }
        assert_eq!(ok, 8);
        let msgs = storage
            .list_messages(&owner(), &ConversationId::from("dm:alice:bob"), 100)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 8);
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(m.sequence as usize, i + 1);
        }
    }

    #[tokio::test]
    async fn many_users_isolated_db_files() {
        let dir = tempfile::tempdir().unwrap();
        let keyring_alice = E2eKeyring::new(owner());
        let cfg = StorageConfig::new(dir.path().to_path_buf());
        let storage_alice = ChatStorage::new(cfg.clone(), keyring_alice);
        storage_alice.init_user(&owner()).await.unwrap();

        let bob = UserId::from("bob-node-id");
        let keyring_bob = E2eKeyring::new(bob.clone());
        let storage_bob = ChatStorage::new(cfg, keyring_bob);
        storage_bob.init_user(&bob).await.unwrap();

        // Alice's writes must NOT be visible in Bob's DB.
        storage_alice
            .save_outbound(&owner(), &envelope())
            .await
            .unwrap();
        let alice_msgs = storage_alice
            .list_messages(&owner(), &ConversationId::from("dm:alice:bob"), 10)
            .await
            .unwrap();
        assert_eq!(alice_msgs.len(), 1);
        let bob_msgs = storage_bob
            .list_messages(&bob, &ConversationId::from("dm:alice:bob"), 10)
            .await
            .unwrap();
        assert!(bob_msgs.is_empty());
    }
}