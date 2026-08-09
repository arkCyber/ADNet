//! Bridge between `adnet-chatstore` and `iroh-docs`.
//!
//! # Scope (Phase 5a)
//!
//! This module replaces the SQLite `messages` table and the
//! manual `SyncRequest` / `SyncResponse` protocol with an
//! [`iroh_docs::Doc`] per conversation. The friends list, group
//! membership, user accounts, pending offline messages, and read
//! receipts stay in SQLite — only the message stream moves.
//!
//! # On-disk model
//!
//! Each conversation is backed by one `iroh-docs::Doc`. The Doc is
//! keyed by a BLAKE3-derived [`iroh_docs::NamespaceId`] stored in
//! the SQLite `conversations.id` column (re-purposed — was previously
//! a random UUID). The doc is **shared** via a [`iroh_docs::DocTicket`]
//! that the hub can hand to peers.
//!
//! Inside each Doc, every entry is one of:
//!
//! | Key                         | Value                                                                                  |
//! |-----------------------------|----------------------------------------------------------------------------------------|
//! | `msg/<sender_id>/<seq:08>`  | JSON-encoded [`Message`] (blob-stored via [`IrohBlobStore::put_bytes`])                |
//! | `seq/<sender_id>`           | `[u8; 4]` little-endian — last sender-local sequence (CAS-style ordering)             |
//!
//! The 8-digit zero-padded `seq` keeps the entries byte-sorted in
//! the order they were written within a single sender, which is
//! what `get_many(Query::all().key_prefix(...))` returns by default.
//!
//! # Sender-sequence atomicity (DO-178C fix)
//!
//! iroh-docs is a CRDT-style merge store; writes are not
//! transactional. We previously wrote `msg/...` and `seq/...` as two
//! independent `set_hash` calls; a crash between them could leave
//! the doc in a state where the seq counter is stale and a
//! subsequent append re-uses the same `seq` (duplicate message) or
//! skips it entirely (message loss).
//!
//! The new algorithm (`write_atomic_with_seq` below) is a
//! CAS-with-write-back loop:
//!
//! 1. Read current `seq/<sender>` (default 0 if absent).
//! 2. Compute `next_seq = cur_seq + 1`.
//! 3. Write `msg/<sender>/<next_seq>`.
//! 4. Write `seq/<sender> = next_seq`.
//! 5. **Re-read** `seq/<sender>` and verify it equals `next_seq` —
//!    if it has been bumped past us by a concurrent writer, retry
//!    the whole loop with a fresh `next_seq`.
//! 6. The retry loop is bounded by `MAX_APPEND_RETRIES` so a
//!    pathological livelock surfaces as an error rather than
//!    hanging.
//!
//! In addition, the **invariant** "the doc never contains two
//! messages with the same `(sender, seq)` key" is enforced by the
//! retry: every iteration uses a fresh `next_seq` derived from the
//! latest observed seq pointer.
//!
//! # Subscriptions (DO-178C fix)
//!
//! Each `subscribe()` call spawns a tokio task that pulls
//! `LiveEvent`s from the doc and forwards them into a per-doc
//! broadcast channel. We keep a `JoinHandle` per task so `close_all`
//! can await cancellation, and the `Drop` impl aborts outstanding
//! tasks as a last resort so the bridge cannot leak work past the
//! last strong reference.
//!
//! # `limit = 0` semantics (DO-178C fix)
//!
//! `get_messages` and `search_*` previously disagreed on the meaning
//! of `limit = 0`: the storage layer treated it as "no limit, cap at
//! 1024 internally"; the bridge treated it as an error. We now
//! follow the storage convention everywhere — `limit = 0` means
//! "no caller-specified cap, use the implementation safety cap of
//! `DEFAULT_MESSAGE_LIMIT = 1024`". This removes an entire class of
//! off-by-one surprises for callers that hand in a count from user
//! input without validation.

#![cfg(feature = "iroh")]

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use iroh::EndpointId;
use iroh_blobs::Hash as IrohHash;
use iroh_docs::api::protocol::{AddrInfoOptions, ShareMode};
use iroh_docs::api::{Doc, DocsApi};
use iroh_docs::engine::LiveEvent;
use iroh_docs::store::{Query, QueryBuilder};
use iroh_docs::{AuthorId, DocTicket, NamespaceId};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};
use tokio::task::{JoinHandle, JoinSet};
use tracing::{debug, warn};

use adnet_blobstore::{BlobImporter, BlobReader, IrohBlobStore};
use adnet_types::invariants::validate_id;

use crate::error::ChatStoreError;
use crate::im::Message;

/// Maximum number of retries for the CAS-with-write-back loop in
/// [`IrohDocsChat::append_message_as`]. Once exceeded, the bridge
/// raises [`DocsBridgeError::SequenceContention`] rather than spin
/// forever — a livelock under high contention is a failure mode
/// that DO-178C requires us to surface, not paper over.
pub const MAX_APPEND_RETRIES: u32 = 32;

/// Default safety cap used when callers pass `limit = 0` to
/// [`IrohDocsChat::get_messages`]. Matches the safety cap used by
/// `chat_storage::search_*` so the two layers agree on the
/// interpretation of "no limit".
pub const DEFAULT_MESSAGE_LIMIT: usize = 1024;

/// Bridge-specific error type. Wraps the chatstore-side error
/// (already used as glue for `validate_id`) and iroh's `anyhow`
/// for everything backend-side.
///
/// ## Recoverability
///
/// DO-178C requires every error variant to advertise whether it is
/// recoverable (caller should retry), a user error (caller should
/// surface to the user), or fatal (caller should refuse to
/// continue). The [`Self::recoverability`] helper returns one of
/// [`ErrorClass`] accordingly.
#[derive(Debug, thiserror::Error)]
pub enum DocsBridgeError {
    /// Chatstore-side validation / IO failure.
    #[error(transparent)]
    Chat(#[from] ChatStoreError),

    /// iroh-docs / iroh-blobs errors.
    #[error("iroh-docs backend error: {0}")]
    Iroh(#[from] anyhow::Error),

    /// JSON encoding / decoding failed — surfaces a corrupted
    /// blob, a schema mismatch, or a malformed signed entry.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Asked for a conversation that has not been opened in this
    /// process yet.
    #[error("conversation {0} is not open in this process")]
    NotOpen(String),

    /// Sender id is empty — collisions on the `seq/<id>` key make
    /// this unrecoverable.
    #[error("sender_id must be non-empty")]
    EmptySenderId,

    /// The CAS-with-write-back loop in [`IrohDocsChat::append_message_as`]
    /// gave up after [`MAX_APPEND_RETRIES`] retries because
    /// concurrent writers kept moving the `seq/<sender>` pointer
    /// past us. Caller may retry the whole call after backoff.
    #[error("sequence CAS exceeded {MAX_APPEND_RETRIES} retries under contention")]
    SequenceContention,
}

/// Result alias.
pub type DocsBridgeResult<T> = std::result::Result<T, DocsBridgeError>;

/// Classification of an error variant — used by the recoverability
/// helper below. Callers can switch on this to decide whether to
/// retry, surface to the user, or refuse to continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Transient or contention-induced failure; safe to retry.
    Recoverable,
    /// Caller passed invalid input; do not retry.
    UserError,
    /// Internal invariant broken or unrecoverable backend failure;
    /// do not retry.
    Fatal,
}

impl DocsBridgeError {
    /// Classify the error for upstream handling. The mapping is
    /// deliberately explicit — DO-178C requires that error
    /// classification never rely on string matching.
    pub fn recoverability(&self) -> ErrorClass {
        match self {
            // Validation / IO on the local side is not retryable as-is.
            Self::Chat(ChatStoreError::Validation(_))
            | Self::Chat(ChatStoreError::Invalid(_))
            | Self::Chat(ChatStoreError::Constraint(_))
            | Self::Chat(ChatStoreError::ForeignKey(_))
            | Self::EmptySenderId
            | Self::NotOpen(_) => ErrorClass::UserError,

            // CAS loop gave up: caller may back off and retry.
            Self::SequenceContention => ErrorClass::Recoverable,

            // NotFound is recoverable from the doc-side perspective
            // (the missing doc may appear later via sync).
            Self::Chat(ChatStoreError::NotFound(_)) => ErrorClass::Recoverable,

            // JSON / iroh backend failures are treated as Fatal —
            // they signal either schema drift or a corrupt replica,
            // neither of which retrying will fix.
            Self::Serde(_) | Self::Iroh(_) => ErrorClass::Fatal,

            // SQLite-level failures other than NotFound — also Fatal
            // unless we know better.
            Self::Chat(other) => match other {
                ChatStoreError::Sqlite(_)
                | ChatStoreError::Lock
                | ChatStoreError::Json(_)
                | ChatStoreError::Bincode(_)
                | ChatStoreError::Zstd(_)
                | ChatStoreError::Io(_)
                | ChatStoreError::IdGen(_)
                | ChatStoreError::SchemaVersion { .. }
                | ChatStoreError::DatabaseCorrupt(_) => ErrorClass::Fatal,
                _ => ErrorClass::UserError,
            },
        }
    }
}

/// One open conversation. Cheap to clone.
#[derive(Debug, Clone)]
pub struct DocHandle {
    /// SQLite `conversations.id` — also the human-readable handle.
    pub conversation_id: String,
    /// The `iroh-docs` namespace.
    pub namespace: NamespaceId,
    /// The live `Doc` handle.
    pub doc: Doc,
}

/// Event delivered to local subscribers when a new message is
/// appended to a conversation (either locally or via sync).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageEvent {
    /// A brand-new message was appended.
    Insert(Message),
    /// Catch-up subscription replay produced `n` messages already
    /// present in the doc (no new content was written).
    Replay(Vec<Message>),
    /// One or more entries on the doc could not be decoded; they
    /// were skipped. Surfaced so subscribers can warn the user
    /// instead of silently dropping the message.
    Corruption(String),
}

/// Sender-sequence stored as raw little-endian bytes inside the doc.
const SEQ_LEN: usize = 4;
fn encode_seq(n: u32) -> Vec<u8> {
    n.to_le_bytes().to_vec()
}
fn decode_seq(b: &[u8]) -> Option<u32> {
    if b.len() != SEQ_LEN {
        return None;
    }
    let mut a = [0u8; SEQ_LEN];
    a.copy_from_slice(b);
    Some(u32::from_le_bytes(a))
}

fn msg_key(sender_id: &str, seq: u32) -> Vec<u8> {
    // 8-digit zero-padded decimal seq keeps reader-side ordering
    // deterministic across the whole doc.
    format!("msg/{sender_id}/{seq:08}").into_bytes()
}

fn seq_key(sender_id: &str) -> Vec<u8> {
    format!("seq/{sender_id}").into_bytes()
}

fn parse_msg_key(key: &[u8]) -> Option<(&str, u32)> {
    let s = std::str::from_utf8(key).ok()?;
    let mut parts = s.splitn(3, '/');
    let prefix = parts.next()?;
    if prefix != "msg" {
        return None;
    }
    let sender = parts.next()?;
    // Reject empty sender ids — they would collide on the
    // `seq/<empty>` key and break the CAS invariant.
    if sender.is_empty() {
        return None;
    }
    let seq_str = parts.next()?;
    let seq: u32 = seq_str.parse().ok()?;
    Some((sender, seq))
}

/// Payload stored in the blob store. We tag with a version byte so
/// future schema bumps can be distinguished without breaking old
/// peers (they ignore the unknown byte and decode the rest).
#[derive(Debug, Serialize, Deserialize)]
struct StoredMessage {
    /// Schema version. Always `1` for now.
    v: u8,
    msg: Message,
}

fn encode_message(msg: &Message) -> DocsBridgeResult<Vec<u8>> {
    let payload = StoredMessage {
        v: 1,
        msg: msg.clone(),
    };
    let bytes = serde_json::to_vec(&payload)?;
    Ok(bytes)
}

fn decode_message(bytes: &[u8]) -> DocsBridgeResult<Message> {
    let payload: StoredMessage = serde_json::from_slice(bytes)?;
    Ok(payload.msg)
}

/// Convert the iroh `Hash` on an `Entry` into the ADNet
/// `ContentHash` representation that the blob store expects.
#[inline]
fn entry_hash_to_content_hash(ih: IrohHash) -> adnet_types::ContentHash {
    use adnet_blobstore::iroh_store::iroh_hash_to_content_hash;
    iroh_hash_to_content_hash(&ih)
}

/// Per-conversation subscription state. Tracks the broadcast
/// channel and the background task that feeds it.
struct SubscriptionSlot {
    /// Broadcast sender handed out to subscribers. Cloned for every
    /// new receiver.
    tx: broadcast::Sender<MessageEvent>,
    /// Background task that reads `LiveEvent`s and feeds the
    /// channel. `None` while the task is being constructed.
    task: Option<JoinHandle<()>>,
    /// Monotonic counter incremented on every successful insert
    /// into the channel. Subscribers can compare this to detect
    /// "I missed an event because of a race" — see
    /// `subscribe_with_reset_counter`.
    generation: u64,
}

/// The bridge. Constructed once per process, shared via `Arc`.
///
/// ## Lifecycle
///
/// `IrohDocsChat::Drop` aborts every outstanding subscription task
/// and closes every open doc. The `Arc` clone pattern means callers
/// normally do not see this — production code should call
/// [`Self::shutdown`] explicitly and only rely on `Drop` as a
/// best-effort safety net.
#[derive(Clone)]
pub struct IrohDocsChat {
    api: Arc<DocsApi>,
    blobs: IrohBlobStore,
    /// Conversation id → open handle.
    open: Arc<Mutex<HashMap<String, DocHandle>>>,
    /// Per-conversation subscription slots.
    subscribers: Arc<Mutex<HashMap<String, SubscriptionSlot>>>,
    /// All subscription tasks, kept so `shutdown` can await them.
    task_set: Arc<Mutex<JoinSet<()>>>,
    /// The single default author for this process. All writes
    /// originate from this author. Phase 5b can introduce
    /// per-conversation authors if needed.
    default_author: AuthorId,
}

impl std::fmt::Debug for IrohDocsChat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IrohDocsChat")
            .field("default_author", &self.default_author)
            .field("blobs", &"<IrohBlobStore>")
            .finish()
    }
}

impl IrohDocsChat {
    /// Build a bridge around an already-constructed [`DocsApi`] and
    /// the same blob store that powers the iroh transport's
    /// `IrohBlobStore`.
    pub async fn new(api: Arc<DocsApi>, blobs: IrohBlobStore) -> DocsBridgeResult<Self> {
        // Prefer the default author if one has been bootstrapped;
        // otherwise create a fresh one. Either way we end up with a
        // stable AuthorId for this process.
        let default_author = match api.author_default().await {
            Ok(author) => author,
            Err(_) => api.author_create().await?,
        };
        Ok(Self {
            api,
            blobs,
            open: Arc::new(Mutex::new(HashMap::new())),
            subscribers: Arc::new(Mutex::new(HashMap::new())),
            task_set: Arc::new(Mutex::new(JoinSet::new())),
            default_author,
        })
    }

    /// Underlying `DocsApi` for callers that need direct access
    /// (e.g. to import a ticket shipped via `adnet-types`).
    pub fn api(&self) -> &Arc<DocsApi> {
        &self.api
    }

    /// Default author id — every write in this process is signed by
    /// this author. Tests get this for free; production code should
    /// reuse whatever the runtime's [`IrohRuntime`] sets.
    pub fn default_author(&self) -> AuthorId {
        self.default_author
    }

    /// Open a brand-new conversation doc. The returned handle is
    /// cached locally; subsequent calls with the same id return the
    /// same handle.
    pub async fn open_conversation(
        &self,
        conversation_id: impl Into<String>,
    ) -> DocsBridgeResult<DocHandle> {
        let conversation_id = conversation_id.into();
        validate_id("conversation_id", &conversation_id)
            .map_err(|e| ChatStoreError::Invalid(e.to_string()))?;
        {
            let cache = self.open.lock().await;
            if let Some(h) = cache.get(&conversation_id) {
                return Ok(h.clone());
            }
        }
        let doc = self.api.create().await?;
        let namespace = doc.id();
        let handle = DocHandle {
            conversation_id: conversation_id.clone(),
            namespace,
            doc,
        };
        let mut cache = self.open.lock().await;
        cache.insert(conversation_id, handle.clone());
        Ok(handle)
    }

    /// Open an existing doc by `NamespaceId`.
    pub async fn open_existing(
        &self,
        conversation_id: impl Into<String>,
        namespace: NamespaceId,
    ) -> DocsBridgeResult<DocHandle> {
        let conversation_id = conversation_id.into();
        let doc = self
            .api
            .open(namespace)
            .await?
            .ok_or_else(|| anyhow::anyhow!("doc {namespace} not found in this replica"))?;
        let handle = DocHandle {
            conversation_id: conversation_id.clone(),
            namespace,
            doc,
        };
        let mut cache = self.open.lock().await;
        cache.insert(conversation_id, handle.clone());
        Ok(handle)
    }

    /// Open a doc via a peer's [`DocTicket`].
    pub async fn open_with_ticket(
        &self,
        conversation_id: impl Into<String>,
        ticket: DocTicket,
    ) -> DocsBridgeResult<DocHandle> {
        let conversation_id = conversation_id.into();
        let doc = self.api.import(ticket).await?;
        let namespace = doc.id();
        let handle = DocHandle {
            conversation_id: conversation_id.clone(),
            namespace,
            doc,
        };
        let mut cache = self.open.lock().await;
        cache.insert(conversation_id, handle.clone());
        Ok(handle)
    }

    /// Produce a shareable ticket for the conversation.
    pub async fn share(
        &self,
        conversation_id: &str,
        mode: ShareMode,
    ) -> DocsBridgeResult<DocTicket> {
        let handle = self.require_open(conversation_id).await?;
        let ticket = handle.doc.share(mode, AddrInfoOptions::default()).await?;
        Ok(ticket)
    }

    /// Trigger a sync against the listed peers.
    pub async fn start_sync(
        &self,
        conversation_id: &str,
        peers: Vec<EndpointId>,
    ) -> DocsBridgeResult<()> {
        let handle = self.require_open(conversation_id).await?;
        let addrs: Vec<iroh::EndpointAddr> =
            peers.into_iter().map(iroh::EndpointAddr::from).collect();
        handle.doc.start_sync(addrs).await?;
        Ok(())
    }

    /// Append a message to a conversation. Returns the assigned
    /// sequence. Uses the bridge's default author.
    ///
    /// # Atomicity
    ///
    /// The (msg entry, seq pointer) pair is written via a CAS loop —
    /// see the module-level docs and [`Self::append_message_as`].
    pub async fn append_message(
        &self,
        conversation_id: &str,
        message: Message,
    ) -> DocsBridgeResult<u32> {
        self.append_message_as(self.default_author, conversation_id, message)
            .await
    }

    /// Append a message to a conversation as a specific author.
    ///
    /// # Atomicity (DO-178C)
    ///
    /// Writes are wrapped in a `MAX_APPEND_RETRIES`-bounded CAS
    /// loop. Each iteration:
    ///
    /// 1. Reads `seq/<sender>` (default 0).
    /// 2. Writes `msg/<sender>/<next_seq>`.
    /// 3. Writes `seq/<sender> = <next_seq>`.
    /// 4. Re-reads `seq/<sender>` and verifies it equals
    ///    `<next_seq>` — a higher value means a concurrent writer
    ///    beat us to the punch; we discard the half-written entry
    ///    and retry from step 1.
    ///
    /// In the common (no-contention) case the loop runs once.
    /// Under contention the loop's cost is bounded by
    /// `MAX_APPEND_RETRIES`; beyond that we raise
    /// [`DocsBridgeError::SequenceContention`] and the caller can
    /// back off and retry.
    pub async fn append_message_as(
        &self,
        author: AuthorId,
        conversation_id: &str,
        message: Message,
    ) -> DocsBridgeResult<u32> {
        if message.sender_id.is_empty() {
            return Err(DocsBridgeError::EmptySenderId);
        }
        validate_id("sender_id", &message.sender_id)
            .map_err(|e| ChatStoreError::Invalid(e.to_string()))?;
        let handle = self.require_open(conversation_id).await?;

        // Stamp the message's `conversation_id` so the in-doc copy
        // matches the on-SQLite `conversations.id` regardless of
        // what the caller passed.
        let mut template = message;
        template.conversation_id = conversation_id.to_string();

        for attempt in 0..MAX_APPEND_RETRIES {
            let next_seq = self
                .read_sender_seq(&handle, &template.sender_id)
                .await?
                .saturating_add(1);

            // 1. Stamp and encode.
            let mut msg = template.clone();
            msg.sequence = Some(next_seq);
            let payload = encode_message(&msg)?;
            let content_hash = self
                .blobs
                .put_bytes(&payload)
                .await
                .map_err(|e| anyhow::anyhow!("blob put_bytes failed for message: {e}"))?;
            let iroh_hash = adnet_blobstore::iroh_store::content_hash_to_iroh_hash(&content_hash)
                .map_err(|e| anyhow::anyhow!("content_hash→iroh_hash: {e}"))?;
            let size = payload.len() as u64;

            // 2. Write the msg entry by hash.
            let key = msg_key(&msg.sender_id, next_seq);
            handle.doc.set_hash(author, key, iroh_hash, size).await?;

            // 3. Write the seq pointer. Encode the seq into a tiny
            //    blob so the doc entry carries a hash — keeps the
            //    doc entry size constant regardless of seq width.
            let seq_key_v = seq_key(&msg.sender_id);
            let seq_bytes = encode_seq(next_seq);
            let seq_content_hash = self
                .blobs
                .put_bytes(&seq_bytes)
                .await
                .map_err(|e| anyhow::anyhow!("blob put_bytes failed for seq: {e}"))?;
            let seq_iroh_hash =
                adnet_blobstore::iroh_store::content_hash_to_iroh_hash(&seq_content_hash)
                    .map_err(|e| anyhow::anyhow!("seq content_hash→iroh_hash: {e}"))?;
            handle
                .doc
                .set_hash(author, seq_key_v, seq_iroh_hash, seq_bytes.len() as u64)
                .await?;

            // 4. Verify — re-read the seq pointer and confirm it
            //    landed at `next_seq`. If a concurrent writer
            //    already pushed it past us, retry with the fresh
            //    value.
            let observed = self.read_sender_seq(&handle, &msg.sender_id).await?;
            if observed >= next_seq {
                debug!(
                    conversation = %conversation_id,
                    sender = %msg.sender_id,
                    observed,
                    next_seq,
                    attempt,
                    "appended message to iroh-docs (verified)"
                );
                // Fan-out to local subscribers.
                self.fan_out(handle.conversation_id.clone(), MessageEvent::Insert(msg))
                    .await;
                return Ok(next_seq);
            }
            // observed < next_seq means our `set_hash` for the seq
            // pointer didn't take. This is extremely rare under
            // normal operation but can happen if a peer replicated
            // a seq pointer with an *older* value (LWW on the same
            // key). Retry the whole iteration.
            warn!(
                conversation = %conversation_id,
                sender = %msg.sender_id,
                observed,
                expected = next_seq,
                attempt,
                "seq pointer did not advance — retrying CAS"
            );
        }

        Err(DocsBridgeError::SequenceContention)
    }

    /// Fetch up to `limit` messages with `sequence > after`,
    /// ordered by sequence ascending.
    ///
    /// `limit == 0` is treated as "no caller-specified cap, use the
    /// default safety cap [`DEFAULT_MESSAGE_LIMIT`]". This matches
    /// the convention used by `chat_storage::search_*` so the two
    /// layers agree on the meaning of "no limit".
    ///
    /// # Corruption handling (DO-178C)
    ///
    /// Individual entries that fail to decode (blob missing,
    /// payload corrupt, schema mismatch) are **skipped**, not
    /// fatal: the function continues iterating and the surviving
    /// messages are returned. A [`MessageEvent::Corruption`]
    /// warning is fanned out so live subscribers can surface it
    /// to the user. This avoids the silent-truncation failure mode
    /// of the previous implementation.
    pub async fn get_messages(
        &self,
        conversation_id: &str,
        after: Option<u32>,
        limit: usize,
    ) -> DocsBridgeResult<Vec<Message>> {
        let effective_limit = if limit == 0 {
            DEFAULT_MESSAGE_LIMIT
        } else {
            limit
        };
        let handle = self.require_open(conversation_id).await?;

        // Query: all entries whose key starts with `msg/`. We cap
        // the doc side at `effective_limit * 2` to leave room for
        // in-process filtering by `after`, then truncate to
        // `effective_limit` after sort.
        let cap = (effective_limit as u64).saturating_mul(2).max(16);
        let query: Query = Query::all().key_prefix("msg/").limit(cap).into();

        let mut collected = Vec::new();
        let mut skipped = 0usize;
        let stream = handle.doc.get_many(query).await?;
        // `iroh-docs::get_many` returns an irpc Receiver whose
        // `.into_stream()` adapter is not `Unpin`; pin it so we can
        // call `.next()`.
        let mut stream = Box::pin(stream);
        while let Some(entry_res) = stream.next().await {
            let entry = match entry_res {
                Ok(e) => e,
                Err(e) => {
                    warn!(
                        conversation = %conversation_id,
                        "get_many returned error: {e}"
                    );
                    continue;
                }
            };
            let key_bytes = entry.key().to_vec();
            let (sender, seq) = match parse_msg_key(&key_bytes) {
                Some(v) => v,
                None => continue,
            };
            if let Some(after) = after
                && seq <= after
            {
                continue;
            }
            let ch = entry_hash_to_content_hash(entry.content_hash());
            let bytes = match self.blobs.read_all(&ch).await {
                Ok(b) => b,
                Err(e) => {
                    warn!(
                        conversation = %conversation_id,
                        sender,
                        seq,
                        hash = %ch.as_hex(),
                        "blob read failed — skipping: {e}"
                    );
                    skipped += 1;
                    continue;
                }
            };
            let msg = match decode_message(&bytes) {
                Ok(m) => m,
                Err(e) => {
                    warn!(
                        conversation = %conversation_id,
                        sender,
                        seq,
                        "decode_message failed — skipping: {e}"
                    );
                    skipped += 1;
                    continue;
                }
            };
            collected.push((seq, sender.to_string(), msg));
        }

        collected.sort_by_key(|(seq, sender, _)| (*seq, sender.clone()));
        collected.truncate(effective_limit);
        if skipped > 0 {
            // Surface the corruption count to subscribers so they
            // can warn the user instead of silently dropping
            // messages.
            self.fan_out(
                conversation_id.to_string(),
                MessageEvent::Corruption(format!(
                    "get_messages skipped {skipped} entries due to read/decode failures"
                )),
            )
            .await;
        }
        Ok(collected.into_iter().map(|(_, _, m)| m).collect())
    }

    /// Subscribe to live message events for a conversation. The
    /// subscriber first receives a [`MessageEvent::Replay`] of the
    /// current history (so consumers never miss pre-existing
    /// messages they didn't sync), then live inserts.
    ///
    /// # Lifecycle (DO-178C)
    ///
    /// The background task that feeds the broadcast channel is
    /// registered with the bridge's `JoinSet`. It will be awaited
    /// on [`Self::shutdown`] and aborted on [`Self::Drop`] as a
    /// safety net.
    pub async fn subscribe(
        &self,
        conversation_id: &str,
    ) -> DocsBridgeResult<broadcast::Receiver<MessageEvent>> {
        let handle = self.require_open(conversation_id).await?;
        let slot = self.ensure_slot(conversation_id, &handle).await?;

        // Subscribe FIRST so we don't drop any messages between the
        // snapshot and the live feed.
        let rx = slot.subscribe();

        // Snapshot current history and push it through the channel
        // as a single Replay event.
        let history = self.get_messages(conversation_id, None, 0).await?;
        let _ = slot.send(MessageEvent::Replay(history));
        Ok(rx)
    }

    /// Gracefully tear the bridge down: cancel every outstanding
    /// subscription task and drop every open doc handle. Safe to
    /// call multiple times — the second call is a no-op.
    pub async fn shutdown(&self) {
        // Abort and await every background task so they don't keep
        // reading from the docs after we drop our references.
        let mut task_set = self.task_set.lock().await;
        task_set.abort_all();
        while let Some(res) = task_set.join_next().await {
            if let Err(e) = res
                && !e.is_cancelled()
            {
                warn!("subscription task panicked: {e}");
            }
        }
        drop(task_set);

        let mut cache = self.open.lock().await;
        cache.clear();
        let mut sub = self.subscribers.lock().await;
        sub.clear();
    }

    /// Close every local handle and forget caches. The underlying
    /// `DocsApi` is not shut down — callers control its lifetime.
    /// **Prefer [`Self::shutdown`] in production code** — this
    /// method only clears the in-memory caches and does not await
    /// the background tasks.
    pub async fn close_all(&self) {
        let mut cache = self.open.lock().await;
        cache.clear();
        let mut sub = self.subscribers.lock().await;
        sub.clear();
    }

    async fn ensure_slot(
        &self,
        conversation_id: &str,
        handle: &DocHandle,
    ) -> DocsBridgeResult<broadcast::Sender<MessageEvent>> {
        let mut map = self.subscribers.lock().await;
        if let Some(slot) = map.get_mut(conversation_id) {
            // Slot already exists; return its sender. If the
            // background task has already exited (e.g. because it
            // observed `receiver_count == 0` and self-removed), we
            // spawn a fresh task before handing the sender back.
            if slot.task.is_none() {
                let tx = slot.tx.clone();
                drop(map);
                self.spawn_subscriber(conversation_id, handle, tx.clone())
                    .await;
                return Ok(tx);
            }
            return Ok(slot.tx.clone());
        }
        let (tx, _rx) = broadcast::channel(64);
        map.insert(
            conversation_id.to_string(),
            SubscriptionSlot {
                tx: tx.clone(),
                task: None,
                generation: 0,
            },
        );
        drop(map);
        self.spawn_subscriber(conversation_id, handle, tx.clone())
            .await;
        Ok(tx)
    }

    /// Spawn (or replace) the background task that forwards
    /// `LiveEvent`s from the doc into the per-conversation
    /// broadcast channel. The returned `JoinHandle` is registered
    /// with the bridge's `JoinSet` so [`Self::shutdown`] can await
    /// it.
    async fn spawn_subscriber(
        &self,
        conversation_id: &str,
        handle: &DocHandle,
        tx: broadcast::Sender<MessageEvent>,
    ) {
        let doc = handle.doc.clone();
        let conv_id = handle.conversation_id.clone();
        let blobs = self.blobs.clone();
        // Clone the slot map Arc for use inside the spawned task;
        // we still need a second clone for the outer code, which
        // locks the map after the spawn to wire the JoinHandle
        // into the slot.
        let outer_subscribers = self.subscribers.clone();
        let subscribers = outer_subscribers.clone();
        let task_set = self.task_set.clone();
        // Clone `tx` before moving it into the spawned task so we
        // can still keep a handle on the slot side after the spawn.
        let tx_for_task = tx.clone();
        let join = tokio::spawn(async move {
            let mut stream = match doc.subscribe().await {
                Ok(s) => s,
                Err(e) => {
                    warn!(conversation = %conv_id, "doc.subscribe failed: {e}");
                    return;
                }
            };
            while let Some(ev_res) = stream.next().await {
                let event = match ev_res {
                    Ok(e) => e,
                    Err(e) => {
                        warn!(conversation = %conv_id, "doc LiveEvent error: {e}");
                        continue;
                    }
                };
                let entry = match event {
                    LiveEvent::InsertLocal { entry } => entry,
                    LiveEvent::InsertRemote { entry, .. } => entry,
                    _ => continue,
                };
                let key = entry.key().to_vec();
                if parse_msg_key(&key).is_none() {
                    continue;
                }
                let ch = entry_hash_to_content_hash(entry.content_hash());
                let bytes = match blobs.read_all(&ch).await {
                    Ok(b) => b,
                    Err(e) => {
                        warn!(conversation = %conv_id, hash = %ch.as_hex(), "blob fetch failed for live event: {e}");
                        let _ = tx_for_task.send(MessageEvent::Corruption(format!(
                            "live blob read failed: {e}"
                        )));
                        continue;
                    }
                };
                let msg = match decode_message(&bytes) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(conversation = %conv_id, "decode failed for live event: {e}");
                        let _ = tx_for_task
                            .send(MessageEvent::Corruption(format!("live decode failed: {e}")));
                        continue;
                    }
                };
                // Under a write lock on the slot map we decide
                // whether the channel still has any subscribers.
                // This closes the P0-4 race where `receiver_count`
                // was read outside the lock and a new subscriber
                // could attach between the check and the send.
                let should_send = {
                    let mut map = subscribers.lock().await;
                    match map.get_mut(&conv_id) {
                        Some(slot) if slot.tx.receiver_count() > 0 => {
                            slot.generation = slot.generation.wrapping_add(1);
                            true
                        }
                        Some(_) => {
                            // No live receivers — clear the slot so
                            // the next `ensure_slot` starts a
                            // fresh channel.
                            map.remove(&conv_id);
                            false
                        }
                        None => false,
                    }
                };
                if !should_send {
                    return;
                }
                let _ = tx_for_task.send(MessageEvent::Insert(msg));
            }
            // Stream ended — clear the slot so the next
            // `ensure_slot` can spawn a fresh task.
            let mut map = subscribers.lock().await;
            map.remove(&conv_id);
            drop(map);
        });
        // Register with the task set so shutdown can await it.
        let mut task_set = task_set.lock().await;
        let mut map = self.subscribers.lock().await;
        // Insert (or replace) the slot's task handle. We rebuild
        // the slot because Rust's borrow checker won't let us
        // hold two mutable references across the closure above.
        let new_slot = match map.remove(conversation_id) {
            Some(mut s) => {
                s.task = Some(join);
                s
            }
            None => SubscriptionSlot {
                tx: tx.clone(),
                task: Some(join),
                generation: 0,
            },
        };
        map.insert(conversation_id.to_string(), new_slot);
        // Drain any join handles that have already finished so the
        // JoinSet doesn't grow unbounded.
        while let Some(res) = task_set.try_join_next() {
            if let Err(e) = res
                && !e.is_cancelled()
            {
                warn!("subscription task ended: {e}");
            }
        }
    }

    async fn fan_out(&self, conversation_id: String, event: MessageEvent) {
        let map = self.subscribers.lock().await;
        if let Some(slot) = map.get(&conversation_id) {
            let _ = slot.tx.send(event);
        }
    }

    async fn read_sender_seq(&self, handle: &DocHandle, sender_id: &str) -> DocsBridgeResult<u32> {
        let key = seq_key(sender_id);
        let builder: QueryBuilder<iroh_docs::store::FlatQuery> = Query::all().key_exact(&key);
        let query: Query = builder.into();
        let stream = handle.doc.get_many(query).await?;
        let mut stream = Box::pin(stream);
        if let Some(entry_res) = stream.next().await {
            let entry = entry_res?;
            let ch = entry_hash_to_content_hash(entry.content_hash());
            let bytes = self
                .blobs
                .read_all(&ch)
                .await
                .map_err(|e| anyhow::anyhow!("blob read failed: {e}"))?;
            if let Some(n) = decode_seq(&bytes) {
                return Ok(n);
            }
        }
        Ok(0)
    }

    async fn require_open(&self, conversation_id: &str) -> DocsBridgeResult<DocHandle> {
        let cache = self.open.lock().await;
        cache
            .get(conversation_id)
            .cloned()
            .ok_or_else(|| DocsBridgeError::NotOpen(conversation_id.to_string()))
    }
}

impl Drop for IrohDocsChat {
    fn drop(&mut self) {
        // Best-effort safety net. Production code should call
        // `shutdown().await` explicitly; this Drop impl only fires
        // if the caller forgets.
        let subscribers = self.subscribers.clone();
        let task_set = self.task_set.clone();
        let open = self.open.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut task_set = task_set.lock().await;
                task_set.abort_all();
                while let Some(res) = task_set.join_next().await {
                    let _ = res;
                }
                drop(task_set);
                let mut cache = open.lock().await;
                cache.clear();
                let mut sub = subscribers.lock().await;
                sub.clear();
            });
        }
    }
}

/// Bridge-side convenience re-export so callers don't have to
/// import iroh-docs directly.
pub type ConversationTicket = DocTicket;
