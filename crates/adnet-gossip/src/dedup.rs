//! Message deduplication and TTL tracking for gossip layer.
//!
//! This module provides:
//! - [`MessageCache`]: LRU-based cache for tracking seen message IDs
//! - [`DedupeFilter`]: Decorates a broadcast receiver to filter duplicates
//! - [`TtlTracker`]: Tracks message expiration based on TTL

use std::time::{Duration, Instant};

use adnet_types::Announcement;

/// Default maximum number of message IDs to track in the deduplication cache.
pub const DEFAULT_DEDUP_CACHE_SIZE: usize = 10_000;

/// Default TTL for messages without an explicit TTL.
pub const DEFAULT_MESSAGE_TTL: Duration = Duration::from_secs(3600);

/// Maximum number of expired messages to process per cleanup cycle.
const CLEANUP_BATCH_SIZE: usize = 100;

/// Message metadata for tracking deduplication and TTL.
#[derive(Debug, Clone)]
pub struct MessageMeta {
    /// When this message was first seen.
    pub seen_at: Instant,
    /// When this message expires.
    pub expires_at: Instant,
}

impl MessageMeta {
    pub fn new(ttl: Duration) -> Self {
        let now = Instant::now();
        Self {
            seen_at: now,
            expires_at: now + ttl,
        }
    }

    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

/// LRU-style cache for tracking seen message IDs.
///
/// Uses a simple Vec-based LRU approximation: when the cache is full,
/// we remove expired entries first, then remove the oldest entries
/// if still over capacity.
#[derive(Debug, Clone)]
pub struct MessageCache {
    /// Map from message ID to metadata.
    entries: std::collections::HashMap<String, MessageMeta>,
    /// Ordered list of message IDs (oldest first) for LRU eviction.
    order: Vec<String>,
    /// Maximum cache size.
    capacity: usize,
    #[allow(dead_code)]
    max_ttl: Duration,
}

impl Default for MessageCache {
    fn default() -> Self {
        Self::new(DEFAULT_DEDUP_CACHE_SIZE, DEFAULT_MESSAGE_TTL)
    }
}

impl MessageCache {
    /// Create a new cache with the given capacity and default TTL.
    pub fn new(capacity: usize, default_ttl: Duration) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            order: Vec::new(),
            capacity,
            max_ttl: default_ttl,
        }
    }

    /// Check if a message ID has been seen before.
    pub fn contains(&self, message_id: &str) -> bool {
        self.entries.contains_key(message_id)
    }

    /// Insert a message ID into the cache. Returns true if this was a new insertion.
    pub fn insert(&mut self, message_id: String, ttl: Duration) -> bool {
        if self.entries.contains_key(&message_id) {
            return false;
        }

        // Clean up if at capacity.
        self.evict_if_needed();

        let meta = MessageMeta::new(ttl);
        self.entries.insert(message_id.clone(), meta);
        self.order.push(message_id);
        true
    }

    /// Remove expired entries and evict oldest if still over capacity.
    fn evict_if_needed(&mut self) {
        // Remove expired entries.
        self.remove_expired();

        // Evict oldest entries if still over capacity.
        while self.entries.len() >= self.capacity && !self.order.is_empty() {
            let oldest = self.order.remove(0);
            self.entries.remove(&oldest);
        }
    }

    /// Remove all expired entries.
    pub fn remove_expired(&mut self) {
        let expired: Vec<String> = self
            .order
            .iter()
            .filter(|id| {
                self.entries
                    .get(id.as_str())
                    .map(|m| m.is_expired())
                    .unwrap_or(true)
            })
            .cloned()
            .take(CLEANUP_BATCH_SIZE)
            .collect();

        for id in &expired {
            self.entries.remove(id);
        }
        self.order.retain(|id| !expired.contains(id));
    }

    /// Get the number of entries in the cache.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear all entries from the cache.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

/// A deduplication filter that wraps a broadcast receiver.
///
/// Wraps incoming `Announcement` payloads and filters out duplicates
/// based on the message ID field. Uses a shared [`MessageCache`] to
/// track seen message IDs across multiple receivers.
#[derive(Debug, Clone)]
pub struct DedupeFilter {
    cache: MessageCache,
}

impl Default for DedupeFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl DedupeFilter {
    /// Create a new deduplication filter with default settings.
    pub fn new() -> Self {
        Self {
            cache: MessageCache::default(),
        }
    }

    /// Create a new deduplication filter with custom settings.
    pub fn with_capacity(capacity: usize, default_ttl: Duration) -> Self {
        Self {
            cache: MessageCache::new(capacity, default_ttl),
        }
    }

    /// Check if an announcement should be accepted (not a duplicate).
    /// If accepted, the message ID is inserted into the cache.
    pub fn check_and_insert(&mut self, ann: &Announcement) -> bool {
        // Generate message ID if not present.
        let mut ann = ann.clone();
        let message_id = ann.get_or_generate_message_id();

        // Get the TTL for this announcement.
        let ttl = ann.effective_ttl();

        // Check and insert.
        let is_new = self.cache.insert(message_id, ttl);

        // Periodically clean up expired entries (every 100 insertions).
        if self.cache.len() % 100 == 0 {
            self.cache.remove_expired();
        }

        is_new
    }

    /// Check if an announcement is a duplicate without inserting.
    pub fn is_duplicate(&self, ann: &Announcement) -> bool {
        let mut ann = ann.clone();
        let message_id = ann.get_or_generate_message_id();
        self.cache.contains(&message_id)
    }

    /// Clean up expired entries from the cache.
    pub fn cleanup(&mut self) {
        self.cache.remove_expired();
    }

    /// Get the current cache size.
    pub fn cache_size(&self) -> usize {
        self.cache.len()
    }

    /// Clear the deduplication cache.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }
}

/// TTL tracker for managing message expiration.
///
/// Tracks when messages should be removed from the system based on their TTL.
#[derive(Debug, Clone, Default)]
pub struct TtlTracker {
    /// Map from message ID to expiration time.
    expirations: std::collections::HashMap<String, Instant>,
    /// Default TTL.
    default_ttl: Duration,
}

impl TtlTracker {
    /// Create a new TTL tracker with the given default TTL.
    pub fn new(default_ttl: Duration) -> Self {
        Self {
            expirations: std::collections::HashMap::new(),
            default_ttl,
        }
    }

    /// Register a message with a specific TTL.
    pub fn register(&mut self, message_id: String, ttl: Duration) {
        let expires_at = Instant::now() + ttl;
        self.expirations.insert(message_id, expires_at);
    }

    /// Register a message with the default TTL.
    pub fn register_default(&mut self, message_id: String) {
        self.register(message_id, self.default_ttl);
    }

    /// Check if a message has expired.
    pub fn is_expired(&self, message_id: &str) -> bool {
        self.expirations
            .get(message_id)
            .map(|&expires_at| Instant::now() >= expires_at)
            .unwrap_or(false)
    }

    /// Get the remaining TTL for a message.
    pub fn remaining_ttl(&self, message_id: &str) -> Option<Duration> {
        self.expirations.get(message_id).map(|&expires_at| {
            let remaining = expires_at.saturating_duration_since(Instant::now());
            remaining
        })
    }

    /// Remove a message from the tracker.
    pub fn remove(&mut self, message_id: &str) {
        self.expirations.remove(message_id);
    }

    /// Clean up all expired messages.
    pub fn cleanup_expired(&mut self) -> Vec<String> {
        let now = Instant::now();
        let expired: Vec<String> = self
            .expirations
            .iter()
            .filter(|item| now >= *item.1)
            .map(|item| item.0.clone())
            .collect();

        for id in &expired {
            self.expirations.remove(id);
        }
        expired
    }

    /// Get the number of tracked messages.
    pub fn len(&self) -> usize {
        self.expirations.len()
    }

    /// Check if the tracker is empty.
    pub fn is_empty(&self) -> bool {
        self.expirations.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::{
        CdnContentKind, ContentHash, NodeId, RoomId,
    };
    use chrono::Utc;

    fn make_announcement() -> Announcement {
        Announcement {
            room_id: RoomId::new("test-room"),
            content_hash: ContentHash::from_bytes(b"test"),
            node_id: NodeId::random(),
            title: "Test".into(),
            kind: CdnContentKind::Article,
            size_bytes: 100,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            message_id: None,
            ttl_secs: Some(3600),
            signer: None,
            signature: None,
        }
    }

    // ─── MessageCache tests ────────────────────────────────────────────────────

    #[test]
    fn cache_insert_detects_duplicates() {
        let mut cache = MessageCache::default();
        let id = "msg1".to_string();

        assert!(cache.insert(id.clone(), DEFAULT_MESSAGE_TTL));
        assert!(!cache.insert(id.clone(), DEFAULT_MESSAGE_TTL));
    }

    #[test]
    fn cache_contains_works() {
        let mut cache = MessageCache::default();
        let id = "msg1".to_string();

        assert!(!cache.contains(&id));
        cache.insert(id.clone(), DEFAULT_MESSAGE_TTL);
        assert!(cache.contains(&id));
    }

    #[test]
    fn cache_evicts_at_capacity() {
        let mut cache = MessageCache::new(3, DEFAULT_MESSAGE_TTL);

        cache.insert("a".to_string(), DEFAULT_MESSAGE_TTL);
        cache.insert("b".to_string(), DEFAULT_MESSAGE_TTL);
        cache.insert("c".to_string(), DEFAULT_MESSAGE_TTL);
        assert_eq!(cache.len(), 3);

        cache.insert("d".to_string(), DEFAULT_MESSAGE_TTL);
        assert_eq!(cache.len(), 3);
        assert!(!cache.contains("a"));
    }

    #[test]
    fn cache_removes_expired() {
        let mut cache = MessageCache::new(100, Duration::from_millis(10));

        cache.insert("a".to_string(), Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(15));
        cache.insert("b".to_string(), Duration::from_millis(100));

        cache.remove_expired();
        assert!(!cache.contains("a"));
        assert!(cache.contains("b"));
    }

    #[test]
    fn cache_clear_works() {
        let mut cache = MessageCache::default();
        cache.insert("a".to_string(), DEFAULT_MESSAGE_TTL);
        cache.insert("b".to_string(), DEFAULT_MESSAGE_TTL);
        assert_eq!(cache.len(), 2);

        cache.clear();
        assert!(cache.is_empty());
    }

    // ─── DedupeFilter tests ───────────────────────────────────────────────────

    #[test]
    fn dedupe_filter_accepts_new_messages() {
        let mut filter = DedupeFilter::default();
        let ann = make_announcement();

        assert!(filter.check_and_insert(&ann));
    }

    #[test]
    fn dedupe_filter_rejects_duplicates() {
        let mut filter = DedupeFilter::default();
        let ann = make_announcement();

        assert!(filter.check_and_insert(&ann));
        assert!(!filter.check_and_insert(&ann));
    }

    #[test]
    fn dedupe_filter_different_messages_not_duplicates() {
        let mut filter = DedupeFilter::default();
        let mut ann1 = make_announcement();
        let mut ann2 = make_announcement();
        ann1.node_id = NodeId::random();
        ann2.node_id = NodeId::random();

        assert!(filter.check_and_insert(&ann1));
        assert!(filter.check_and_insert(&ann2));
    }

    #[test]
    fn dedupe_filter_cache_size_tracks_insertions() {
        let mut filter = DedupeFilter::default();
        let initial_size = filter.cache_size();

        for i in 0..10 {
            let mut ann = make_announcement();
            ann.message_id = Some(format!("msg-{}", i));
            filter.check_and_insert(&ann);
        }

        assert_eq!(filter.cache_size(), initial_size + 10);
    }

    // ─── TtlTracker tests ─────────────────────────────────────────────────────

    #[test]
    fn ttl_tracker_registers_and_expires() {
        let mut tracker = TtlTracker::new(Duration::from_millis(10));

        tracker.register_default("msg1".to_string());
        assert!(!tracker.is_expired("msg1"));

        std::thread::sleep(Duration::from_millis(15));
        assert!(tracker.is_expired("msg1"));
    }

    #[test]
    fn ttl_tracker_remaining_ttl() {
        let mut tracker = TtlTracker::new(Duration::from_secs(100));

        tracker.register("msg1".to_string(), Duration::from_secs(100));
        let remaining = tracker.remaining_ttl("msg1");
        assert!(remaining.is_some());
        assert!(remaining.unwrap() > Duration::from_secs(90));
    }

    #[test]
    fn ttl_tracker_cleanup_returns_expired() {
        let mut tracker = TtlTracker::new(Duration::from_millis(10));

        tracker.register_default("msg1".to_string());
        tracker.register_default("msg2".to_string());

        std::thread::sleep(Duration::from_millis(15));

        let expired = tracker.cleanup_expired();
        assert!(expired.contains(&"msg1".to_string()));
        assert!(tracker.is_empty());
    }

    #[test]
    fn ttl_tracker_remove_works() {
        let mut tracker = TtlTracker::default();
        tracker.register_default("msg1".to_string());

        assert!(!tracker.is_empty());
        tracker.remove("msg1");
        assert!(tracker.is_empty());
    }

    // ─── Integration tests ─────────────────────────────────────────────────────

    #[test]
    fn announcement_roundtrip_with_message_id() {
        let mut ann = make_announcement();
        let id = ann.get_or_generate_message_id();
        assert!(ann.message_id.is_some());
        assert_eq!(id.as_str(), ann.message_id.as_ref().unwrap().as_str());

        // Roundtrip through JSON.
        let json = serde_json::to_string(&ann).unwrap();
        let back: Announcement = serde_json::from_str(&json).unwrap();
        assert_eq!(back.message_id, ann.message_id);
    }

    #[test]
    fn deduplication_uses_generated_id_when_missing() {
        use chrono::Utc;

        // Create two announcements with identical fields (including timestamp).
        let timestamp = Utc::now();
        let ann1 = Announcement {
            room_id: RoomId::new("test-room"),
            content_hash: ContentHash::from_bytes(b"same-content"),
            node_id: NodeId::random(),
            title: "Test".into(),
            kind: CdnContentKind::Article,
            size_bytes: 100,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp,
            message_id: None,
            ttl_secs: Some(3600),
            signer: None,
            signature: None,
        };
        let ann2 = Announcement {
            room_id: ann1.room_id.clone(),
            content_hash: ann1.content_hash.clone(),
            node_id: ann1.node_id.clone(),
            title: ann1.title.clone(),
            kind: ann1.kind.clone(),
            size_bytes: ann1.size_bytes,
            mime_type: ann1.mime_type.clone(),
            source_url: ann1.source_url.clone(),
            ticket: ann1.ticket.clone(),
            timestamp: ann1.timestamp,
            message_id: None,
            ttl_secs: ann1.ttl_secs,
            signer: ann1.signer.clone(),
            signature: ann1.signature.clone(),
        };

        // Same announcement content (including timestamp) should generate same message ID.
        let id1 = ann1.generate_message_id();
        let id2 = ann2.generate_message_id();
        assert_eq!(id1, id2, "identical announcements should produce identical message IDs");
    }
}
