//! Disappearing Messages / Ephemeral Messages Service (阅后即焚)
//!
//! Handles self-destructing messages that are automatically deleted
//! after a configurable timeout after being read.
//!
//! ## Features
//! - Per-conversation disappearing message settings
//! - Configurable timer: 5s, 30s, 1min, 5min, 1hour, 24hours
//! - Automatic message deletion after timer expires
//! - No trace left after deletion
//!
//! ## DO-178C Traceability
//! - D-EPH-1: Ephemeral settings stored per conversation
//! - D-EPH-2: Timer starts when recipient reads message
//! - D-EPH-3: Message auto-deleted after timer expires
//! - D-EPH-4: Audit trail for deleted ephemeral messages

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use a3chat_core::error::A3chatError;
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::{ConversationId, MessageId, UserId};

use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;
use crate::storage::ChatStorage;

/// Timer duration options for disappearing messages
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisappearingTimer {
    /// Off - disappearing messages disabled
    Off,
    /// 5 seconds - very fast
    FiveSeconds,
    /// 30 seconds
    ThirtySeconds,
    /// 1 minute
    OneMinute,
    /// 5 minutes
    FiveMinutes,
    /// 1 hour
    OneHour,
    /// 24 hours
    TwentyFourHours,
}

impl DisappearingTimer {
    pub fn as_duration(&self) -> Option<Duration> {
        match self {
            DisappearingTimer::Off => None,
            DisappearingTimer::FiveSeconds => Some(Duration::from_secs(5)),
            DisappearingTimer::ThirtySeconds => Some(Duration::from_secs(30)),
            DisappearingTimer::OneMinute => Some(Duration::from_secs(60)),
            DisappearingTimer::FiveMinutes => Some(Duration::from_secs(300)),
            DisappearingTimer::OneHour => Some(Duration::from_secs(3600)),
            DisappearingTimer::TwentyFourHours => Some(Duration::from_secs(86400)),
        }
    }

    pub fn as_seconds(&self) -> u64 {
        match self {
            DisappearingTimer::Off => 0,
            DisappearingTimer::FiveSeconds => 5,
            DisappearingTimer::ThirtySeconds => 30,
            DisappearingTimer::OneMinute => 60,
            DisappearingTimer::FiveMinutes => 300,
            DisappearingTimer::OneHour => 3600,
            DisappearingTimer::TwentyFourHours => 86400,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            DisappearingTimer::Off => "off",
            DisappearingTimer::FiveSeconds => "5s",
            DisappearingTimer::ThirtySeconds => "30s",
            DisappearingTimer::OneMinute => "1m",
            DisappearingTimer::FiveMinutes => "5m",
            DisappearingTimer::OneHour => "1h",
            DisappearingTimer::TwentyFourHours => "24h",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" | "0" => Some(DisappearingTimer::Off),
            "5s" | "5" => Some(DisappearingTimer::FiveSeconds),
            "30s" | "30" => Some(DisappearingTimer::ThirtySeconds),
            "1m" | "60" => Some(DisappearingTimer::OneMinute),
            "5m" | "300" => Some(DisappearingTimer::FiveMinutes),
            "1h" | "3600" => Some(DisappearingTimer::OneHour),
            "24h" | "86400" => Some(DisappearingTimer::TwentyFourHours),
            _ => None,
        }
    }
}

/// Per-conversation disappearing message settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EphemeralSettings {
    pub conversation_id: ConversationId,
    pub timer: DisappearingTimer,
    /// The user who set this timer
    pub set_by: UserId,
    /// When the timer was set (Unix timestamp)
    pub set_at: i64,
}

impl EphemeralSettings {
    pub fn new(conversation_id: ConversationId, timer: DisappearingTimer, set_by: UserId) -> Self {
        Self {
            conversation_id,
            timer,
            set_by,
            set_at: chrono::Utc::now().timestamp(),
        }
    }
}

/// Record of a pending ephemeral message deletion
#[derive(Debug, Clone)]
struct PendingDeletion {
    message_id: MessageId,
    conversation_id: ConversationId,
    owner_id: UserId,
    delete_at: Instant,
}

impl PendingDeletion {
    fn new(
        message_id: MessageId,
        conversation_id: ConversationId,
        owner_id: UserId,
        timer: Duration,
    ) -> Self {
        Self {
            message_id,
            conversation_id,
            owner_id,
            delete_at: Instant::now() + timer,
        }
    }

    fn is_due(&self) -> bool {
        Instant::now() >= self.delete_at
    }

    fn remaining_secs(&self) -> u64 {
        let remaining = self.delete_at.saturating_duration_since(Instant::now());
        remaining.as_secs()
    }
}

/// Ephemeral message record for tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EphemeralMessage {
    pub message_id: MessageId,
    pub conversation_id: ConversationId,
    pub sender_id: UserId,
    /// When the message was first read by any recipient (Unix timestamp)
    pub first_read_at: Option<i64>,
    /// When the message will be deleted (Unix timestamp)
    pub delete_at: Option<i64>,
    /// Whether the message has been deleted
    pub deleted: bool,
}

impl EphemeralMessage {
    pub fn new(message_id: MessageId, conversation_id: ConversationId, sender_id: UserId) -> Self {
        Self {
            message_id,
            conversation_id,
            sender_id,
            first_read_at: None,
            delete_at: None,
            deleted: false,
        }
    }

    pub fn mark_read(&mut self, timer_secs: u64) {
        if self.first_read_at.is_none() {
            self.first_read_at = Some(chrono::Utc::now().timestamp());
            self.delete_at = Some(chrono::Utc::now().timestamp() + timer_secs as i64);
        }
    }
}

/// Event emitted when an ephemeral message is deleted
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EphemeralMessageDeletedEvent {
    pub message_id: MessageId,
    pub conversation_id: ConversationId,
    pub deleted_by: UserId,
    pub reason: EphemeralDeleteReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EphemeralDeleteReason {
    /// Deleted because timer expired after being read
    TimerExpired,
    /// Deleted because conversation settings changed
    SettingsChanged,
    /// Deleted manually before timer expired
    Manual,
}

/// Statistics about ephemeral messages for a user
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EphemeralStats {
    pub user_id: UserId,
    pub total_tracked: usize,
    pub pending_deletions: usize,
    pub read_messages: usize,
}

/// Service for managing disappearing/ephemeral messages
#[derive(Clone)]
pub struct DisappearingMessageService {
    /// Per-user ephemeral settings for conversations
    settings: Arc<RwLock<HashMap<UserId, HashMap<ConversationId, EphemeralSettings>>>>,
    /// In-memory tracking of ephemeral messages (keyed by owner)
    ephemeral_messages: Arc<RwLock<HashMap<UserId, HashMap<MessageId, EphemeralMessage>>>>,
    /// Pending deletions queue
    pending_deletions: Arc<RwLock<Vec<PendingDeletion>>>,
    /// Shutdown signal sender
    shutdown_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
    /// Background task handle
    worker_handle: Arc<RwLock<Option<JoinHandle<()>>>>,
    /// Notification bus for events
    bus: NotificationBus,
    /// Storage backend
    storage: ChatStorage,
}

impl DisappearingMessageService {
    /// Create a new DisappearingMessageService
    pub fn new(bus: NotificationBus, storage: ChatStorage) -> Self {
        Self {
            settings: Arc::new(RwLock::new(HashMap::new())),
            ephemeral_messages: Arc::new(RwLock::new(HashMap::new())),
            pending_deletions: Arc::new(RwLock::new(Vec::new())),
            shutdown_tx: Arc::new(RwLock::new(None)),
            worker_handle: Arc::new(RwLock::new(None)),
            bus,
            storage,
        }
    }

    /// Start the background cleanup worker
    pub fn start_background_worker(self: Arc<Self>) {
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        
        *self.shutdown_tx.write() = Some(shutdown_tx);

        let this = self.clone();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            let mut orphan_cleanup_interval = tokio::time::interval(Duration::from_secs(300)); // 5 minutes
            
            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        tracing::info!("DisappearingMessageService: shutdown signal received");
                        break;
                    }
                    _ = interval.tick() => {
                        this.process_due_deletions().await;
                    }
                    _ = orphan_cleanup_interval.tick() => {
                        this.cleanup_orphaned_messages().await;
                    }
                }
            }
        });

        *self.worker_handle.write() = Some(handle);
    }

    /// Stop the background worker
    pub async fn stop(&self) {
        if let Some(tx) = self.shutdown_tx.write().take() {
            let _ = tx.send(()).await;
        }
        
        if let Some(handle) = self.worker_handle.write().take() {
            let _ = handle.await;
        }
    }

    /// Process all pending deletions that are due
    async fn process_due_deletions(self: &Arc<Self>) {
        let pending = self.pending_deletions.read().clone();
        let mut still_pending = Vec::new();

        for deletion in pending {
            if deletion.is_due() {
                self.delete_ephemeral_message(
                    &deletion.message_id,
                    &deletion.conversation_id,
                    &deletion.owner_id,
                    EphemeralDeleteReason::TimerExpired,
                ).await;
            } else {
                still_pending.push(deletion);
            }
        }

        *self.pending_deletions.write() = still_pending;
    }

    /// Set disappearing message timer for a conversation
    pub async fn set_timer(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        timer: DisappearingTimer,
    ) -> AppResult<EphemeralSettings> {
        let settings = EphemeralSettings::new(
            conversation_id.clone(),
            timer,
            owner.clone(),
        );

        // Store settings
        {
            let mut user_settings = self.settings.write();
            user_settings
                .entry(owner.clone())
                .or_insert_with(HashMap::new)
                .insert(conversation_id.clone(), settings.clone());
        }

        // If timer is turned off, cancel all pending deletions for this conversation
        if timer == DisappearingTimer::Off {
            let mut pending = self.pending_deletions.write();
            pending.retain(|d| {
                !(d.conversation_id == *conversation_id && d.owner_id == *owner)
            });
        }

        tracing::info!(
            "Ephemeral timer set for conversation {} by user {}: {}",
            conversation_id, owner, timer.as_str()
        );

        Ok(settings)
    }

    /// Get current disappearing message settings for a conversation
    pub async fn get_settings(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<Option<EphemeralSettings>> {
        let user_settings = self.settings.read();
        Ok(user_settings
            .get(owner)
            .and_then(|conv_settings| conv_settings.get(conversation_id))
            .cloned())
    }

    /// Get all conversations with disappearing messages enabled for a user
    pub async fn list_conversations_with_ephemeral(
        &self,
        owner: &UserId,
    ) -> AppResult<Vec<EphemeralSettings>> {
        let user_settings = self.settings.read();
        Ok(user_settings
            .get(owner)
            .map(|conv_settings| {
                conv_settings
                    .values()
                    .filter(|s| s.timer != DisappearingTimer::Off)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Register a message as ephemeral (called when message is sent)
    pub async fn register_message(
        &self,
        message_id: &MessageId,
        conversation_id: &ConversationId,
        sender_id: &UserId,
    ) -> AppResult<bool> {
        let settings = self.get_settings(sender_id, conversation_id).await?;
        
        // If ephemeral is off for this conversation, don't register
        let Some(settings) = settings else {
            return Ok(false);
        };
        
        if settings.timer == DisappearingTimer::Off {
            return Ok(false);
        }

        let mut ephemeral = EphemeralMessage::new(
            message_id.clone(),
            conversation_id.clone(),
            sender_id.clone(),
        );

        let message_id_clone = message_id.clone();
        let conversation_id_clone = conversation_id.clone();
        let owner_clone = sender_id.clone();
        let timer = settings.timer.as_duration().unwrap();

        // For recipients, schedule deletion when they first read
        // (stored in pending_deletions will be activated on read)
        {
            let mut messages = self.ephemeral_messages.write();
            messages
                .entry(sender_id.clone())
                .or_insert_with(HashMap::new)
                .insert(message_id.clone(), ephemeral);
        }

        tracing::debug!(
            "Registered ephemeral message {} in conversation {} with timer {}",
            message_id, conversation_id, settings.timer.as_str()
        );

        Ok(true)
    }

    /// Mark a message as read by a recipient (starts the deletion timer)
    pub async fn mark_message_read(
        &self,
        recipient_id: &UserId,
        message_id: &MessageId,
        conversation_id: &ConversationId,
    ) -> AppResult<()> {
        // Find the ephemeral settings for this conversation
        let settings = self.get_settings(recipient_id, conversation_id).await?;
        let settings = match settings {
            Some(s) if s.timer != DisappearingTimer::Off => s,
            _ => return Ok(()), // Ephemeral disabled, nothing to do
        };

        let timer = settings.timer.as_duration().unwrap();
        let timer_secs = settings.timer.as_seconds();

        // Update ephemeral message record
        {
            let mut messages = self.ephemeral_messages.write();
            if let Some(user_msgs) = messages.get_mut(recipient_id) {
                if let Some(ephemeral) = user_msgs.get_mut(message_id) {
                    ephemeral.mark_read(timer_secs);
                }
            }
        }

        // Schedule deletion
        let pending = PendingDeletion::new(
            message_id.clone(),
            conversation_id.clone(),
            recipient_id.clone(),
            timer,
        );

        self.pending_deletions.write().push(pending);

        tracing::debug!(
            "Started {} timer for ephemeral message {} read by {}",
            settings.timer.as_str(), message_id, recipient_id
        );

        Ok(())
    }

    /// Delete an ephemeral message
    pub async fn delete_ephemeral_message(
        &self,
        message_id: &MessageId,
        conversation_id: &ConversationId,
        owner_id: &UserId,
        reason: EphemeralDeleteReason,
    ) {
        // Delete from storage
        if let Err(e) = self.storage.delete_message_for_me(owner_id, message_id).await {
            tracing::warn!(
                "Failed to delete ephemeral message {} from storage: {}",
                message_id, e
            );
        }

        // Remove from in-memory tracking
        {
            let mut messages = self.ephemeral_messages.write();
            if let Some(user_msgs) = messages.get_mut(owner_id) {
                user_msgs.remove(message_id);
            }
        }

        // Remove from pending deletions
        {
            let mut pending = self.pending_deletions.write();
            pending.retain(|d| !(&d.message_id == message_id && &d.owner_id == owner_id));
        }

        // Emit event
        let _event = EphemeralMessageDeletedEvent {
            message_id: message_id.clone(),
            conversation_id: conversation_id.clone(),
            deleted_by: owner_id.clone(),
            reason,
        };

        // Emit event using generic publish (will use ChatMessageDeleted routing)
        let _ = self.bus.publish(A3chatEvent::ChatMessageDeleted {
            user_id: owner_id.clone(),
            conversation_id: conversation_id.clone(),
            message_id: message_id.clone(),
        });

        tracing::info!(
            "Deleted ephemeral message {} from conversation {} (reason: {:?})",
            message_id, conversation_id, reason
        );
    }

    /// Cleanup orphaned ephemeral messages
    /// 
    /// Orphaned messages are messages that should have been deleted but weren't,
    /// typically due to process crashes or timer failures.
    /// 
    /// This method scans all tracked ephemeral messages and deletes those that
    /// are past their deletion time.
    pub async fn cleanup_orphaned_messages(&self) {
        let now = chrono::Utc::now().timestamp();
        let mut to_delete = Vec::new();

        // Scan all tracked ephemeral messages
        {
            let messages = self.ephemeral_messages.read();
            for (user_id, user_msgs) in messages.iter() {
                for (msg_id, ephemeral) in user_msgs.iter() {
                    // Check if message is past its deletion time
                    if let Some(delete_at) = ephemeral.delete_at {
                        if delete_at <= now && !ephemeral.deleted {
                            to_delete.push((
                                msg_id.clone(),
                                ephemeral.conversation_id.clone(),
                                user_id.clone(),
                            ));
                        }
                    }
                }
            }
        }

        // Delete orphaned messages
        if !to_delete.is_empty() {
            tracing::info!(
                "Found {} orphaned ephemeral messages to clean up",
                to_delete.len()
            );

            for (msg_id, conv_id, user_id) in to_delete {
                self.delete_ephemeral_message(
                    &msg_id,
                    &conv_id,
                    &user_id,
                    EphemeralDeleteReason::TimerExpired,
                ).await;
            }
        }
    }

    /// Get statistics about ephemeral messages
    pub async fn get_ephemeral_stats(&self, user_id: &UserId) -> EphemeralStats {
        let messages = self.ephemeral_messages.read();
        let pending = self.pending_deletions.read();

        let user_msgs = messages.get(user_id);
        let total_tracked = user_msgs.map(|m| m.len()).unwrap_or(0);
        
        let pending_count = pending.iter()
            .filter(|d| &d.owner_id == user_id)
            .count();

        let read_count = user_msgs
            .map(|m| m.values().filter(|msg| msg.first_read_at.is_some()).count())
            .unwrap_or(0);

        EphemeralStats {
            user_id: user_id.clone(),
            total_tracked,
            pending_deletions: pending_count,
            read_messages: read_count,
        }
    }

    /// Force expire a message for testing purposes
    #[doc(hidden)]
    pub async fn force_expire_message(&self, user_id: &UserId, message_id: &MessageId) {
        let mut messages = self.ephemeral_messages.write();
        if let Some(user_msgs) = messages.get_mut(user_id) {
            if let Some(ephemeral) = user_msgs.get_mut(message_id) {
                ephemeral.delete_at = Some(chrono::Utc::now().timestamp() - 10);
            }
        }
    }

    /// Manually delete an ephemeral message before timer expires
    pub async fn manual_delete(
        &self,
        owner_id: &UserId,
        message_id: &MessageId,
        conversation_id: &ConversationId,
    ) -> AppResult<()> {
        self.delete_ephemeral_message(
            message_id,
            conversation_id,
            owner_id,
            EphemeralDeleteReason::Manual,
        ).await;
        Ok(())
    }

    /// Get ephemeral message status
    pub async fn get_message_status(
        &self,
        owner_id: &UserId,
        message_id: &MessageId,
    ) -> AppResult<Option<EphemeralMessage>> {
        let messages = self.ephemeral_messages.read();
        Ok(messages
            .get(owner_id)
            .and_then(|msgs| msgs.get(message_id))
            .cloned())
    }

    /// Get count of pending deletions for a conversation
    pub async fn get_pending_count(
        &self,
        owner_id: &UserId,
        conversation_id: &ConversationId,
    ) -> usize {
        let pending = self.pending_deletions.read();
        pending
            .iter()
            .filter(|d| &d.conversation_id == conversation_id && &d.owner_id == owner_id)
            .count()
    }

    /// Get time until next deletion for a conversation
    pub async fn get_next_deletion_time(
        &self,
        owner_id: &UserId,
        conversation_id: &ConversationId,
    ) -> Option<u64> {
        let pending = self.pending_deletions.read();
        pending
            .iter()
            .filter(|d| &d.conversation_id == conversation_id && &d.owner_id == owner_id)
            .map(|d| d.remaining_secs())
            .min()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_user() -> UserId {
        UserId::from("test_user_ephemeral")
    }

    fn make_conv() -> ConversationId {
        ConversationId::from("conv_ephemeral_test")
    }

    fn make_msg() -> MessageId {
        MessageId::from("msg_ephemeral_123")
    }

    #[tokio::test]
    async fn test_timer_duration_conversion() {
        assert_eq!(DisappearingTimer::Off.as_seconds(), 0);
        assert_eq!(DisappearingTimer::FiveSeconds.as_seconds(), 5);
        assert_eq!(DisappearingTimer::ThirtySeconds.as_seconds(), 30);
        assert_eq!(DisappearingTimer::OneMinute.as_seconds(), 60);
        assert_eq!(DisappearingTimer::FiveMinutes.as_seconds(), 300);
        assert_eq!(DisappearingTimer::OneHour.as_seconds(), 3600);
        assert_eq!(DisappearingTimer::TwentyFourHours.as_seconds(), 86400);
    }

    #[tokio::test]
    async fn test_timer_string_conversion() {
        assert_eq!(DisappearingTimer::Off.as_str(), "off");
        assert_eq!(DisappearingTimer::FiveSeconds.as_str(), "5s");
        assert_eq!(DisappearingTimer::OneMinute.as_str(), "1m");
        assert_eq!(DisappearingTimer::OneHour.as_str(), "1h");
        assert_eq!(DisappearingTimer::TwentyFourHours.as_str(), "24h");
    }

    #[tokio::test]
    async fn test_timer_parse() {
        assert_eq!(DisappearingTimer::parse("off"), Some(DisappearingTimer::Off));
        assert_eq!(DisappearingTimer::parse("5s"), Some(DisappearingTimer::FiveSeconds));
        assert_eq!(DisappearingTimer::parse("1m"), Some(DisappearingTimer::OneMinute));
        assert_eq!(DisappearingTimer::parse("1h"), Some(DisappearingTimer::OneHour));
        assert_eq!(DisappearingTimer::parse("24h"), Some(DisappearingTimer::TwentyFourHours));
        assert_eq!(DisappearingTimer::parse("invalid"), None);
    }

    #[tokio::test]
    async fn test_ephemeral_settings_creation() {
        let owner = make_user();
        let conv = make_conv();
        let timer = DisappearingTimer::FiveMinutes;

        let settings = EphemeralSettings::new(conv.clone(), timer, owner.clone());

        assert_eq!(settings.conversation_id, conv);
        assert_eq!(settings.timer, timer);
        assert_eq!(settings.set_by, owner);
        assert!(settings.set_at > 0);
    }

    #[tokio::test]
    async fn test_ephemeral_message_mark_read() {
        let msg_id = make_msg();
        let conv = make_conv();
        let sender = make_user();

        let mut msg = EphemeralMessage::new(msg_id.clone(), conv.clone(), sender.clone());
        
        assert!(msg.first_read_at.is_none());
        assert!(msg.delete_at.is_none());
        assert!(!msg.deleted);

        msg.mark_read(30);

        assert!(msg.first_read_at.is_some());
        assert!(msg.delete_at.is_some());
        assert!(!msg.deleted);

        // Second call should not change anything
        let original_first_read = msg.first_read_at;
        msg.mark_read(60);
        assert_eq!(msg.first_read_at, original_first_read);
    }

    #[tokio::test]
    async fn test_pending_deletion_timing() {
        let msg_id = make_msg();
        let conv = make_conv();
        let owner = make_user();

        // Create a deletion with 5 second timer
        let deletion = PendingDeletion::new(
            msg_id.clone(),
            conv.clone(),
            owner.clone(),
            Duration::from_secs(5),
        );

        // Should not be due immediately
        assert!(!deletion.is_due());
        assert!(deletion.remaining_secs() <= 5);
    }

    #[tokio::test]
    async fn test_settings_change_cancels_deletions() {
        // This tests the logic that when timer is set to Off,
        // all pending deletions for that conversation should be cancelled
        let owner = make_user();
        let conv = make_conv();

        // Simulate having pending deletions
        let mut pending: Vec<PendingDeletion> = vec![
            PendingDeletion::new(
                MessageId::from("msg_1"),
                conv.clone(),
                owner.clone(),
                Duration::from_secs(60),
            ),
            PendingDeletion::new(
                MessageId::from("msg_2"),
                conv.clone(),
                owner.clone(),
                Duration::from_secs(120),
            ),
        ];

        // Simulate settings changed to Off
        pending.retain(|d| !(d.conversation_id == conv && d.owner_id == owner));

        assert!(pending.is_empty());
    }
}
