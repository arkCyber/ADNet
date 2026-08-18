//! ChatService — handles `chat.conversation.list`,
//! `chat.conversation.open`, `chat.message.send`,
//! `chat.message.recall`, `chat.message.ack`.

use std::sync::Arc;

use a3chat_core::conversation::{ConversationMeta, ConversationRecord};
use a3chat_core::error::A3chatError;
use a3chat_core::id::{ConversationId, MessageId, UserId};
use a3chat_core::message::{ChatMessage, MessageBody, MessageEnvelope};
use a3chat_core::rpc::A3chatRpcMethod;
use tokio::sync::RwLock;

use crate::error::{AppError, AppResult};
use crate::moderation_service::ModerationService;
use crate::notification_bus::NotificationBus;
use crate::storage::{ChatStorage, StoredMessage};

#[cfg(feature = "iroh")]
use a3net_chatstore::IrohDocsChat;

/// The chat service. Cloning is cheap (`Arc`-wrapped state).
#[derive(Clone)]
pub struct ChatService {
    storage: ChatStorage,
    bus: NotificationBus,
    moderation: Option<ModerationService>,
    /// Phase 5b/5c: optional iroh-docs bridge for dual-write.
    /// Stored behind an `RwLock` so it can be injected after
    /// construction via [`ChatService::with_iroh_docs_chat`].
    #[cfg(feature = "iroh")]
    iroh_docs_chat: Arc<RwLock<Option<Arc<IrohDocsChat>>>>,
    /// GB-22 — async mute gate. When `Some`, every outbound
    /// `send_message` consults it for group conversations and
    /// short-circuits with [`AppError::Forbidden`] when the gate
    /// returns `true`. The hook is wired in `A3chatApp::new`
    /// (`chat.with_mute_gate(group.mute_gate)`) so chat and
    /// group remain loosely coupled.
    ///
    /// The closure is stored behind `Arc` so concurrent
    /// `send_message` calls can read the gate without racing the
    /// writer and without an `Arc::get_mut` dance.
    mute_gate: Arc<std::sync::Mutex<Option<MuteGate>>>,
    /// F-25 / B-7 — blocklist gate. When `Some`, every
    /// `send_message` consults it for the receiver and rejects
    /// with [`AppError::Forbidden`] when the receiver is on the
    /// owner's blocklist. The gate is wired in `A3chatApp::new`
    /// (`chat.with_blocklist_gate(contact.is_blocked_gate)`) so
    /// chat and contact remain loosely coupled and the chat
    /// service never imports `ContactService` directly (that would
    /// be a circular dependency).
    blocklist_gate: Arc<std::sync::Mutex<Option<BlocklistGate>>>,
    /// Presence touch gate. When `Some`, every `send_message` calls it
    /// to update the sender's `last_seen` and `is_online` in the
    /// group membership table.
    presence_touch_gate: Arc<std::sync::Mutex<Option<PresenceTouchGate>>>,
}

/// Async predicate for GB-22. `(conversation_id, sender) -> true`
/// means the sender is currently muted in this conversation and the
/// outbound message must be rejected.
///
/// `MuteGate` is `Arc<dyn Fn(...)> + Send + Sync` so it is sized,
/// cheaply cloneable, and storable under [`ChatService::mute_gate`]
/// (`Arc<Mutex<Option<Arc<MuteGate>>>>`). Callers wrap their
/// closure in `Arc::new` before handing it to
/// [`ChatService::with_mute_gate`].
///
/// The returned future only needs `Send` (not `Sync`) — it is
/// held inside an async function which moves it across `.await`
/// points without ever being shared. `Sync` would force every
/// awaited future to also be `Sync`, which leaks a `Sync`
/// requirement into every async trait object the closure might
/// call (e.g. `RosterStore::get_contact` inside the blocklist
/// gate). See [`BlocklistGate`] for the same reasoning applied to
/// the blocklist predicate.
pub type MuteGate = Arc<
    dyn Fn(
            ConversationId,
            UserId,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = bool> + Send>,
        > + Send
        + Sync,
>;

/// F-25 / B-7 — async blocklist check. `(owner, peer) -> true`
/// means `owner` has `peer` on their blocklist and the outbound
/// message must be rejected with [`AppError::Forbidden`].
///
/// Like [`MuteGate`], this is a `Send + Sync` `dyn Fn` trait object
/// so callers can `Arc::new(...)` their closure without an extra
/// `Box::new`. Stored behind `Arc<Mutex<Option<...>>>` so concurrent
/// reads can clone the inner `Arc` cheaply without racing the writer.
///
/// The returned future only needs `Send` (not `Sync`) — it is held
/// inside an async function which moves it across `.await` points
/// without ever being shared. `Sync` would force every awaited
/// future to also be `Sync`, which leaks a `Sync` requirement into
/// every async trait object the closure might call (e.g.
/// `RosterStore::get_contact`).
pub type BlocklistGate = Arc<
    dyn Fn(UserId, UserId) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

/// Presence touch gate — called after a message is sent to update
/// the sender's `last_seen` and `is_online` in group membership.
///
/// `(conversation_id, sender, is_online) -> future`
///
/// Stored behind `Arc<Mutex<Option<...>>>` so concurrent reads can
/// clone the inner `Arc` cheaply without racing the writer.
pub type PresenceTouchGate = Arc<
    dyn Fn(
            a3chat_core::id::ConversationId,
            UserId,
            bool,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// F-11 / B-27 — WeChat-style 2-minute recall window. A sender can
/// retract a message within this many seconds of `timestamp`. After
/// the window closes the recall RPC returns `AppError::Forbidden` so
/// the UI can show "撤回超过 2 分钟，不允许撤回" without any client
/// clock check.
pub const RECALL_WINDOW_SECS: i64 = 120;

impl ChatService {
    pub fn new(storage: ChatStorage, bus: NotificationBus) -> Self {
        Self {
            storage,
            bus,
            moderation: None,
            mute_gate: Arc::new(std::sync::Mutex::new(None)),
            blocklist_gate: Arc::new(std::sync::Mutex::new(None)),
            presence_touch_gate: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(feature = "iroh")]
            iroh_docs_chat: Arc::new(RwLock::new(None)),
        }
    }

    /// Attach a moderation policy. When attached, every
    /// `send_message` runs the body through the policy before the
    /// SQLite write — a denied message is rejected with
    /// `AppError::Forbidden` and never reaches the bus.
    pub fn with_moderation(mut self, moderation: ModerationService) -> Self {
        self.moderation = Some(moderation);
        self
    }

    /// GB-22 — attach a group-mute gate. Returns `self` so
    /// callers can chain `.with_moderation(...).with_mute_gate(...)`
    /// during bootstrap.
    ///
    /// The gate is consulted in [`ChatService::send_message`] only
    /// for conversations whose [`ConversationId::kind_hint`] is
    /// `Group`; DMs bypass the check entirely.
    pub fn with_mute_gate(mut self, gate: MuteGate) -> Self {
        // `MuteGate = Arc<dyn Fn(...)>` — the caller hands us an
        // `Arc` wrapping the trait object. Install it under the
        // mutex (or take the `Arc::get_mut` fast path).
        if let Some(slot) = Arc::get_mut(&mut self.mute_gate) {
            // `slot: &mut Mutex<Option<MuteGate>>` — go through
            // the Mutex API so we hit the proper write path.
            *slot.get_mut().expect("mute_gate mutex poisoned") = Some(gate);
        } else {
            let gate_arc = self.mute_gate.clone();
            tokio::spawn(async move {
                *gate_arc.lock().expect("mute_gate mutex poisoned") = Some(gate);
            });
        }
        self
    }

    /// F-25 / B-7 — install the blocklist gate.
    pub fn with_blocklist_gate(mut self, gate: BlocklistGate) -> Self {
        if let Some(slot) = Arc::get_mut(&mut self.blocklist_gate) {
            *slot.get_mut().expect("blocklist_gate mutex poisoned") = Some(gate);
        } else {
            let gate_arc = self.blocklist_gate.clone();
            let gate_for_task = gate.clone();
            tokio::spawn(async move {
                *gate_arc.lock().expect("blocklist_gate mutex poisoned") = Some(gate_for_task);
            });
        }
        self
    }

    /// Install the presence touch gate. After a message is sent,
    /// this gate is called to update the sender's `last_seen` and
    /// `is_online` in the group membership table.
    pub fn with_presence_touch_gate(mut self, gate: PresenceTouchGate) -> Self {
        if let Some(slot) = Arc::get_mut(&mut self.presence_touch_gate) {
            *slot.get_mut().expect("presence_touch_gate mutex poisoned") = Some(gate);
        } else {
            let gate_arc = self.presence_touch_gate.clone();
            let gate_for_task = gate.clone();
            tokio::spawn(async move {
                *gate_arc.lock().expect("presence_touch_gate mutex poisoned") = Some(gate_for_task);
            });
        }
        self
    }

    /// Phase 5c: attach an `IrohDocsChat` for dual-write.
    /// Call this after construction (typically from `A3chatApp::new`).
    #[cfg(feature = "iroh")]
    pub async fn with_iroh_docs_chat(&self, chat: Arc<IrohDocsChat>) {
        self.iroh_docs_chat.write().await.replace(chat);
    }

    pub fn storage(&self) -> &ChatStorage {
        &self.storage
    }

    pub fn bus(&self) -> &NotificationBus {
        &self.bus
    }

    /// `a3chat.chat.conversation.list` — every conversation the
    /// owner can see.
    pub async fn list_conversations(&self, owner: &UserId) -> AppResult<Vec<ConversationMeta>> {
        self.storage.list_conversations(owner).await
    }

    /// `a3chat.chat.conversation.open` — fetch the full
    /// `ConversationRecord` for `conversation_id`.
    pub async fn open_conversation(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<Option<ConversationRecord>> {
        self.storage.open_conversation(owner, conversation_id).await
    }

    /// `a3chat.chat.message.list` — messages for `conversation_id`.
    ///
    /// Phase 5c: when `iroh_docs_chat` is attached, this merges
    /// messages from SQLite (source of truth for local state) with
    /// messages from iroh-docs (remote peers' writes not yet synced
    /// to our SQLite). The merge is by `(sender_id, sequence)`:
    /// SQLite rows are kept, iroh rows fill gaps from senders we
    /// haven't synced yet. The result is sorted by sequence ascending.
    ///
    /// If the iroh fetch fails, SQLite results are returned unchanged
    /// (best-effort — we do not fail the RPC on iroh unavailability).
    pub async fn list_messages(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        limit: u32,
    ) -> AppResult<Vec<ChatMessage>> {
        // Primary: authoritative SQLite read.
        let mut sqlite_msgs = self.storage.list_messages(owner, conversation_id, limit).await?;

        #[cfg(feature = "iroh")]
        if let Some(docs_chat) = self.iroh_docs_chat.read().await.as_ref() {
            let conv_id = conversation_id.as_str();
            // Take the max sequence we already have from SQLite as the
            // cursor — iroh returns only newer messages from that point.
            let after_seq = sqlite_msgs.last().map(|m| m.sequence);
            let limit_usize = limit as usize;
            match docs_chat.get_messages(conv_id, after_seq, limit_usize).await {
                Ok(iroh_msgs) => {
                    let converted: Vec<ChatMessage> = iroh_msgs
                        .into_iter()
                        .filter_map(|m| crate::storage::iroh_message_to_chat_message(m, conversation_id))
                        .collect();
                    // Merge: deduplicate by (sender_id, sequence), preferring SQLite rows.
                    // SQLite is authoritative for all locally-written messages; iroh rows
                    // are only merged in for remote peers not yet synced to SQLite.
                    let sqlite_keys: std::collections::HashSet<_> = sqlite_msgs
                        .iter()
                        .map(|m| (m.sender_id.as_str().to_string(), m.sequence))
                        .collect();
                    let new_from_iroh: Vec<ChatMessage> = converted
                        .into_iter()
                        .filter(|m| !sqlite_keys.contains(&(m.sender_id.as_str().to_string(), m.sequence)))
                        .collect();
                    sqlite_msgs.extend(new_from_iroh);
                    sqlite_msgs.sort_by_key(|m| m.sequence);
                }
                Err(e) => {
                    tracing::warn!(conv = %conv_id, "iroh-docs list_messages failed: {e}");
                }
            }
        }

        Ok(sqlite_msgs)
    }

    /// `a3chat.chat.message.send` — save the envelope + emit a
    /// `chat.message.received` notification to the bus.
    pub async fn send_message(
        &self,
        owner: &UserId,
        envelope: &MessageEnvelope,
    ) -> AppResult<StoredMessage> {
        envelope.validate()?;
        // F-25 / B-7 — blocklist gate. When the local owner has the
        // receiver on their blocklist, refuse to send the message
        // (and never let it land in storage). This mirrors WeChat
        // semantics ("你已将对方屏蔽，无法发送消息"). System messages
        // bypass the gate so moderation/system cues still propagate.
        //
        // The gate is an Arc<dyn Fn> so we clone the Arc out of the
        // slot lock before any await to keep the slot mutex-free for
        // concurrent callers.
        if !matches!(
            envelope.message_type,
            a3chat_core::message::MessageType::System
        ) {
            let gate_opt = self
                .blocklist_gate
                .lock()
                .expect("blocklist_gate mutex poisoned")
                .clone();
            if let Some(f) = gate_opt {
                if f(owner.clone(), envelope.receiver_id.clone()).await {
                    return Err(AppError::Forbidden(format!(
                        "user is on {}'s blocklist",
                        owner.as_str()
                    )));
                }
            }
        }
        // Content moderation gate. Only run the policy on
        // plaintext bodies (`MessageBody::Plain`), since encrypted
        // bodies are opaque to the moderator (the receiving device
        // runs the same gate after decryption). System messages
        // (`MessageType::System`) bypass the gate so server-emitted
        // moderation cues always reach the user.
        if let Some(m) = &self.moderation {
            if !matches!(envelope.message_type, a3chat_core::message::MessageType::System) {
                if let MessageBody::Plain { content } = &envelope.body {
                    let decision = m.check_content(owner, content);
                    if !decision.is_allowed() {
                        return Err(AppError::Forbidden(format!(
                            "moderation denied message: {}",
                            decision.reason
                        )));
                    }
                }
            }
        }
        // GB-22 — group mute gate. Only consulted for group
        // conversations (`ConversationKindHint::Group`). A muted
        // sender returns Forbidden, mirroring WeChat semantics
        // ("你已被禁言"). System messages bypass the gate.
        if matches!(
            envelope.conversation_id.kind_hint(),
            a3chat_core::id::ConversationKindHint::Group
        ) && !matches!(
            envelope.message_type,
            a3chat_core::message::MessageType::System
        ) {
            // Clone the `MuteGate` (an `Arc<dyn Fn...>`) out under
            // the lock so the inner `.await` runs without holding
            // it. The `Arc::clone` keeps the trait object alive
            // and lets callers re-enter the gate concurrently.
            let gate_opt = self.mute_gate.lock().expect("mute_gate mutex poisoned").clone();
            if let Some(f) = gate_opt {
                if f(envelope.conversation_id.clone(), owner.clone()).await {
                    return Err(AppError::Forbidden("you are muted in this group".into()));
                }
            }
        }
        // `save_outbound` is atomic: message insert + conversation
        // meta upsert + sender read-receipt all happen in one
        // SQLite transaction, so a crash mid-flight can no longer
        // leave a stranded message row whose conversation was never
        // updated.
        let stored = self.storage.save_outbound(owner, envelope).await?;

        // Phase 5b dual-write: mirror the message to iroh-docs.
        // This is best-effort — SQLite is the authoritative store and
        // we do not fail the RPC if iroh is slow or unavailable.
        #[cfg(feature = "iroh")]
        if let Some(docs_chat) = self.iroh_docs_chat.read().await.as_ref() {
            let im_msg = crate::storage::im_message_from_chat_message(&stored);
            let conv_id = envelope.conversation_id.as_str().to_string();
            let author = docs_chat.default_author();
            if let Err(e) = docs_chat.append_message_as(author, &conv_id, im_msg).await {
                tracing::warn!(conv = %conv_id, "iroh-docs dual-write failed: {e}");
            }
        }

        self.bus
            .publish(a3chat_core::event::A3chatEvent::ChatMessageReceived {
                user_id: envelope.receiver_id.clone(),
                conversation_id: envelope.conversation_id.clone(),
                message: stored.message.clone(),
            });

        // Touch presence: update sender's last_seen and is_online for group messages.
        // This runs after the message is persisted so we don't delay the RPC response.
        if matches!(
            envelope.conversation_id.kind_hint(),
            a3chat_core::id::ConversationKindHint::Group
        ) {
            let gate_opt = self
                .presence_touch_gate
                .lock()
                .expect("presence_touch_gate mutex poisoned")
                .clone();
            if let Some(f) = gate_opt {
                f(
                    envelope.conversation_id.clone(),
                    owner.clone(),
                    true,
                )
                .await;
            }
        }

        Ok(stored)
    }

    /// `a3chat.chat.message.ack` — mark a message as read.
    pub async fn ack_message(&self, owner: &UserId, message_id: &MessageId) -> AppResult<()> {
        self.storage.ack_message(owner, message_id).await?;
        // Emit a read receipt so the sender's other devices update
        // the "blue ticks". The unread counter is already
        // decremented atomically inside `storage.ack_message` — we
        // do *not* call `mark_conversation_read` here, or we'd
        // double-decrement.
        let got = self.storage.get_message(owner, message_id).await?;
        if let Some(m) = got {
            self.bus
                .publish(a3chat_core::event::A3chatEvent::ChatMessageRead {
                    user_id: owner.clone(),
                    conversation_id: m.conversation_id.clone(),
                    message_id: m.message_id.clone(),
                    read_at_unix: m.read_at.map(|t| t.timestamp()).unwrap_or(0),
                });
        }
        Ok(())
    }

    /// `a3chat.chat.message.recall` — retract a message we sent.
    pub async fn recall_message(
        &self,
        owner: &UserId,
        message_id: &MessageId,
    ) -> AppResult<ChatMessage> {
        // First try to find the message in `owner`'s storage (the
        // sender's local DB). If it's not there, the user can never
        // recall it — return NotFound.
        let got = self.storage.get_message(owner, message_id).await?;
        let m = match got {
            Some(m) => m,
            None => {
                return Err(AppError::Domain(format!(
                    "message {} not found",
                    message_id.as_str()
                )));
            }
        };
        // Only the original sender may recall.
        if m.sender_id != *owner {
            return Err(AppError::Forbidden(
                "only the sender can recall a message".into(),
            ));
        }
        // F-11 / B-27 — WeChat enforces a 2-minute recall window. We
        // use the message's own `timestamp` (the wall-clock second the
        // sender claims to have hit "send"). If the clock is wildly
        // wrong the request is rejected; clients should sync NTP.
        let now = chrono::Utc::now().timestamp();
        let age_secs = now.saturating_sub(m.timestamp);
        if age_secs > RECALL_WINDOW_SECS {
            return Err(AppError::Forbidden(format!(
                "recall window expired: message is {age_secs}s old, \
                 max {RECALL_WINDOW_SECS}s"
            )));
        }
        self.storage.recall_message(owner, message_id).await?;
        let updated = self
            .storage
            .get_message(owner, message_id)
            .await?
            .ok_or_else(|| AppError::Domain("message vanished after recall".into()))?;
        self.bus
            .publish(a3chat_core::event::A3chatEvent::ChatMessageRecalled {
                user_id: updated.receiver_id.clone(),
                conversation_id: updated.conversation_id.clone(),
                message_id: updated.message_id.clone(),
                recalled_at_unix: updated
                    .recalled_at
                    .map(|t| t.timestamp())
                    .unwrap_or_else(|| chrono::Utc::now().timestamp()),
            });
        Ok(updated)
    }

    /// `a3chat.chat.typing` — best-effort notification; not persisted.
    pub async fn notify_typing(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        expires_at: i64,
    ) -> AppResult<()> {
        self.bus
            .publish(a3chat_core::event::A3chatEvent::ChatTyping {
                user_id: owner.clone(),
                conversation_id: conversation_id.clone(),
                expires_at,
            });
        Ok(())
    }

    /// `a3chat.chat.message.edit` — replace the body of a message
    /// the sender previously sent. Returns the updated
    /// `ChatMessage` so the caller can update the UI in-place.
    pub async fn edit_message(
        &self,
        owner: &UserId,
        message_id: &MessageId,
        new_body: &MessageBody,
    ) -> AppResult<ChatMessage> {
        let updated = self
            .storage
            .edit_message(owner, message_id, new_body)
            .await?;
        self.bus
            .publish(a3chat_core::event::A3chatEvent::ChatMessageEdited {
                user_id: owner.clone(),
                conversation_id: updated.conversation_id.clone(),
                message: updated.clone(),
            });
        Ok(updated)
    }

    /// `a3chat.chat.message.delete` — local-only delete ("delete
    /// for me"). The remote device is *not* notified — by design.
    /// For "delete everywhere" semantics, see recall_message.
    pub async fn delete_message(
        &self,
        owner: &UserId,
        message_id: &MessageId,
    ) -> AppResult<ConversationId> {
        // Capture the conversation_id before deletion so we can
        // emit the post-delete event.
        let conv_id = self
            .storage
            .get_message(owner, message_id)
            .await?
            .map(|m| m.conversation_id)
            .ok_or_else(|| AppError::Domain(format!("message {} not found", message_id.as_str())))?;
        self.storage
            .delete_message_for_me(owner, message_id)
            .await?;
        self.bus
            .publish(a3chat_core::event::A3chatEvent::ChatMessageDeleted {
                user_id: owner.clone(),
                conversation_id: conv_id.clone(),
                message_id: message_id.clone(),
            });
        Ok(conv_id)
    }

    /// `a3chat.chat.search` — full-text search across all
    /// conversations the local user has. Returns up to
    /// `MAX_SEARCH_HITS` matching messages.
    pub async fn search_messages(
        &self,
        owner: &UserId,
        needle: &str,
        conversation_id: Option<&ConversationId>,
        limit: u32,
    ) -> AppResult<Vec<ChatMessage>> {
        if needle.is_empty() {
            return Err(AppError::Domain("search needle is empty".into()));
        }
        self.storage
            .search_messages(crate::storage::SearchQuery {
                owner,
                needle,
                conversation_id,
                limit,
            })
            .await
    }

    /// Helper used by tests / Tauri UI to look up the JSON-RPC
    /// method constants.
    pub fn method_list() -> &'static [&'static str] {
        &[
            A3chatRpcMethod::CHAT_CONVERSATION_LIST,
            A3chatRpcMethod::CHAT_CONVERSATION_OPEN,
            A3chatRpcMethod::CHAT_MESSAGE_SEND,
            A3chatRpcMethod::CHAT_MESSAGE_RECALL,
            A3chatRpcMethod::CHAT_MESSAGE_ACK,
            A3chatRpcMethod::CHAT_MESSAGE_EDIT,
            A3chatRpcMethod::CHAT_MESSAGE_DELETE,
            A3chatRpcMethod::CHAT_SEARCH,
            A3chatRpcMethod::CHAT_TYPING,
        ]
    }
}

/// Concrete dispatcher used by `a3chat-rpc`. Each arm calls into
/// [`ChatService`] above.
pub async fn dispatch(
    svc: Arc<ChatService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        A3chatRpcMethod::CHAT_CONVERSATION_LIST => {
            let convos = svc
                .list_conversations(owner)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(convos).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHAT_CONVERSATION_OPEN => {
            let id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let r = svc
                .open_conversation(owner, &id)
                .await
                .map_err(A3chatError::from)?
                .ok_or_else(|| A3chatError::NotFound(format!("conversation {id} not found")))?;
            serde_json::to_value(r).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHAT_CONVERSATION_CREATE_DIRECT => {
            let peer: UserId = serde_json::from_value(
                params
                    .get("peer_user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("peer_user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let meta = svc
                .storage()
                .create_direct_conversation(owner, &peer)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(meta).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHAT_MESSAGE_SEND => {
            let env: MessageEnvelope = serde_json::from_value(params).map_err(A3chatError::from)?;
            let stored = svc
                .send_message(owner, &env)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(stored).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHAT_MESSAGE_ACK => {
            let id: MessageId = serde_json::from_value(
                params
                    .get("message_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("message_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.ack_message(owner, &id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::CHAT_MESSAGE_RECALL => {
            let id: MessageId = serde_json::from_value(
                params
                    .get("message_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("message_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let m = svc
                .recall_message(owner, &id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(m).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHAT_TYPING => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let expires_at: i64 = serde_json::from_value(
                params
                    .get("expires_at")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("expires_at missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.notify_typing(owner, &conversation_id, expires_at)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::CHAT_MESSAGE_EDIT => {
            let id: MessageId = serde_json::from_value(
                params
                    .get("message_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("message_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let body: MessageBody = serde_json::from_value(
                params
                    .get("body")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("body missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let m = svc
                .edit_message(owner, &id, &body)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(m).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHAT_MESSAGE_DELETE => {
            let id: MessageId = serde_json::from_value(
                params
                    .get("message_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("message_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let conv = svc
                .delete_message(owner, &id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({
                "ok": true,
                "conversation_id": conv,
            }))
        }
        A3chatRpcMethod::CHAT_SEARCH => {
            let needle = params
                .get("needle")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("needle missing".into()))?;
            let conv_filter: Option<ConversationId> = params
                .get("conversation_id")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            let limit: u32 = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32)
                .unwrap_or(50);
            let hits = svc
                .search_messages(owner, needle, conv_filter.as_ref(), limit)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(hits).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHAT_THREAD_LIST => {
            let root_id: MessageId = serde_json::from_value(
                params
                    .get("root_message_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("root_message_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let limit: u32 = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| n.min(1000) as u32)
                .unwrap_or(100);
            let replies = svc
                .storage()
                .list_thread_replies(owner, &root_id, limit)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(replies).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHAT_THREAD_GET => {
            let root_id: MessageId = serde_json::from_value(
                params
                    .get("root_message_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("root_message_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let root = svc
                .storage()
                .get_message(owner, &root_id)
                .await
                .map_err(A3chatError::from)?
                .ok_or_else(|| A3chatError::NotFound(format!("root {} not found", root_id.as_str())))?;
            let replies = svc
                .storage()
                .list_thread_replies(owner, &root_id, 1000)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({
                "root": root,
                "replies": replies,
                "reply_count": replies.len(),
            }))
        }
        A3chatRpcMethod::CHAT_TAP => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let target: Option<UserId> = params
                .get("target_user_id")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            svc.bus.publish(a3chat_core::event::A3chatEvent::ChatTap {
                user_id: owner.clone(),
                conversation_id: conversation_id.clone(),
                target_user_id: target.clone(),
                actor_user_id: owner.clone(),
            });
            Ok(serde_json::json!({
                "ok": true,
                "conversation_id": conversation_id,
                "target_user_id": target,
            }))
        }
        A3chatRpcMethod::CHAT_MESSAGE_SEND_LOCATION => {
            // F-15 — share a location card. The typed payload is
            // validated (range check on lat/lon + content check on
            // the label) and then embedded as a JSON document so the
            // receiver's UI can render a map preview. The message
            // type discriminator is `Location` so the UI doesn't
            // mistake it for a normal text bubble.
            let conversation_id: ConversationId = serde_json::from_value(
                params.get("conversation_id").cloned().ok_or_else(|| {
                    A3chatError::InvalidInput("conversation_id missing".into())
                })?,
            )
            .map_err(A3chatError::from)?;
            let payload: a3chat_core::message::LocationPayload =
                serde_json::from_value(
                    params
                        .get("location")
                        .cloned()
                        .ok_or_else(|| A3chatError::InvalidInput("location missing".into()))?,
                )
                .map_err(A3chatError::from)?;
            payload
                .validate()
                .map_err(|e| A3chatError::InvalidInput(e.to_string()))?;
            let receiver_id: UserId = serde_json::from_value(
                params
                    .get("receiver_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("receiver_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let body_json = serde_json::to_string(&payload).map_err(A3chatError::from)?;
            let envelope = MessageEnvelope {
                conversation_id: conversation_id.clone(),
                receiver_id: receiver_id.clone(),
                message_type: a3chat_core::message::MessageType::Location,
                body: MessageBody::Plain { content: body_json },
                attachments: vec![],
                reply_to: None,
                sequence: 0,
                timestamp: chrono::Utc::now().timestamp(),
            };
            let stored = svc.send_message(owner, &envelope).await.map_err(A3chatError::from)?;
            Ok(serde_json::json!({
                "ok": true,
                "message_id": stored.message.message_id,
                "conversation_id": conversation_id,
                "message_type": "location",
            }))
        }
        A3chatRpcMethod::CHAT_MESSAGE_SEND_CONTACT_CARD => {
            // F-16 — share a contact card. Same shape as the
            // location RPC but with a ContactCardPayload.
            let conversation_id: ConversationId = serde_json::from_value(
                params.get("conversation_id").cloned().ok_or_else(|| {
                    A3chatError::InvalidInput("conversation_id missing".into())
                })?,
            )
            .map_err(A3chatError::from)?;
            let payload: a3chat_core::message::ContactCardPayload =
                serde_json::from_value(
                    params
                        .get("contact_card")
                        .cloned()
                        .ok_or_else(|| {
                            A3chatError::InvalidInput("contact_card missing".into())
                        })?,
                )
                .map_err(A3chatError::from)?;
            payload
                .validate()
                .map_err(|e| A3chatError::InvalidInput(e.to_string()))?;
            let receiver_id: UserId = serde_json::from_value(
                params
                    .get("receiver_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("receiver_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let body_json = serde_json::to_string(&payload).map_err(A3chatError::from)?;
            let envelope = MessageEnvelope {
                conversation_id: conversation_id.clone(),
                receiver_id: receiver_id.clone(),
                message_type: a3chat_core::message::MessageType::ContactCard,
                body: MessageBody::Plain { content: body_json },
                attachments: vec![],
                reply_to: None,
                sequence: 0,
                timestamp: chrono::Utc::now().timestamp(),
            };
            let stored = svc.send_message(owner, &envelope).await.map_err(A3chatError::from)?;
            Ok(serde_json::json!({
                "ok": true,
                "message_id": stored.message.message_id,
                "conversation_id": conversation_id,
                "message_type": "contact_card",
            }))
        }
        _ => Err(A3chatError::Internal(format!(
            "ChatService does not handle {method}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::message::{Attachment, AttachmentKind, MessageBody, MessageType};
    use tempfile::tempdir;

    fn owner() -> UserId {
        UserId::from("alice-node-id")
    }
    fn peer() -> UserId {
        UserId::from("bob-node-id")
    }

    async fn fresh_svc() -> (tempfile::TempDir, ChatService) {
        let dir = tempdir().unwrap();
        let keyring = crate::keyring::E2eKeyring::new(owner());
        let storage = ChatStorage::new(
            crate::storage::StorageConfig::new(dir.path().to_path_buf()),
            keyring,
        );
        storage.init_user(&owner()).await.unwrap();
        let bus = NotificationBus::new(64);
        (dir, ChatService::new(storage, bus))
    }

    fn envelope() -> MessageEnvelope {
        MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice:bob"),
            receiver_id: peer(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "hello".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }

    #[tokio::test]
    async fn send_message_emits_notification() {
        let (_dir, svc) = fresh_svc().await;
        let mut rx = svc.bus().subscribe_for(peer());
        let stored = svc.send_message(&owner(), &envelope()).await.unwrap();
        assert_eq!(stored.message.sequence, 1);
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        match evt {
            a3chat_core::event::A3chatEvent::ChatMessageReceived { user_id, .. } => {
                assert_eq!(user_id, peer());
            }
            _ => panic!("expected ChatMessageReceived"),
        }
    }

    #[tokio::test]
    async fn send_message_creates_conversation_meta() {
        let (_dir, svc) = fresh_svc().await;
        // Empty conversation list at first.
        let before = svc.list_conversations(&owner()).await.unwrap();
        assert!(before.is_empty(), "no conversations before send");
        svc.send_message(&owner(), &envelope()).await.unwrap();
        let after = svc.list_conversations(&owner()).await.unwrap();
        assert_eq!(after.len(), 1);
        let meta = &after[0];
        assert_eq!(meta.conversation_id, ConversationId::from("dm:alice:bob"));
        assert_eq!(meta.kind, a3chat_core::conversation::ConversationKind::Dm);
        assert_eq!(meta.peer_user_id.as_ref(), Some(&peer()));
        assert_eq!(meta.message_count, 1);
        assert_eq!(meta.unread_count, 0, "outbound messages don't bump unread");
        assert_eq!(meta.last_activity, envelope().timestamp);
        assert!(!meta.last_message_preview.is_empty());
    }

    #[tokio::test]
    async fn send_message_updates_existing_conversation() {
        let (_dir, svc) = fresh_svc().await;
        // First message creates the row.
        svc.send_message(&owner(), &envelope()).await.unwrap();
        // Second message bumps message_count and last_activity.
        let mut env2 = envelope();
        env2.sequence = 2;
        env2.timestamp = envelope().timestamp + 5;
        env2.body = MessageBody::Plain {
            content: "second".into(),
        };
        svc.send_message(&owner(), &env2).await.unwrap();
        let list = svc.list_conversations(&owner()).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].message_count, 2);
        assert_eq!(list[0].last_activity, env2.timestamp);
        // With E2E enabled the preview is "[encrypted]" — what the
        // UI renders when it can't yet decrypt (real receivers
        // re-derive the plaintext preview once they `open()`).
        assert!(
            list[0].last_message_preview.contains("encrypted")
                || list[0].last_message_preview.contains("second"),
            "preview should either be sealed marker or contain plaintext: got {:?}",
            list[0].last_message_preview
        );
    }

    #[tokio::test]
    async fn ack_decrements_unread_count() {
        let (_dir, svc) = fresh_svc().await;
        // Simulate inbound: insert a message authored by `peer` into
        // owner's storage with `record_message` set as if owner
        // received it. We can't easily call the inbound path from
        // here, so drive the storage directly.
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
        svc.storage()
            .record_inbound(&owner(), &inbound)
            .await
            .unwrap();
        let before = svc.list_conversations(&owner()).await.unwrap();
        assert_eq!(before[0].unread_count, 1);
        svc.ack_message(&owner(), &inbound.message_id)
            .await
            .unwrap();
        let after = svc.list_conversations(&owner()).await.unwrap();
        assert_eq!(after[0].unread_count, 0);
    }

    #[tokio::test]
    async fn ack_emits_read_receipt() {
        let (_dir, svc) = fresh_svc().await;
        let stored = svc.send_message(&owner(), &envelope()).await.unwrap();
        let mut rx = svc.bus().subscribe();
        svc.ack_message(&owner(), &stored.message.message_id)
            .await
            .unwrap();
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        assert!(matches!(
            evt,
            a3chat_core::event::A3chatEvent::ChatMessageRead { .. }
        ));
    }

    #[tokio::test]
    async fn recall_sets_recalled_at() {
        let (_dir, svc) = fresh_svc().await;
        let stored = svc.send_message(&owner(), &envelope()).await.unwrap();
        let updated = svc
            .recall_message(&owner(), &stored.message.message_id)
            .await
            .unwrap();
        assert!(updated.recalled_at.is_some());
    }

    #[tokio::test]
    async fn recall_by_non_sender_is_forbidden() {
        let (_dir, svc) = fresh_svc().await;
        let stored = svc.send_message(&owner(), &envelope()).await.unwrap();
        // The owner-dir storage is the only one reachable through
        // the service. To simulate a peer trying to recall, we
        // call the storage directly with a stored message whose
        // sender_id differs from the lookup owner.
        let mut foreign_message = stored.message.clone();
        foreign_message.sender_id = peer(); // pretend peer authored it
        svc.storage()
            .record_inbound(&owner(), &foreign_message)
            .await
            .unwrap();
        // Owner (Alice) tries to recall a message authored by Bob,
        // which she has stored.
        let r = svc
            .recall_message(&owner(), &foreign_message.message_id)
            .await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
    }

    #[tokio::test]
    async fn recall_unknown_message_errors_with_not_found() {
        let (_dir, svc) = fresh_svc().await;
        let r = svc
            .recall_message(&owner(), &MessageId::from("1".repeat(64)))
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn typing_emits_event() {
        let (_dir, svc) = fresh_svc().await;
        let mut rx = svc.bus().subscribe();
        svc.notify_typing(&owner(), &ConversationId::from("dm:alice:bob"), 99)
            .await
            .unwrap();
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        assert!(matches!(
            evt,
            a3chat_core::event::A3chatEvent::ChatTyping { .. }
        ));
    }

    #[tokio::test]
    async fn open_conversation_returns_none_when_missing() {
        let (_dir, svc) = fresh_svc().await;
        let r = svc
            .open_conversation(&owner(), &ConversationId::from("dm:unknown"))
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn dispatch_unknown_method_returns_error() {
        let (_dir, svc) = fresh_svc().await;
        let err = dispatch(
            Arc::new(svc),
            "a3chat.no.such.method",
            &owner(),
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::Internal(_)));
    }

    #[tokio::test]
    async fn dispatch_send_message() {
        let (_dir, svc) = fresh_svc().await;
        let params = serde_json::json!(envelope());
        let r = dispatch(
            Arc::new(svc),
            A3chatRpcMethod::CHAT_MESSAGE_SEND,
            &owner(),
            params,
        )
        .await
        .unwrap();
        assert!(r.get("message").is_some());
    }

    #[test]
    fn method_list_includes_all_chat_methods() {
        let list = ChatService::method_list();
        assert!(list.len() >= 9);
        for m in list {
            assert!(m.starts_with("a3chat."));
        }
    }

    // Suppress dead-code warning on AttachmentKind used only via type inference.
    #[test]
    fn attachment_kind_smoke() {
        let _a = Attachment {
            attachment_id: "a".into(),
            file_type: AttachmentKind::File,
            blob_hash: "0".repeat(64),
            file_name: "f".into(),
            file_size: 0,
            thumbnail_hash: None,
        };
    }

    #[tokio::test]
    async fn edit_message_via_service_emits_event() {
        let (_dir, svc) = fresh_svc().await;
        let stored = svc.send_message(&owner(), &envelope()).await.unwrap();
        let mut rx = svc.bus().subscribe();
        let new_body = MessageBody::Plain {
            content: "edited".into(),
        };
        let updated = svc
            .edit_message(&owner(), &stored.message.message_id, &new_body)
            .await
            .unwrap();
        assert!(updated.is_edited);
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        assert!(matches!(
            evt,
            a3chat_core::event::A3chatEvent::ChatMessageEdited { .. }
        ));
    }

    #[tokio::test]
    async fn delete_message_for_me_returns_conversation_id() {
        let (_dir, svc) = fresh_svc().await;
        let stored = svc.send_message(&owner(), &envelope()).await.unwrap();
        let mut rx = svc.bus().subscribe();
        let conv = svc
            .delete_message(&owner(), &stored.message.message_id)
            .await
            .unwrap();
        assert_eq!(conv, stored.message.conversation_id);
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        assert!(matches!(
            evt,
            a3chat_core::event::A3chatEvent::ChatMessageDeleted { .. }
        ));
    }

    #[tokio::test]
    async fn delete_unknown_message_errors() {
        let (_dir, svc) = fresh_svc().await;
        let r = svc
            .delete_message(&owner(), &MessageId::from("4".repeat(64)))
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn search_messages_returns_empty_for_empty_needle() {
        let (_dir, svc) = fresh_svc().await;
        let r = svc.search_messages(&owner(), "", None, 10).await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn dispatch_edit_message_routes_to_service() {
        let (_dir, svc) = fresh_svc().await;
        let stored = svc.send_message(&owner(), &envelope()).await.unwrap();
        let params = serde_json::json!({
            "message_id": stored.message.message_id,
            "body": {
                "kind": "plain",
                "content": "edited via rpc",
            },
        });
        let r = dispatch(
            Arc::new(svc),
            A3chatRpcMethod::CHAT_MESSAGE_EDIT,
            &owner(),
            params,
        )
        .await
        .unwrap();
        assert_eq!(r["is_edited"], true);
    }

    #[tokio::test]
    async fn dispatch_edit_message_rejects_missing_body() {
        let (_dir, svc) = fresh_svc().await;
        let stored = svc.send_message(&owner(), &envelope()).await.unwrap();
        let params = serde_json::json!({
            "message_id": stored.message.message_id,
        });
        let err = dispatch(
            Arc::new(svc),
            A3chatRpcMethod::CHAT_MESSAGE_EDIT,
            &owner(),
            params,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn dispatch_delete_message_returns_conversation_id() {
        let (_dir, svc) = fresh_svc().await;
        let stored = svc.send_message(&owner(), &envelope()).await.unwrap();
        let params = serde_json::json!({
            "message_id": stored.message.message_id,
        });
        let r = dispatch(
            Arc::new(svc),
            A3chatRpcMethod::CHAT_MESSAGE_DELETE,
            &owner(),
            params,
        )
        .await
        .unwrap();
        assert_eq!(r["ok"], true);
        assert!(r["conversation_id"].is_string());
    }

    #[tokio::test]
    async fn dispatch_search_returns_array() {
        let (_dir, svc) = fresh_svc().await;
        let params = serde_json::json!({ "needle": "hello" });
        let r = dispatch(
            Arc::new(svc),
            A3chatRpcMethod::CHAT_SEARCH,
            &owner(),
            params,
        )
        .await
        .unwrap();
        assert!(r.is_array());
    }

    #[tokio::test]
    async fn dispatch_search_rejects_missing_needle() {
        let (_dir, svc) = fresh_svc().await;
        let err = dispatch(
            Arc::new(svc),
            A3chatRpcMethod::CHAT_SEARCH,
            &owner(),
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }
}
