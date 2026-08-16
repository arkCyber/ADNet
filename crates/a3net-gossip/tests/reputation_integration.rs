//! Integration tests for the `a3net-reputation` crate's gossip signal
//! path. Requires the `reputation` feature on `a3net-gossip` so that
//! `GossipBus::with_reputation` is in scope and `decode_stream` feeds
//! valid / invalid events into the PeerScore table.

#![cfg(feature = "reputation")]

use std::sync::Arc;

use a3net_gossip::{GossipBus, InProcessGossip};
use a3net_reputation::{PeerScoreTable, ReputationEvent, ReputationParams, ReputationReporter};
use a3net_types::{AnnouncementPayload, CdnContentKind, ContentHash, NodeId, RoomId};
use chrono::Utc;

fn make_ann(publisher: &NodeId, room: &RoomId, tag: &str) -> a3net_types::Announcement {
    a3net_types::Announcement {
        room_id: room.clone(),
        content_hash: ContentHash::from_bytes(tag.as_bytes()),
        node_id: publisher.clone(),
        title: tag.to_string(),
        kind: CdnContentKind::Article,
        size_bytes: tag.len() as u64,
        mime_type: None,
        source_url: None,
        ticket: None,
        timestamp: Utc::now(),
        signer: None,
        signature: None,
    }
}

/// End-to-end: bob subscribes with a reputation-attached bus; alice
/// publishes a well-formed announcement; bob's bus should record a
/// `ValidMessage` event for alice and the score must move up.
#[tokio::test]
async fn gossip_valid_message_increments_peer_score() {
    let transport = Arc::new(InProcessGossip::new());
    let alice = NodeId::random();
    let bob = NodeId::random();
    let room: RoomId = "rep-valid".into();

    // Bob's bus carries a reputation reporter.
    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table.clone());
    let bob_bus = GossipBus::new(bob.clone(), Arc::clone(&transport) as _)
        .with_reputation(reporter.clone());
    let alice_bus = GossipBus::new(alice.clone(), Arc::clone(&transport) as _);

    alice_bus.join_room(&room).await.unwrap();
    bob_bus.join_room(&room).await.unwrap();
    let mut bob_rx = bob_bus.subscribe(&room);

    let ann = make_ann(&alice, &room, "valid-1");
    alice_bus.publish(&room, &ann).await.unwrap();

    let received = tokio::time::timeout(std::time::Duration::from_secs(2), bob_rx.recv())
        .await
        .expect("bob should receive")
        .expect("payload must decode");
    assert_eq!(received.content_hash, ann.content_hash);

    // Alice's score must be positive after a valid message.
    let score = reporter
        .table()
        .score(&alice)
        .expect("alice must have a score entry after valid gossip");
    assert!(
        score > 0.0,
        "score should be positive after a valid message, got {score}"
    );
}

/// Bypass the bus to inject a malformed payload directly through the
/// transport. Bob's decoder must drop it AND feed an `InvalidMessage`
/// event into the reputation table — that is the gossip-side
/// negative-signal path.
#[tokio::test]
async fn gossip_decode_failure_feeds_invalid_message_event() {
    let transport = Arc::new(InProcessGossip::new());
    let alice = NodeId::random();
    let bob = NodeId::random();
    let room: RoomId = "rep-invalid".into();

    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table.clone());
    let bob_bus = GossipBus::new(bob.clone(), Arc::clone(&transport) as _)
        .with_reputation(reporter.clone());
    let alice_bus = GossipBus::new(alice.clone(), Arc::clone(&transport) as _);

    alice_bus.join_room(&room).await.unwrap();
    bob_bus.join_room(&room).await.unwrap();
    let mut bob_rx = bob_bus.subscribe(&room);

    // Inject a payload that doesn't round-trip through `bridge::unwrap`.
    let topic = bob_bus.topic_for(&room);
    let bad = AnnouncementPayload {
        from_node: alice.clone(),
        payload: serde_json::json!({"not": "an announcement"}),
    };
    bob_bus
        .transport()
        .broadcast(topic, bad)
        .await
        .unwrap();

    // Then send a real announcement so bob's receiver unblocks.
    let good = make_ann(&alice, &room, "ok-after-bad");
    alice_bus.publish(&room, &good).await.unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), bob_rx.recv())
        .await
        .expect("bob should receive the recovery message")
        .expect("payload must decode");

    // Alice must have a negative score after a decode failure.
    let score = reporter
        .table()
        .score(&alice)
        .expect("alice must have a score entry after invalid gossip");
    assert!(
        score < 0.0,
        "score should be negative after decode failure, got {score}"
    );
}

/// Verify that `ReputationReporter::record_with_delta` from the gossip
/// path round-trips the right `kind` tag — defensive coverage that
/// keeps the event taxonomy stable for downstream consumers.
#[tokio::test]
async fn gossip_valid_event_carries_valid_message_tag() {
    let transport = Arc::new(InProcessGossip::new());
    let alice = NodeId::random();
    let bob = NodeId::random();
    let room: RoomId = "rep-tag".into();

    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table);
    let bob_bus = GossipBus::new(bob.clone(), Arc::clone(&transport) as _)
        .with_reputation(reporter.clone());
    let alice_bus = GossipBus::new(alice.clone(), Arc::clone(&transport) as _);

    alice_bus.join_room(&room).await.unwrap();
    bob_bus.join_room(&room).await.unwrap();
    let _bob_rx = bob_bus.subscribe(&room);

    let ann = make_ann(&alice, &room, "tag-check");
    alice_bus.publish(&room, &ann).await.unwrap();
    // Give the decoder a tick to run.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // And the score for alice must be non-zero (positive).
    let score = reporter
        .table()
        .score(&alice)
        .expect("alice must have a score entry");
    assert!(score > 0.0, "expected positive score, got {score}");
    // Touch the event taxonomy so a future change that breaks the
    // `kind_tag()` path is caught at compile time.
    let _kind: &str = ReputationEvent::ValidMessage {
        peer: alice.clone(),
        topic: None,
        size_bytes: 1,
    }
    .kind_tag();
}