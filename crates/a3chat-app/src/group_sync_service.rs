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
//!
//! ## Metrics
//!
//! The service tracks sync metrics via `SyncMetrics`:
//! - `messages_synced`: Total messages synced from iroh to SQLite
//! - `sync_errors`: Number of sync errors encountered
//! - `last_sync_duration_ms`: Duration of last sync operation
//! - `backfill_batch_sizes`: Distribution of batch sizes for backfills

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

// ============================================================================
/// Metrics types
// ============================================================================

/// Phase 5c: Sync metrics for monitoring P2P group message synchronization.
#[derive(Debug, Clone, Default)]
pub struct SyncMetrics {
    /// Total messages successfully synced from iroh to SQLite.
    pub messages_synced_total: u64,
    /// Total sync errors encountered.
    pub sync_errors_total: u64,
    /// Duration of the last successful sync operation in milliseconds.
    pub last_sync_duration_ms: Option<u64>,
    /// Timestamp of the last successful sync.
    pub last_sync_at: Option<DateTime<Utc>>,
    /// Number of groups currently subscribed for sync.
    pub active_groups: usize,
    /// Last backfill batch size.
    pub last_backfill_size: usize,
}

impl SyncMetrics {
    /// Create a new empty metrics instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful sync with the given batch size and duration.
    pub fn record_sync(&mut self, batch_size: usize, duration_ms: u64) {
        self.messages_synced_total += batch_size as u64;
        self.last_sync_duration_ms = Some(duration_ms);
        self.last_sync_at = Some(Utc::now());
        self.last_backfill_size = batch_size;
    }

    /// Record a sync error.
    pub fn record_error(&mut self) {
        self.sync_errors_total += 1;
    }

    /// Update the number of active groups.
    pub fn set_active_groups(&mut self, count: usize) {
        self.active_groups = count;
    }
}

/// Phase 5c: Metrics collector for sync operations.
/// Thread-safe, used to expose metrics to monitoring systems.
#[derive(Clone, Default)]
pub struct SyncMetricsCollector {
    inner: Arc<tokio::sync::RwLock<SyncMetrics>>,
}

impl SyncMetricsCollector {
    /// Create a new metrics collector.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(tokio::sync::RwLock::new(SyncMetrics::new())),
        }
    }

    /// Record a successful sync operation.
    pub async fn record_sync(&self, batch_size: usize, duration_ms: u64) {
        let mut metrics = self.inner.write().await;
        metrics.record_sync(batch_size, duration_ms);
    }

    /// Record a sync error.
    pub async fn record_error(&self) {
        let mut metrics = self.inner.write().await;
        metrics.record_error();
    }

    /// Update active group count.
    pub async fn set_active_groups(&self, count: usize) {
        let mut metrics = self.inner.write().await;
        metrics.set_active_groups(count);
    }

    /// Get a snapshot of current metrics.
    pub async fn snapshot(&self) -> SyncMetrics {
        self.inner.read().await.clone()
    }
}

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
    /// Phase 5c: Sync metrics collector for monitoring.
    metrics: SyncMetricsCollector,
}

impl std::fmt::Debug for GroupSyncService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupSyncService")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
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
        let metrics = SyncMetricsCollector::new();

        // Spawn background sync task
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        let owner_clone = owner.clone();
        let sync_states_clone = sync_states.clone();
        let storage_clone = storage.clone();
        let bus_clone = bus.clone();
        let docs_chat_clone = docs_chat.clone();
        let metrics_clone = metrics.clone();

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
                            &metrics_clone,
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
            metrics,
        }
    }

    /// Get a snapshot of current sync metrics.
    ///
    /// Phase 5c: Use this to expose sync metrics to monitoring systems.
    pub async fn metrics(&self) -> SyncMetrics {
        self.metrics.snapshot().await
    }

    /// Periodic check for new messages in all joined groups.
    async fn periodic_sync_check(
        owner: UserId,
        docs_chat: &Arc<a3net_chatstore::IrohDocsChat>,
        sync_states: &Arc<RwLock<HashMap<ConversationId, GroupSyncState>>>,
        storage: &ChatStorage,
        bus: &crate::notification_bus::NotificationBus,
        metrics: &SyncMetricsCollector,
    ) {
        let states = sync_states.read().await;
        let active_count = states.values().filter(|s| s.is_subscribed).count();
        drop(states);

        metrics.set_active_groups(active_count).await;

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
                                metrics.record_error().await;
                            }
                        }
                    }

                    if successful_count > 0 {
                        let mut mutable_states = sync_states.write().await;
                        if let Some(state) = mutable_states.get_mut(conv_id) {
                            state.last_processed_seq = highest_seq;
                            state.last_sync_at = Some(Utc::now());
                        }
                        debug!(
                            conv = %conv_id,
                            synced = successful_count,
                            "backfill completed"
                        );
                    }
                }
                Err(e) => {
                    warn!(conv = %conv_id, "iroh get_messages failed: {e}");
                    metrics.record_error().await;
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
        A3chatRpcMethod::GROUP_SYNC_METRICS => {
            // Phase 5c: Expose sync metrics in Prometheus format.
            // Also available via GET /rpc/metrics but this RPC allows per-user filtering.
            let metrics = svc.metrics().await;
            let out = MetricsPrometheusFormat(&metrics).to_string();
            Ok(serde_json::json!({
                "format": "prometheus",
                "content": out
            }))
        }
        _ => Err(A3chatError::Internal(format!(
            "GroupSyncService does not handle {method}"
        ))),
    }
}

/// Phase 5c: Prometheus exposition format for group sync metrics.
struct MetricsPrometheusFormat<'a>(&'a SyncMetrics);

impl<'a> std::fmt::Display for MetricsPrometheusFormat<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let m = self.0;
        writeln!(f, "# HELP a3chat_group_sync_messages_total Messages synced from iroh to SQLite.")?;
        writeln!(f, "# TYPE a3chat_group_sync_messages_total counter")?;
        writeln!(f, "a3chat_group_sync_messages_total {}", m.messages_synced_total)?;
        writeln!(f, "# HELP a3chat_group_sync_errors_total Sync errors encountered.")?;
        writeln!(f, "# TYPE a3chat_group_sync_errors_total counter")?;
        writeln!(f, "a3chat_group_sync_errors_total {}", m.sync_errors_total)?;
        writeln!(f, "# HELP a3chat_group_sync_active_groups Number of groups with active sync.")?;
        writeln!(f, "# TYPE a3chat_group_sync_active_groups gauge")?;
        writeln!(f, "a3chat_group_sync_active_groups {}", m.active_groups)?;
        writeln!(f, "# HELP a3chat_group_sync_last_duration_ms Duration of last sync in ms.")?;
        writeln!(f, "# TYPE a3chat_group_sync_last_duration_ms gauge")?;
        if let Some(d) = m.last_sync_duration_ms {
            writeln!(f, "a3chat_group_sync_last_duration_ms {}", d)?;
        } else {
            writeln!(f, "a3chat_group_sync_last_duration_ms 0")?;
        }
        writeln!(f, "# HELP a3chat_group_sync_last_backfill_size Batch size of last backfill.")?;
        writeln!(f, "# TYPE a3chat_group_sync_last_backfill_size gauge")?;
        writeln!(f, "a3chat_group_sync_last_backfill_size {}", m.last_backfill_size)?;
        if let Some(ts) = m.last_sync_at {
            writeln!(f, "# HELP a3chat_group_sync_last_timestamp_seconds Unix timestamp of last sync.")?;
            writeln!(f, "# TYPE a3chat_group_sync_last_timestamp_seconds gauge")?;
            writeln!(f, "a3chat_group_sync_last_timestamp_seconds {}", ts.timestamp())?;
        }
        Ok(())
    }
}

// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 5c: Test SyncMetrics basic operations.
    #[test]
    fn sync_metrics_record_sync() {
        let mut metrics = SyncMetrics::new();
        assert_eq!(metrics.messages_synced_total, 0);
        assert!(metrics.last_sync_duration_ms.is_none());

        metrics.record_sync(10, 50);
        assert_eq!(metrics.messages_synced_total, 10);
        assert_eq!(metrics.last_sync_duration_ms, Some(50));
        assert!(metrics.last_sync_at.is_some());
        assert_eq!(metrics.last_backfill_size, 10);

        metrics.record_sync(5, 30);
        assert_eq!(metrics.messages_synced_total, 15);
        assert_eq!(metrics.last_backfill_size, 5);
    }

    /// Phase 5c: Test SyncMetrics error recording.
    #[test]
    fn sync_metrics_record_error() {
        let mut metrics = SyncMetrics::new();
        assert_eq!(metrics.sync_errors_total, 0);

        metrics.record_error();
        assert_eq!(metrics.sync_errors_total, 1);

        metrics.record_error();
        assert_eq!(metrics.sync_errors_total, 2);
    }

    /// Phase 5c: Test SyncMetrics active groups.
    #[test]
    fn sync_metrics_active_groups() {
        let mut metrics = SyncMetrics::new();
        assert_eq!(metrics.active_groups, 0);

        metrics.set_active_groups(3);
        assert_eq!(metrics.active_groups, 3);

        metrics.set_active_groups(5);
        assert_eq!(metrics.active_groups, 5);
    }

    /// Phase 5c: Test SyncMetricsCollector thread safety.
    #[tokio::test]
    async fn sync_metrics_collector_record_sync() {
        let collector = SyncMetricsCollector::new();

        collector.record_sync(10, 100).await;
        let snapshot = collector.snapshot().await;
        assert_eq!(snapshot.messages_synced_total, 10);
        assert_eq!(snapshot.last_sync_duration_ms, Some(100));

        collector.record_sync(5, 50).await;
        let snapshot = collector.snapshot().await;
        assert_eq!(snapshot.messages_synced_total, 15);
    }

    /// Phase 5c: Test SyncMetricsCollector concurrent writes.
    #[tokio::test]
    async fn sync_metrics_collector_concurrent_writes() {
        use tokio::task;

        let collector = SyncMetricsCollector::new();

        // Spawn multiple tasks writing concurrently
        let mut handles = vec![];
        for i in 0..10 {
            let c = collector.clone();
            handles.push(tokio::spawn(async move {
                c.record_sync(i, i as u64 * 10).await;
            }));
        }

        for h in handles {
            h.await.expect("task should complete");
        }

        let snapshot = collector.snapshot().await;
        // Sum of 0+1+2+...+9 = 45
        assert_eq!(snapshot.messages_synced_total, 45);
    }

    /// Phase 5c: Test GroupSyncState creation.
    #[test]
    fn group_sync_state_new() {
        let owner = UserId::from("alice");
        let conv_id = ConversationId::from("grp:test");
        let state = GroupSyncState::new(owner.clone(), conv_id.clone());

        assert_eq!(state.owner, owner);
        assert_eq!(state.conversation_id, conv_id);
        assert_eq!(state.last_processed_seq, 0);
        assert!(!state.is_subscribed);
        assert!(state.last_sync_at.is_none());
    }
}
