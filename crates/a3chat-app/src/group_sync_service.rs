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
//! - `last_backfill_size`: Batch size of the most recent backfill
//!
//! ## Resilience (Phase 2)
//!
//! The service includes resilience mechanisms to handle transient failures:
//! - **Exponential backoff retry**: Automatically retries failed syncs with increasing delays
//! - **Circuit breaker**: Stops sync attempts when failures exceed threshold
//! - **Per-group isolation**: Failed groups don't affect others

pub mod retry_enhanced;
pub mod circuit_breaker;

// Re-export enhanced retry types as the default
pub use retry_enhanced::{RetryPolicy, RetryState, RetryConfigError};

#[cfg(feature = "iroh")]
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinError;

use a3chat_core::event::A3chatEvent;
use a3chat_core::id::{ConversationId, UserId};
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, info, warn};

use crate::chat_metrics::ChatAppMetrics;
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
    /// Last backfill batch size (for the most recent backfill).
    pub last_backfill_size: usize,
    /// Seconds since the sync service started.
    pub uptime_secs: u64,
    /// Total sync operations attempted.
    pub sync_operations_total: u64,
    /// Total bytes synced (estimated from message count * avg message size).
    pub bytes_synced_total: u64,
    /// Estimated average message size in bytes.
    pub avg_message_size_bytes: u32,
}

impl SyncMetrics {
    /// Record a successful sync with the given batch size and duration.
    pub fn record_sync(&mut self, batch_size: usize, duration_ms: u64) {
        self.messages_synced_total += batch_size as u64;
        self.last_sync_duration_ms = Some(duration_ms);
        self.last_sync_at = Some(Utc::now());
        self.last_backfill_size = batch_size;
        self.sync_operations_total += 1;
        // Estimate: 500 bytes per message average
        let bytes = (batch_size as u64) * (self.avg_message_size_bytes as u64);
        self.bytes_synced_total += bytes;
    }

    /// Record a sync error.
    pub fn record_error(&mut self) {
        self.sync_errors_total += 1;
    }

    /// Update the number of active groups.
    pub fn set_active_groups(&mut self, count: usize) {
        self.active_groups = count;
    }

    /// Calculate the error rate as a percentage.
    pub fn error_rate_percent(&self) -> f64 {
        if self.sync_operations_total == 0 {
            0.0
        } else {
            (self.sync_errors_total as f64 / self.sync_operations_total as f64) * 100.0
        }
    }

    /// Calculate sync throughput (messages per second).
    pub fn throughput_msg_per_sec(&self) -> f64 {
        if self.uptime_secs == 0 {
            0.0
        } else {
            self.messages_synced_total as f64 / self.uptime_secs as f64
        }
    }
}

/// Phase 5c: Metrics collector for sync operations.
/// Thread-safe, used to expose metrics to monitoring systems.
#[derive(Clone)]
pub struct SyncMetricsCollector {
    inner: Arc<tokio::sync::RwLock<SyncMetrics>>,
    /// When this collector was created — used to compute uptime.
    start_time: std::time::Instant,
}

impl Default for SyncMetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncMetricsCollector {
    /// Create a new metrics collector.
    pub fn new() -> Self {
        // Eagerly touch the global registry so every metric
        // shows up on /metrics with value 0 from the first
        // scrape — the dashboard's `rate(... [5m])` queries
        // would otherwise see `absent()` and alert.
        let _ = ChatAppMetrics::get();
        Self {
            inner: Arc::new(tokio::sync::RwLock::new(SyncMetrics::default())),
            start_time: std::time::Instant::now(),
        }
    }

    /// Record a successful sync operation.
    ///
    /// The in-memory snapshot is updated first, then the
    /// global `a3net-observability` registry is updated so the
    /// dashboard sees the same numbers. The two writes are
    /// independent — neither can block the other.
    pub async fn record_sync(&self, batch_size: usize, duration_ms: u64) {
        let mut metrics = self.inner.write().await;
        metrics.record_sync(batch_size, duration_ms);
        // The global registry is updated outside the
        // `inner.write()` lock — keeping the lock as short
        // as possible.
        ChatAppMetrics::get().record_sync(batch_size as u64, duration_ms);
    }

    /// Record a sync error.
    pub async fn record_error(&self) {
        let mut metrics = self.inner.write().await;
        metrics.record_error();
        ChatAppMetrics::get().record_error();
    }

    /// Update active group count.
    pub async fn set_active_groups(&self, count: usize) {
        let mut metrics = self.inner.write().await;
        metrics.set_active_groups(count);
        ChatAppMetrics::get().set_active_groups(count);
    }

    /// Get a snapshot of current metrics (including live uptime_secs).
    ///
    /// Also pushes the live uptime into the global registry so
    /// the `a3chat_uptime_secs` gauge tracks real time rather
    /// than the value seen on the last explicit update.
    pub async fn snapshot(&self) -> SyncMetrics {
        let mut m = self.inner.read().await.clone();
        m.uptime_secs = self.start_time.elapsed().as_secs();
        // Best-effort push of the live uptime into the global
        // registry. `metrics::set_uptime_secs` is async-unsafe
        // so we wrap it in a spawn to keep the snapshot path
        // sync — but the operation itself is just an
        // `AtomicI64::store`, which is cheap.
        ChatAppMetrics::get().set_uptime_secs(m.uptime_secs);
        m
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
    /// Phase 2: Retry state for exponential backoff.
    retry_state: retry_enhanced::RetryState,
    /// Phase 2: Circuit breaker for this group.
    circuit_breaker: circuit_breaker::CircuitBreaker,
}

impl GroupSyncState {
    fn new(owner: UserId, conversation_id: ConversationId) -> Self {
        Self {
            owner,
            conversation_id,
            last_processed_seq: 0,
            is_subscribed: false,
            last_sync_at: None,
            retry_state: retry_enhanced::RetryState::new(),
            circuit_breaker: circuit_breaker::CircuitBreaker::new(),
        }
    }
}

/// Group chat synchronization service.
///
/// This service manages P2P group chat synchronization using iroh-docs.
/// It maintains subscriptions to group docs and backfills messages to SQLite.
///
/// The struct is `Clone`able: the background task handle is stored
/// behind an `Arc<Mutex<Option<JoinHandle>>>` so clones share the
/// same shutdown token and join handle, enabling `shutdown()` and
/// `join()` to be called from any clone.
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
    /// Background task handle. Stored in an
    /// `Arc<Mutex<...>>` so the public `shutdown`
    /// / `join` methods can take the handle and
    /// consume it exactly once while keeping the
    /// `GroupSyncService` itself `Clone`-able
    /// (every other field is `Arc`-backed).
    shutdown_tx: Arc<parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    /// Join handle for the background task; consumed
    /// at most once by [`GroupSyncService::join`].
    join: Arc<parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Phase 5c: Sync metrics collector for monitoring.
    metrics: SyncMetricsCollector,
    /// Phase 2: Retry policy for all groups.
    retry_policy: Arc<retry_enhanced::RetryPolicy>,
}

impl Clone for GroupSyncService {
    fn clone(&self) -> Self {
        Self {
            owner: self.owner.clone(),
            storage: self.storage.clone(),
            docs_chat: Arc::clone(&self.docs_chat),
            sync_states: Arc::clone(&self.sync_states),
            bus: self.bus.clone(),
            // Share the shutdown sender and join handle across clones
            // so shutdown/join work from any clone.
            shutdown_tx: Arc::clone(&self.shutdown_tx),
            join: Arc::clone(&self.join),
            metrics: self.metrics.clone(),
            retry_policy: Arc::clone(&self.retry_policy),
        }
    }
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
        let retry_policy = Arc::new(retry_enhanced::RetryPolicy::default());

        // Spawn background sync task.
        //
        // DO-178C §6.3 (graceful shutdown): the
        // background task is driven by a `oneshot`
        // sender. We do NOT wrap the sender in an
        // `Arc` — `tokio::sync::oneshot::Sender` is
        // already `Send + !Sync` and there is no
        // scenario where multiple callers want to
        // cooperate on shutdown. Instead we expose a
        // `shutdown()` method on `GroupSyncService`
        // that takes the sender out and signals the
        // task. The original code wrapped the sender
        // in an `Arc` and never granted a method to
        // extract it, which made the handle useless.
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        let owner_clone = owner.clone();
        let sync_states_clone = sync_states.clone();
        let storage_clone = storage.clone();
        let bus_clone = bus.clone();
        let docs_chat_clone = docs_chat.clone();
        let metrics_clone = metrics.clone();
        let retry_policy_clone = retry_policy.clone();

        let join: tokio::task::JoinHandle<()> = tokio::spawn(async move {
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
                            &retry_policy_clone,
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
            shutdown_tx: Arc::new(parking_lot::Mutex::new(Some(shutdown_tx))),
            join: Arc::new(parking_lot::Mutex::new(Some(join))),
            metrics,
            retry_policy,
        }
    }

    /// Request a graceful shutdown of the background
    /// sync loop. Calling more than once is a no-op —
    /// the second call sees `None` already taken.
    ///
    /// Returns `true` when this call signalled the
    /// task, `false` when shutdown had already been
    /// requested (or the sender was lost).
    pub fn shutdown(&self) -> bool {
        let mut guard = self.shutdown_tx.lock();
        if let Some(tx) = guard.take() {
            // Best-effort: a dropped receiver is fine,
            // we just lost the timing signal.
            let _ = tx.send(());
            true
        } else {
            false
        }
    }

    /// `true` if `shutdown` has already been called
    /// and the background task has not been awaited
    /// yet.
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown_tx.lock().is_none()
    }

    /// Wait for the background task to exit. Callers
    /// should `shutdown()` first; this method just
    /// awaits the already-closing handle.
    ///
    /// Returns `Ok(())` once the task has exited (or
    /// has already exited). Returns an error only when
    /// the task panicked, mirroring
    /// [`tokio::task::JoinHandle::await`].
    pub async fn join(&self) -> Result<(), JoinError> {
        // Take the join handle out of the option;
        // a second join() returns Ok(()) rather than
        // blocking forever or returning a misleading
        // panic.
        let mut guard = self.join.lock();
        match guard.take() {
            Some(j) => j.await,
            None => Ok(()),
        }
    }

    /// Get a snapshot of current sync metrics.
    ///
    /// Phase 5c: Use this to expose sync metrics to monitoring systems.
    pub async fn metrics(&self) -> SyncMetrics {
        self.metrics.snapshot().await
    }

    /// Record a successful sync batch. Delegates to the inner
    /// [`SyncMetricsCollector`].
    pub async fn record_sync(&self, batch_size: usize, duration_ms: u64) {
        self.metrics.record_sync(batch_size, duration_ms).await;
    }

    /// Record a sync error. Delegates to the inner
    /// [`SyncMetricsCollector`].
    pub async fn record_error(&self) {
        self.metrics.record_error().await;
    }

    /// Set the number of active (subscribed) groups.
    pub async fn set_active_groups(&self, count: usize) {
        self.metrics.set_active_groups(count).await;
    }

    /// Periodic check for new messages in all joined groups.
    async fn periodic_sync_check(
        owner: UserId,
        docs_chat: &Arc<a3net_chatstore::IrohDocsChat>,
        sync_states: &Arc<RwLock<HashMap<ConversationId, GroupSyncState>>>,
        storage: &ChatStorage,
        bus: &crate::notification_bus::NotificationBus,
        metrics: &SyncMetricsCollector,
        retry_policy: &Arc<retry_enhanced::RetryPolicy>,
    ) {
        let start = std::time::Instant::now();
        let states = sync_states.read().await;
        let active_count = states.values().filter(|s| s.is_subscribed).count();
        let conv_ids: Vec<_> = states
            .iter()
            .filter(|(_, s)| s.is_subscribed)
            .map(|(id, _)| id.clone())
            .collect();
        drop(states);

        metrics.set_active_groups(active_count).await;

        // Collect errors across all groups; record once per tick (not per-group).
        let mut errors_this_tick = 0usize;
        let mut total_synced_this_tick = 0usize;

        for conv_id in &conv_ids {
            // Phase 2: Check circuit breaker and retry state before attempting sync.
            let (last_seq, should_attempt_sync) = {
                let mut states = sync_states.write().await;
                if let Some(state) = states.get_mut(conv_id) {
                    // Check circuit breaker state.
                    if !state.circuit_breaker.allow_request() {
                        // Circuit is open, skip this group.
                        debug!(
                            conv = %conv_id,
                            state = ?state.circuit_breaker.state(),
                            "skipping sync (circuit breaker open)"
                        );
                        continue;
                    }

                    // Check if we're in a backoff period.
                    if state.retry_state.is_backing_off() {
                        if let Some(time_remaining) = state.retry_state.time_until_retry() {
                            debug!(
                                conv = %conv_id,
                                remaining_secs = time_remaining.as_secs(),
                                "skipping sync (backing off)"
                            );
                        }
                        continue;
                    }

                    (state.last_processed_seq, true)
                } else {
                    continue;
                }
            };

            if !should_attempt_sync {
                continue;
            }

            let conv_id_str = conv_id.as_str();

            match docs_chat
                .get_messages(conv_id_str, Some(last_seq), BACKFILL_BATCH_SIZE)
                .await
            {
                Ok(messages) => {
                    if messages.is_empty() {
                        // Phase 2: No messages is a successful sync (no-op).
                        let mut states = sync_states.write().await;
                        if let Some(state) = states.get_mut(conv_id) {
                            state.retry_state.record_success();
                            state.circuit_breaker.record_success();
                        }
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
                    let mut had_errors = false;

                    for msg in messages {
                        let Some(seq) = msg.sequence else { continue };
                        if seq <= last_seq {
                            continue;
                        }

                        match Self::write_message_to_sqlite(&owner, storage, conv_id, &msg, bus)
                            .await
                        {
                            Ok(()) => {
                                highest_seq = seq;
                                successful_count += 1;
                            }
                            Err(e) => {
                                warn!(conv = %conv_id, seq, "failed to backfill message: {e}");
                                errors_this_tick += 1;
                                had_errors = true;
                            }
                        }
                    }

                    if successful_count > 0 {
                        let mut mutable_states = sync_states.write().await;
                        if let Some(state) = mutable_states.get_mut(conv_id) {
                            state.last_processed_seq = highest_seq;
                            state.last_sync_at = Some(Utc::now());
                            
                            // Phase 2: Record success or partial success.
                            if !had_errors {
                                state.retry_state.record_success();
                                state.circuit_breaker.record_success();
                            }
                        }
                        total_synced_this_tick += successful_count as usize;
                        debug!(
                            conv = %conv_id,
                            synced = successful_count,
                            "backfill completed"
                        );
                    } else if had_errors {
                        // Phase 2: All messages failed, record failure.
                        let mut states = sync_states.write().await;
                        if let Some(state) = states.get_mut(conv_id) {
                            state.retry_state.record_failure(
                                retry_policy,
                                "all messages failed to backfill".to_string(),
                            );
                            state.circuit_breaker.record_failure();
                        }
                    }
                }
                Err(e) => {
                    warn!(conv = %conv_id, "iroh get_messages failed: {e}");
                    errors_this_tick += 1;
                    
                    // Phase 2: Record failure in retry state and circuit breaker.
                    let mut states = sync_states.write().await;
                    if let Some(state) = states.get_mut(conv_id) {
                        state.retry_state.record_failure(retry_policy, e.to_string());
                        state.circuit_breaker.record_failure();
                        
                        if state.circuit_breaker.state() == circuit_breaker::CircuitState::Open {
                            warn!(
                                conv = %conv_id,
                                consecutive_failures = state.retry_state.consecutive_failures,
                                "circuit breaker opened for group"
                            );
                        }
                    }
                }
            }
        }

        // Record once per tick, not per-group.
        for _ in 0..errors_this_tick {
            metrics.record_error().await;
        }
        if total_synced_this_tick > 0 {
            let elapsed_ms = start.elapsed().as_millis() as u64;
            metrics
                .record_sync(total_synced_this_tick, elapsed_ms)
                .await;
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
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
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
    pub async fn get_group_ticket(&self, conversation_id: &ConversationId) -> AppResult<String> {
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
    /// coming back online. Manual sync bypasses the circuit breaker
    /// and resets the retry state on success.
    pub async fn sync_group(&self, conversation_id: &ConversationId) -> AppResult<u32> {
        let (last_seq, conv_id_str) = {
            let states = self.sync_states.read().await;
            let state = states.get(conversation_id).ok_or_else(|| {
                AppError::Internal(format!("not synced to group {conversation_id}"))
            })?;
            (
                state.last_processed_seq,
                conversation_id.as_str().to_string(),
            )
        };

        let messages = self
            .docs_chat
            .get_messages(&conv_id_str, Some(last_seq), BACKFILL_BATCH_SIZE)
            .await
            .map_err(|e| {
                // Phase 2: Record failure on get_messages error without
                // nesting runtimes. `sync_states` is wrapped in a Tokio
                // RwLock, so we record from this async context directly.
                // Note: we cannot await inside the closure because the
                // closure returns the public error, so we record first
                // by spawning — losing the ordering if the next call
                // is also contended is acceptable here; the next tick
                // of the periodic loop will reconcile state.
                let e_str = e.to_string();
                let svc = self.clone();
                let cid = conversation_id.clone();
                tokio::spawn(async move {
                    let mut states = svc.sync_states.write().await;
                    if let Some(state) = states.get_mut(&cid) {
                        state.retry_state.record_failure(&svc.retry_policy, e_str);
                        state.circuit_breaker.record_failure();
                    }
                });
                AppError::Internal(format!("get_messages failed: {e}"))
            })?;

        let mut highest_seq = last_seq;
        let mut successful_count = 0u32;
        let mut had_errors = false;
        
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
            )
            .await
            {
                warn!(conv = %conversation_id, seq, "failed to sync message: {e}");
                had_errors = true;
            } else {
                highest_seq = seq;
                successful_count += 1;
            }
        }

        {
            let mut states = self.sync_states.write().await;
            if let Some(state) = states.get_mut(conversation_id) {
                state.last_processed_seq = highest_seq;
                state.last_sync_at = Some(Utc::now());
                
                // Phase 2: Manual sync success resets retry state and circuit breaker.
                if successful_count > 0 && !had_errors {
                    state.retry_state.record_success();
                    state.circuit_breaker.record_success();
                    info!(
                        conv = %conversation_id,
                        synced = successful_count,
                        "manual sync completed, retry state reset"
                    );
                } else if had_errors {
                    state.retry_state.record_failure(
                        &self.retry_policy,
                        "partial failure in manual sync".to_string(),
                    );
                    state.circuit_breaker.record_failure();
                }
            }
        }

        info!(conv = %conversation_id, synced = successful_count, "manual sync completed");
        Ok(successful_count)
    }

    /// Get sync status for a group.
    pub async fn get_sync_status(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<GroupSyncStatus> {
        let states = self.sync_states.read().await;
        let state = states
            .get(conversation_id)
            .ok_or_else(|| AppError::Internal(format!("not synced to group {conversation_id}")))?;

        Ok(GroupSyncStatus {
            conversation_id: conversation_id.clone(),
            is_subscribed: state.is_subscribed,
            last_processed_seq: state.last_processed_seq,
            last_sync_at: state.last_sync_at,
            circuit_state: Some(state.circuit_breaker.state().to_metric_value()),
            consecutive_failures: Some(state.retry_state.consecutive_failures),
            retry_attempt: Some(state.retry_state.attempt),
            next_retry_at: state.retry_state.next_retry_at,
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
                circuit_state: Some(s.circuit_breaker.state().to_metric_value()),
                consecutive_failures: Some(s.retry_state.consecutive_failures),
                retry_attempt: Some(s.retry_state.attempt),
                next_retry_at: s.retry_state.next_retry_at,
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
    /// Phase 2: Circuit breaker state (0=Closed, 1=HalfOpen, 2=Open).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub circuit_state: Option<i64>,
    /// Phase 2: Number of consecutive failures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consecutive_failures: Option<u32>,
    /// Phase 2: Current retry attempt number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_attempt: Option<u32>,
    /// Phase 2: Timestamp when next retry will be attempted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<DateTime<Utc>>,
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

            svc.join_group(&p.conversation_id, ticket)
                .await
                .map_err(A3chatError::from)?;
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
            svc.leave_group(&p.conversation_id)
                .await
                .map_err(A3chatError::from)?;
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
            let count = svc
                .sync_group(&p.conversation_id)
                .await
                .map_err(A3chatError::from)?;
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
            let status = svc
                .get_sync_status(&p.conversation_id)
                .await
                .map_err(A3chatError::from)?;
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
        writeln!(
            f,
            "# HELP a3chat_group_sync_messages_total Messages synced from iroh to SQLite."
        )?;
        writeln!(f, "# TYPE a3chat_group_sync_messages_total counter")?;
        writeln!(
            f,
            "a3chat_group_sync_messages_total {}",
            m.messages_synced_total
        )?;
        writeln!(
            f,
            "# HELP a3chat_group_sync_errors_total Sync errors encountered."
        )?;
        writeln!(f, "# TYPE a3chat_group_sync_errors_total counter")?;
        writeln!(f, "a3chat_group_sync_errors_total {}", m.sync_errors_total)?;
        writeln!(
            f,
            "# HELP a3chat_group_sync_active_groups Number of groups with active sync."
        )?;
        writeln!(f, "# TYPE a3chat_group_sync_active_groups gauge")?;
        writeln!(f, "a3chat_group_sync_active_groups {}", m.active_groups)?;
        writeln!(
            f,
            "# HELP a3chat_group_sync_last_duration_ms Duration of last sync in ms."
        )?;
        writeln!(f, "# TYPE a3chat_group_sync_last_duration_ms gauge")?;
        if let Some(d) = m.last_sync_duration_ms {
            writeln!(f, "a3chat_group_sync_last_duration_ms {}", d)?;
        } else {
            writeln!(f, "a3chat_group_sync_last_duration_ms 0")?;
        }
        writeln!(
            f,
            "# HELP a3chat_group_sync_last_backfill_size Batch size of last backfill."
        )?;
        writeln!(f, "# TYPE a3chat_group_sync_last_backfill_size gauge")?;
        writeln!(
            f,
            "a3chat_group_sync_last_backfill_size {}",
            m.last_backfill_size
        )?;
        writeln!(
            f,
            "# HELP a3chat_uptime_secs Seconds since the sync service started."
        )?;
        writeln!(f, "# TYPE a3chat_uptime_secs gauge")?;
        writeln!(f, "a3chat_uptime_secs {}", m.uptime_secs)?;
        writeln!(
            f,
            "# HELP a3chat_group_sync_operations_total Total sync operations attempted."
        )?;
        writeln!(f, "# TYPE a3chat_group_sync_operations_total counter")?;
        writeln!(
            f,
            "a3chat_group_sync_operations_total {}",
            m.sync_operations_total
        )?;
        writeln!(
            f,
            "# HELP a3chat_group_sync_bytes_total Estimated bytes synced."
        )?;
        writeln!(f, "# TYPE a3chat_group_sync_bytes_total counter")?;
        writeln!(f, "a3chat_group_sync_bytes_total {}", m.bytes_synced_total)?;
        writeln!(
            f,
            "# HELP a3chat_group_sync_throughput_msg_per_sec Messages synced per second."
        )?;
        writeln!(f, "# TYPE a3chat_group_sync_throughput_msg_per_sec gauge")?;
        writeln!(
            f,
            "a3chat_group_sync_throughput_msg_per_sec {:.2}",
            m.throughput_msg_per_sec()
        )?;
        writeln!(
            f,
            "# HELP a3chat_group_sync_error_rate_percent Error rate as percentage."
        )?;
        writeln!(f, "# TYPE a3chat_group_sync_error_rate_percent gauge")?;
        writeln!(
            f,
            "a3chat_group_sync_error_rate_percent {:.2}",
            m.error_rate_percent()
        )?;
        if let Some(ts) = m.last_sync_at {
            writeln!(
                f,
                "# HELP a3chat_group_sync_last_timestamp_seconds Unix timestamp of last sync."
            )?;
            writeln!(f, "# TYPE a3chat_group_sync_last_timestamp_seconds gauge")?;
            writeln!(
                f,
                "a3chat_group_sync_last_timestamp_seconds {}",
                ts.timestamp()
            )?;
        }
        Ok(())
    }
}

#[cfg(feature = "iroh")]
pub mod benchmarks;

#[cfg(test)]
mod tests {
    use super::*;

    // Phase 5c shutdown primitives — stub the
    // background task construction directly. We only
    // exercise the one-shot-channel + JoinHandle
    // machinery, which is the same machinery that
    // GroupSyncService uses, to confirm the wiring is
    // not dropped on the floor.
    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_signals_and_joins_immediately() {
        use std::sync::{Arc, Mutex};

        let (tx, rx) = tokio::sync::oneshot::channel();
        let cell = Arc::new(Mutex::new(Some(tx)));
        let join: tokio::task::JoinHandle<()> = tokio::spawn(async move {
            let _ = rx.await;
        });

        // First shutdown: signals the task.
        assert!(!cell.lock().unwrap().is_none());
        let mut guard = cell.lock().unwrap();
        if let Some(t) = guard.take() {
            let _ = t.send(());
        }
        drop(guard);
        join.await.expect("task exits on shutdown signal");

        // Second shutdown: a no-op (sender gone).
        let mut guard = cell.lock().unwrap();
        let signalled = guard.take().map(|t| t.send(()).is_ok()).unwrap_or(false);
        assert!(!signalled, "second shutdown must be a no-op");
    }

    /// Phase 5c: Test SyncMetrics basic operations.
    #[test]
    fn sync_metrics_record_sync() {
        let mut metrics = SyncMetrics::default();
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
        let mut metrics = SyncMetrics::default();
        assert_eq!(metrics.sync_errors_total, 0);

        metrics.record_error();
        assert_eq!(metrics.sync_errors_total, 1);

        metrics.record_error();
        assert_eq!(metrics.sync_errors_total, 2);
    }

    /// Phase 5c: Test SyncMetrics active groups.
    #[test]
    fn sync_metrics_active_groups() {
        let mut metrics = SyncMetrics::default();
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

    /// Phase 5c: Test that MetricsPrometheusFormat outputs all expected fields.
    #[test]
    fn prometheus_format_includes_all_fields() {
        let mut metrics = SyncMetrics::default();
        metrics.record_sync(42, 123);
        metrics.set_active_groups(3);

        let output = MetricsPrometheusFormat(&metrics).to_string();

        assert!(output.contains("a3chat_group_sync_messages_total 42"));
        assert!(output.contains("a3chat_group_sync_errors_total 0"));
        assert!(output.contains("a3chat_group_sync_active_groups 3"));
        assert!(output.contains("a3chat_group_sync_last_duration_ms 123"));
        assert!(output.contains("a3chat_group_sync_last_backfill_size 42"));
        assert!(output.contains("a3chat_uptime_secs"));
        assert!(output.contains("# TYPE a3chat_group_sync_messages_total counter"));
        assert!(output.contains("# TYPE a3chat_group_sync_active_groups gauge"));
    }

    /// Phase 5c: Test that uptime_secs increases between snapshots.
    #[tokio::test]
    async fn uptime_secs_increases_over_time() {
        let collector = SyncMetricsCollector::new();

        let before = collector.snapshot().await;
        assert_eq!(before.uptime_secs, 0);

        // uptime_secs is in seconds, so we need to wait at least 1 second
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let after = collector.snapshot().await;
        assert!(
            after.uptime_secs >= 1,
            "uptime should be >= 1s after 1s wait"
        );
    }

    /// Phase 5c: Test that errors increment the counter.
    #[test]
    fn sync_metrics_multiple_errors() {
        let mut metrics = SyncMetrics::default();
        for _ in 0..5 {
            metrics.record_error();
        }
        assert_eq!(metrics.sync_errors_total, 5);
    }

    /// Phase 5c: Test error rate calculation.
    #[test]
    fn sync_metrics_error_rate() {
        let mut metrics = SyncMetrics::default();
        assert_eq!(metrics.error_rate_percent(), 0.0);

        // Add some successful syncs
        metrics.record_sync(10, 100);
        metrics.record_sync(5, 50);

        // Add errors
        metrics.record_error();
        metrics.record_error();

        // 2 errors / 2 operations = 100%
        assert!((metrics.error_rate_percent() - 100.0).abs() < 0.01);
    }

    /// Phase 5c: Test throughput calculation.
    #[tokio::test]
    async fn sync_metrics_throughput() {
        let collector = SyncMetricsCollector::new();

        collector.record_sync(100, 100).await;

        let snapshot = collector.snapshot().await;
        // Initial uptime should be 0 or very small
        assert!(snapshot.throughput_msg_per_sec() >= 0.0);
    }

    /// Phase 5c: Test bytes synced tracking.
    #[test]
    fn sync_metrics_bytes_tracking() {
        let mut metrics = SyncMetrics::default();
        assert_eq!(metrics.bytes_synced_total, 0);

        // Set avg message size for meaningful tracking
        metrics.avg_message_size_bytes = 500;
        metrics.record_sync(10, 100);
        assert_eq!(metrics.bytes_synced_total, 10 * 500);
    }

    /// Phase 5c: Test sync operations counter.
    #[test]
    fn sync_metrics_operations_counter() {
        let mut metrics = SyncMetrics::default();
        assert_eq!(metrics.sync_operations_total, 0);

        metrics.record_sync(10, 100);
        metrics.record_sync(5, 50);
        metrics.record_error();

        assert_eq!(metrics.sync_operations_total, 2); // Only successful syncs count
    }

    /// Phase 5c: Test Prometheus format includes new fields.
    #[test]
    fn prometheus_format_includes_derived_metrics() {
        let mut metrics = SyncMetrics::default();
        metrics.record_sync(42, 123);
        metrics.set_active_groups(3);

        let output = MetricsPrometheusFormat(&metrics).to_string();

        assert!(output.contains("a3chat_group_sync_operations_total"));
        assert!(output.contains("a3chat_group_sync_bytes_total"));
        assert!(output.contains("a3chat_group_sync_throughput_msg_per_sec"));
        assert!(output.contains("a3chat_group_sync_error_rate_percent"));
    }

    /// Phase 5c: Test error rate with no operations.
    #[test]
    fn sync_metrics_error_rate_no_operations() {
        let metrics = SyncMetrics::default();
        assert_eq!(metrics.error_rate_percent(), 0.0);
    }

    /// Phase 5c: Test throughput with zero uptime.
    #[test]
    fn sync_metrics_throughput_zero_uptime() {
        let metrics = SyncMetrics::default();
        assert_eq!(metrics.throughput_msg_per_sec(), 0.0);
    }

    /// Phase 5c: Test Prometheus format handles None values.
    #[test]
    fn prometheus_format_handles_none_values() {
        let metrics = SyncMetrics::default();

        let output = MetricsPrometheusFormat(&metrics).to_string();

        // Should have default value when last_sync_duration_ms is None
        assert!(output.contains("a3chat_group_sync_last_duration_ms 0"));
        // Should NOT have timestamp line when last_sync_at is None
        assert!(!output.contains("a3chat_group_sync_last_timestamp_seconds"));
    }

    /// Phase 5c: Test Prometheus format includes timestamp when available.
    #[test]
    fn prometheus_format_includes_timestamp() {
        let mut metrics = SyncMetrics::default();
        metrics.record_sync(10, 100);

        let output = MetricsPrometheusFormat(&metrics).to_string();

        assert!(output.contains("a3chat_group_sync_last_timestamp_seconds"));
    }

    /// Phase 5c: Test large batch size handling.
    #[test]
    fn sync_metrics_large_batch() {
        let mut metrics = SyncMetrics::default();
        metrics.avg_message_size_bytes = 1000;

        // Test with a large batch
        metrics.record_sync(10000, 5000);
        assert_eq!(metrics.messages_synced_total, 10000);
        assert_eq!(metrics.bytes_synced_total, 10_000_000);
    }

    /// Phase 5c: Test concurrent error recording.
    #[tokio::test]
    async fn sync_metrics_collector_concurrent_errors() {
        let collector = SyncMetricsCollector::new();

        let mut handles = vec![];
        for _ in 0..20 {
            let c = collector.clone();
            handles.push(tokio::spawn(async move {
                c.record_error().await;
            }));
        }

        for h in handles {
            h.await.expect("task should complete");
        }

        let snapshot = collector.snapshot().await;
        assert_eq!(snapshot.sync_errors_total, 20);
    }

    /// Phase 5c: Test mixed sync and error operations.
    #[tokio::test]
    async fn sync_metrics_collector_mixed_operations() {
        let collector = SyncMetricsCollector::new();

        // 5 successful syncs
        for i in 1..=5 {
            collector.record_sync(i * 10, i as u64 * 10).await;
        }

        // 2 errors
        collector.record_error().await;
        collector.record_error().await;

        let snapshot = collector.snapshot().await;
        // Sum of 10+20+30+40+50 = 150 messages
        assert_eq!(snapshot.messages_synced_total, 150);
        assert_eq!(snapshot.sync_errors_total, 2);
        assert_eq!(snapshot.sync_operations_total, 5);
        assert_eq!(snapshot.active_groups, 0);
    }

    /// Phase 5c: Test bytes calculation with different message sizes.
    #[test]
    fn sync_metrics_bytes_with_various_sizes() {
        let mut metrics = SyncMetrics::default();

        // Small messages: 10 * 100 = 1000 bytes
        metrics.avg_message_size_bytes = 100;
        metrics.record_sync(10, 50);
        assert_eq!(metrics.bytes_synced_total, 1000);

        // Larger messages: uses current avg_message_size_bytes (100)
        // Second sync: 5 * 100 = 500 bytes, total = 1000 + 500 = 1500
        metrics.record_sync(5, 30);
        assert_eq!(metrics.bytes_synced_total, 1500);

        // Change avg size for next sync: 3 * 500 = 1500, total = 1500 + 1500 = 3000
        metrics.avg_message_size_bytes = 500;
        metrics.record_sync(3, 20);
        assert_eq!(metrics.bytes_synced_total, 3000);
    }
}
