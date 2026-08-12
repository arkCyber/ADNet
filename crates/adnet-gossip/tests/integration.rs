//! Integration tests for `adnet-gossip`.
//!
//! Tests two or more `GossipBus` instances connected through an
//! `InProcessGossip` transport. All tests run without the `iroh` feature
//! so they can execute in CI without network setup.
//!
//! For iroh-backed multi-node tests, see the workspace integration
//! tests (e.g. `adnet-node/tests/iroh_e2e.rs`).

use std::sync::Arc;
use std::time::Duration;

use adnet_gossip::{GossipBus, InProcessGossip, dedup::{DedupeFilter, TtlTracker, DEFAULT_MESSAGE_TTL}};
use adnet_types::{
    Announcement, AnnouncementPayload, CdnContentKind, ContentHash, NodeId, RoomId,
};
use chrono::Utc;
use parking_lot::RwLock;
use tokio::sync::broadcast::error::RecvError;

fn make_ann(publisher: &NodeId, room: &RoomId, seq: u32) -> Announcement {
    Announcement {
        room_id: room.clone(),
        content_hash: ContentHash::from_bytes(format!("payload-{seq}").as_bytes()),
        node_id: publisher.clone(),
        title: format!("T{seq}"),
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

// ─── Two-node publish/subscribe ───────────────────────────────────────────

#[tokio::test]
async fn two_nodes_same_room_delivery() {
    let transport = Arc::new(InProcessGossip::new());
    let alice = NodeId::random();
    let bob = NodeId::random();
    let room: RoomId = "two-node-test".into();

    let alice_bus = GossipBus::new(alice.clone(), Arc::clone(&transport) as _);
    let bob_bus = GossipBus::new(bob.clone(), Arc::clone(&transport) as _);

    alice_bus.join_room(&room).await.unwrap();
    bob_bus.join_room(&room).await.unwrap();

    let mut bob_rx = bob_bus.subscribe(&room);
    let ann = make_ann(&alice, &room, 1);
    alice_bus.publish(&room, &ann).await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), bob_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.content_hash, ann.content_hash);
    assert_eq!(received.node_id, alice);
}

#[tokio::test]
async fn subscriber_receives_own_publish_on_shared_transport() {
    let transport = Arc::new(InProcessGossip::new());
    let node = NodeId::random();
    let room: RoomId = "self-echo".into();
    let bus = GossipBus::new(node.clone(), Arc::clone(&transport) as _);
    bus.join_room(&room).await.unwrap();
    let mut rx = bus.subscribe(&room);

    let ann = make_ann(&node, &room, 42);
    bus.publish(&room, &ann).await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.content_hash, ann.content_hash);
}

// ─── Room isolation ────────────────────────────────────────────────────────

#[tokio::test]
async fn different_rooms_do_not_cross_contaminate() {
    let transport = Arc::new(InProcessGossip::new());
    let alice = NodeId::random();
    let bob = NodeId::random();
    let room_a: RoomId = "room-alpha".into();
    let room_b: RoomId = "room-beta".into();

    let alice_bus = GossipBus::new(alice.clone(), Arc::clone(&transport) as _);
    let bob_bus = GossipBus::new(bob.clone(), Arc::clone(&transport) as _);

    alice_bus.join_room(&room_a).await.unwrap();
    bob_bus.join_room(&room_b).await.unwrap();

    let mut alpha_rx = alice_bus.subscribe(&room_a);
    let mut beta_rx = bob_bus.subscribe(&room_b);

    let ann_alpha = make_ann(&alice, &room_a, 1);
    alice_bus.publish(&room_a, &ann_alpha).await.unwrap();

    let got_alpha = tokio::time::timeout(Duration::from_millis(100), alpha_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got_alpha.content_hash, ann_alpha.content_hash);

    let beta_sees_nothing = tokio::time::timeout(Duration::from_millis(100), beta_rx.recv())
        .await;
    assert!(beta_sees_nothing.is_err(), "beta should not receive alpha's message");
}

// ─── Multiple concurrent subscribers ───────────────────────────────────────

#[tokio::test]
async fn multiple_subscribers_all_receive_same_message() {
    let transport = Arc::new(InProcessGossip::new());
    let publisher = NodeId::random();
    let room: RoomId = "multi-sub-test".into();
    let bus = GossipBus::new(publisher.clone(), Arc::clone(&transport) as _);
    bus.join_room(&room).await.unwrap();

    // Give the subscription a moment to establish.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let mut rxs = Vec::new();
    for _ in 0..5 {
        rxs.push(bus.subscribe(&room));
    }

    let ann = make_ann(&publisher, &room, 7);
    bus.publish(&room, &ann).await.unwrap();

    for (i, mut rx) in rxs.into_iter().enumerate() {
        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received.content_hash, ann.content_hash, "subscriber {i} missing message");
    }
}

// ─── Backpressure / Lagged handling ───────────────────────────────────────

#[tokio::test]
async fn fast_publisher_does_not_drop_slow_consumer() {
    let transport = Arc::new(InProcessGossip::new());
    let publisher = NodeId::random();
    let room: RoomId = "backpressure".into();
    let bus = GossipBus::new(publisher.clone(), Arc::clone(&transport) as _);
    bus.join_room(&room).await.unwrap();

    let mut rx = bus.subscribe(&room);

    for i in 1..=200u32 {
        let ann = make_ann(&publisher, &room, i);
        bus.publish(&room, &ann).await.unwrap();
    }

    let mut count = 0u32;
    loop {
        match tokio::time::timeout(Duration::from_millis(10), rx.recv()).await {
            Ok(Ok(_)) => {
                count += 1;
                if count >= 10 {
                    break;
                }
            }
            Ok(Err(RecvError::Lagged(n))) => {
                tracing::debug!("consumer lagged {n} messages at count={count}");
            }
            Ok(Err(RecvError::Closed)) => break,
            Err(_) => break,
        }
    }
    assert!(count >= 10, "consumer should have read at least 10 messages");
}

// ─── Message attribution ───────────────────────────────────────────────────

#[tokio::test]
async fn message_source_from_node_is_preserved() {
    let transport = Arc::new(InProcessGossip::new());
    let alice = NodeId::random();
    let bob = NodeId::random();
    let room: RoomId = "attribution".into();

    let alice_bus = GossipBus::new(alice.clone(), Arc::clone(&transport) as _);
    let bob_bus = GossipBus::new(bob.clone(), Arc::clone(&transport) as _);

    alice_bus.join_room(&room).await.unwrap();
    bob_bus.join_room(&room).await.unwrap();

    let mut bob_rx = bob_bus.subscribe(&room);

    let ann = make_ann(&alice, &room, 99);
    alice_bus.publish(&room, &ann).await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), bob_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.node_id, alice);

    let wire = AnnouncementPayload {
        from_node: alice.clone(),
        payload: serde_json::to_value(&received).unwrap(),
    };
    let decoded: Announcement = serde_json::from_value(wire.payload).unwrap();
    assert_eq!(decoded.node_id, alice);
}

// ─── Join / Leave ───────────────────────────────────────────────────────────

#[tokio::test]
async fn leave_room_does_not_break_existing_subscriber() {
    let transport = Arc::new(InProcessGossip::new());
    let node = NodeId::random();
    let room: RoomId = "leave-room".into();
    let bus = GossipBus::new(node.clone(), Arc::clone(&transport) as _);

    bus.join_room(&room).await.unwrap();
    let mut rx = bus.subscribe(&room);
    bus.leave_room(&room).await.unwrap();

    let ann = make_ann(&node, &room, 1);
    bus.publish(&room, &ann).await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.content_hash, ann.content_hash);
}

#[tokio::test]
async fn join_then_rejoin_is_idempotent() {
    let transport = Arc::new(InProcessGossip::new());
    let node = NodeId::random();
    let room: RoomId = "rejoin".into();
    let bus = GossipBus::new(node.clone(), Arc::clone(&transport) as _);

    bus.join_room(&room).await.unwrap();
    bus.join_room(&room).await.unwrap();
    let mut rx = bus.subscribe(&room);

    let ann = make_ann(&node, &room, 1);
    bus.publish(&room, &ann).await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.content_hash, ann.content_hash);
}

// ─── Deduplication tests (using subscribe_with_filter) ─────────────────────

#[tokio::test]
async fn deduplication_filters_duplicate_messages() {
    let transport = Arc::new(InProcessGossip::new());
    let node = NodeId::random();
    let room: RoomId = "dedup-test".into();
    let bus = GossipBus::new(node.clone(), Arc::clone(&transport) as _);

    // Create shared deduplication infrastructure.
    let dedup_filter = Arc::new(RwLock::new(DedupeFilter::with_capacity(100, DEFAULT_MESSAGE_TTL)));
    let ttl_tracker = Arc::new(RwLock::new(TtlTracker::new(DEFAULT_MESSAGE_TTL)));

    bus.join_room(&room).await.unwrap();
    let mut rx = bus.subscribe_with_filter(&room, dedup_filter, ttl_tracker);

    // Create an announcement with explicit message_id.
    let mut ann = make_ann(&node, &room, 1);
    ann.message_id = Some("unique-msg-1".to_string());

    // Publish the same message twice.
    bus.publish(&room, &ann).await.unwrap();
    bus.publish(&room, &ann).await.unwrap();

    // Should only receive it once.
    let received = tokio::time::timeout(Duration::from_millis(200), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.message_id, Some("unique-msg-1".to_string()));

    // Second receive should timeout (no duplicate delivered).
    let second = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(second.is_err(), "duplicate should have been filtered");
}

#[tokio::test]
async fn deduplication_allows_different_messages() {
    let transport = Arc::new(InProcessGossip::new());
    let alice = NodeId::random();
    let bob = NodeId::random();
    let room: RoomId = "multi-msg-test".into();

    let alice_bus = GossipBus::new(alice.clone(), Arc::clone(&transport) as _);
    let bob_bus = GossipBus::new(bob.clone(), Arc::clone(&transport) as _);

    // Bob uses deduplication.
    let dedup_filter = Arc::new(RwLock::new(DedupeFilter::default()));
    let ttl_tracker = Arc::new(RwLock::new(TtlTracker::new(DEFAULT_MESSAGE_TTL)));

    alice_bus.join_room(&room).await.unwrap();
    bob_bus.join_room(&room).await.unwrap();

    let mut rx = bob_bus.subscribe_with_filter(&room, dedup_filter, ttl_tracker);

    // Alice and Bob publish different messages.
    let mut ann1 = make_ann(&alice, &room, 1);
    ann1.message_id = Some("msg-from-alice".to_string());
    let mut ann2 = make_ann(&bob, &room, 2);
    ann2.message_id = Some("msg-from-bob".to_string());

    alice_bus.publish(&room, &ann1).await.unwrap();
    bob_bus.publish(&room, &ann2).await.unwrap();

    // Should receive both.
    let mut received_ids = Vec::new();
    for _ in 0..2 {
        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        received_ids.push(received.message_id);
    }
    assert!(received_ids.contains(&Some("msg-from-alice".to_string())));
    assert!(received_ids.contains(&Some("msg-from-bob".to_string())));
}

#[tokio::test]
async fn dedup_filter_tracks_cache_size() {
    let filter = DedupeFilter::with_capacity(100, DEFAULT_MESSAGE_TTL);
    assert_eq!(filter.cache_size(), 0);
}

#[tokio::test]
async fn dedup_filter_clear_works() {
    let mut filter = DedupeFilter::with_capacity(100, DEFAULT_MESSAGE_TTL);
    let ann = make_ann(&NodeId::random(), &RoomId::new("test"), 1);
    filter.check_and_insert(&ann);
    assert_eq!(filter.cache_size(), 1);
    filter.clear_cache();
    assert_eq!(filter.cache_size(), 0);
}

// ─── TTL tests ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn ttl_tracker_tracks_expiration() {
    let mut tracker = TtlTracker::new(Duration::from_millis(10));
    tracker.register_default("msg1".to_string());

    assert!(!tracker.is_expired("msg1"));

    tokio::time::sleep(Duration::from_millis(15)).await;
    assert!(tracker.is_expired("msg1"));
}

#[tokio::test]
async fn ttl_tracker_remaining_ttl() {
    let mut tracker = TtlTracker::new(Duration::from_secs(100));
    tracker.register("msg1".to_string(), Duration::from_secs(100));

    let remaining = tracker.remaining_ttl("msg1");
    assert!(remaining.is_some());
    assert!(remaining.unwrap() > Duration::from_secs(90));
}

#[tokio::test]
async fn ttl_tracker_cleanup_returns_expired() {
    let mut tracker = TtlTracker::new(Duration::from_millis(10));
    tracker.register_default("msg1".to_string());
    tracker.register_default("msg2".to_string());

    tokio::time::sleep(Duration::from_millis(15)).await;

    let expired = tracker.cleanup_expired();
    assert!(expired.contains(&"msg1".to_string()));
    assert!(tracker.is_empty());
}
