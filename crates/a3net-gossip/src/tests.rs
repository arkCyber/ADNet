// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Unit tests for a3net-gossip.

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_gossip::{
        GossipBus, InProcessGossip, TopicId,
        AccessControl, RoomAccessPolicy, CredentialType, AccessCheckResult,
        MessagePersistence, PersistenceConfig, StoredMessage,
        DedupeFilter, TtlTracker,
        Priority, MessageSource, determine_strategy, RetrievalStrategy,
    };
    use a3net_types::{
        Announcement, AnnouncementPayload, CdnContentKind, ContentHash,
        NodeId, RoomId, Topic,
    };
    use chrono::Utc;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    fn make_node_id() -> NodeId {
        NodeId::random()
    }

    fn make_room() -> RoomId {
        RoomId::from("test-room".to_string())
    }

    fn make_announcement(room: &RoomId, seq: u32) -> Announcement {
        let node_id = make_node_id();
        Announcement {
            room_id: room.clone(),
            content_hash: ContentHash::from_bytes(format!("content-{seq}").as_bytes()),
            node_id,
            title: format!("Announcement {seq}"),
            kind: CdnContentKind::Article,
            size_bytes: seq as u64,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            message_id: None,
            ttl_secs: None,
            signer: None,
            signature: None,
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // GossipBus tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_gossip_bus_new() {
        let node_id = make_node_id();
        let transport = Arc::new(InProcessGossip::new());
        let bus = GossipBus::new(node_id.clone(), transport);

        assert_eq!(bus.local_node(), &node_id);
    }

    #[tokio::test]
    async fn test_gossip_bus_topic_for() {
        let transport = Arc::new(InProcessGossip::new());
        let bus = GossipBus::new(make_node_id(), transport);
        let room = make_room();

        let topic = bus.topic_for(&room);
        assert!(topic.as_str().contains("room"));
        assert!(topic.as_str().contains("test-room"));
    }

    #[tokio::test]
    async fn test_gossip_bus_join_room() {
        let transport = Arc::new(InProcessGossip::new());
        let bus = GossipBus::new(make_node_id(), transport);
        let room = make_room();

        bus.join_room(&room).await.expect("join should succeed");
    }

    #[tokio::test]
    async fn test_gossip_bus_publish_and_subscribe() {
        let transport = Arc::new(InProcessGossip::new());
        let bus = GossipBus::new(make_node_id(), transport);
        let room = make_room();

        bus.join_room(&room).await.expect("join should succeed");

        // Subscribe before publishing
        let mut rx = bus.subscribe(&room);

        // Publish a message
        let ann = make_announcement(&room, 1);
        bus.publish(&room, &ann).await.expect("publish should succeed");

        // Should receive the message
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(received)) => {
                assert_eq!(received.title, "Announcement 1");
            }
            Ok(Err(e)) => panic!("recv error: {}", e),
            Err(_) => panic!("timeout waiting for message"),
        }
    }

    #[tokio::test]
    async fn test_gossip_bus_multiple_subscribers() {
        let transport = Arc::new(InProcessGossip::new());
        let bus = GossipBus::new(make_node_id(), transport);
        let room = make_room();

        bus.join_room(&room).await.expect("join should succeed");

        // Create multiple subscribers
        let mut rx1 = bus.subscribe(&room);
        let mut rx2 = bus.subscribe(&room);
        let mut rx3 = bus.subscribe(&room);

        // Publish a message
        let ann = make_announcement(&room, 42);
        bus.publish(&room, &ann).await.expect("publish should succeed");

        // All subscribers should receive
        for (i, rx) in [&mut rx1, &mut rx2, &mut rx3].iter().enumerate() {
            match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
                Ok(Ok(received)) => {
                    assert_eq!(received.title, "Announcement 42");
                }
                _ => panic!("subscriber {} failed to receive", i),
            }
        }
    }

    #[tokio::test]
    async fn test_gossip_bus_leave_room() {
        let transport = Arc::new(InProcessGossip::new());
        let bus = GossipBus::new(make_node_id(), transport);
        let room = make_room();

        bus.join_room(&room).await.expect("join should succeed");
        bus.leave_room(&room).await.expect("leave should succeed");
    }

    // ────────────────────────────────────────────────────────────────────
    // InProcessGossip tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_in_process_gossip_new() {
        let gossip = InProcessGossip::new();
        assert!(gossip.topics().is_empty());
    }

    #[tokio::test]
    async fn test_in_process_gossip_subscribe() {
        let gossip = InProcessGossip::new();
        let topic = Topic::from_label("test-topic");
        let node_id = make_node_id();

        gossip.join(topic.clone(), node_id.clone()).await.expect("join should succeed");
        let _rx = gossip.subscribe(topic.clone());
        assert!(gossip.topics().contains_key(&topic));
    }

    #[tokio::test]
    async fn test_in_process_gossip_broadcast() {
        let gossip = Arc::new(InProcessGossip::new());
        let topic = Topic::from_label("broadcast-test");
        let node_id = make_node_id();

        gossip.join(topic.clone(), node_id).await.expect("join should succeed");
        let mut rx = gossip.subscribe(topic.clone());

        let payload = b"test message".to_vec();
        gossip.broadcast(topic.clone(), payload.clone()).await.expect("broadcast should succeed");

        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(received)) => {
                assert_eq!(&received[..], &payload[..]);
            }
            _ => panic!("broadcast not received"),
        }
    }

    #[tokio::test]
    async fn test_in_process_gossip_leave() {
        let gossip = InProcessGossip::new();
        let topic = Topic::from_label("leave-test");
        let node_id = make_node_id();

        gossip.join(topic.clone(), node_id).await.expect("join should succeed");
        assert!(gossip.topics().contains_key(&topic));

        gossip.leave(topic.clone()).await.expect("leave should succeed");
        assert!(!gossip.topics().contains_key(&topic));
    }

    // ────────────────────────────────────────────────────────────────────
    // AccessControl tests
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn test_room_access_policy_open() {
        let policy = RoomAccessPolicy::Open;
        let result = policy.check(&CredentialType::None, None);
        assert_eq!(result, AccessCheckResult::Allowed);
    }

    #[test]
    fn test_room_access_policy_key() {
        let key = b"secret-key-12345".to_vec();
        let policy = RoomAccessPolicy::KeyRequired { key: key.clone() };

        // Correct key
        assert_eq!(
            policy.check(&CredentialType::Key(key.clone()), None),
            AccessCheckResult::Allowed
        );

        // Wrong key
        assert_eq!(
            policy.check(&CredentialType::Key(b"wrong-key".to_vec()), None),
            AccessCheckResult::Denied("Invalid key".to_string())
        );
    }

    #[test]
    fn test_room_access_policy_allowlist() {
        let allowed_ids: Vec<_> = (0..5).map(|_| NodeId::random()).collect();
        let policy = RoomAccessPolicy::AllowList {
            allowed: allowed_ids.clone(),
        };

        // Allowed node
        assert_eq!(
            policy.check(&CredentialType::NodeId(allowed_ids[0].clone()), None),
            AccessCheckResult::Allowed
        );

        // Disallowed node
        let unknown = NodeId::random();
        assert_eq!(
            policy.check(&CredentialType::NodeId(unknown), None),
            AccessCheckResult::Denied("Not on allow list".to_string())
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // DedupeFilter tests
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn test_dedupe_filter_new() {
        let filter = DedupeFilter::new(1000, 10);
        assert!(filter.is_empty());
    }

    #[test]
    fn test_dedupe_filter_insert_and_check() {
        let mut filter = DedupeFilter::new(1000, 10);
        let hash = [1u8; 32];

        assert!(!filter.contains(&hash));
        filter.insert(&hash);
        assert!(filter.contains(&hash));
    }

    #[test]
    fn test_dedupe_filter_eviction() {
        let mut filter = DedupeFilter::new(10, 3); // Small cache, max 3 entries

        // Insert more than capacity
        for i in 0..20 {
            let hash = [i; 32];
            filter.insert(&hash);
        }

        // Old entries should be evicted
        assert!(!filter.contains(&[0u8; 32]));
    }

    // ────────────────────────────────────────────────────────────────────
    // TtlTracker tests
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn test_ttl_tracker_new() {
        let tracker = TtlTracker::new(Duration::from_secs(60));
        assert!(tracker.is_empty());
    }

    #[test]
    fn test_ttl_tracker_insert_and_check() {
        let tracker = Arc::new(parking_lot::RwLock::new(TtlTracker::new(Duration::from_secs(60))));
        let mut tracker = tracker.write();

        let hash = [1u8; 32];
        tracker.insert(&hash);

        assert!(tracker.contains(&hash));
    }

    #[test]
    fn test_ttl_tracker_expired() {
        let tracker = Arc::new(parking_lot::RwLock::new(TtlTracker::new(Duration::from_millis(50))));
        let hash = [1u8; 32];

        {
            let mut tracker = tracker.write();
            tracker.insert(&hash);
        }

        // Immediately should exist
        {
            let tracker = tracker.read();
            assert!(tracker.contains(&hash));
        }

        // After expiration
        std::thread::sleep(Duration::from_millis(100));
        {
            let tracker = tracker.read();
            assert!(!tracker.contains(&hash));
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // RetrievalStrategy tests
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn test_determine_strategy() {
        // Priority high with local source
        let strategy = determine_strategy(
            Priority::High,
            MessageSource::Local,
        );
        assert_eq!(strategy, RetrievalStrategy::LocalFirst);

        // Priority low with remote source
        let strategy = determine_strategy(
            Priority::Low,
            MessageSource::Remote,
        );
        assert_eq!(strategy, RetrievalStrategy::WaitForBatch);

        // Priority normal with caching source
        let strategy = determine_strategy(
            Priority::Normal,
            MessageSource::Cache,
        );
        assert_eq!(strategy, RetrievalStrategy::CachePreferred);
    }

    // ────────────────────────────────────────────────────────────────────
    // PersistenceConfig tests
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn test_persistence_config_default() {
        let config = PersistenceConfig::default();
        assert_eq!(config.max_messages, 10000);
        assert_eq!(config.max_age, Duration::from_secs(86400));
    }

    #[test]
    fn test_persistence_config_custom() {
        let config = PersistenceConfig {
            max_messages: 5000,
            max_age: Duration::from_secs(3600),
            max_size_bytes: 100 * 1024 * 1024,
        };

        assert_eq!(config.max_messages, 5000);
        assert!(config.max_size_bytes.is_some());
    }

    // ────────────────────────────────────────────────────────────────────
    // Concurrent tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_concurrent_publish() {
        let transport = Arc::new(InProcessGossip::new());
        let bus = GossipBus::new(make_node_id(), transport);
        let room = make_room();

        bus.join_room(&room).await.expect("join should succeed");
        let mut rx = bus.subscribe(&room);

        // Publish from multiple tasks concurrently
        let bus1 = bus.clone();
        let bus2 = bus.clone();
        let room1 = room.clone();
        let room2 = room.clone();

        let h1 = tokio::spawn(async move {
            for i in 0..5 {
                let ann = make_announcement(&room1, i);
                bus1.publish(&room1, &ann).await.unwrap();
            }
        });

        let h2 = tokio::spawn(async move {
            for i in 5..10 {
                let ann = make_announcement(&room2, i);
                bus2.publish(&room2, &ann).await.unwrap();
            }
        });

        h1.await.unwrap();
        h2.await.unwrap();

        // Should receive all messages
        let mut received = 0;
        while received < 10 {
            match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
                Ok(Ok(_)) => received += 1,
                Ok(Err(broadcast::error::RecvError::Lagged(n))) => received += n as usize,
                Ok(Err(broadcast::error::RecvError::Closed)) => break,
                Err(_) => break,
            }
        }

        assert_eq!(received, 10);
    }
}

// Import time for timeout
use std::time::Duration;
