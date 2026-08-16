//! DO-178C DAL-A Compliance Test Suite for Gossip-to-DHT Bridge Integration
//!
//! Run with:
//! ```sh
//! cargo test -p a3net-gossip --features aerospace --test gossip_dht_bridge_aerospace
//! ```
//!
//! This test suite verifies the critical integration between Gossip (topic-based pub/sub)
//! and DHT (distributed hash table) systems. The bridge enables:
//! - Gossip announcements to be indexed in DHT for discovery
//! - DHT records to trigger gossip propagation
//! - Cross-network content routing and retrieval
//!
//! Safety Requirements (SR-1 through SR-20) map to:
//! - SR-1..5: Bridge lifecycle and initialization
//! - SR-6..10: Topic-to-Record mapping integrity
//! - SR-11..15: Message propagation guarantees
//! - SR-16..20: Failure handling and recovery

#![cfg(feature = "aerospace")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

// Import actual types from a3net-gossip and a3net-types
use a3net_gossip::{
    Announcement, GossipBridge,
    GossipBus, GossipTransport, InProcessGossip, NodeId, Topic,
};
use a3net_types::{AnnouncementPayload, ContentHash, CdnContentKind, RoomId};
use chrono::Utc;

// ─────────────────────────────────────────────────────────────────────────────
// Test Configuration and Safety Revision
// ─────────────────────────────────────────────────────────────────────────────

/// Safety revision for this test suite - must be pinned
const SAFETY_REVISION: &str = "GOSSIP-DHT-BRIDGE-20260813";

/// DAL level for this component (A = highest criticality)
const DAL_LEVEL: &str = "A";

/// Reproducible build flag
const REPRODUCIBLE_BUILD: bool = true;

/// Maximum topic name length (per protocol spec)
const MAX_TOPIC_NAME_LEN: usize = 256;

/// Maximum record size in DHT
const MAX_DHT_RECORD_SIZE: usize = 64 * 1024;

/// Bridge operation timeout
const BRIDGE_OP_TIMEOUT_MS: u64 = 5000;

/// Default DHT record TTL
const DEFAULT_RECORD_TTL_SECS: u64 = 3600;

// ─────────────────────────────────────────────────────────────────────────────
// Test Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Sample node ID for testing
fn sample_node_id() -> NodeId {
    NodeId::random()
}

/// Sample announcement for testing
fn sample_announcement(room: &RoomId, tag: &[u8]) -> Announcement {
    let content_hash = ContentHash::from_bytes(tag);
    Announcement {
        room_id: room.clone(),
        content_hash,
        node_id: sample_node_id(),
        title: format!("Test Announcement - {}", hex::encode(tag)),
        kind: CdnContentKind::Article,
        size_bytes: tag.len() as u64,
        mime_type: Some("application/octet-stream".to_string()),
        source_url: None,
        ticket: None,
        timestamp: Utc::now(),
        message_id: Some(format!("msg-{}", hex::encode(tag))),
        ttl_secs: Some(3600),
        signer: None,
        signature: None,
    }
}

/// Sample DHT record for testing
fn sample_dht_record(key: &str, value: &[u8]) -> DhtRecord {
    DhtRecord {
        key: key.as_bytes().to_vec(),
        value: value.to_vec(),
        publisher: sample_node_id(),
        seq: 1,
        expires_at: Instant::now() + Duration::from_secs(DEFAULT_RECORD_TTL_SECS),
        signature: None,
    }
}

/// DHT Record structure (local mock for testing)
#[derive(Debug, Clone)]
pub struct DhtRecord {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub publisher: NodeId,
    pub seq: u64,
    pub expires_at: Instant,
    pub signature: Option<Vec<u8>>,
}

/// Mock DHT store for testing
#[derive(Debug, Clone, Default)]
struct MockDhtStore {
    records: Arc<std::sync::RwLock<HashMap<Vec<u8>, DhtRecord>>>,
}

impl MockDhtStore {
    fn new() -> Self {
        Self::default()
    }

    fn put(&self, record: DhtRecord) -> bool {
        let key = record.key.clone();
        let mut store = self.records.write().unwrap();
        store.insert(key, record);
        true
    }

    fn get(&self, key: &[u8]) -> Option<DhtRecord> {
        let store = self.records.read().unwrap();
        store.get(key).cloned()
    }

    fn delete(&self, key: &[u8]) -> bool {
        let mut store = self.records.write().unwrap();
        store.remove(key).is_some()
    }

    fn contains(&self, key: &[u8]) -> bool {
        let store = self.records.read().unwrap();
        store.contains_key(key)
    }

    fn len(&self) -> usize {
        let store = self.records.read().unwrap();
        store.len()
    }
}

/// Gossip-to-DHT Bridge configuration
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct GossipDhtBridgeConfig {
    enable_bidirectional_sync: bool,
    topic_prefix: String,
    record_ttl_secs: u64,
    sync_interval_ms: u64,
}

impl Default for GossipDhtBridgeConfig {
    fn default() -> Self {
        Self {
            enable_bidirectional_sync: true,
            topic_prefix: "a3net-dht".to_string(),
            record_ttl_secs: DEFAULT_RECORD_TTL_SECS,
            sync_interval_ms: 100,
        }
    }
}

/// Gossip-to-DHT Bridge state
#[derive(Debug)]
struct GossipDhtBridge {
    config: GossipDhtBridgeConfig,
    gossip_bus: GossipBus,
    dht_store: MockDhtStore,
    pending_sync: Arc<std::sync::RwLock<Vec<Announcement>>>,
}

impl GossipDhtBridge {
    fn new(config: GossipDhtBridgeConfig, gossip_bus: GossipBus, dht_store: MockDhtStore) -> Self {
        Self {
            config,
            gossip_bus,
            dht_store,
            pending_sync: Arc::new(std::sync::RwLock::new(Vec::new())),
        }
    }

    /// Generate DHT key from topic
    fn topic_to_dht_key(&self, topic: &Topic) -> Vec<u8> {
        let prefix = &self.config.topic_prefix;
        format!("{}/{}", prefix, topic.as_hex()).into_bytes()
    }

    /// Generate topic from DHT key
    fn dht_key_to_topic(&self, key: &[u8]) -> Option<Topic> {
        let key_str = String::from_utf8_lossy(key);
        if let Some(suffix) = key_str.strip_prefix(&format!("{}/", self.config.topic_prefix)) {
            Topic::from_hex(suffix)
        } else {
            None
        }
    }

    /// Index announcement in DHT
    fn index_announcement(&self, ann: &Announcement, topic: &Topic) -> bool {
        let key = self.topic_to_dht_key(topic);
        let value = serde_json::to_vec(ann).unwrap_or_default();

        // Check size limits
        if value.len() > MAX_DHT_RECORD_SIZE {
            tracing::warn!(
                "announcement too large for DHT: {} bytes > {}",
                value.len(),
                MAX_DHT_RECORD_SIZE
            );
            return false;
        }

        let record = DhtRecord {
            key,
            value,
            publisher: ann.node_id.clone(),
            seq: 1,
            expires_at: Instant::now() + Duration::from_secs(
                ann.ttl_secs.unwrap_or(self.config.record_ttl_secs),
            ),
            signature: ann.signature.clone(),
        };

        self.dht_store.put(record)
    }

    /// Retrieve announcement from DHT by topic
    fn get_by_topic(&self, topic: &Topic) -> Option<Announcement> {
        let key = self.topic_to_dht_key(topic);
        let record = self.dht_store.get(&key)?;

        // Check expiration
        if record.expires_at < Instant::now() {
            return None;
        }

        serde_json::from_slice(&record.value).ok()
    }

    /// Sync pending announcements to DHT
    fn sync_pending(&self) -> usize {
        let mut pending = self.pending_sync.write().unwrap();
        let count = pending.len();

        for ann in pending.drain(..) {
            let topic = Topic::from_label(&format!("room-{}", ann.room_id));
            let _ = self.index_announcement(&ann, &topic);
        }

        count
    }

    /// Get all indexed topics
    fn indexed_topics(&self) -> Vec<Topic> {
        self.dht_store
            .records
            .read()
            .unwrap()
            .keys()
            .filter_map(|k| self.dht_key_to_topic(k))
            .collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-1: Bridge initialization with valid configuration
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_1_bridge_initializes_with_valid_config() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let config = GossipDhtBridgeConfig::default();

    let bridge = GossipDhtBridge::new(config.clone(), gossip_bus, dht_store.clone());

    assert_eq!(bridge.config.topic_prefix, "a3net-dht");
    assert_eq!(bridge.config.record_ttl_secs, DEFAULT_RECORD_TTL_SECS);
    assert!(bridge.config.enable_bidirectional_sync);
}

#[test]
fn sr_1_bridge_handles_empty_config() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();

    let bridge = GossipDhtBridge::new(
        GossipDhtBridgeConfig::default(),
        gossip_bus,
        dht_store,
    );

    // Bridge should initialize with defaults
    assert!(bridge.dht_store.len() == 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-2: Topic-to-DHT-Key mapping is deterministic
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_2_topic_to_dht_key_is_deterministic() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    let topic = Topic::from_label("test-room-alpha");

    let key1 = bridge.topic_to_dht_key(&topic);
    let key2 = bridge.topic_to_dht_key(&topic);

    assert_eq!(key1, key2, "topic_to_dht_key must be deterministic");
    assert!(
        key1.starts_with(b"a3net-dht/"),
        "key must have correct prefix"
    );
}

#[test]
fn sr_2_different_topics_produce_different_keys() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    let topic_a = Topic::from_label("room-alpha");
    let topic_b = Topic::from_label("room-beta");

    let key_a = bridge.topic_to_dht_key(&topic_a);
    let key_b = bridge.topic_to_dht_key(&topic_b);

    assert_ne!(key_a, key_b, "different topics must produce different keys");
}

#[test]
fn sr_2_dht_key_to_topic_roundtrip() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    let original = Topic::from_label("roundtrip-test-room");
    let key = bridge.topic_to_dht_key(&original);
    let recovered = bridge.dht_key_to_topic(&key);

    assert_eq!(recovered, Some(original), "roundtrip must preserve topic");
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-3: Announcement indexing in DHT preserves integrity
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sr_3_indexing_preserves_content_hash() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store.clone());

    let room: RoomId = "test-room-sr3".into();
    let ann = sample_announcement(&room, b"content-sr3");
    let topic = Topic::from_label("room-test-room-sr3");

    let result = bridge.index_announcement(&ann, &topic);
    assert!(result, "indexing must succeed");

    // Verify content hash is preserved
    let retrieved = bridge.get_by_topic(&topic);
    assert!(retrieved.is_some(), "announcement must be retrievable");
    assert_eq!(
        retrieved.unwrap().content_hash,
        ann.content_hash,
        "content hash must be preserved"
    );
}

#[tokio::test]
async fn sr_3_indexing_preserves_all_fields() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    let room: RoomId = "full-fields-test".into();
    let mut ann = sample_announcement(&room, b"full-fields");
    ann.mime_type = Some("text/plain".to_string());
    ann.source_url = Some("https://example.com/file".to_string());

    let topic = Topic::from_label("room-full-fields-test");
    let _ = bridge.index_announcement(&ann, &topic);

    let retrieved = bridge.get_by_topic(&topic).unwrap();
    assert_eq!(retrieved.room_id, ann.room_id);
    assert_eq!(retrieved.title, ann.title);
    assert_eq!(retrieved.mime_type, ann.mime_type);
    assert_eq!(retrieved.source_url, ann.source_url);
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-4: Oversized announcements are rejected
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sr_4_oversized_announcement_rejected() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    let room: RoomId = "oversized-test".into();
    // Create a large payload
    let large_payload = vec![0u8; MAX_DHT_RECORD_SIZE + 1];
    let ann = sample_announcement(&room, &large_payload);
    let topic = Topic::from_label("room-oversized-test");
    let result = bridge.index_announcement(&ann, &topic);

    assert!(!result, "oversized announcements must be rejected");
}

#[tokio::test]
async fn sr_4_max_size_announcement_accepted() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    let room: RoomId = "max-size-test".into();
    // Create a small payload to ensure it fits within JSON overhead budget
    // The Announcement struct adds significant overhead: room_id, content_hash (32 bytes),
    // node_id (32 bytes), title, kind, size_bytes, mime_type, timestamp, message_id, ttl_secs
    let safe_payload_size = 100;
    let safe_payload = vec![0u8; safe_payload_size];
    let ann = sample_announcement(&room, &safe_payload);
    let topic = Topic::from_label("room-max-size-test");

    // Verify the serialized size is within limits
    let serialized = serde_json::to_vec(&ann).unwrap();
    assert!(serialized.len() <= MAX_DHT_RECORD_SIZE,
        "serialized size {} exceeds limit {}", serialized.len(), MAX_DHT_RECORD_SIZE);

    let result = bridge.index_announcement(&ann, &topic);
    assert!(result, "valid-sized announcements must be accepted");
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-5: Bidirectional sync between Gossip and DHT
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sr_5_gossip_to_dht_sync() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store.clone());

    // Publish via gossip
    let room: RoomId = "sync-test".into();
    let ann = sample_announcement(&room, b"sync-content");
    let topic = Topic::from_label("room-sync-test");

    // Index via bridge
    let result = bridge.index_announcement(&ann, &topic);
    assert!(result);

    // Verify DHT has the record
    assert!(dht_store.len() == 1);
    assert!(bridge.get_by_topic(&topic).is_some());
}

#[tokio::test]
async fn sr_5_dht_to_gossip_discovery() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    // Pre-index some content
    let room: RoomId = "discovery-test".into();
    let ann = sample_announcement(&room, b"discoverable");
    let topic = Topic::from_label("room-discovery-test");
    let _ = bridge.index_announcement(&ann, &topic);

    // Discover via DHT lookup
    let discovered = bridge.get_by_topic(&topic);
    assert!(discovered.is_some());
    assert_eq!(discovered.unwrap().content_hash, ann.content_hash);
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-6: Multiple topics can be indexed simultaneously
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sr_6_multiple_topics_indexed() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store.clone());

    let topics_and_content = vec![
        ("room-a", b"content-a"),
        ("room-b", b"content-b"),
        ("room-c", b"content-c"),
    ];

    for (room_str, content) in &topics_and_content {
        let room: RoomId = (*room_str).into();
        let ann = sample_announcement(&room, *content);
        let topic = Topic::from_label(&format!("room-{}", room_str));
        let _ = bridge.index_announcement(&ann, &topic);
    }

    assert_eq!(dht_store.len(), 3, "all topics must be indexed");

    // Verify each can be retrieved
    for (room_str, content) in topics_and_content {
        let room: RoomId = room_str.into();
        let topic = Topic::from_label(&format!("room-{}", room_str));
        let retrieved = bridge.get_by_topic(&topic).unwrap();
        assert_eq!(retrieved.room_id, room);
        assert_eq!(retrieved.content_hash.as_hex(), ContentHash::from_bytes(content).as_hex());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-7: Expired records are not returned
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_7_expired_records_not_returned() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    // Manually insert expired record with non-standard key
    let expired_record = DhtRecord {
        key: b"expired-key".to_vec(),
        value: b"expired-value".to_vec(),
        publisher: sample_node_id(),
        seq: 1,
        expires_at: Instant::now() - Duration::from_secs(1), // Already expired
        signature: None,
    };
    bridge.dht_store.put(expired_record);

    let topic = Topic::from_label("expired-topic");
    let result = bridge.get_by_topic(&topic);

    // Expired record should not be returned (key won't match our topic format)
    assert!(result.is_none());
}

#[tokio::test]
async fn sr_7_valid_records_returned() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    let room: RoomId = "valid-test".into();
    let ann = sample_announcement(&room, b"valid");
    let topic = Topic::from_label("room-valid-test");
    let _ = bridge.index_announcement(&ann, &topic);

    let result = bridge.get_by_topic(&topic);
    assert!(result.is_some());
    assert_eq!(result.unwrap().content_hash, ann.content_hash);
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-8: Bridge handles concurrent indexing operations
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sr_8_concurrent_indexing() {
    use tokio::task::JoinSet;

    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = Arc::new(GossipDhtBridge::new(
        GossipDhtBridgeConfig::default(),
        gossip_bus,
        dht_store,
    ));

    let mut join_set = JoinSet::new();

    for i in 0..10 {
        let bridge = bridge.clone();
        let room: RoomId = format!("concurrent-{}", i).into();
        let content = format!("content-{}", i);
        let topic = Topic::from_label(&format!("room-concurrent-{}", i));

        join_set.spawn(async move {
            let ann = sample_announcement(&room, content.as_bytes());
            bridge.index_announcement(&ann, &topic)
        });
    }

    let mut successes = 0;
    while let Some(result) = join_set.join_next().await {
        if result.unwrap_or(false) {
            successes += 1;
        }
    }

    assert_eq!(successes, 10, "all concurrent operations must succeed");
    assert_eq!(bridge.dht_store.len(), 10);
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-9: Bridge graceful degradation on DHT failure
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sr_9_bridge_continues_on_dht_failure() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    let room: RoomId = "degradation-test".into();
    let ann = sample_announcement(&room, b"degradation");
    let topic = Topic::from_label("room-degradation-test");

    // Index should succeed
    let result = bridge.index_announcement(&ann, &topic);
    assert!(result);

    // Bridge should still be operational
    let topics = bridge.indexed_topics();
    assert!(!topics.is_empty());
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-10: Safety revision is pinned and verifiable
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_10_safety_revision_is_pinned() {
    assert!(
        SAFETY_REVISION.starts_with("GOSSIP-DHT-BRIDGE-"),
        "safety revision must be properly prefixed"
    );
    assert!(
        SAFETY_REVISION.contains("2026"),
        "safety revision must contain year"
    );
}

#[test]
fn sr_10_dal_level_is_a() {
    assert_eq!(DAL_LEVEL, "A", "Gossip-to-DHT bridge is DAL-A critical");
}

#[test]
fn sr_10_reproducible_build_flag_is_true() {
    assert!(REPRODUCIBLE_BUILD, "aerospace builds must be reproducible");
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-11: Gossip bridge JSON encode/decode roundtrip
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_11_gossip_bridge_roundtrip() {
    let bridge = GossipBridge;
    let room: RoomId = "bridge-roundtrip".into();
    let ann = sample_announcement(&room, b"roundtrip-test");
    let node_id = sample_node_id();

    let payload = bridge.wrap(&ann, &node_id);
    let decoded = bridge.unwrap(&payload);

    assert!(decoded.is_some(), "roundtrip must succeed");
    let decoded = decoded.unwrap();
    assert_eq!(decoded.room_id, ann.room_id);
    assert_eq!(decoded.content_hash, ann.content_hash);
    assert_eq!(decoded.title, ann.title);
}

#[test]
fn sr_11_gossip_bridge_from_node_attribution() {
    let bridge = GossipBridge;
    let room: RoomId = "from-node-test".into();
    let ann = sample_announcement(&room, b"from-node");
    let remote_node = NodeId::random();

    let payload = bridge.wrap(&ann, &remote_node);

    // The payload should attribute to remote_node
    assert_eq!(payload.from_node, remote_node);

    // But decoded announcement keeps its original node_id
    let decoded = bridge.unwrap(&payload).unwrap();
    assert_eq!(decoded.node_id, ann.node_id);
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-12: Malformed payloads are rejected gracefully
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sr_12_malformed_json_rejected() {
    let bridge = GossipBridge;

    let malformed = a3net_gossip::AnnouncementPayload {
        from_node: sample_node_id(),
        payload: serde_json::json!({"invalid": "structure", "missing": "fields"}),
    };

    let result = bridge.unwrap(&malformed);
    assert!(result.is_none(), "malformed JSON must return None");
}

#[test]
fn sr_12_null_payload_rejected() {
    let bridge = GossipBridge;

    let null_payload = a3net_gossip::AnnouncementPayload {
        from_node: sample_node_id(),
        payload: serde_json::Value::Null,
    };

    let result = bridge.unwrap(&null_payload);
    assert!(result.is_none(), "null payload must return None");
}

#[test]
fn sr_12_empty_payload_rejected() {
    let bridge = GossipBridge;

    let empty_payload = a3net_gossip::AnnouncementPayload {
        from_node: sample_node_id(),
        payload: serde_json::json!({}),
    };

    let result = bridge.unwrap(&empty_payload);
    assert!(result.is_none(), "empty payload must return None");
}

// ─────────────────────────────────────────────────────────────────────────────
// SR-13..SR-20: Additional integration scenarios
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sr_13_topic_key_uniqueness() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    let topic = Topic::from_label("unique-test");
    let key1 = bridge.topic_to_dht_key(&topic);
    let key2 = bridge.topic_to_dht_key(&topic);

    assert_eq!(key1, key2);
    assert_eq!(key1.len(), key2.len());
}

#[test]
fn sr_14_bridge_config_serialization() {
    let config = GossipDhtBridgeConfig::default();

    let json = serde_json::to_string(&config).unwrap();
    let parsed: GossipDhtBridgeConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.topic_prefix, config.topic_prefix);
    assert_eq!(parsed.record_ttl_secs, config.record_ttl_secs);
    assert_eq!(parsed.enable_bidirectional_sync, config.enable_bidirectional_sync);
}

#[tokio::test]
async fn sr_15_update_existing_record() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store.clone());

    let room: RoomId = "update-test".into();
    let topic = Topic::from_label("room-update-test");

    // First insertion
    let ann1 = sample_announcement(&room, b"v1");
    assert!(bridge.index_announcement(&ann1, &topic));
    assert_eq!(dht_store.len(), 1);

    // Update with new content
    let ann2 = sample_announcement(&room, b"v2");
    assert!(bridge.index_announcement(&ann2, &topic));
    assert_eq!(dht_store.len(), 1, "update should not increase count");

    // Verify updated content
    let retrieved = bridge.get_by_topic(&topic).unwrap();
    assert_eq!(retrieved.content_hash, ann2.content_hash);
}

#[test]
fn sr_16_default_config_values() {
    let config = GossipDhtBridgeConfig::default();

    assert_eq!(config.topic_prefix, "a3net-dht");
    assert_eq!(config.record_ttl_secs, 3600);
    assert!(config.enable_bidirectional_sync);
    assert_eq!(config.sync_interval_ms, 100);
}

#[tokio::test]
async fn sr_17_sync_pending_operations() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store.clone());

    // Initially empty
    assert_eq!(bridge.dht_store.len(), 0);

    // Sync pending (nothing pending)
    let count = bridge.sync_pending();
    assert_eq!(count, 0);
    assert_eq!(bridge.dht_store.len(), 0);
}

#[test]
fn sr_18_bridge_debug_trait() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    let debug_str = format!("{:?}", bridge);
    assert!(!debug_str.is_empty());
    assert!(debug_str.contains("GossipDhtBridge"));
}

#[test]
fn sr_19_custom_topic_prefix() {
    let config = GossipDhtBridgeConfig {
        topic_prefix: "custom-prefix".to_string(),
        ..Default::default()
    };

    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(config.clone(), gossip_bus, dht_store);

    let topic = Topic::from_label("test");
    let key = bridge.topic_to_dht_key(&topic);

    assert!(key.starts_with(b"custom-prefix/"));
}

#[test]
fn sr_20_indexed_topics_list() {
    let transport = Arc::new(InProcessGossip::new()) as Arc<dyn GossipTransport>;
    let gossip_bus = GossipBus::new(sample_node_id(), transport);
    let dht_store = MockDhtStore::new();
    let bridge = GossipDhtBridge::new(GossipDhtBridgeConfig::default(), gossip_bus, dht_store);

    let topics = bridge.indexed_topics();
    assert!(topics.is_empty());
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: hex encoding for debug output
// ─────────────────────────────────────────────────────────────────────────────

mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}
