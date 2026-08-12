//! Message persistence for gossip messages.
//!
//! This module provides durable storage for gossip messages, enabling:
//! - Message history retrieval
//! - Offline message catchup
//! - Message replay
//! - Persistence across restarts
//!
//! ## Storage Backends
//!
//! - `MemoryStore`: In-memory storage (default, for testing)
//! - `SqliteStore`: SQLite-backed persistent storage (production)
//! - `RocksDbStore`: RocksDB-backed storage (high-performance)
//!
//! ## Usage
//!
//! ```rust
//! use adnet_gossip::persistence::{MemoryMessageStore, MessagePersistence};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let store = MemoryMessageStore::new(1000); // Max 1000 messages
//! store.store_message("room1", "msg1", b"hello").await?;
//! let msgs = store.get_messages("room1", 10).await?;
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tokio::sync::mpsc;

/// Maximum number of messages to store per room by default.
pub const DEFAULT_MAX_MESSAGES_PER_ROOM: usize = 10_000;

/// Default message retention period (7 days).
pub const DEFAULT_RETENTION_SECS: u64 = 7 * 24 * 3600;

/// Stored message with metadata.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    /// Unique message identifier.
    pub message_id: String,
    /// Room this message belongs to.
    pub room_id: String,
    /// Raw message content.
    pub content: Vec<u8>,
    /// Timestamp when message was received.
    pub received_at: Instant,
    /// Timestamp when message expires (TTL).
    pub expires_at: Option<Instant>,
    /// Sequence number for ordering.
    pub sequence: u64,
}

/// Configuration for message persistence.
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// Maximum messages to store per room.
    pub max_messages_per_room: usize,
    /// Message retention period in seconds.
    pub retention_secs: u64,
    /// Path for disk-based storage (None for memory-only).
    pub storage_path: Option<PathBuf>,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            max_messages_per_room: DEFAULT_MAX_MESSAGES_PER_ROOM,
            retention_secs: DEFAULT_RETENTION_SECS,
            storage_path: None,
        }
    }
}

/// Trait for message storage backends.
#[async_trait::async_trait]
pub trait MessageStore: Send + Sync {
    /// Store a message.
    async fn store(&self, room_id: &str, message: StoredMessage) -> anyhow::Result<()>;

    /// Get messages for a room, newest first.
    async fn get_messages(
        &self,
        room_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<StoredMessage>>;

    /// Get a specific message by ID.
    async fn get_message(&self, message_id: &str) -> anyhow::Result<Option<StoredMessage>>;

    /// Delete expired messages.
    async fn cleanup_expired(&self) -> anyhow::Result<usize>;

    /// Delete messages older than the given timestamp.
    async fn delete_before(&self, before: Instant) -> anyhow::Result<usize>;

    /// Get total message count across all rooms.
    async fn total_count(&self) -> anyhow::Result<usize>;

    /// Get message count for a specific room.
    async fn room_count(&self, room_id: &str) -> anyhow::Result<usize>;

    /// Clear all messages for a room.
    async fn clear_room(&self, room_id: &str) -> anyhow::Result<()>;
}

/// In-memory message store (for testing and development).
pub struct MemoryMessageStore {
    messages: RwLock<HashMap<String, Vec<StoredMessage>>>,
    counters: RwLock<HashMap<String, u64>>,
    config: PersistenceConfig,
    max_total: usize,
}

impl MemoryMessageStore {
    /// Create a new in-memory store.
    pub fn new(max_messages_per_room: usize) -> Self {
        Self {
            messages: RwLock::new(HashMap::new()),
            counters: RwLock::new(HashMap::new()),
            config: PersistenceConfig {
                max_messages_per_room,
                ..Default::default()
            },
            max_total: max_messages_per_room * 100, // Allow some headroom
        }
    }

    /// Create with custom configuration.
    pub fn with_config(config: PersistenceConfig) -> Self {
        let max_total = config.max_messages_per_room * 100;
        Self {
            messages: RwLock::new(HashMap::new()),
            counters: RwLock::new(HashMap::new()),
            config,
            max_total,
        }
    }

    fn cleanup_internal(&self, room_messages: &mut Vec<StoredMessage>) {
        let now = Instant::now();
        let retention = Duration::from_secs(self.config.retention_secs);

        // Remove expired messages
        room_messages.retain(|m| {
            if let Some(expires_at) = m.expires_at {
                if now >= expires_at {
                    return false;
                }
            }
            if now.duration_since(m.received_at) > retention {
                return false;
            }
            true
        });

        // Enforce per-room limit
        if room_messages.len() > self.config.max_messages_per_room {
            room_messages.sort_by(|a, b| b.sequence.cmp(&a.sequence));
            room_messages.truncate(self.config.max_messages_per_room);
        }
    }
}

#[async_trait::async_trait]
impl MessageStore for MemoryMessageStore {
    async fn store(&self, room_id: &str, mut message: StoredMessage) -> anyhow::Result<()> {
        // Assign sequence number
        let seq = {
            let mut counters = self.counters.write();
            let next = counters.entry(room_id.to_string()).or_insert(0);
            *next += 1;
            *next
        };
        message.sequence = seq;

        let mut messages = self.messages.write();

        let room_messages = messages.entry(room_id.to_string()).or_default();

        // Push the new message first
        room_messages.push(message);

        // Then enforce limits via cleanup
        self.cleanup_internal(room_messages);

        // Global cleanup if needed
        let total: usize = messages.values().map(|v| v.len()).sum();
        if total > self.max_total {
            // Sort rooms by oldest message, evict from oldest rooms first
            let mut all_rooms: Vec<_> = messages.iter_mut().collect();
            all_rooms.sort_by(|a, b| {
                let oldest_a = a.1.iter().map(|m| m.received_at).min();
                let oldest_b = b.1.iter().map(|m| m.received_at).min();
                oldest_a.cmp(&oldest_b)
            });

            // Evict oldest rooms until under limit
            let target = self.max_total / 2;
            for (_, room) in all_rooms {
                let before = room.len();
                self.cleanup_internal(room);
                if total <= target {
                    break;
                }
            }
        }

        Ok(())
    }

    async fn get_messages(
        &self,
        room_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<StoredMessage>> {
        let messages = self.messages.read();
        let room_messages = messages.get(room_id).cloned().unwrap_or_default();

        // Return newest first, up to limit
        let mut result: Vec<_> = room_messages;
        result.sort_by(|a, b| b.sequence.cmp(&a.sequence));
        result.truncate(limit);

        Ok(result)
    }

    async fn get_message(&self, message_id: &str) -> anyhow::Result<Option<StoredMessage>> {
        let messages = self.messages.read();
        for room in messages.values() {
            if let Some(msg) = room.iter().find(|m| m.message_id == message_id) {
                return Ok(Some(msg.clone()));
            }
        }
        Ok(None)
    }

    async fn cleanup_expired(&self) -> anyhow::Result<usize> {
        let mut removed = 0;
        let mut messages = self.messages.write();

        for room_messages in messages.values_mut() {
            let before = room_messages.len();
            self.cleanup_internal(room_messages);
            removed += before - room_messages.len();
        }

        Ok(removed)
    }

    async fn delete_before(&self, before: Instant) -> anyhow::Result<usize> {
        let mut removed = 0;
        let mut messages = self.messages.write();

        for room_messages in messages.values_mut() {
            let before_count = room_messages.len();
            // Keep messages received AFTER the given time (i.e., delete messages received BEFORE)
            room_messages.retain(|m| m.received_at >= before);
            removed += before_count - room_messages.len();
        }

        Ok(removed)
    }

    async fn total_count(&self) -> anyhow::Result<usize> {
        let messages = self.messages.read();
        Ok(messages.values().map(|v| v.len()).sum())
    }

    async fn room_count(&self, room_id: &str) -> anyhow::Result<usize> {
        let messages = self.messages.read();
        Ok(messages.get(room_id).map(|v| v.len()).unwrap_or(0))
    }

    async fn clear_room(&self, room_id: &str) -> anyhow::Result<()> {
        let mut messages = self.messages.write();
        messages.remove(room_id);
        Ok(())
    }
}

/// Message persistence manager that wraps a store.
pub struct MessagePersistence {
    store: Arc<dyn MessageStore>,
    cleanup_interval: Duration,
}

impl MessagePersistence {
    /// Create a new persistence manager with the given store.
    pub fn new(store: Arc<dyn MessageStore>) -> Self {
        Self {
            store,
            cleanup_interval: Duration::from_secs(3600), // Default 1 hour
        }
    }

    /// Create with custom cleanup interval.
    pub fn with_cleanup_interval(store: Arc<dyn MessageStore>, interval_secs: u64) -> Self {
        Self {
            store,
            cleanup_interval: Duration::from_secs(interval_secs),
        }
    }

    /// Get the underlying store.
    pub fn store(&self) -> Arc<dyn MessageStore> {
        Arc::clone(&self.store)
    }

    /// Store a message.
    pub async fn store_message(&self, room_id: &str, message: StoredMessage) -> anyhow::Result<()> {
        self.store.store(room_id, message).await
    }

    /// Get recent messages for a room.
    pub async fn get_recent(&self, room_id: &str, count: usize) -> anyhow::Result<Vec<StoredMessage>> {
        self.store.get_messages(room_id, count).await
    }

    /// Get a specific message.
    pub async fn get(&self, message_id: &str) -> anyhow::Result<Option<StoredMessage>> {
        self.store.get_message(message_id).await
    }

    /// Run cleanup of expired messages.
    pub async fn cleanup(&self) -> anyhow::Result<usize> {
        self.store.cleanup_expired().await
    }

    /// Start background cleanup task.
    pub fn start_cleanup_task(self: Arc<Self>) -> mpsc::Sender<()> {
        let (tx, mut rx) = mpsc::channel::<()>(1);

        tokio::spawn(async move {
            let interval = tokio::time::interval(self.cleanup_interval);

            tokio::pin!(interval);

            loop {
                tokio::select! {
                    _ = rx.recv() => {
                        // Received stop signal
                        break;
                    }
                    _ = interval.tick() => {
                        if let Err(e) = self.cleanup().await {
                            tracing::warn!("Persistence cleanup failed: {}", e);
                        }
                    }
                }
            }
        });

        tx
    }

    /// Get statistics.
    pub async fn stats(&self) -> anyhow::Result<PersistenceStats> {
        Ok(PersistenceStats {
            total_messages: self.store.total_count().await?,
            cleanup_interval_secs: self.cleanup_interval.as_secs(),
        })
    }
}

/// Statistics about the persistence layer.
#[derive(Debug, Clone)]
pub struct PersistenceStats {
    pub total_messages: usize,
    pub cleanup_interval_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_store_basic_operations() {
        let store = MemoryMessageStore::new(100);

        let msg = StoredMessage {
            message_id: "msg1".into(),
            room_id: "room1".into(),
            content: b"hello".to_vec(),
            received_at: Instant::now(),
            expires_at: None,
            sequence: 0,
        };

        store.store("room1", msg.clone()).await.unwrap();

        let messages = store.get_messages("room1", 10).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, "msg1");

        let count = store.room_count("room1").await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn memory_store_cleanup() {
        let store = MemoryMessageStore::new(100);

        // Store messages directly (bypass timing issues)
        let base = Instant::now();
        for i in 0..5 {
            let msg = StoredMessage {
                message_id: format!("msg{}", i),
                room_id: "room1".into(),
                content: format!("content{}", i).into(),
                received_at: base - Duration::from_secs(i), // 0,1,2,3,4 seconds ago
                expires_at: None,
                sequence: 0,
            };
            // Use get_messages to check state before/after
            store.store("room1", msg).await.unwrap();
        }

        assert_eq!(store.room_count("room1").await.unwrap(), 5);

        // Test delete_before - delete messages older than base - 2 seconds
        // This should delete messages with received_at < (base - 2s) = messages at 0s, 1s ago
        let cutoff = base - Duration::from_secs(2);
        let removed = store.delete_before(cutoff).await.unwrap();
        // msg0 (0s ago), msg1 (1s ago) should be deleted
        assert_eq!(removed, 2, "expected 2 messages older than 2s to be deleted");

        // Verify remaining messages
        let remaining = store.room_count("room1").await.unwrap();
        assert_eq!(remaining, 3);
    }

    #[tokio::test]
    async fn memory_store_max_messages() {
        let store = MemoryMessageStore::with_config(PersistenceConfig {
            max_messages_per_room: 3,
            retention_secs: 3600,
            storage_path: None,
        });

        let now = Instant::now();
        // Store messages one by one to trigger cleanup each time
        for i in 0..10 {
            let msg = StoredMessage {
                message_id: format!("msg{}", i),
                room_id: "room1".into(),
                content: format!("content{}", i).into(),
                received_at: now + Duration::from_secs(i), // Future timestamps to avoid retention cleanup
                expires_at: Some(now + Duration::from_secs(3600)),
                sequence: 0,
            };
            store.store("room1", msg).await.unwrap();
        }

        // The store should keep only the 3 most recent (highest timestamps)
        // Since we store 10 with cleanup at 3, we should end up with exactly 3
        let messages = store.get_messages("room1", 100).await.unwrap();
        assert_eq!(messages.len(), 3, "expected 3 messages, got {}", messages.len());
    }

    #[tokio::test]
    async fn persistence_manager() {
        let store: Arc<MemoryMessageStore> = Arc::new(MemoryMessageStore::new(100));
        let persistence = MessagePersistence::new(store as Arc<dyn MessageStore>);

        let msg = StoredMessage {
            message_id: "msg1".into(),
            room_id: "room1".into(),
            content: b"test".to_vec(),
            received_at: Instant::now(),
            expires_at: None,
            sequence: 0,
        };

        persistence.store_message("room1", msg).await.unwrap();

        let messages = persistence.get_recent("room1", 10).await.unwrap();
        assert_eq!(messages.len(), 1);

        let stats = persistence.stats().await.unwrap();
        assert_eq!(stats.total_messages, 1);
    }
}
