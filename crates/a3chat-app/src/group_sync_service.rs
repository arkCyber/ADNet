//! `GroupSyncService` — P2P group chat synchronization via iroh-docs.
//!
//! This module provides distributed group chat sync using the existing
//! iroh-docs infrastructure. It enables:
//!
//! 1. **Offline message sync**: When users come back online, they
//!    automatically receive missed messages from the group's iroh-docs doc.
//! 2. **Ticket distribution**: Group members receive doc tickets to join
//!    the sync network.
//! 3. **SQLite backfill**: Messages received from iroh are written back
//!    to local SQLite for persistence.
//!
//! ## Sync Flow
//!
//! 1. **Join Group**: `GroupSyncService::join_group` imports the group's
//!    DocTicket and opens the conversation doc in iroh-docs.
//! 2. **Send Message**: `ChatService::send_message` dual-writes to SQLite
//!    (authoritative) and iroh-docs (sync fan-out).
//! 3. **Receive Sync**: When iroh delivers a remote insert, it propagates
//!    via `subscribe()` → `MessageEvent::Insert`.
//! 4. **SQLite Backfill**: The sync service writes received messages back
//!    to SQLite with proper deduplication.
//! 5. **Bus Notification**: Once in SQLite, a `ChatMessageReceived` event
//!    is published so SSE subscribers get notified.

#[cfg(feature = "iroh")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use a3chat_core::event::A3chatEvent;
use a3chat_core::id::{ConversationId, UserId};
use chrono::{DateTime, Utc};
use tokio::sync::broadcast;
use tokio::sync::RwLock;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, info, warn};

use crate::error::{AppError, AppResult};
use crate::storage::ChatStorage;

/// Maximum number of messages to backfill from iroh in one batch.
const BACKFILL_BATCH_SIZE: usize = 100;

/// Interval between periodic sync checks for all joined groups.
const SYNC_TICK_INTERVAL: Duration = Duration::from_secs(30);

/// Group sync state tracking.
#[derive(Debug, Clone)]
struct GroupSyncState {
    conversation_id: ConversationId,
    /// Last sequence we've processed from this group.
    last_processed_seq: u32,
    /// Whether we have an active subscription.
    is_subscribed: bool,
    /// Last time we checked for new messages.
    last_sync_at: Option<DateTime<Utc>>,
}

impl GroupSyncState {
    fn new(conversation_id: ConversationId) -> Self {
        Self {
            conversation_id,
            last_processed_seq: 0,
            is_subscribed: false,
            last_sync_at: None,
        }
    }
}

/// Group chat synchronization service.
///
/// This service manages P2P group chat synchronization using iroh-docs.
/// It maintains subscriptions to group docs and backfills messages to SQLite.
#[derive(Clone)]
pub struct GroupSyncService {
    storage: ChatStorage,
    /// The shared iroh-docs chat bridge.
    docs_chat: Arc<a3net_chatstore::IrohDocsChat>,
    /// Per-conversation sync state.
    sync_states: Arc<RwLock<HashMap<ConversationId, GroupSyncState>>>,
    /// Event bus for sync notifications.
    bus: crate::notification_bus::NotificationBus,
    /// Channel to receive iroh message events.
    event_rx: Arc<RwLock<Option<broadcast::Receiver<a3net_chatstore::MessageEvent>>>>,
    /// Background task handle.
    _background_handle: Arc<tokio::sync::oneshot::Sender<()>>,
}

impl std::fmt::Debug for GroupSyncService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupSyncService").finish()
    }
}

impl GroupSyncService {
    /// Create a new group sync service.
    ///
    /// This attaches to the iroh-docs chat bridge and starts background
    /// sync tasks. Call `shutdown()` to stop.
    pub fn new(
        storage: ChatStorage,
        docs_chat: a3net_chatstore::IrohDocsChat,
        bus: crate::notification_bus::NotificationBus,
    ) -> Self {
        let docs_chat = Arc::new(docs_chat);
        let sync_states: Arc<RwLock<HashMap<ConversationId, GroupSyncState>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let event_rx = Arc::new(RwLock::new(None::<broadcast::Receiver<a3net_chatstore::MessageEvent>>));

        // Spawn background sync task
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        let sync_states_clone = sync_states.clone();
        let storage_clone = storage.clone();
        let bus_clone = bus.clone();
        let docs_chat_clone = docs_chat.clone();

        // Spawn the background task
        tokio::spawn(async move {
            let mut tick_interval = interval(SYNC_TICK_INTERVAL);
            tick_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        info!("GroupSyncService background task shutting down");
                        break;
                    }
                    _ = tick_interval.tick() => {
                        Self::periodic_sync_check(
                            &docs_chat_clone,
                            &sync_states_clone,
                            &storage_clone,
                            &bus_clone,
                        ).await;
                    }
                }
            }
        });

        Self {
            storage,
            docs_chat,
            sync_states,
            bus,
            event_rx,
            _background_handle: Arc::new(shutdown_tx),
        }
    }

    /// Periodic check for new messages in all joined groups.
    async fn periodic_sync_check(
        docs_chat: &Arc<a3net_chatstore::IrohDocsChat>,
        sync_states: &Arc<RwLock<HashMap<ConversationId, GroupSyncState>>>,
        storage: &ChatStorage,
        bus: &crate::notification_bus::NotificationBus,
    ) {
        let states = sync_states.read().await;
        for (conv_id, state) in states.iter() {
            if !state.is_subscribed {
                continue;
            }

            let last_seq = state.last_processed_seq;
            let conv_id_str = conv_id.as_str();

            // Fetch messages after our last processed sequence
            match docs_chat.get_messages(conv_id_str, Some(last_seq), BACKFILL_BATCH_SIZE).await {
                Ok(messages) => {
                    if messages.is_empty() {
                        continue;
                    }

                    debug!(
                        conv = %conv_id,
                        count = messages.len(),
                        last_seq,
                        "backfilling messages from iroh"
                    );

                    let mut mutable_states = sync_states.write().await;

                    for msg in messages {
                        if let Some(seq) = msg.sequence {
                            if seq <= last_seq {
                                continue;
                            }

                            // Convert and write to SQLite
                            if let Err(e) = Self::write_message_to_sqlite(storage, conv_id, &msg, bus).await {
                                warn!(conv = %conv_id, seq, "failed to backfill message: {e}");
                            } else if let Some(state) = mutable_states.get_mut(conv_id) {
                                state.last_processed_seq = seq;
                                state.last_sync_at = Some(Utc::now());
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(conv = %conv_id, "iroh get_messages failed: {e}");
                }
            }
        }
    }

    /// Write a message from iroh to SQLite with deduplication.
    async fn write_message_to_sqlite(
        storage: &ChatStorage,
        conversation_id: &ConversationId,
        im_msg: &a3net_chatstore::im::Message,
        bus: &crate::notification_bus::NotificationBus,
    ) -> AppResult<()> {
        // Convert iroh Message to ChatMessage
        let edited_at = im_msg.edited_at.as_ref().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.with_timezone(&chrono::Utc))
        });

        // SECURITY: Extract actual sender from the message, not hardcoded "system"
        let actual_sender = UserId::from(im_msg.sender_id.as_str());

        let chat_msg = a3chat_core::message::ChatMessage {
            message_id: a3chat_core::id::MessageId::from(im_msg.id.as_str()),
            conversation_id: conversation_id.clone(),
            sender_id: actual_sender.clone(),
            receiver_id: im_msg.receiver_id.as_ref().map(|r| UserId::from(r.as_str())).unwrap_or_else(|| UserId::from("")),
            message_type: a3chat_core::message::MessageType::Text,
            body: a3chat_core::message::MessageBody::Plain { content: im_msg.content.clone() },
            attachments: vec![],
            reply_to: im_msg.reply_to.as_ref().map(|r| a3chat_core::id::MessageId::from(r.as_str())),
            sequence: im_msg.sequence.unwrap_or(0),
            timestamp: im_msg.timestamp.timestamp(),
            read_at: None,
            is_edited: im_msg.is_edited,
            edited_at,
            integrity_hash: im_msg.integrity_hash.clone(),
            recalled_at: None,
        };

        // SECURITY: Use actual sender for deduplication check
        let existing = storage.get_message(&actual_sender, &chat_msg.message_id).await?;
        if existing.is_some() {
            debug!(msg_id = %chat_msg.message_id, "message already exists, skipping");
            return Ok(());
        }

        // Store the message using record_inbound with actual sender
        storage.record_inbound(&actual_sender, &chat_msg).await?;

        // Publish notification for SSE subscribers with actual sender
        bus.publish(A3chatEvent::ChatMessageReceived {
            user_id: actual_sender,
            conversation_id: conversation_id.clone(),
            message: chat_msg,
        });

        Ok(())
    }

    /// Join a group sync session by importing the group's DocTicket.
    ///
    /// This opens the conversation doc in iroh-docs and starts syncing.
    pub async fn join_group(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        ticket: a3net_chatstore::ConversationTicket,
    ) -> AppResult<()> {
        let conv_id_str = conversation_id.as_str();

        // Open the doc with the ticket
        self.docs_chat
            .open_with_ticket(conv_id_str, ticket)
            .await
            .map_err(|e| AppError::Internal(format!("failed to open group doc: {e}")))?;

        // Initialize sync state
        {
            let mut states = self.sync_states.write().await;
            states.insert(conversation_id.clone(), GroupSyncState::new(conversation_id.clone()));
        }

        // Get last processed sequence from local SQLite by listing messages
        let messages = self.storage.list_messages(owner, conversation_id, 1).await?;
        let last_local_seq = messages.first().map(|m| m.sequence).unwrap_or(0);

        // Update state with local sequence
        {
            let mut states = self.sync_states.write().await;
            if let Some(state) = states.get_mut(conversation_id) {
                state.last_processed_seq = last_local_seq;
                state.is_subscribed = true;
            }
        }

        info!(conv = %conversation_id, last_seq = last_local_seq, "joined group sync");
        Ok(())
    }

    /// Leave a group sync session.
    ///
    /// This stops syncing for the group but does not delete local messages.
    pub async fn leave_group(&self, conversation_id: &ConversationId) -> AppResult<()> {
        let mut states = self.sync_states.write().await;
        if let Some(state) = states.remove(conversation_id) {
            info!(conv = %conversation_id, last_seq = state.last_processed_seq, "left group sync");
        }
        Ok(())
    }

    /// Get the shareable ticket for a group conversation.
    ///
    /// New members can use this ticket to join the group's sync network.
    pub async fn get_group_ticket(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<a3net_chatstore::ConversationTicket> {
        let conv_id_str = conversation_id.as_str();
        let mode = iroh_docs::api::protocol::ShareMode::Write;

        self.docs_chat
            .share(conv_id_str, mode)
            .await
            .map_err(|e| AppError::Internal(format!("failed to share group doc: {e}")))
    }

    /// Force a sync for a specific group.
    ///
    /// This is useful when the user wants to manually trigger a sync.
    pub async fn sync_group(&self, conversation_id: &ConversationId) -> AppResult<u32> {
        let states = self.sync_states.read().await;
        let state = states
            .get(conversation_id)
            .ok_or_else(|| AppError::Internal(format!("not synced to group {conversation_id}")))?;

        let last_seq = state.last_processed_seq;
        let conv_id_str = conversation_id.as_str();

        drop(states);

        // Fetch new messages
        let messages = self.docs_chat
            .get_messages(conv_id_str, Some(last_seq), BACKFILL_BATCH_SIZE)
            .await
            .map_err(|e| AppError::Internal(format!("get_messages failed: {e}")))?;

        let count = messages.len();

        // Extract last message before consuming in loop
        let last_msg = messages.last().cloned();

        // Process messages
        for msg in messages {
            if let Some(seq) = msg.sequence {
                if seq <= last_seq {
                    continue;
                }

                if let Err(e) = Self::write_message_to_sqlite(&self.storage, conversation_id, &msg, &self.bus).await {
                    warn!(conv = %conversation_id, seq, "failed to sync message: {e}");
                }
            }
        }

        // Update state
        if let Some(ref last_msg) = last_msg {
            if let Some(seq) = last_msg.sequence {
                let mut states = self.sync_states.write().await;
                if let Some(state) = states.get_mut(conversation_id) {
                    state.last_processed_seq = seq;
                    state.last_sync_at = Some(Utc::now());
                }
            }
        }

        info!(conv = %conversation_id, count, "manual sync completed");
        Ok(count as u32)
    }

    /// Get sync status for a group.
    pub async fn get_sync_status(&self, conversation_id: &ConversationId) -> AppResult<GroupSyncStatus> {
        let states = self.sync_states.read().await;
        let state = states
            .get(conversation_id)
            .ok_or_else(|| AppError::Internal(format!("not synced to group {conversation_id}")))?;

        Ok(GroupSyncStatus {
            conversation_id: conversation_id.clone(),
            is_subscribed: state.is_subscribed,
            last_processed_seq: state.last_processed_seq,
            last_sync_at: state.last_sync_at,
        })
    }

    /// List all groups we're currently syncing.
    pub async fn list_synced_groups(&self) -> Vec<GroupSyncStatus> {
        let states = self.sync_states.read().await;
        states
            .values()
            .map(|s| GroupSyncStatus {
                conversation_id: s.conversation_id.clone(),
                is_subscribed: s.is_subscribed,
                last_processed_seq: s.last_processed_seq,
                last_sync_at: s.last_sync_at,
            })
            .collect()
    }
}

/// Sync status for a group conversation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GroupSyncStatus {
    pub conversation_id: ConversationId,
    pub is_subscribed: bool,
    pub last_processed_seq: u32,
    pub last_sync_at: Option<DateTime<Utc>>,
}
