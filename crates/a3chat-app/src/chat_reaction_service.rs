//! ChatReactionService — message reactions / emoji replies.
//!
//! Provides the ability to add and remove emoji reactions on messages.
//! Reactions are stored alongside the chat storage and emit events for SSE delivery.
//!
//! Storage shape: `message_id → (conversation_id, user_id → reaction_type)`.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use a3chat_core::error::A3chatError;
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::{MessageId, UserId, ConversationId};
use a3chat_core::message::{MessageReaction, ReactionType};

use crate::error::AppResult;
use crate::notification_bus::NotificationBus;

/// Aggregated reaction summary for a message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReactionSummary {
    pub message_id: MessageId,
    pub reactions: HashMap<String, u32>, // reaction_type -> count
    pub total_count: u32,
}

/// In-memory record for a single message's reactions.
#[derive(Debug, Clone, Default)]
struct MessageRecord {
    conversation_id: Option<ConversationId>,
    /// user_id -> reaction_type
    by_user: HashMap<String, ReactionType>,
}

/// Service for managing message reactions.
#[derive(Clone, Debug)]
pub struct ChatReactionService {
    bus: NotificationBus,
    /// In-memory reaction store. In production, this would be persisted
    /// alongside the chat storage. Map: message_id -> record
    reactions: Arc<tokio::sync::RwLock<HashMap<String, MessageRecord>>>,
}

impl Default for ChatReactionService {
    fn default() -> Self {
        Self::new(NotificationBus::default())
    }
}

impl ChatReactionService {
    /// Create a new reaction service.
    #[must_use = "constructing a reaction service without using it is a bug"]
    pub fn new(bus: NotificationBus) -> Self {
        Self {
            bus,
            reactions: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Add or update a reaction on a message.
    /// If the user already reacted with this type, this is a no-op.
    /// If the user reacted with a different type, it replaces the old one.
    pub async fn add_reaction(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        message_id: &MessageId,
        reaction_type: ReactionType,
    ) -> AppResult<MessageReaction> {
        let mut store = self.reactions.write().await;
        let msg_key = message_id.as_str();

        let record = store.entry(msg_key.to_string()).or_default();
        // Pin the conversation_id the first time we touch this message
        // so `get_conversation_reactions` can filter correctly.
        record.conversation_id.get_or_insert_with(|| conversation_id.clone());

        // Check if user already has this exact reaction
        if let Some(existing) = record.by_user.get(owner.as_str()) {
            if *existing == reaction_type {
                // Already reacted with this type - return existing
                return Ok(MessageReaction::new(
                    message_id.clone(),
                    owner.clone(),
                    reaction_type,
                ));
            }
        }

        // Replace old reaction
        record.by_user.insert(owner.as_str().to_string(), reaction_type);

        let reaction = MessageReaction::new(
            message_id.clone(),
            owner.clone(),
            reaction_type,
        );

        // Emit event. `user_id` is the *owner* (i.e. the recipient of
        // the notification in the per-user bus subscription model),
        // while `reactor_id` is the user who actually reacted.
        // Previously both were stamped with the reactor, which made
        // per-user SSE filters ignore reactions that did *not* belong
        // to the reactor.
        self.bus.publish(A3chatEvent::ChatMessageReactionToggled {
            user_id: owner.clone(),
            conversation_id: conversation_id.clone(),
            message_id: message_id.clone(),
            reactor_id: owner.clone(),
            reaction_type: reaction_type.as_str().to_string(),
            is_added: true,
        });

        Ok(reaction)
    }

    /// Remove a reaction from a message.
    pub async fn remove_reaction(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        message_id: &MessageId,
    ) -> AppResult<bool> {
        let mut store = self.reactions.write().await;
        let msg_key = message_id.as_str();

        // Look up the existing record first — do *not* create an
        // empty entry on a no-op removal; that bloats the store
        // with a phantom line per "remove a non-existent reaction"
        // RPC call.
        let removed = match store.get_mut(msg_key) {
            Some(record) => {
                let removed = record.by_user.remove(owner.as_str());
                if record.by_user.is_empty() && record.conversation_id.is_none() {
                    // Nothing left under this message — drop the entry.
                    store.remove(msg_key);
                }
                removed
            }
            None => None,
        };

        if let Some(removed) = removed {
            self.bus.publish(A3chatEvent::ChatMessageReactionToggled {
                user_id: owner.clone(),
                conversation_id: conversation_id.clone(),
                message_id: message_id.clone(),
                reactor_id: owner.clone(),
                reaction_type: removed.as_str().to_string(),
                is_added: false,
            });
        }

        Ok(removed.is_some())
    }

    /// Get all reactions for a message.
    pub async fn get_reactions(&self, message_id: &MessageId) -> AppResult<Vec<MessageReaction>> {
        let store = self.reactions.read().await;
        let msg_key = message_id.as_str();

        let record = match store.get(msg_key) {
            Some(r) => r,
            None => return Ok(vec![]),
        };

        let reactions: Vec<MessageReaction> = record
            .by_user
            .iter()
            .map(|(user_id, rx_type)| {
                MessageReaction::new(message_id.clone(), UserId::from(user_id.clone()), *rx_type)
            })
            .collect();

        Ok(reactions)
    }

    /// Get aggregated reaction summary for a message.
    pub async fn get_summary(&self, message_id: &MessageId) -> AppResult<ReactionSummary> {
        let store = self.reactions.read().await;
        let msg_key = message_id.as_str();

        let record = match store.get(msg_key) {
            Some(r) => r,
            None => {
                return Ok(ReactionSummary {
                    message_id: message_id.clone(),
                    reactions: HashMap::new(),
                    total_count: 0,
                });
            }
        };

        let mut counts: HashMap<String, u32> = HashMap::new();
        for rx_type in record.by_user.values() {
            *counts.entry(rx_type.as_str().to_string()).or_insert(0) += 1;
        }

        let total_count: u32 = counts.values().sum();

        Ok(ReactionSummary {
            message_id: message_id.clone(),
            reactions: counts,
            total_count,
        })
    }

    /// Get all reactions for a conversation. Now actually filters
    /// by `conversation_id` (previously the parameter was ignored).
    pub async fn get_conversation_reactions(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<HashMap<MessageId, ReactionSummary>> {
        let store = self.reactions.read().await;
        let mut result: HashMap<MessageId, ReactionSummary> = HashMap::new();

        for (msg_key, record) in store.iter() {
            // Filter: only reactions whose message is in THIS conversation.
            if record.conversation_id.as_ref() != Some(conversation_id) {
                continue;
            }
            let mut counts: HashMap<String, u32> = HashMap::new();
            for rx_type in record.by_user.values() {
                *counts.entry(rx_type.as_str().to_string()).or_insert(0) += 1;
            }
            let total_count: u32 = counts.values().sum();

            let msg_id = MessageId::from(msg_key.clone());
            result.insert(
                msg_id.clone(),
                ReactionSummary {
                    message_id: msg_id,
                    reactions: counts,
                    total_count,
                },
            );
        }

        Ok(result)
    }
}

/// Dispatch helper used by `a3chat-rpc`.
pub async fn dispatch(
    svc: Arc<ChatReactionService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        "a3chat.chat.reaction.add" => {
            let message_id: MessageId = serde_json::from_value(
                params.get("message_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("message_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;

            let conversation_id: ConversationId = serde_json::from_value(
                params.get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;

            let reaction_str = params
                .get("reaction_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("reaction_type missing".into()))?;

            let reaction_type = ReactionType::from_str(reaction_str)
                .ok_or_else(|| A3chatError::InvalidInput(format!("unknown reaction_type: {reaction_str}")))?;

            let reaction = svc
                .add_reaction(owner, &conversation_id, &message_id, reaction_type)
                .await
                .map_err(A3chatError::from)?;

            serde_json::to_value(reaction).map_err(A3chatError::from)
        }
        "a3chat.chat.reaction.remove" => {
            let message_id: MessageId = serde_json::from_value(
                params.get("message_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("message_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;

            let conversation_id: ConversationId = serde_json::from_value(
                params.get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;

            let removed = svc
                .remove_reaction(owner, &conversation_id, &message_id)
                .await
                .map_err(A3chatError::from)?;

            Ok(serde_json::json!({ "removed": removed }))
        }
        "a3chat.chat.reaction.get" => {
            let message_id: MessageId = serde_json::from_value(
                params.get("message_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("message_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;

            let summary = svc
                .get_summary(&message_id)
                .await
                .map_err(A3chatError::from)?;

            serde_json::to_value(summary).map_err(A3chatError::from)
        }
        _ => Err(A3chatError::Internal(format!(
            "ChatReactionService does not handle {method}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> UserId {
        UserId::from("alice")
    }

    fn message_id() -> MessageId {
        MessageId::from("msg-test-123")
    }

    fn conversation_id() -> ConversationId {
        ConversationId::from("dm:alice:bob")
    }

    #[tokio::test]
    async fn add_reaction_emits_event() {
        let svc = ChatReactionService::new(NotificationBus::default());
        let mut rx = svc.bus.subscribe_for(owner());

        let reaction = svc
            .add_reaction(&owner(), &conversation_id(), &message_id(), ReactionType::Like)
            .await
            .unwrap();

        assert_eq!(reaction.reaction_type, ReactionType::Like);
        assert_eq!(reaction.user_id, owner());

        // Check event was emitted
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event arrives")
            .expect("event is some");

        assert!(matches!(
            evt,
            A3chatEvent::ChatMessageReactionToggled { is_added: true, .. }
        ));
    }

    #[tokio::test]
    async fn remove_reaction_emits_event() {
        let svc = ChatReactionService::new(NotificationBus::default());

        // First add
        svc.add_reaction(&owner(), &conversation_id(), &message_id(), ReactionType::Love)
            .await
            .unwrap();

        let mut rx = svc.bus.subscribe_for(owner());

        // Then remove
        let removed = svc
            .remove_reaction(&owner(), &conversation_id(), &message_id())
            .await
            .unwrap();

        assert!(removed);

        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event arrives")
            .expect("event is some");

        assert!(matches!(
            evt,
            A3chatEvent::ChatMessageReactionToggled { is_added: false, .. }
        ));
    }

    #[tokio::test]
    async fn get_reactions_returns_list() {
        let svc = ChatReactionService::new(NotificationBus::default());

        let bob = UserId::from("bob");

        svc.add_reaction(&owner(), &conversation_id(), &message_id(), ReactionType::Like)
            .await
            .unwrap();
        svc.add_reaction(&bob, &conversation_id(), &message_id(), ReactionType::Like)
            .await
            .unwrap();
        svc.add_reaction(&bob, &conversation_id(), &message_id(), ReactionType::Love)
            .await
            .unwrap();

        let reactions = svc.get_reactions(&message_id()).await.unwrap();
        assert_eq!(reactions.len(), 2); // bob's first reaction was replaced
    }

    #[tokio::test]
    async fn get_summary_aggregates_counts() {
        let svc = ChatReactionService::new(NotificationBus::default());

        let bob = UserId::from("bob");
        let carol = UserId::from("carol");

        svc.add_reaction(&owner(), &conversation_id(), &message_id(), ReactionType::Like)
            .await
            .unwrap();
        svc.add_reaction(&bob, &conversation_id(), &message_id(), ReactionType::Like)
            .await
            .unwrap();
        svc.add_reaction(&carol, &conversation_id(), &message_id(), ReactionType::Love)
            .await
            .unwrap();

        let summary = svc.get_summary(&message_id()).await.unwrap();
        assert_eq!(summary.total_count, 3);
        assert_eq!(summary.reactions.get("like"), Some(&2));
        assert_eq!(summary.reactions.get("love"), Some(&1));
    }

    #[tokio::test]
    async fn duplicate_reaction_is_noop() {
        let svc = ChatReactionService::new(NotificationBus::default());

        let r1 = svc
            .add_reaction(&owner(), &conversation_id(), &message_id(), ReactionType::Like)
            .await
            .unwrap();

        let r2 = svc
            .add_reaction(&owner(), &conversation_id(), &message_id(), ReactionType::Like)
            .await
            .unwrap();

        // Both should have same user and reaction type
        assert_eq!(r1.user_id, r2.user_id);
        assert_eq!(r1.reaction_type, r2.reaction_type);

        let reactions = svc.get_reactions(&message_id()).await.unwrap();
        assert_eq!(reactions.len(), 1);
    }

    #[tokio::test]
    async fn reaction_type_from_str() {
        assert_eq!(ReactionType::from_str("like"), Some(ReactionType::Like));
        assert_eq!(ReactionType::from_str("love"), Some(ReactionType::Love));
        assert_eq!(ReactionType::from_str("unknown"), None);
    }

    #[tokio::test]
    async fn remove_reaction_does_not_create_empty_record() {
        let svc = ChatReactionService::new(NotificationBus::default());
        let msg = MessageId::from("msg-empty-remove");
        let conv = ConversationId::from("dm:alice:bob");

        // Removing a reaction that was never added must be a no-op
        // and must NOT leave an empty record in the store.
        let removed = svc.remove_reaction(&owner(), &conv, &msg).await.unwrap();
        assert!(!removed);

        let reactions = svc.get_reactions(&msg).await.unwrap();
        assert!(reactions.is_empty());
        let summary = svc.get_summary(&msg).await.unwrap();
        assert_eq!(summary.total_count, 0);
    }

    #[tokio::test]
    async fn get_conversation_reactions_filters_by_conversation() {
        let svc = ChatReactionService::new(NotificationBus::default());
        let conv_a = ConversationId::from("dm:alice:bob");
        let conv_b = ConversationId::from("dm:alice:carol");
        let msg_a = MessageId::from("msg-a");
        let msg_b = MessageId::from("msg-b");

        svc.add_reaction(&owner(), &conv_a, &msg_a, ReactionType::Like).await.unwrap();
        svc.add_reaction(&owner(), &conv_b, &msg_b, ReactionType::Like).await.unwrap();

        let only_a = svc.get_conversation_reactions(&conv_a).await.unwrap();
        assert_eq!(only_a.len(), 1);
        assert!(only_a.contains_key(&msg_a));
        assert!(!only_a.contains_key(&msg_b));
    }

    #[tokio::test]
    async fn reaction_remove_cleans_up_empty_record() {
        let svc = ChatReactionService::new(NotificationBus::default());
        let conv = ConversationId::from("dm:alice:bob");
        let msg = MessageId::from("msg-cleanup");

        svc.add_reaction(&owner(), &conv, &msg, ReactionType::Like).await.unwrap();
        svc.remove_reaction(&owner(), &conv, &msg).await.unwrap();

        // Subsequent "remove" should still be a no-op and not create
        // a new entry.
        let removed = svc.remove_reaction(&owner(), &conv, &msg).await.unwrap();
        assert!(!removed);
    }
}
