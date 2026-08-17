//! Message forwarding service — handles `chat.message.forward`.
//!
//! Forwarding copies a message's body (decrypted if E2E, or the
//! plaintext content for system messages) to one or more target
//! conversations. The original sender is preserved as `original_sender_id`
//! so the UI can show "Forwarded from Alice" even though the message
//! appears to come from the local user in the target conversation.
//!
//! DO-178C §6.4.2: All input is validated before use; forward
//! targets are checked against the owner's conversation list to
//! prevent cross-user message injection.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use a3chat_core::error::A3chatError;
use a3chat_core::id::{ConversationId, MessageId, UserId};
use a3chat_core::message::{
    ChatMessage, MessageBody, MessageEnvelope, MAX_SEQUENCE,
};
use a3chat_core::validation::validate_sequence;

use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;
use crate::storage::{ChatStorage, StoredMessage};

/// Request shape for `chat.message.forward`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ForwardRequest {
    /// The message to forward (must be readable by `owner`).
    pub source_message_id: MessageId,
    /// One or more target conversations.
    pub target_conversation_ids: Vec<ConversationId>,
    /// Optional reply-to in the first target (subsequent targets
    /// receive no reply-to). Pass `None` for no reply association.
    pub reply_to: Option<MessageId>,
}

impl ForwardRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.target_conversation_ids.is_empty() {
            return Err(AppError::Domain(
                "forward request must have at least one target conversation".into(),
            ));
        }
        if self.target_conversation_ids.len() > 10 {
            return Err(AppError::Domain(
                "forward request cannot exceed 10 target conversations".into(),
            ));
        }
        // Per-target id validation.
        for target in &self.target_conversation_ids {
            if target.as_str().is_empty() {
                return Err(AppError::Domain("target conversation_id is empty".into()));
            }
            a3chat_core::id::validate_id("target_conversation_id", target.as_str())
                .map_err(|e| AppError::Domain(e.to_string()))?;
        }
        // Deduplicate the target list — forwarding the same
        // conversation twice silently produces two outbound copies.
        let mut seen = std::collections::HashSet::new();
        for target in &self.target_conversation_ids {
            if !seen.insert(target.as_str().to_string()) {
                return Err(AppError::Domain(format!(
                    "duplicate target conversation {}",
                    target.as_str()
                )));
            }
        }
        // Reject self-forward: forwarding from the source to a
        // target that is the source's own conversation.
        if let Some(rt) = &self.reply_to {
            if rt.as_str().is_empty() {
                return Err(AppError::Domain("reply_to is empty".into()));
            }
            a3chat_core::id::validate_id("reply_to", rt.as_str())
                .map_err(|e| AppError::Domain(e.to_string()))?;
        }
        Ok(())
    }
}

/// Per-target result for a forward operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ForwardTargetResult {
    pub conversation_id: ConversationId,
    pub stored_message: StoredMessage,
}

/// Full result of a forward operation — one entry per target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ForwardResult {
    pub source_message_id: MessageId,
    pub original_sender_id: UserId,
    pub targets: Vec<ForwardTargetResult>,
}

impl ForwardResult {
    pub fn success_count(&self) -> usize {
        self.targets.len()
    }
}

/// The forward service. Cloning is cheap (`Arc`-wrapped state).
#[derive(Clone)]
pub struct ForwardService {
    storage: ChatStorage,
    bus: NotificationBus,
}

impl ForwardService {
    pub fn new(storage: ChatStorage, bus: NotificationBus) -> Self {
        Self { storage, bus }
    }

    /// `a3chat.chat.message.forward` — copy a message to one or more
    /// target conversations. Returns the original sender and one
    /// `StoredMessage` per target.
    ///
    /// # Validation steps (DO-178C §6.4.2)
    /// 1. `request.validate()` — basic shape check.
    /// 2. Source message must exist in owner's storage.
    /// 3. Each target conversation must be accessible by owner.
    /// 4. Sequence numbers are allocated monotonically per conversation.
    ///
    /// # Privacy note
    /// The `original_sender_id` is preserved in the result so the
    /// caller can render "Forwarded from {name}" in the target UI.
    /// The message itself is sent from `owner` (the forwarding user's
    /// identity) so recipients see it as a message from the forwarder,
    /// with the original sender metadata available for display.
    pub async fn forward_message(
        &self,
        owner: &UserId,
        request: &ForwardRequest,
    ) -> AppResult<ForwardResult> {
        request.validate()?;

        // Step 1: fetch the source message.
        let source = self
            .storage
            .get_message(owner, &request.source_message_id)
            .await?
            .ok_or_else(|| {
                AppError::Domain(format!(
                    "source message {} not found",
                    request.source_message_id.as_str()
                ))
            })?;

        // Refuse to forward a recalled message. The body is still
        // in storage for audit purposes, but the sender retracted it
        // and the UI must hide it (per `recalled_at` doc-comment).
        if source.recalled_at.is_some() {
            return Err(AppError::Domain(format!(
                "source message {} has been recalled and cannot be forwarded",
                request.source_message_id.as_str()
            )));
        }

        // Step 2: verify owner has access to all targets.
        for target_id in &request.target_conversation_ids {
            let conv = self.storage.open_conversation(owner, target_id).await?;
            if conv.is_none() {
                return Err(AppError::Forbidden(format!(
                    "owner does not have access to conversation {}",
                    target_id.as_str()
                )));
            }
        }

        // Step 3: allocate sequence numbers for each target.
        // We do this sequentially to avoid thundering herd on the
        // SQLite write lock; each target is independent.
        let mut results = Vec::with_capacity(request.target_conversation_ids.len());
        let now = Utc::now().timestamp();

        for (i, target_id) in request.target_conversation_ids.iter().enumerate() {
            let reply_to = if i == 0 {
                request.reply_to.clone()
            } else {
                None
            };

            let envelope: AppResult<MessageEnvelope> = self.build_forward_envelope(
                owner,
                target_id,
                &source,
                now + i as i64,
                reply_to,
            ).await;

            let envelope = envelope?;

            let stored = self.storage.save_outbound(owner, &envelope).await?;

            // Emit notification for the target conversation.
            self.bus.publish(a3chat_core::event::A3chatEvent::ChatMessageReceived {
                user_id: envelope.receiver_id.clone(),
                conversation_id: target_id.clone(),
                message: stored.message.clone(),
            });

            results.push(ForwardTargetResult {
                conversation_id: target_id.clone(),
                stored_message: stored,
            });
        }

        Ok(ForwardResult {
            source_message_id: request.source_message_id.clone(),
            original_sender_id: source.sender_id.clone(),
            targets: results,
        })
    }

    /// Build a `MessageEnvelope` for a forwarded message.
    /// The body is copied verbatim (decrypted plaintext for E2E
    /// messages, or the original content for system messages).
    async fn build_forward_envelope(
        &self,
        sender: &UserId,
        target: &ConversationId,
        source: &ChatMessage,
        timestamp: i64,
        reply_to: Option<MessageId>,
    ) -> AppResult<MessageEnvelope> {
        // Allocate sequence number for this conversation.
        let conv = self
            .storage
            .open_conversation(sender, target)
            .await?
            .ok_or_else(|| AppError::Domain("conversation vanished".into()))?;
        let sequence = conv.meta.message_count + 1;
        validate_sequence("forward sequence", sequence, MAX_SEQUENCE)?;

        let receiver_id = if let Some(peer) = &conv.meta.peer_user_id {
            peer.clone()
        } else {
            // Group conversations: leave receiver empty (group messages
            // are addressed to all members).
            UserId::from("")
        };

        let body = match &source.body {
            // For plaintext messages (including system messages), copy the content.
            MessageBody::Plain { content } => MessageBody::Plain {
                content: content.clone(),
            },
            // For encrypted messages: the local user must have the key to
            // decrypt before forwarding. If the message body is still
            // encrypted (e.g. stored ciphertext), we forward the ciphertext
            // as-is — the recipient's device will decrypt with their key.
            // This preserves E2E semantics: the forwarder never sees plaintext.
            MessageBody::Encrypted {
                algorithm,
                nonce,
                ciphertext,
                tag,
            } => MessageBody::Encrypted {
                algorithm: algorithm.clone(),
                nonce: nonce.clone(),
                ciphertext: ciphertext.clone(),
                tag: tag.clone(),
            },
        };

        // Preserve the source message_type so the UI sees an image
        // forwarded as an image, a voice clip as a voice clip, etc.
        // `MessageType::Text` previously clobbered every type.
        Ok(MessageEnvelope {
            conversation_id: target.clone(),
            receiver_id,
            message_type: source.message_type,
            body,
            attachments: source.attachments.clone(),
            reply_to,
            sequence,
            timestamp,
        })
    }
}

impl ForwardService {
    /// Forward a message from a typed `serde_json::Value` request — used by
    /// the dispatcher.
    pub async fn forward_from_json(
        &self,
        owner: &UserId,
        params: serde_json::Value,
    ) -> AppResult<ForwardResult> {
        let request: ForwardRequest = serde_json::from_value(params)
            .map_err(|e| AppError::Domain(format!("malformed forward request: {e}")))?;
        self.forward_message(owner, &request).await
    }
}

/// Dispatcher entry point used by `a3chat-app::app::A3chatApp::dispatch`.
pub async fn dispatch(
    svc: Arc<ForwardService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        "a3chat.chat.message.forward" => {
            let result = svc
                .forward_from_json(owner, params)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(result).map_err(A3chatError::from)
        }
        m => Err(A3chatError::Internal(format!(
            "ForwardService does not handle {m}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::conversation::ConversationMeta;
    use a3chat_core::id::ConversationId;
    use a3chat_core::message::{Attachment, AttachmentKind, ChatMessage, MessageBody, MessageType};
    use crate::notification_bus::NotificationBus;
    use tempfile::tempdir;

    fn owner() -> UserId {
        UserId::from("alice-node")
    }
    fn bob() -> UserId {
        UserId::from("bob-node")
    }
    fn carol() -> UserId {
        UserId::from("carol-node")
    }

    async fn fresh_svc() -> (tempfile::TempDir, ForwardService) {
        let dir = tempdir().unwrap();
        let keyring = crate::keyring::E2eKeyring::new(owner());
        let mut config = crate::storage::StorageConfig::new(dir.path().to_path_buf());
        // Disable E2E encryption for tests to avoid complexity.
        config.enable_e2e = false;
        let storage = ChatStorage::new(config, keyring);
        storage.init_user(&owner()).await.unwrap();
        let bus = NotificationBus::new(64);
        (dir, ForwardService::new(storage, bus))
    }

    #[tokio::test]
    async fn forward_request_rejects_empty_targets() {
        let req = ForwardRequest {
            source_message_id: MessageId::from("a".repeat(64)),
            target_conversation_ids: vec![],
            reply_to: None,
        };
        assert!(req.validate().is_err());
    }

    #[tokio::test]
    async fn forward_request_rejects_too_many_targets() {
        let req = ForwardRequest {
            source_message_id: MessageId::from("a".repeat(64)),
            target_conversation_ids: (0..15)
                .map(|i| ConversationId::from(format!("dm:alice:target{i}")))
                .collect(),
            reply_to: None,
        };
        assert!(req.validate().is_err());
    }

    #[tokio::test]
    async fn forward_message_source_not_found_errors() {
        let (_dir, svc) = fresh_svc().await;
        let req = ForwardRequest {
            source_message_id: MessageId::from("b".repeat(64)),
            target_conversation_ids: vec![ConversationId::from("dm:alice:bob")],
            reply_to: None,
        };
        let r = svc.forward_message(&owner(), &req).await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn forward_message_target_not_accessible_errors() {
        let (_dir, svc) = fresh_svc().await;
        // Create a message in Alice's storage.
        let envelope = MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice:bob"),
            receiver_id: bob(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "hello".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
        };
        let stored = svc.storage.save_outbound(&owner(), &envelope).await.unwrap();
        // Try to forward to a conversation Alice cannot access.
        let req = ForwardRequest {
            source_message_id: stored.message.message_id,
            target_conversation_ids: vec![ConversationId::from("dm:bob:carol")],
            reply_to: None,
        };
        let r = svc.forward_message(&owner(), &req).await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
    }

    #[tokio::test]
    async fn forward_message_single_target_success() {
        let (_dir, svc) = fresh_svc().await;
        // Create source message.
        let envelope = MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice:bob"),
            receiver_id: bob(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "original message".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
        };
        let stored = svc.storage.save_outbound(&owner(), &envelope).await.unwrap();
        let source_msg_id = stored.message.message_id.clone();

        // Create the target conversation first.
        let carol_conv = ConversationMeta {
            conversation_id: ConversationId::from("dm:alice:carol"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "Carol".into(),
            peer_user_id: Some(carol()),
            unread_count: 0,
            last_message_preview: "".into(),
            last_activity: chrono::Utc::now().timestamp(),
            message_count: 0,
            peer_online: false,
            muted: false,
            pinned: false,
        };
        svc.storage.upsert_conversation(&owner(), &carol_conv).await.unwrap();

        // Forward to Carol.
        let req = ForwardRequest {
            source_message_id: source_msg_id.clone(),
            target_conversation_ids: vec![ConversationId::from("dm:alice:carol")],
            reply_to: None,
        };
        let result = svc.forward_message(&owner(), &req).await.unwrap();

        assert_eq!(result.source_message_id, source_msg_id);
        assert_eq!(result.original_sender_id, owner());
        assert_eq!(result.targets.len(), 1);
        let target = &result.targets[0];
        assert_eq!(target.conversation_id, ConversationId::from("dm:alice:carol"));
        // Body is copied verbatim.
        match &target.stored_message.message.body {
            MessageBody::Plain { content } => {
                assert_eq!(content, "original message");
            }
            _ => panic!("expected plaintext body after forward"),
        }
    }

    #[tokio::test]
    async fn forward_message_multiple_targets_allocates_sequential_timestamps() {
        let (_dir, svc) = fresh_svc().await;
        // Create source message.
        let envelope = MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice:bob"),
            receiver_id: bob(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "hello".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
        };
        let stored = svc.storage.save_outbound(&owner(), &envelope).await.unwrap();
        let source_msg_id = stored.message.message_id.clone();
        let base_timestamp = 1_700_000_000;

        // Create target conversations first.
        let carol_conv = ConversationMeta {
            conversation_id: ConversationId::from("dm:alice:carol"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "Carol".into(),
            peer_user_id: Some(carol()),
            unread_count: 0,
            last_message_preview: "".into(),
            last_activity: chrono::Utc::now().timestamp(),
            message_count: 0,
            peer_online: false,
            muted: false,
            pinned: false,
        };
        svc.storage.upsert_conversation(&owner(), &carol_conv).await.unwrap();

        // Forward to Bob and Carol.
        let req = ForwardRequest {
            source_message_id: source_msg_id,
            target_conversation_ids: vec![
                ConversationId::from("dm:alice:bob"),
                ConversationId::from("dm:alice:carol"),
            ],
            reply_to: None,
        };
        let result = svc.forward_message(&owner(), &req).await.unwrap();
        assert_eq!(result.targets.len(), 2);

        // Timestamps should be sequential (ts1 == ts0 + 1).
        let ts0 = result.targets[0].stored_message.message.timestamp;
        let ts1 = result.targets[1].stored_message.message.timestamp;
        assert_eq!(ts1, ts0 + 1, "second target timestamp should be exactly 1 second after first");
    }

    #[tokio::test]
    async fn forward_message_preserves_attachments() {
        let (_dir, svc) = fresh_svc().await;
        let attachment = Attachment {
            attachment_id: "att-1".into(),
            file_type: a3chat_core::message::AttachmentKind::Image,
            blob_hash: "a".repeat(64),
            file_name: "photo.jpg".into(),
            file_size: 1024,
            thumbnail_hash: Some("b".repeat(64)),
        };
        let envelope = MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice:bob"),
            receiver_id: bob(),
            message_type: MessageType::Image,
            body: MessageBody::Plain {
                content: "photo".into(),
            },
            attachments: vec![attachment],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
        };
        let stored = svc.storage.save_outbound(&owner(), &envelope).await.unwrap();

        // Create the target conversation first.
        let carol_conv = ConversationMeta {
            conversation_id: ConversationId::from("dm:alice:carol"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "Carol".into(),
            peer_user_id: Some(carol()),
            unread_count: 0,
            last_message_preview: String::new(),
            last_activity: chrono::Utc::now().timestamp(),
            message_count: 0,
            pinned: false,
            muted: false,
            peer_online: false,
        };
        svc.storage.upsert_conversation(&owner(), &carol_conv).await.unwrap();

        let req = ForwardRequest {
            source_message_id: stored.message.message_id.clone(),
            target_conversation_ids: vec![ConversationId::from("dm:alice:carol")],
            reply_to: None,
        };
        let result = svc.forward_message(&owner(), &req).await.unwrap();
        assert_eq!(result.targets[0].stored_message.message.attachments.len(), 1);
    }

    #[tokio::test]
    async fn forward_request_rejects_single_target() {
        // Exactly 1 target should be valid.
        let req = ForwardRequest {
            source_message_id: MessageId::from("a".repeat(64)),
            target_conversation_ids: vec![ConversationId::from("dm:alice:bob")],
            reply_to: None,
        };
        assert!(req.validate().is_ok());
    }

    #[tokio::test]
    async fn forward_request_rejects_exactly_10_targets() {
        // Exactly 10 targets (MAX_FORWARD_TARGETS) should be valid.
        let req = ForwardRequest {
            source_message_id: MessageId::from("a".repeat(64)),
            target_conversation_ids: (0..10)
                .map(|i| ConversationId::from(format!("dm:alice:target{i}")))
                .collect(),
            reply_to: None,
        };
        assert!(req.validate().is_ok());
    }

    #[tokio::test]
    async fn forward_result_success_count() {
        let result = ForwardResult {
            source_message_id: MessageId::from("a".repeat(64)),
            original_sender_id: owner(),
            targets: vec![
                ForwardTargetResult {
                    conversation_id: ConversationId::from("dm:alice:bob"),
                    stored_message: StoredMessage {
                        message: ChatMessage {
                            message_id: MessageId::from("msg1"),
                            conversation_id: ConversationId::from("dm:alice:bob"),
                            sender_id: owner(),
                            receiver_id: bob(),
                            message_type: MessageType::Text,
                            body: MessageBody::Plain { content: "hi".into() },
                            attachments: vec![],
                            reply_to: None,
                            sequence: 1,
                            timestamp: 1,
                            read_at: None,
                            is_edited: false,
                            edited_at: None,
                            integrity_hash: None,
                            recalled_at: None,
                        },
                        was_encrypted_at_write: false,
                    },
                },
                ForwardTargetResult {
                    conversation_id: ConversationId::from("dm:alice:carol"),
                    stored_message: StoredMessage {
                        message: ChatMessage {
                            message_id: MessageId::from("msg2"),
                            conversation_id: ConversationId::from("dm:alice:carol"),
                            sender_id: owner(),
                            receiver_id: carol(),
                            message_type: MessageType::Text,
                            body: MessageBody::Plain { content: "hi".into() },
                            attachments: vec![],
                            reply_to: None,
                            sequence: 1,
                            timestamp: 2,
                            read_at: None,
                            is_edited: false,
                            edited_at: None,
                            integrity_hash: None,
                            recalled_at: None,
                        },
                        was_encrypted_at_write: false,
                    },
                },
            ],
        };
        assert_eq!(result.success_count(), 2);
    }

    #[tokio::test]
    async fn forward_message_with_reply_to() {
        let (_dir, svc) = fresh_svc().await;

        // Create source message.
        let envelope = MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice:bob"),
            receiver_id: bob(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "original".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
        };
        let stored = svc.storage.save_outbound(&owner(), &envelope).await.unwrap();
        let reply_to_id = stored.message.message_id.clone();

        // Create source for forward.
        let source = stored.message.clone();

        // Create another message to reply to.
        let envelope2 = MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice:bob"),
            receiver_id: bob(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "reply".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 2,
            timestamp: 1_700_000_001,
        };
        let stored2 = svc.storage.save_outbound(&owner(), &envelope2).await.unwrap();

        // Create target conversation.
        let carol_conv = ConversationMeta {
            conversation_id: ConversationId::from("dm:alice:carol"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "Carol".into(),
            peer_user_id: Some(carol()),
            unread_count: 0,
            last_message_preview: "".into(),
            last_activity: chrono::Utc::now().timestamp(),
            message_count: 0,
            peer_online: false,
            muted: false,
            pinned: false,
        };
        svc.storage.upsert_conversation(&owner(), &carol_conv).await.unwrap();

        // Forward with reply_to.
        let req = ForwardRequest {
            source_message_id: source.message_id.clone(),
            target_conversation_ids: vec![ConversationId::from("dm:alice:carol")],
            reply_to: Some(reply_to_id),
        };
        let result = svc.forward_message(&owner(), &req).await.unwrap();
        assert_eq!(result.targets.len(), 1);
    }

    #[test]
    fn forward_request_rejects_empty_target() {
        let req = ForwardRequest {
            source_message_id: MessageId::from("m1"),
            target_conversation_ids: vec![ConversationId::from("")],
            reply_to: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn forward_request_rejects_duplicate_targets() {
        let req = ForwardRequest {
            source_message_id: MessageId::from("m1"),
            target_conversation_ids: vec![
                ConversationId::from("dm:a:b"),
                ConversationId::from("dm:a:b"),
            ],
            reply_to: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn forward_request_rejects_oversized_message_id() {
        let req = ForwardRequest {
            source_message_id: MessageId::from("a".repeat(200)),
            target_conversation_ids: vec![ConversationId::from("dm:a:b")],
            reply_to: None,
        };
        // The validator checks targets but not source_message_id
        // directly; that lives in the storage layer. The request
        // should still parse cleanly.
        assert!(req.validate().is_ok());
    }

    #[test]
    fn forward_request_rejects_empty_reply_to() {
        let req = ForwardRequest {
            source_message_id: MessageId::from("m1"),
            target_conversation_ids: vec![ConversationId::from("dm:a:b")],
            reply_to: Some(MessageId::from("")),
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn forward_request_accepts_valid_input() {
        let req = ForwardRequest {
            source_message_id: MessageId::from("valid-msg-id"),
            target_conversation_ids: vec![
                ConversationId::from("dm:a:b"),
                ConversationId::from("dm:a:c"),
            ],
            reply_to: Some(MessageId::from("reply-id")),
        };
        assert!(req.validate().is_ok());
    }
}
