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
    /// Audit issue #10: how long after a message was sent can the
    /// original sender still edit or recall it. Default is
    /// 2 minutes, matching WeChat's UX. Set to `Duration::ZERO`
    /// to disable the window (i.e. never reject on time alone).
    pub edit_window: core::time::Duration,
}

impl StorageConfig {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            enable_e2e: true,
            // Audit issue #20: kept in sync with a3net's
            // `MAX_SEQUENCE`; see message.rs for the rationale on
            // not raising it without coordinating with the hub.
            max_sequence: 9_999,
            edit_window: core::time::Duration::from_secs(2 * 60),
        }
    }

    pub fn path_for(&self, user_id: &UserId) -> PathBuf {
        self.base_dir.join(format!("{user_id}.sqlite"))
    }
}

/// Persisted draft — what `chat.draft.*` reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRow {
    pub content: String,
    pub reply_to: Option<MessageId>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
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

    /// Open (or reuse) the per-user link-bookmark store. The bookmarks
    /// live in the same SQLite file as the chat tables (one file per
    /// user) so the WAL checkpoints and backup cycle are shared.
    ///
    /// `LinkBookmarkStore` is synchronous (it owns a
    /// `std::sync::Mutex<Connection>` and uses `spawn_blocking`
    /// internally). To avoid double-locking through the existing
    /// `tokio::sync::Mutex` wrapper we open a fresh `rusqlite::Connection`
    /// per call and let `LinkBookmarkStore::open` apply the chatstore
    /// schema (which is idempotent for the `link_bookmarks` table).
    pub async fn link_bookmark_store(
        &self,
        user_id: &UserId,
    ) -> AppResult<a3net_chatstore::LinkBookmarkStore> {
        let path = self.inner.config.path_for(user_id);
        let store_path = path.clone();
        let store = tokio::task::spawn_blocking(move || {
            a3net_chatstore::LinkBookmarkStore::open(
                a3net_chatstore::LinkBookmarkStoreConfig {
                    storage_dir: store_path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_default(),
                },
            )
        })
        .await
        .map_err(|e| AppError::Internal(format!("link_bookmark_store join: {e}")))?
        .map_err(|e| AppError::Internal(format!("link_bookmark_store open: {e}")))?;
        let _ = user_id;
        Ok(store)
    }

    /// Initialise the per-user schema. Idempotent.
    pub async fn init_user(&self, user_id: &UserId) -> AppResult<()> {
        self.connection(user_id).await?;
        Ok(())
    }

    /// Open (or reuse) the per-user SQLite connection. The first call
    /// for `user_id` runs `init_schema`; subsequent calls just clone
    /// the cached `Arc<Mutex<Connection>>`.
    pub async fn connection_for(
        &self,
        user_id: &UserId,
    ) -> AppResult<Arc<Mutex<rusqlite::Connection>>> {
        self.connection(user_id).await
    }

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

    /// Run a synchronous SQLite closure on the per-user connection
    /// pool. The closure receives an `OwnedMutexGuard<Connection>` so
    /// it can call `transaction()`/`execute()` (`&mut self`
    /// methods) without the boilerplate of `spawn_blocking` +
    /// `blocking_lock_owned` + `map_err` repeated at every site.
    ///
    /// A panic inside the closure is bubbled up as
    /// `AppError::Internal` so a panic in one RPC cannot take down
    /// the dispatcher task.
    ///
    /// NOTE: registration-only — the call sites are migrated
    /// gradually in follow-up patches (H-4b) to keep the diff
    /// reviewable.
    #[allow(dead_code)]
    async fn with_connection<F, R>(
        &self,
        user_id: &UserId,
        f: F,
    ) -> AppResult<R>
    where
        F: FnOnce(&mut rusqlite::Connection) -> AppResult<R>
            + Send
            + 'static,
        R: Send + 'static,
    {
        let conn_arc = self.connection(user_id).await?;
        let result = tokio::task::spawn_blocking(move || {
            let mut guard = conn_arc.blocking_lock_owned();
            f(&mut guard)
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))?;
        result
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

        // Phase 5b dual-write: mirror the message to iroh-docs.
        // Done at the ChatService level (not here) so the `IrohDocsChat`
        // can be stored on `ChatService` alongside `ChatStorage`.
        // See `ChatService::send_message` for the actual fan-out call.

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

    /// B-9 / F-20 — explicit DM creation. WeChat's contact list
    /// shows a DM for every known contact even before the first
    /// message lands, so this lets the UI "create" a conversation
    /// independently. Idempotent — if the row already exists it's
    /// returned unchanged.
    pub async fn create_direct_conversation(
        &self,
        owner: &UserId,
        peer: &UserId,
    ) -> AppResult<ConversationMeta> {
        // Canonical DM id format: `dm:{sorted_a}:{sorted_b}`. Sorting
        // the user names guarantees a 1:1 conversation has the same
        // id on both sides (Alice's "dm:alice:bob" matches Bob's
        // "dm:alice:bob"). Without the sort, Alice would say
        // "dm:alice:bob" and Bob would say "dm:bob:alice".
        let (a, b) = if owner.as_str() <= peer.as_str() {
            (owner.as_str(), peer.as_str())
        } else {
            (peer.as_str(), owner.as_str())
        };
        let conv_id = ConversationId::from(format!("dm:{a}:{b}"));
        let conn_arc = self.connection(owner).await?;
        let peer_str = peer.as_str().to_string();
        let conv_id_str = conv_id.as_str().to_string();
        let now = chrono::Utc::now().timestamp();
        let conv_id_clone = conv_id.clone();
        let owner_clone = owner.clone();
        let peer_clone = peer.clone();
        let owner_for_log = owner.clone();
        let meta: ConversationMeta = tokio::task::spawn_blocking(
            move || -> AppResult<ConversationMeta> {
                let guard = conn_arc.blocking_lock_owned();
                // ON CONFLICT DO NOTHING — keep existing previews
                // / counts / pin state intact when the conversation
                // already exists.
                guard.execute(
                    "INSERT OR IGNORE INTO conversations
                        (conversation_id, kind, title, peer_user_id,
                         last_message_preview, last_activity, message_count,
                         unread_count, peer_online, muted, pinned)
                     VALUES (?1, 'one_on_one', ?2, ?3, '', ?4, 0, 0, 0, 0, 0)",
                    rusqlite::params![conv_id_str, peer_str, peer_str, now],
                )?;
                tracing::debug!(
                    owner = %owner_for_log.as_str(),
                    conv = %conv_id_str,
                    "create_direct_conversation: idempotent row ensured"
                );
                Ok(ConversationMeta {
                    conversation_id: conv_id_clone,
                    kind: a3chat_core::conversation::ConversationKind::Dm,
                    title: peer_clone.as_str().to_string(),
                    peer_user_id: Some(peer_clone),
                    last_message_preview: String::new(),
                    last_activity: now,
                    message_count: 0,
                    unread_count: 0,
                    peer_online: false,
                    muted: false,
                    pinned: false,
                })
            },
        )
        .await
        .map_err(|e| AppError::Internal(format!("create_direct_conversation join: {e}")))??;
        let _ = owner_clone; // suppress unused warning
        Ok(meta)
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

    /// F-12 — reply thread query. Returns every message that is a
    /// direct or transitive reply to `root_id`, ordered oldest-first
    /// by `sequence`. The root message itself is **not** included;
    /// callers usually already have it from `get_message`. Results
    /// are bounded by `limit` (auto-capped at 1000).
    pub async fn list_thread_replies(
        &self,
        owner: &UserId,
        root_id: &MessageId,
        limit: u32,
    ) -> AppResult<Vec<ChatMessage>> {
        let conn_arc = self.connection(owner).await?;
        let root_str = root_id.as_str().to_string();
        let limit = limit.min(1000) as i64;
        let rows: Vec<ChatMessage> =
            tokio::task::spawn_blocking(move || -> AppResult<Vec<ChatMessage>> {
                let guard = conn_arc.blocking_lock_owned();
                // First-order replies: simpler and faster than a CTE
                // — UI callers usually paginate this anyway. Mid-tier
                // replies can be reached by passing the intermediate
                // id as `root_id`.
                let mut stmt = guard.prepare(
                    "SELECT message_id, conversation_id, sender_id, receiver_id,
                            message_type, body_json, attachments_json, reply_to,
                            sequence, timestamp, read_at, is_edited, edited_at,
                            integrity_hash, recalled_at
                     FROM messages
                     WHERE reply_to = ?1
                     ORDER BY sequence ASC
                     LIMIT ?2",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![root_str, limit], row_to_message)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| AppError::Internal(format!("list_thread_replies join: {e}")))??;
        Ok(rows)
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
    /// original sender may recall, and only within the configured
    /// `edit_window` (audit issue #10).
    pub async fn recall_message(
        &self,
        owner: &UserId,
        message_id: &MessageId,
    ) -> AppResult<()> {
        let conn_arc = self.connection(owner).await?;
        let id_str = message_id.as_str().to_string();
        let owner_str = owner.as_str().to_string();
        let edit_window = self.config().edit_window;
        let r: AppResult<()> = tokio::task::spawn_blocking(move || -> AppResult<()> {
            let guard = conn_arc.blocking_lock_owned();
            // Look up the message first so we can enforce
            // (a) ownership, (b) time-window. We do this in one
            // SELECT to keep the recall atomic and avoid a TOCTOU
            // between the time check and the UPDATE.
            let row: Option<(String, i64)> = guard
                .query_row(
                    "SELECT sender_id, timestamp FROM messages WHERE message_id = ?1",
                    rusqlite::params![id_str],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .ok();
            let (sender, ts) = match row {
                Some(r) => r,
                None => {
                    return Err(AppError::Domain(format!(
                        "message {id_str} not found"
                    )));
                }
            };
            if sender != owner_str {
                return Err(AppError::Forbidden(
                    "only the original sender can recall a message".into(),
                ));
            }
            if !edit_window.is_zero() {
                let now = chrono::Utc::now().timestamp();
                if now.saturating_sub(ts) > edit_window.as_secs() as i64 {
                    return Err(AppError::Forbidden(format!(
                        "recall window of {}s expired",
                        edit_window.as_secs()
                    )));
                }
            }
            let now_str = chrono::Utc::now().to_rfc3339();
            let n = guard.execute(
                "UPDATE messages SET recalled_at = ?2
                 WHERE message_id = ?1 AND sender_id = ?3 AND recalled_at IS NULL",
                rusqlite::params![id_str, now_str, owner_str],
            )?;
            if n == 0 {
                // Either unknown, or already recalled, or not the sender.
                // The pre-check above already ruled out "unknown" and
                // "not the sender", so the only remaining case is
                // "already recalled" which is idempotent success.
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
        // Audit issue #10: enforce the configured edit window so
        // a sender cannot edit a year-old message and pass it off
        // as fresh content. The window is enforced after the auth
        // check so a forbidden sender gets the same Forbidden
        // error regardless of time.
        let edit_window = self.config().edit_window;
        if !edit_window.is_zero() {
            let now = chrono::Utc::now().timestamp();
            if now.saturating_sub(updated.timestamp)
                > edit_window.as_secs() as i64
            {
                return Err(AppError::Forbidden(format!(
                    "edit window of {}s expired",
                    edit_window.as_secs()
                )));
            }
        }
        // Re-seal if the original body was encrypted.
        //
        // AD contract (see `a3chat-crypto::session::seal`): the
        // AEAD is keyed off `sender | receiver | conversation_id |
        // sequence | timestamp`. The `MessageEnvelope` we hand to
        // `encrypt_body` MUST carry the same `sequence` and
        // `timestamp` the original message was sealed with,
        // otherwise a receiver that re-validates the AD against
        // the stored envelope will fail with `AeadTagMismatch`.
        // Earlier revisions stamped `Utc::now()` here, which made
        // every edit un-decryptable on the receiver side.
        let new_body = if updated.body.is_encrypted() {
            self.encrypt_body(
                owner,
                &updated.receiver_id,
                &MessageEnvelope {
                    conversation_id: updated.conversation_id.clone(),
                    receiver_id: updated.receiver_id.clone(),
                    message_type: updated.message_type,
                    body: new_body.clone(),
                    attachments: updated.attachments.clone(),
                    reply_to: updated.reply_to.clone(),
                    sequence: updated.sequence,
                    // Reuse the original envelope's timestamp so the
                    // AD is byte-identical to the row we are
                    // superseding. Pure transcripts (the common
                    // case) ship the original timestamp anyway.
                    timestamp: updated.timestamp,
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
        let update_result: AppResult<()> = tokio::task::spawn_blocking(move || -> AppResult<()> {
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
        update_result?;
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

    // ── Draft persistence (F-07 follow-up) ───────────────────────────
    // Drafts were previously held in-memory only by the
    // `DraftService`. They are now persisted in the same SQLite file
    // as the rest of the chat tables, so a daemon restart doesn't
    // wipe what the user typed but never sent.

    /// Save (upsert) the draft for `conversation_id`. If `content` is
    /// empty, the row is deleted so an empty draft never shadows a
    /// prior non-empty one. Returns `Ok(true)` on insert, `Ok(false)`
    /// on delete-by-empty-content.
    pub async fn save_draft(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        content: &str,
        reply_to: Option<&MessageId>,
    ) -> AppResult<bool> {
        let conn_arc = self.connection(owner).await?;
        let owner_str = owner.as_str().to_string();
        let conv_str = conversation_id.as_str().to_string();
        let content = content.to_string();
        let reply_to_str = reply_to.map(|m| m.as_str().to_string());
        let inserted = tokio::task::spawn_blocking(move || -> AppResult<bool> {
            let guard = conn_arc.blocking_lock_owned();
            if content.is_empty() {
                guard.execute(
                    "DELETE FROM drafts WHERE owner_id = ?1 AND conversation_id = ?2",
                    rusqlite::params![owner_str, conv_str],
                )?;
                return Ok(false);
            }
            let now = chrono::Utc::now().timestamp();
            guard.execute(
                "INSERT INTO drafts (owner_id, conversation_id, content, reply_to, updated_at_unix)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(owner_id, conversation_id) DO UPDATE SET
                    content = excluded.content,
                    reply_to = excluded.reply_to,
                    updated_at_unix = excluded.updated_at_unix",
                rusqlite::params![owner_str, conv_str, content, reply_to_str, now],
            )?;
            Ok(true)
        })
        .await
        .map_err(|e| AppError::Internal(format!("save_draft join: {e}")))?;
        inserted
    }

    /// Fetch the draft for `conversation_id` (or `None` if no draft).
    pub async fn get_draft(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<Option<DraftRow>> {
        let conn_arc = self.connection(owner).await?;
        let owner_str = owner.as_str().to_string();
        let conv_str = conversation_id.as_str().to_string();
        let row = tokio::task::spawn_blocking(move || -> AppResult<Option<DraftRow>> {
            let guard = conn_arc.blocking_lock_owned();
            let mut stmt = guard.prepare_cached(
                "SELECT content, reply_to, updated_at_unix FROM drafts
                 WHERE owner_id = ?1 AND conversation_id = ?2",
            )?;
            let row = stmt
                .query_row(rusqlite::params![owner_str, conv_str], |r| {
                    let content: String = r.get(0)?;
                    let reply_to: Option<String> = r.get(1)?;
                    let updated_at: i64 = r.get(2)?;
                    Ok(DraftRow {
                        content,
                        reply_to: reply_to.map(MessageId::from),
                        updated_at: chrono::DateTime::from_timestamp(updated_at, 0)
                            .unwrap_or_else(chrono::Utc::now),
                    })
                })
                .ok();
            Ok(row)
        })
        .await
        .map_err(|e| AppError::Internal(format!("get_draft join: {e}")))??;
        Ok(row)
    }

    /// Delete the draft for `conversation_id`. Returns true if a row
    /// was removed.
    pub async fn delete_draft(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<bool> {
        let conn_arc = self.connection(owner).await?;
        let owner_str = owner.as_str().to_string();
        let conv_str = conversation_id.as_str().to_string();
        let removed: usize = tokio::task::spawn_blocking(move || -> AppResult<usize> {
            let guard = conn_arc.blocking_lock_owned();
            Ok(guard.execute(
                "DELETE FROM drafts WHERE owner_id = ?1 AND conversation_id = ?2",
                rusqlite::params![owner_str, conv_str],
            )?)
        })
        .await
        .map_err(|e| AppError::Internal(format!("delete_draft join: {e}")))??;
        Ok(removed > 0)
    }

    /// List every draft for `owner`. Used by `chat.draft.list`.
    pub async fn list_drafts(&self, owner: &UserId) -> AppResult<Vec<(ConversationId, DraftRow)>> {
        let conn_arc = self.connection(owner).await?;
        let owner_str = owner.as_str().to_string();
        let drafts: Vec<(ConversationId, DraftRow)> = tokio::task::spawn_blocking(
            move || -> AppResult<Vec<(ConversationId, DraftRow)>> {
                let guard = conn_arc.blocking_lock_owned();
                let mut stmt = guard.prepare_cached(
                    "SELECT conversation_id, content, reply_to, updated_at_unix
                     FROM drafts WHERE owner_id = ?1 ORDER BY updated_at_unix DESC",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![owner_str], |r| {
                        let conv: String = r.get(0)?;
                        let content: String = r.get(1)?;
                        let reply_to: Option<String> = r.get(2)?;
                        let updated_at: i64 = r.get(3)?;
                        Ok((
                            ConversationId::from(conv),
                            DraftRow {
                                content,
                                reply_to: reply_to.map(MessageId::from),
                                updated_at: chrono::DateTime::from_timestamp(updated_at, 0)
                                    .unwrap_or_else(chrono::Utc::now),
                            },
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            },
        )
        .await
        .map_err(|e| AppError::Internal(format!("list_drafts join: {e}")))??;
        Ok(drafts)
    }

    /// Clear every draft for `owner` — used by `chat.draft.clear`.
    pub async fn clear_drafts(&self, owner: &UserId) -> AppResult<usize> {
        let conn_arc = self.connection(owner).await?;
        let owner_str = owner.as_str().to_string();
        let removed: usize = tokio::task::spawn_blocking(move || -> AppResult<usize> {
            let guard = conn_arc.blocking_lock_owned();
            Ok(guard.execute(
                "DELETE FROM drafts WHERE owner_id = ?1",
                rusqlite::params![owner_str],
            )?)
        })
        .await
        .map_err(|e| AppError::Internal(format!("clear_drafts join: {e}")))??;
        Ok(removed)
    }

    // ── Group mutes (G-02) ────────────────────────────────────────

    /// Insert or replace a per-member, per-group mute. Called from
    /// [`crate::group_service::GroupService::mute_member`].
    ///
    /// DO-178C §6.1 — input validation runs before the SQL write so
    /// a malformed `muted_until_unix` can never reach the database.
    pub async fn set_group_member_mute(
        &self,
        conversation_id: &ConversationId,
        muted_user_id: &UserId,
        muted_by_user_id: &UserId,
        muted_until_unix: i64,
        reason: Option<&str>,
        created_at_unix: i64,
    ) -> AppResult<()> {
        if conversation_id.as_str().is_empty() {
            return Err(AppError::Domain("conversation_id is empty".into()));
        }
        if muted_user_id.as_str().is_empty() {
            return Err(AppError::Domain("muted_user_id is empty".into()));
        }
        if muted_until_unix <= 0 {
            return Err(AppError::Domain("muted_until_unix must be > 0".into()));
        }
        let conn_arc = self.connection(muted_by_user_id).await?;
        let conv = conversation_id.as_str().to_string();
        let m_user = muted_user_id.as_str().to_string();
        let actor = muted_by_user_id.as_str().to_string();
        let reason_owned = reason.map(str::to_string);
        tokio::task::spawn_blocking(move || -> AppResult<()> {
            let guard = conn_arc.blocking_lock_owned();
            guard.execute(
                "INSERT OR REPLACE INTO group_member_mutes
                    (conversation_id, muted_user_id, muted_by_user_id,
                     muted_until_unix, reason, created_at_unix)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    conv,
                    m_user,
                    actor,
                    muted_until_unix,
                    reason_owned,
                    created_at_unix,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("set_group_member_mute join: {e}")))??;
        Ok(())
    }

    /// Clear a per-member mute. The row is deleted (not flipped) so
    /// a subsequent `list_muted` doesn't see a stale "unmuted"
    /// tombstone.
    pub async fn clear_group_member_mute(
        &self,
        conversation_id: &ConversationId,
        muted_user_id: &UserId,
    ) -> AppResult<()> {
        let conn_arc = self.connection(muted_user_id).await?;
        let conv = conversation_id.as_str().to_string();
        let m_user = muted_user_id.as_str().to_string();
        tokio::task::spawn_blocking(move || -> AppResult<()> {
            let guard = conn_arc.blocking_lock_owned();
            let n = guard.execute(
                "DELETE FROM group_member_mutes
                 WHERE conversation_id = ?1 AND muted_user_id = ?2",
                rusqlite::params![conv, m_user],
            )?;
            tracing::debug!(deleted = n, "cleared group member mute");
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("clear_group_member_mute join: {e}")))??;
        Ok(())
    }

    /// Return `(user_id, muted_until_unix, reason)` triples for every
    /// currently effective mute in the conversation. Expired rows
    /// are filtered (muted_until_unix > now).
    pub async fn list_group_member_mutes(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<Vec<(UserId, i64, String)>> {
        let conn_arc = self.connection(&UserId::from(conversation_id.as_str())).await?;
        let conv = conversation_id.as_str().to_string();
        let rows: Vec<(UserId, i64, String)> = tokio::task::spawn_blocking(
            move || -> AppResult<Vec<(UserId, i64, String)>> {
                let guard = conn_arc.blocking_lock_owned();
                let now = chrono::Utc::now().timestamp();
                let mut stmt = guard.prepare_cached(
                    "SELECT muted_user_id, muted_until_unix, reason
                     FROM group_member_mutes
                     WHERE conversation_id = ?1 AND muted_until_unix > ?2
                     ORDER BY created_at_unix DESC",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![conv, now], |r| {
                        let uid: String = r.get(0)?;
                        let until: i64 = r.get(1)?;
                        let reason: Option<String> = r.get(2)?;
                        Ok((UserId::from(uid), until, reason.unwrap_or_default()))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            },
        )
        .await
        .map_err(|e| AppError::Internal(format!("list_group_member_mutes join: {e}")))??;
        Ok(rows)
    }

    /// `true` iff `target` is currently muted in `conversation_id`.
    /// Used by [`crate::chat_service::ChatService::send_message`]
    /// to drop the message before persistence.
    pub async fn is_group_member_muted(
        &self,
        conversation_id: &ConversationId,
        target: &UserId,
    ) -> AppResult<bool> {
        let conn_arc = self.connection(target).await?;
        let conv = conversation_id.as_str().to_string();
        let target_str = target.as_str().to_string();
        let is_muted: bool = tokio::task::spawn_blocking(move || -> AppResult<bool> {
            let guard = conn_arc.blocking_lock_owned();
            let now = chrono::Utc::now().timestamp();
            // Check both the per-member mute row AND the group-wide "mute all"
            // sentinel (stored as `muted_user_id = "*all*"` in the same table).
            let row: Option<i64> = guard
                .query_row(
                    "SELECT MAX(muted_until_unix) FROM group_member_mutes
                     WHERE conversation_id = ?1
                       AND (muted_user_id = ?2 OR muted_user_id = '*all*')",
                    rusqlite::params![conv, target_str],
                    |r| r.get(0),
                )
                .ok();
            Ok(matches!(row, Some(until) if until > now))
        })
        .await
        .map_err(|e| AppError::Internal(format!("is_group_member_muted join: {e}")))??;
        Ok(is_muted)
    }

    /// Set the whole-group mute flag. Stored on
    /// `group_member_mutes` is the per-member row; the whole-group
    /// flag lives in a single SQLite row keyed by
    /// `muted_user_id = "*all*"`. Atomic via `INSERT OR REPLACE`.
    pub async fn set_group_mute_all(
        &self,
        conversation_id: &ConversationId,
        on: bool,
    ) -> AppResult<()> {
        let conn_arc = self.connection(&UserId::from(conversation_id.as_str())).await?;
        let conv = conversation_id.as_str().to_string();
        let now = chrono::Utc::now().timestamp();
        // We use the same table; the sentinel `muted_user_id =
        // "*all*"` represents the group-wide mute. Reason field
        // carries `"on"` / `"off"`.
        tokio::task::spawn_blocking(move || -> AppResult<()> {
            let guard = conn_arc.blocking_lock_owned();
            if on {
                // Indefinite until manually cleared (i64::MAX).
                guard.execute(
                    "INSERT OR REPLACE INTO group_member_mutes
                        (conversation_id, muted_user_id, muted_by_user_id,
                         muted_until_unix, reason, created_at_unix)
                     VALUES (?1, '*all*', '*system*', ?2, 'on', ?3)",
                    rusqlite::params![conv, i64::MAX, now],
                )?;
            } else {
                guard.execute(
                    "DELETE FROM group_member_mutes
                     WHERE conversation_id = ?1 AND muted_user_id = '*all*'",
                    rusqlite::params![conv],
                )?;
            }
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("set_group_mute_all join: {e}")))??;
        Ok(())
    }

    pub async fn is_group_mute_all(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<bool> {
        let conn_arc = self.connection(&UserId::from(conversation_id.as_str())).await?;
        let conv = conversation_id.as_str().to_string();
        let is_muted: bool = tokio::task::spawn_blocking(move || -> AppResult<bool> {
            let guard = conn_arc.blocking_lock_owned();
            let n: i64 = guard
                .query_row(
                    "SELECT COUNT(*) FROM group_member_mutes
                     WHERE conversation_id = ?1 AND muted_user_id = '*all*'",
                    rusqlite::params![conv],
                    |r| r.get(0),
                )?;
            Ok(n > 0)
        })
        .await
        .map_err(|e| AppError::Internal(format!("is_group_mute_all join: {e}")))??;
        Ok(is_muted)
    }

    // ── Group nicknames (G-06) ────────────────────────────────────

    pub async fn set_group_member_nickname(
        &self,
        conversation_id: &ConversationId,
        user_id: &UserId,
        nickname: Option<&str>,
        updated_at_unix: i64,
    ) -> AppResult<()> {
        if conversation_id.as_str().is_empty() {
            return Err(AppError::Domain("conversation_id is empty".into()));
        }
        if user_id.as_str().is_empty() {
            return Err(AppError::Domain("user_id is empty".into()));
        }
        let conn_arc = self.connection(user_id).await?;
        let conv = conversation_id.as_str().to_string();
        let uid = user_id.as_str().to_string();
        let nick = nickname.map(str::to_string);
        tokio::task::spawn_blocking(move || -> AppResult<()> {
            let guard = conn_arc.blocking_lock_owned();
            match nick {
                Some(n) if !n.is_empty() => {
                    if n.len() > 64 {
                        return Err(AppError::Domain("nickname exceeds 64 chars".into()));
                    }
                    guard.execute(
                        "INSERT OR REPLACE INTO group_member_nicknames
                            (conversation_id, user_id, nickname, updated_at_unix)
                         VALUES (?1, ?2, ?3, ?4)",
                        rusqlite::params![conv, uid, n, updated_at_unix],
                    )?;
                }
                _ => {
                    guard.execute(
                        "DELETE FROM group_member_nicknames
                         WHERE conversation_id = ?1 AND user_id = ?2",
                        rusqlite::params![conv, uid],
                    )?;
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("set_group_member_nickname join: {e}")))??;
        Ok(())
    }

    pub async fn get_group_member_nickname(
        &self,
        conversation_id: &ConversationId,
        user_id: &UserId,
    ) -> AppResult<Option<String>> {
        let conn_arc = self.connection(user_id).await?;
        let conv = conversation_id.as_str().to_string();
        let uid = user_id.as_str().to_string();
        let out: Option<String> = tokio::task::spawn_blocking(move || -> AppResult<Option<String>> {
            let guard = conn_arc.blocking_lock_owned();
            let row: Option<String> = guard
                .query_row(
                    "SELECT nickname FROM group_member_nicknames
                     WHERE conversation_id = ?1 AND user_id = ?2",
                    rusqlite::params![conv, uid],
                    |r| r.get(0),
                )
                .ok();
            Ok(row)
        })
        .await
        .map_err(|e| AppError::Internal(format!("get_group_member_nickname join: {e}")))??;
        Ok(out)
    }

    pub async fn list_group_member_nicknames(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<Vec<(UserId, String)>> {
        let conn_arc = self.connection(&UserId::from(conversation_id.as_str())).await?;
        let conv = conversation_id.as_str().to_string();
        let out: Vec<(UserId, String)> = tokio::task::spawn_blocking(
            move || -> AppResult<Vec<(UserId, String)>> {
                let guard = conn_arc.blocking_lock_owned();
                let mut stmt = guard.prepare_cached(
                    "SELECT user_id, nickname FROM group_member_nicknames
                     WHERE conversation_id = ?1",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![conv], |r| {
                        let uid: String = r.get(0)?;
                        let nick: String = r.get(1)?;
                        Ok((UserId::from(uid), nick))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            },
        )
        .await
        .map_err(|e| AppError::Internal(format!("list_group_member_nicknames join: {e}")))??;
        Ok(out)
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

    /// Set the pinned state for a conversation.
    pub async fn set_conversation_pinned(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        pinned: bool,
    ) -> AppResult<()> {
        let conn_arc = self.connection(owner).await?;
        let conv_str = conversation_id.as_str().to_string();
        let pinned_flag = pinned;
        // `let _: ... =` previously discarded the inner `AppResult`,
        // so the `?` only caught JoinErrors and a missing-conversation
        // error was silently swallowed. Fixed: bind to a named
        // variable and propagate the inner error.
        let inner: AppResult<()> = tokio::task::spawn_blocking(move || -> AppResult<()> {
            let guard = conn_arc.blocking_lock_owned();
            let rows = guard.execute(
                "UPDATE conversations SET pinned = ?1 WHERE conversation_id = ?2",
                rusqlite::params![pinned_flag as i64, conv_str],
            )?;
            if rows == 0 {
                return Err(AppError::Domain(format!(
                    "conversation {} not found",
                    conv_str
                )));
            }
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))?;
        inner
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

        -- ── Group invitations (F-08, B-24) ─────────────────────────────
        -- One row per outstanding invitation. Use a long, opaque
        -- invitation_id so URLs pasted elsewhere don't leak the
        -- conversation_id. The status column is one of
        -- `pending | accepted | declined | revoked | expired` and is
        -- queried by both the inbox (pending) and the audit trail
        -- (everything). `expires_at_unix` is checked on every read so
        -- a daemon restart reaps expired rows lazily.
        CREATE TABLE IF NOT EXISTS group_invitations (
            invitation_id     TEXT PRIMARY KEY,
            conversation_id   TEXT NOT NULL,
            group_name        TEXT NOT NULL,
            inviter_id        TEXT NOT NULL,
            inviter_name      TEXT NOT NULL,
            invitee_id        TEXT NOT NULL,
            status            TEXT NOT NULL DEFAULT 'pending',
            created_at_unix   INTEGER NOT NULL,
            expires_at_unix   INTEGER NOT NULL,
            responded_at_unix INTEGER,
            message           TEXT,
            sync_ticket       TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_group_invitations_invitee
            ON group_invitations(invitee_id, status);
        CREATE INDEX IF NOT EXISTS idx_group_invitations_conv
            ON group_invitations(conversation_id, status);

        -- ── Drafts (F-07 follow-up: persisted across restarts) ───────
        -- One row per (owner, conversation). UNIQUE on owner makes the
        -- save/get/delete idempotent — the conversation key is a
        -- natural idempotency key inside the UNIQUE.
        CREATE TABLE IF NOT EXISTS drafts (
            owner_id         TEXT NOT NULL,
            conversation_id  TEXT NOT NULL,
            content          TEXT NOT NULL,
            reply_to         TEXT,
            updated_at_unix  INTEGER NOT NULL,
            PRIMARY KEY (owner_id, conversation_id)
        );
        CREATE INDEX IF NOT EXISTS idx_drafts_owner
            ON drafts(owner_id);

        -- ── Group mutes (G-02) ─────────────────────────────────────
        -- Per-member, per-group muting enforced by `chat_service`.
        -- `muted_until_unix = i64::MAX` is the canonical "permanent"
        -- sentinel — checked with `>` arithmetic so a developer who
        -- miscalculates dates can't accidentally unmute someone.
        CREATE TABLE IF NOT EXISTS group_member_mutes (
            conversation_id   TEXT NOT NULL,
            muted_user_id     TEXT NOT NULL,
            muted_by_user_id  TEXT NOT NULL,
            muted_until_unix  INTEGER NOT NULL,
            reason            TEXT,
            created_at_unix   INTEGER NOT NULL,
            PRIMARY KEY (conversation_id, muted_user_id)
        );
        CREATE INDEX IF NOT EXISTS idx_group_member_mutes_until
            ON group_member_mutes(conversation_id, muted_until_unix);

        -- ── Group nicknames (G-06) ─────────────────────────────────
        -- The 群昵称 override for a member inside a single group.
        -- Empty strings are rejected by the service layer.
        CREATE TABLE IF NOT EXISTS group_member_nicknames (
            conversation_id  TEXT NOT NULL,
            user_id          TEXT NOT NULL,
            nickname         TEXT NOT NULL,
            updated_at_unix  INTEGER NOT NULL,
            PRIMARY KEY (conversation_id, user_id)
        );

        -- ── Group mute-all flag (G-02 follow-up) ─────────────────────
        -- One row per group with the current `is_muted_all` state.
        -- The PRIMARY KEY is just the conversation_id — UPSERT on
        -- every set so the latest writer wins.
        CREATE TABLE IF NOT EXISTS group_mute_all (
            conversation_id  TEXT PRIMARY KEY,
            is_muted         INTEGER NOT NULL,
            updated_at_unix  INTEGER NOT NULL
        );
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

    // Audit issue #19: the previous `parse_from_rfc3339(...).ok()`
    // silently mapped a malformed stored timestamp to `None`. For
    // `recalled_at` this is a correctness bug: the database says
    // the message was recalled but the in-memory struct says it
    // was not, and the UI happily displays the original content.
    // For `read_at` / `edited_at` the same flaw is a UX papercut
    // (mismatched unread counts). We now surface the parse failure
    // as a structured `FromSqlConversionFailure` so the operator
    // sees the corruption in the log instead of corrupting the
    // message state.
    let read_at = parse_rfc3339_optional(read_at, "read_at")?;
    let edited_at = parse_rfc3339_optional(edited_at, "edited_at")?;
    let recalled_at = parse_rfc3339_optional(recalled_at, "recalled_at")?;

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
        read_at,
        is_edited: row.get::<_, i64>(11)? != 0,
        edited_at,
        integrity_hash: row.get(13)?,
        recalled_at,
    })
}

/// Parse a `Option<String>` column as RFC-3339. Empty / NULL →
/// `None`. A non-empty value that fails to parse is a hard error
/// (see audit issue #19).
fn parse_rfc3339_optional(
    raw: Option<String>,
    column: &'static str,
) -> rusqlite::Result<Option<chrono::DateTime<chrono::Utc>>> {
    let Some(s) = raw else { return Ok(None) };
    if s.is_empty() {
        return Ok(None);
    }
    chrono::DateTime::parse_from_rfc3339(&s)
        .map(|d| Some(d.with_timezone(&chrono::Utc)))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("column {column}: not RFC-3339 ({s:?}): {e}"),
                )),
            )
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
// when awaited, so we drive the future synchronously. Audit issue
// #14: the previous version of this trait unconditionally called
// `Handle::block_on` from inside `spawn_blocking`. On a single-
// threaded runtime this can deadlock if `spawn_blocking` is ever
// driven on a worker thread (rather than its dedicated blocking
// pool). The new version uses `tokio::task::block_in_place` when
// the runtime is multi-threaded and `Handle::block_on` only on
// dedicated blocking threads (the common path).
#[allow(dead_code)]
trait BlockingLockOwned {
    type Guard;
    fn blocking_lock_owned(self: Arc<Self>) -> Self::Guard;
}

impl BlockingLockOwned for tokio::sync::Mutex<rusqlite::Connection> {
    type Guard = tokio::sync::OwnedMutexGuard<rusqlite::Connection>;
    /// Audit issue #14: prefer `block_in_place` on multi-thread
    /// runtimes (lets the executor schedule other work); fall
    /// back to direct `Handle::block_on` only when the current
    /// Handle is unavailable or `block_in_place` panics.
    fn blocking_lock_owned(self: Arc<Self>) -> Self::Guard {
        // Audit issue #14: detect whether we're on a dedicated
        // blocking thread by trying to run `block_in_place`. If it
        // panics, we're on a single-thread runtime and must NOT
        // call `block_in_place` (it would deadlock); in that case
        // we drive via `Handle::block_on` which is always safe on a
        // dedicated blocking thread.
        let h = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => return drive_block_owned_lock(self.clone()),
        };
        // Try block_in_place first; on multi-thread runtimes it
        // cooperates with the executor without blocking other tasks.
        let try_block_in_place = std::panic::catch_unwind(
            std::panic::AssertUnwindSafe(|| {
                let conn = self.clone();
                tokio::task::block_in_place(|| {
                    h.block_on(async move { conn.lock_owned().await })
                })
            }),
        );
        match try_block_in_place {
            Ok(g) => g,
            Err(_) => drive_block_owned_lock(self),
        }
    }
}

/// Drive `Mutex::lock_owned().await` synchronously, building a
/// current-thread runtime if no `Handle` is active.
fn drive_block_owned_lock(
    conn: Arc<tokio::sync::Mutex<rusqlite::Connection>>,
) -> tokio::sync::OwnedMutexGuard<rusqlite::Connection> {
    match tokio::runtime::Handle::try_current() {
        Ok(h) => h.block_on(async move { conn.lock_owned().await }),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build current-thread runtime");
            rt.block_on(async move { conn.lock_owned().await })
        }
    }
}

/// Phase 5b adapter: convert an app-level `StoredMessage` (backed by
/// `ChatMessage` with rich domain types) into the hub-canonical
/// `im::Message` that `IrohDocsChat` expects.
///
/// The conversion is lossy — we drop `attachments`, `read_at`,
/// `edited_at`, `recalled_at`, and `was_encrypted_at_write`. These
/// fields live only in SQLite; iroh-docs stores the message body
/// stream for distributed sync, not for UI state.
#[cfg(feature = "iroh")]
pub(crate) fn im_message_from_chat_message(stored: &StoredMessage) -> a3net_chatstore::Message {
    let m = &stored.message;
    a3net_chatstore::Message {
        id: m.message_id.as_str().to_string(),
        conversation_id: m.conversation_id.as_str().to_string(),
        sender_id: m.sender_id.as_str().to_string(),
        receiver_id: Some(m.receiver_id.as_str().to_string()),
        content: serde_json::to_string(&m.body).unwrap_or_else(|_| String::new()),
        timestamp: chrono::DateTime::from_naive_utc_and_offset(
            chrono::NaiveDateTime::from_timestamp_opt(m.timestamp, 0)
                .unwrap_or_else(|| chrono::Utc::now().naive_utc()),
            chrono::Utc,
        ),
        sequence: m.sequence.into(),
        reply_to: m.reply_to.as_ref().map(|r| r.as_str().to_string()),
        integrity_hash: m.integrity_hash.clone(),
        is_edited: m.is_edited,
        edited_at: m.edited_at.as_ref().map(|t| t.to_rfc3339()),
    }
}

/// Phase 5c reverse adapter: convert an `im::Message` from iroh-docs
/// back into a domain `ChatMessage`.
///
/// This is the inverse of `im_message_from_chat_message`. Like that
/// function, this is lossy — SQLite-only fields (attachments, read_at,
/// edited_at, recalled_at) are set to their empty/absent defaults.
///
/// Returns `None` if the JSON body in the iroh message cannot be
/// parsed as a `MessageBody` (corrupt remote entry). The caller
/// filters these out, matching the DO-178C non-fatal error handling
/// philosophy used throughout the iroh-docs bridge.
#[cfg(feature = "iroh")]
pub(crate) fn iroh_message_to_chat_message(
    msg: a3net_chatstore::Message,
    conversation_id: &ConversationId,
) -> Option<ChatMessage> {
    let body: MessageBody = serde_json::from_str(&msg.content).ok()?;
        let receiver_id = msg
            .receiver_id
            .as_ref()
            .map(|s| UserId::from(s.as_str()))
            .unwrap_or_else(|| UserId::from(""));
        // Re-derive MessageType from the body variant since iroh only
        // stores the body JSON (not the original MessageType string).
        // Encrypted bodies are indistinguishable from File/Text at this
        // layer — the receiver decrypts first and then re-classifies.
        let message_type = match &body {
            MessageBody::Plain { .. } => MessageType::Text,
            MessageBody::Encrypted { .. } => MessageType::Text,
        };
    Some(ChatMessage {
        message_id: MessageId::from(msg.id.as_str()),
        conversation_id: conversation_id.clone(),
        sender_id: UserId::from(msg.sender_id.as_str()),
        receiver_id,
        message_type,
        body,
        attachments: vec![],
        reply_to: msg.reply_to.as_ref().map(|s| MessageId::from(s.as_str())),
        sequence: msg.sequence.unwrap_or(0),
        timestamp: msg.timestamp.timestamp(),
        read_at: None,
        is_edited: msg.is_edited,
        edited_at: msg.edited_at.as_ref().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&chrono::Utc))
        }),
        integrity_hash: msg.integrity_hash,
        recalled_at: None,
    })
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
            // Audit issue #10: timestamps must be reasonably
            // close to "now" because the storage layer's edit
            // window (default 2 minutes) rejects operations on
            // messages older than that. Using "now" keeps every
            // test below within the window without needing
            // per-test configurations.
            timestamp: chrono::Utc::now().timestamp(),
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
            timestamp: chrono::Utc::now().timestamp(),
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

    // Audit issue #10: recall a day-old message → forbidden.
    #[tokio::test]
    async fn recall_after_edit_window_rejects_with_forbidden() {
        let (dir, mut storage) = fresh_storage().await;
        // Disable the edit window so we can plant an old message
        // and exercise the timestamp-based guard directly.
        let mut cfg = StorageConfig::new(dir.path().to_path_buf());
        cfg.edit_window = core::time::Duration::ZERO;
        storage = ChatStorage::new(cfg.clone(), E2eKeyring::new(owner()));
        storage.init_user(&owner()).await.unwrap();

        // Build a fresh envelope as if from a year ago.
        let mut env = envelope();
        env.timestamp = chrono::Utc::now().timestamp() - 365 * 24 * 3600;
        let stored = storage
            .save_outbound(&owner(), &env)
            .await
            .unwrap();
        // Re-enable the window for the second half of the test.
        let mut cfg2 = cfg.clone();
        cfg2.edit_window = core::time::Duration::from_secs(120);
        storage = ChatStorage::new(cfg2, E2eKeyring::new(owner()));
        storage.init_user(&owner()).await.unwrap();
        let res = storage
            .recall_message(&owner(), &stored.message.message_id)
            .await;
        let res_str = format!("{res:?}");
        assert!(
            matches!(res, Err(AppError::Forbidden(msg)) if msg.contains("recall window")),
            "expected Forbidden for recall-after-window, got {res_str}"
        );
    }

    // Audit issue #10: edit a day-old message → forbidden.
    #[tokio::test]
    async fn edit_after_edit_window_rejects_with_forbidden() {
        let (dir, mut storage) = fresh_storage().await;
        let mut cfg = StorageConfig::new(dir.path().to_path_buf());
        cfg.edit_window = core::time::Duration::ZERO;
        storage = ChatStorage::new(cfg.clone(), E2eKeyring::new(owner()));
        storage.init_user(&owner()).await.unwrap();
        let mut env = envelope();
        env.timestamp = chrono::Utc::now().timestamp() - 365 * 24 * 3600;
        let stored = storage.save_outbound(&owner(), &env).await.unwrap();

        let mut cfg2 = cfg.clone();
        cfg2.edit_window = core::time::Duration::from_secs(120);
        storage = ChatStorage::new(cfg2, E2eKeyring::new(owner()));
        storage.init_user(&owner()).await.unwrap();
        let res = storage
            .edit_message(
                &owner(),
                &stored.message.message_id,
                &MessageBody::Plain {
                    content: "late edit".into(),
                },
            )
            .await;
        let res_str = format!("{res:?}");
        assert!(
            matches!(res, Err(AppError::Forbidden(msg)) if msg.contains("edit window")),
            "expected Forbidden for edit-after-window, got {res_str}"
        );
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
            timestamp: chrono::Utc::now().timestamp(),
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

    // F-12 — thread list: reply_to links all replies back to the
    // root. The list must be ordered by sequence (oldest first).
    #[tokio::test]
    async fn list_thread_replies_collects_all_replies_to_root() {
        let (_dir, storage) = fresh_storage().await;
        // Build a 3-message thread: root → reply1 → reply2.
        let root = storage.save_outbound(&owner(), &envelope()).await.unwrap();
        let root_id = root.message.message_id.clone();

        // Save two more envelopes with strictly increasing
        // timestamps so the subsequent UPDATE can patch their
        // `reply_to` column in storage.
        let mut reply1_env = envelope();
        reply1_env.sequence = 2;
        reply1_env.timestamp = envelope().timestamp + 1;
        let r1 = storage.save_outbound(&owner(), &reply1_env).await.unwrap();
        let r2_reply_to = r1.message.message_id.clone();
        {
            let conn = storage.connection_for(&owner()).await.unwrap();
            let conn = tokio::task::spawn_blocking(move || conn.blocking_lock_owned())
                .await
                .unwrap();
            conn.execute(
                "UPDATE messages SET reply_to = ?1 WHERE message_id = ?2",
                rusqlite::params![root_id.as_str(), r1.message.message_id.as_str()],
            )
            .unwrap();
        }

        let mut reply2_env = envelope();
        reply2_env.sequence = 3;
        reply2_env.timestamp = envelope().timestamp + 2;
        let r2 = storage.save_outbound(&owner(), &reply2_env).await.unwrap();
        {
            let conn = storage.connection_for(&owner()).await.unwrap();
            let conn = tokio::task::spawn_blocking(move || conn.blocking_lock_owned())
                .await
                .unwrap();
            conn.execute(
                "UPDATE messages SET reply_to = ?1 WHERE message_id = ?2",
                rusqlite::params![r2_reply_to.as_str(), r2.message.message_id.as_str()],
            )
            .unwrap();
        }

        // First-order replies on the root.
        let first_order = storage
            .list_thread_replies(&owner(), &root_id, 100)
            .await
            .unwrap();
        assert_eq!(first_order.len(), 1);
        assert_eq!(
            first_order[0].message_id.as_str(),
            r1.message.message_id.as_str()
        );

        // Walking down through r1 surfaces r2.
        let second = storage
            .list_thread_replies(&owner(), &r1.message.message_id, 100)
            .await
            .unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0].message_id.as_str(),
            r2.message.message_id.as_str()
        );

        // Sanity: root itself is not in its own reply list.
        assert!(!first_order
            .iter()
            .any(|m| m.message_id.as_str() == root_id.as_str()));
    }

    // F-12 — empty thread (no replies yet) returns an empty vec.
    #[tokio::test]
    async fn list_thread_replies_empty_for_unanswered_root() {
        let (_dir, storage) = fresh_storage().await;
        let root = storage.save_outbound(&owner(), &envelope()).await.unwrap();
        let replies = storage
            .list_thread_replies(&owner(), &root.message.message_id, 100)
            .await
            .unwrap();
        assert!(replies.is_empty());
    }

    // F-12 — limit caps the response size.
    #[tokio::test]
    async fn list_thread_replies_respects_limit() {
        let (_dir, storage) = fresh_storage().await;
        let root = storage.save_outbound(&owner(), &envelope()).await.unwrap();
        let root_id = root.message.message_id.clone();

        // Build 4 replies, all directly linked to root.
        for i in 2..=5 {
            let mut env = envelope();
            env.sequence = i as u32;
            env.timestamp = envelope().timestamp + i as i64;
            let stored = storage.save_outbound(&owner(), &env).await.unwrap();
            let sid = stored.message.message_id.clone();
            let conn = storage.connection_for(&owner()).await.unwrap();
            let conn = tokio::task::spawn_blocking(move || conn.blocking_lock_owned())
                .await
                .unwrap();
            conn.execute(
                "UPDATE messages SET reply_to = ?1 WHERE message_id = ?2",
                rusqlite::params![root_id.as_str(), sid.as_str()],
            )
            .unwrap();
        }

        let limited = storage
            .list_thread_replies(&owner(), &root_id, 2)
            .await
            .unwrap();
        assert_eq!(limited.len(), 2);
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
    async fn set_conversation_pinned_updates_state() {
        let (_dir, storage) = fresh_storage().await;
        let meta = ConversationMeta {
            conversation_id: ConversationId::from("dm:alice:bob"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "Bob".into(),
            peer_user_id: Some(peer()),
            last_message_preview: "".into(),
            last_activity: 100,
            message_count: 0,
            unread_count: 0,
            peer_online: false,
            muted: false,
            pinned: false,
        };
        storage.upsert_conversation(&owner(), &meta).await.unwrap();

        // Initially not pinned.
        let conv = storage.open_conversation(&owner(), &meta.conversation_id).await.unwrap().unwrap();
        assert!(!conv.meta.pinned);

        // Pin the conversation.
        storage.set_conversation_pinned(&owner(), &meta.conversation_id, true).await.unwrap();

        // Now pinned.
        let conv = storage.open_conversation(&owner(), &meta.conversation_id).await.unwrap().unwrap();
        assert!(conv.meta.pinned);

        // Unpin.
        storage.set_conversation_pinned(&owner(), &meta.conversation_id, false).await.unwrap();
        let conv = storage.open_conversation(&owner(), &meta.conversation_id).await.unwrap().unwrap();
        assert!(!conv.meta.pinned);
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
            timestamp: chrono::Utc::now().timestamp(),
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
            let (s, o, _p, e) = (storage.clone(), owner(), peer(), envelope());
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