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
//! 3. **Periodic Backfill**: The background task periodically fetches new
//!    messages from iroh and backfills them into SQLite.
//! 4. **SQLite Backfill**: The sync service writes received messages back
//!    to SQLite with proper deduplication.
//! 5. **Bus Notification**: Once in SQLite, a `ChatMessageReceived` event
//!    is published for SSE subscribers.
//!
//! ## Key invariants
//!
//! - `owner` (the local device) is used for all SQLite operations because
//!   SQLite rows are owned by the local user.
//! - `sender_id` inside the `ChatMessage` reflects the actual message author
//!   from the iroh doc.
//! - Bus events use `owner` as the `user_id` so the correct recipient is
//!   notified.

#[cfg(feature = "iroh")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use a3chat_core::event::A3chatEvent;
use a3chat_core::id::{ConversationId, UserId};
use chrono::{DateTime, Utc};
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
    /// Local device owner — used for SQLite operations on this device.
    owner: UserId,
    conversation_id: ConversationId,
    /// Last sequence we've processed from this group.
    last_processed_seq: u32,
    /// Whether we have an active subscription.
    is_subscribed: bool,
    /// Last time we checked for new messages.
    last_sync_at: Option<DateTime<Utc>>,
}

impl GroupSyncState {
    fn new(owner: UserId, conversation_id: ConversationId) -> Self {
        Self {
            owner,
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
    /// Local device owner — used for all SQLite operations.
    owner: UserId,
    storage: ChatStorage,
    /// The shared iroh-docs chat bridge.
    docs_chat: Arc<a3net_chatstore::IrohDocsChat>,
    /// Per-conversation sync state.
    sync_states: Arc<RwLock<HashMap<ConversationId, GroupSyncState>>>,
    /// Event bus for sync notifications.
    bus: crate::notification_bus::NotificationBus,
    /// Background task handle.
    _background_handle: Arc<tokio::sync::oneshot::Sender<()>>,
}

impl std::fmt::Debug for GroupSyncService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupSyncService")
            .field("owner", &self.owner)
            .finish()
    }
}

impl GroupSyncService {
    /// Create a new group sync service.
    ///
    /// `owner` is the local device's UserId, used for all SQLite operations
    /// since rows are always owned by the local user.
    pub fn new(
        owner: UserId,
        storage: ChatStorage,
        docs_chat: a3net_chatstore::IrohDocsChat,
        bus: crate::notification_bus::NotificationBus,
    ) -> Self {
        let docs_chat = Arc::new(docs_chat);
        let sync_states: Arc<RwLock<HashMap<ConversationId, GroupSyncState>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Spawn background sync task
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        let owner_clone = owner.clone();
        let sync_states_clone = sync_states.clone();
        let storage_clone = storage.clone();
        let bus_clone = bus.clone();
        let docs_chat_clone = docs_chat.clone();

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
                            owner_clone.clone(),
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
            owner,
            storage,
            docs_chat,
            sync_states,
            bus,
            _background_handle: Arc::new(shutdown_tx),
        }
    }

    /// Periodic check for new messages in all joined groups.
    async fn periodic_sync_check(
        owner: UserId,
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

                    let mut highest_seq = last_seq;
                    let mut successful_count = 0u32;

                    for msg in messages {
                        let Some(seq) = msg.sequence else { continue };
                        if seq <= last_seq {
                            continue;
                        }

                        match Self::write_message_to_sqlite(
                            &owner,
                            storage,
                            conv_id,
                            &msg,
                            bus,
                        ).await {
                            Ok(()) => {
                                highest_seq = seq;
                                successful_count += 1;
                            }
                            Err(e) => {
                                warn!(conv = %conv_id, seq, "failed to backfill message: {e}");
                            }
                        }
                    }

                    if successful_count > 0 {
                        let mut mutable_states = sync_states.write().await;
                        if let Some(state) = mutable_states.get_mut(conv_id) {
                            state.last_processed_seq = highest_seq;
                            state.last_sync_at = Some(Utc::now());
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
    ///
    /// `owner` is the local device — used for SQLite row operations and
    /// bus notifications. `actual_sender` is placed inside `ChatMessage.sender_id`
    /// to correctly attribute authorship.
    async fn write_message_to_sqlite(
        owner: &UserId,
        storage: &ChatStorage,
        conversation_id: &ConversationId,
        im_msg: &a3net_chatstore::im::Message,
        bus: &crate::notification_bus::NotificationBus,
    ) -> AppResult<()> {
        let edited_at = im_msg.edited_at.as_ref().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.with_timezone(&chrono::Utc))
        });

        let actual_sender = UserId::from(im_msg.sender_id.as_str());

        let chat_msg = a3chat_core::message::ChatMessage {
            message_id: a3chat_core::id::MessageId::from(im_msg.id.as_str()),
            conversation_id: conversation_id.clone(),
            // Correct authorship inside the message.
            sender_id: actual_sender.clone(),
            receiver_id: im_msg
                .receiver_id
                .as_ref()
                .map(|r| UserId::from(r.as_str()))
                .unwrap_or_else(|| UserId::from("")),
            message_type: a3chat_core::message::MessageType::Text,
            body: a3chat_core::message::MessageBody::Plain {
                content: im_msg.content.clone(),
            },
            attachments: vec![],
            reply_to: im_msg
                .reply_to
                .as_ref()
                .map(|r| a3chat_core::id::MessageId::from(r.as_str())),
            sequence: im_msg.sequence.unwrap_or(0),
            timestamp: im_msg.timestamp.timestamp(),
            read_at: None,
            is_edited: im_msg.is_edited,
            edited_at,
            integrity_hash: im_msg.integrity_hash.clone(),
            recalled_at: None,
        };

        // Deduplication: SQLite rows are keyed by (owner, msg_id).
        let existing = storage.get_message(owner, &chat_msg.message_id).await?;
        if existing.is_some() {
            debug!(msg_id = %chat_msg.message_id, "message already exists, skipping");
            return Ok(());
        }

        // SQLite insert: rows are owned by the local device.
        storage.record_inbound(owner, &chat_msg).await?;

        // Bus: notify the local user that they received a message in this group.
        // `actual_sender` (the author) goes inside ChatMessage; `owner` (the
        // recipient on this device) is the SSE subscriber key.
        bus.publish(A3chatEvent::ChatMessageReceived {
            user_id: owner.clone(),
            conversation_id: conversation_id.clone(),
            message: chat_msg,
        });

        Ok(())
    }

    /// Join a group sync session by importing the group's DocTicket.
    ///
    /// This opens the conversation doc in iroh-docs and starts the periodic
    /// backfill loop. Existing messages are NOT replayed here — the periodic
    /// tick will pick them up on the next interval.
    pub async fn join_group(
        &self,
        conversation_id: &ConversationId,
        ticket: iroh_docs::DocTicket,
    ) -> AppResult<()> {
        let conv_id_str = conversation_id.as_str();

        self.docs_chat
            .open_with_ticket(conv_id_str, ticket)
            .await
            .map_err(|e| AppError::Internal(format!("failed to open group doc: {e}")))?;

        // Get the last sequence we've already persisted locally so we don't
        // re-process history on reconnect.
        let messages = self
            .storage
            .list_messages(&self.owner, conversation_id, 1)
            .await?;
        let last_local_seq = messages.first().map(|m| m.sequence).unwrap_or(0);

        let mut states = self.sync_states.write().await;
        states.insert(
            conversation_id.clone(),
            GroupSyncState::new(self.owner.clone(), conversation_id.clone()),
        );
        if let Some(state) = states.get_mut(conversation_id) {
            state.last_processed_seq = last_local_seq;
            state.is_subscribed = true;
        }

        info!(
            conv = %conversation_id,
            last_seq = last_local_seq,
            "joined group sync"
        );
        Ok(())
    }

    /// Leave a group sync session.
    ///
    /// This stops syncing for the group but does not delete local messages.
    pub async fn leave_group(&self, conversation_id: &ConversationId) -> AppResult<()> {
        let mut states = self.sync_states.write().await;
        if let Some(state) = states.remove(conversation_id) {
            info!(
                conv = %conversation_id,
                last_seq = state.last_processed_seq,
                "left group sync"
            );
        }
        Ok(())
    }

    /// Get the shareable ticket for a group conversation.
    ///
    /// New members can use this ticket to join the group's sync network.
    /// The returned ticket is already JSON+base64 encoded for wire transmission.
    pub async fn get_group_ticket(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<String> {
        use base64::Engine;
        let conv_id_str = conversation_id.as_str();
        let mode = iroh_docs::api::protocol::ShareMode::Write;

        let ticket = self
            .docs_chat
            .share(conv_id_str, mode)
            .await
            .map_err(|e| AppError::Internal(format!("failed to share group doc: {e}")))?;

        let json = serde_json::to_string(&ticket)
            .map_err(|e| AppError::Internal(format!("ticket serialization failed: {e}")))?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&json))
    }

    /// Force an immediate sync for a specific group.
    ///
    /// Useful when the user wants to manually trigger a sync or after
    /// coming back online.
    pub async fn sync_group(&self, conversation_id: &ConversationId) -> AppResult<u32> {
        let (last_seq, conv_id_str) = {
            let states = self.sync_states.read().await;
            let state = states
                .get(conversation_id)
                .ok_or_else(|| {
                    AppError::Internal(format!("not synced to group {conversation_id}"))
                })?;
            (state.last_processed_seq, conversation_id.as_str().to_string())
        };

        let messages = self
            .docs_chat
            .get_messages(&conv_id_str, Some(last_seq), BACKFILL_BATCH_SIZE)
            .await
            .map_err(|e| AppError::Internal(format!("get_messages failed: {e}")))?;

        let count = messages.len();

        let mut highest_seq = last_seq;
        for msg in messages {
            let Some(seq) = msg.sequence else { continue };
            if seq <= last_seq {
                continue;
            }

            if let Err(e) = Self::write_message_to_sqlite(
                &self.owner,
                &self.storage,
                conversation_id,
                &msg,
                &self.bus,
            ).await {
                warn!(conv = %conversation_id, seq, "failed to sync message: {e}");
            } else {
                highest_seq = seq;
            }
        }

        {
            let mut states = self.sync_states.write().await;
            if let Some(state) = states.get_mut(conversation_id) {
                state.last_processed_seq = highest_seq;
                state.last_sync_at = Some(Utc::now());
            }
        }

        info!(conv = %conversation_id, count, "manual sync completed");
        Ok(count as u32)
    }

    /// Get sync status for a group.
    pub async fn get_sync_status(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<GroupSyncStatus> {
        let states = self.sync_states.read().await;
        let state = states
            .get(conversation_id)
            .ok_or_else(|| {
                AppError::Internal(format!("not synced to group {conversation_id}"))
            })?;

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

/// Sync status for a single group conversation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GroupSyncStatus {
    pub conversation_id: ConversationId,
    pub is_subscribed: bool,
    pub last_processed_seq: u32,
    pub last_sync_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// RPC dispatch
// ---------------------------------------------------------------------------

use a3chat_core::error::A3chatError;
use a3chat_core::rpc::A3chatRpcMethod;

/// Route `a3chat.group.sync.*` RPC methods to [`GroupSyncService`].
pub async fn dispatch(
    svc: Arc<GroupSyncService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        A3chatRpcMethod::GROUP_SYNC_JOIN => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct JoinParams {
                conversation_id: ConversationId,
                ticket: String,
            }
            let p: JoinParams = serde_json::from_value(params)
                .map_err(|e| A3chatError::InvalidInput(format!("join params: {e}")))?;

            // Decode the base64-encoded DocTicket JSON.
            use base64::Engine;
            let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&p.ticket)
                .map_err(|e| A3chatError::InvalidInput(format!("ticket base64: {e}")))?;
            let ticket: iroh_docs::DocTicket = serde_json::from_slice(&json)
                .map_err(|e| A3chatError::InvalidInput(format!("ticket JSON: {e}")))?;

            svc.join_group(&p.conversation_id, ticket).await.map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::GROUP_SYNC_LEAVE => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct LeaveParams {
                conversation_id: ConversationId,
            }
            let p: LeaveParams = serde_json::from_value(params)
                .map_err(|e| A3chatError::InvalidInput(format!("leave params: {e}")))?;
            svc.leave_group(&p.conversation_id).await.map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::GROUP_SYNC_FORCE => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct ForceParams {
                conversation_id: ConversationId,
            }
            let p: ForceParams = serde_json::from_value(params)
                .map_err(|e| A3chatError::InvalidInput(format!("force params: {e}")))?;
            let count = svc.sync_group(&p.conversation_id).await.map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "synced": count }))
        }
        A3chatRpcMethod::GROUP_SYNC_STATUS => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct StatusParams {
                conversation_id: ConversationId,
            }
            let p: StatusParams = serde_json::from_value(params)
                .map_err(|e| A3chatError::InvalidInput(format!("status params: {e}")))?;
            let status = svc.get_sync_status(&p.conversation_id).await.map_err(A3chatError::from)?;
            serde_json::to_value(&status).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_SYNC_LIST => {
            let groups = svc.list_synced_groups().await;
            serde_json::to_value(&groups).map_err(A3chatError::from)
        }
        _ => Err(A3chatError::Internal(format!(
            "GroupSyncService does not handle {method}"
        ))),
    }
}
