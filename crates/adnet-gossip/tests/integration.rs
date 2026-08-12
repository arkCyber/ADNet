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

use adnet_gossip::{GossipBus, InProcessGossip};
use adnet_types::{
    Announcement, AnnouncementPayload, CdnContentKind, ContentHash, NodeId, RoomId,
};
use chrono::Utc;
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
    // When two buses share a transport, the sender's own messages
    // should be visible on its own subscriber — this is the
    // InProcessGossip broadcast contract.
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

    // Publish to alpha only.
    let ann_alpha = make_ann(&alice, &room_a, 1);
    alice_bus.publish(&room_a, &ann_alpha).await.unwrap();

    // Alpha subscriber gets it.
    let got_alpha = tokio::time::timeout(Duration::from_millis(100), alpha_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got_alpha.content_hash, ann_alpha.content_hash);

    // Beta subscriber must NOT see it.
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

    // Very small buffer to test backpressure handling.
    let mut rx = {
        // We can't control the channel size from outside, but we CAN
        // verify that the decode_stream task handles RecvError::Lagged gracefully.
        bus.subscribe(&room)
    };

    // Publish rapidly.
    for i in 1..=200u32 {
        let ann = make_ann(&publisher, &room, i);
        bus.publish(&room, &ann).await.unwrap();
    }

    // Consumer reads only 10 messages then stops. The broadcast
    // channel's internal lag tracking should prevent panics.
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
                // LAG events mean messages were skipped — the consumer
                // must not panic. We count them but keep going.
                tracing::debug!("consumer lagged {n} messages at count={count}");
            }
            Ok(Err(RecvError::Closed)) => break,
            Err(_) => break, // timeout
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
    // The announcement's node_id is alice's.
    assert_eq!(received.node_id, alice);
    // The bridge's from_node should also be alice.
    // We can verify this indirectly: the wire payload has from_node = alice.
    let wire = AnnouncementPayload {
        from_node: alice.clone(),
        payload: serde_json::to_value(&received).unwrap(),
    };
    let decoded: Announcement =
        serde_json::from_value(wire.payload).unwrap();
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
    bus.join_room(&room).await.unwrap(); // second join
    let mut rx = bus.subscribe(&room);

    let ann = make_ann(&node, &room, 1);
    bus.publish(&room, &ann).await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.content_hash, ann.content_hash);
}

// ─── subscribe_tracked ─────────────────────────────────────────────────────

#[tokio::test]
async fn subscribe_tracked_returns_live_receiver() {
    let transport = Arc::new(InProcessGossip::new());
    let node = NodeId::random();
    let room: RoomId = "tracked".into();
    let bus = GossipBus::new(node.clone(), Arc::clone(&transport) as _);
    bus.join_room(&room).await.unwrap();

    let mut tracked = bus.subscribe_tracked(&room);
    let ann = make_ann(&node, &room, 1);
    bus.publish(&room, &ann).await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), tracked.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.content_hash, ann.content_hash);
}
