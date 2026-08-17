//! ChatService — handles `chat.conversation.list`,
//! `chat.conversation.open`, `chat.message.send`,
//! `chat.message.recall`, `chat.message.ack`.

use std::sync::Arc;

use a3chat_core::conversation::{ConversationMeta, ConversationRecord};
use a3chat_core::error::A3chatError;
use a3chat_core::id::{ConversationId, MessageId, UserId};
use a3chat_core::message::{ChatMessage, MessageBody, MessageEnvelope};
use a3chat_core::rpc::A3chatRpcMethod;

#[cfg(feature = "iroh")]
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
}

impl ChatService {
    pub fn new(storage: ChatStorage, bus: NotificationBus) -> Self {
        Self {
            storage,
            bus,
            moderation: None,
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

    /// `a3chat.chat.message.send` — save the envelope + emit a
    /// `chat.message.received` notification to the bus.
    pub async fn send_message(
        &self,
        owner: &UserId,
        envelope: &MessageEnvelope,
    ) -> AppResult<StoredMessage> {
        envelope.validate()?;
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
            timestamp: 1_700_000_000,
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
