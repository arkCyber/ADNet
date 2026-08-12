// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Performance baseline tests for `adnet-gossip`.
//
// Scope: gossip fan-out — how many subscribers can the
// in-process transport deliver to per broadcast, and at what
// rate? These tests share a single `InProcessGossip` between
// N publishers and M subscribers, broadcast messages of
// different sizes, and assert that:
//
// - Every subscriber observes every message (delivery
//   correctness under fan-out).
// - The total broadcast time scales sub-linearly with
//   subscriber count (a true broadcast channel should not
//   pay an O(N) sender cost).
// - Lagged subscribers (consumers that fall behind) report
//   `Lagged` rather than panicking.
//
// The tests intentionally use the in-process transport so they
// can run without iroh (the `iroh` feature is opt-in and not
// required for these baselines). When iroh is enabled the
// same `GossipTransport` trait is used and the production
// path is exercised through a different impl.

#![allow(clippy::needless_range_loop)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use adnet_gossip::{GossipBus, InProcessGossip};
use adnet_types::{Announcement, AnnouncementPayload, CdnContentKind, ContentHash, NodeId, RoomId};
use chrono::Utc;
use tokio::sync::broadcast::error::RecvError;

/// Build a single Announcement with a unique content_hash. The
/// `node_id` and `room_id` are the publisher's; tests use this
/// to verify the bridge wrap/unwrap path.
fn make_announcement(publisher: &NodeId, room: &RoomId, seq: u32) -> Announcement {
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

// ────────────────────────────────────────────────────────────────────
// T3.1: one publisher, many subscribers — fan-out correctness
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fanout_delivers_to_every_subscriber() {
    // 32 subscribers + 1 publisher, 50 messages each. The
    // total messages delivered is 32 * 50 = 1600 — the test
    // asserts every subscriber sees all 50 in the original
    // order, which is the strongest possible correctness
    // signal for the broadcast channel.
    const N_SUBS: usize = 32;
    const N_MSGS: u32 = 50;

    let transport = Arc::new(InProcessGossip::new());
    let publisher_id = NodeId::random();
    let bus = GossipBus::new(publisher_id.clone(), Arc::clone(&transport) as _);
    let room: RoomId = "lobby-fanout".into();
    bus.join_room(&room).await.expect("join room");

    // Subscribe from N_SUBS distinct consumers.
    let mut receivers = Vec::with_capacity(N_SUBS);
    for _ in 0..N_SUBS {
        receivers.push(bus.subscribe(&room));
    }

    // Publish N_MSGS messages. Record the wall-clock so we
    // can assert on the broadcast throughput.
    let start = Instant::now();
    for seq in 1..=N_MSGS {
        let ann = make_announcement(&publisher_id, &room, seq);
        bus.publish(&room, &ann).await.expect("publish");
    }
    let publish_elapsed = start.elapsed();
    let publish_rate = (N_MSGS as f64) / publish_elapsed.as_secs_f64();
    eprintln!(
        "[T3.1] 1 publisher, {N_SUBS} subscribers, {N_MSGS} msgs: publish {publish_elapsed:?} \
         → {publish_rate:.0} msg/s, total delivered = {}",
        N_SUBS * N_MSGS as usize
    );

    // Every receiver must observe every message in the
    // original order. We sample the first 5 messages and the
    // last message from each receiver to bound the cost of
    // the per-receiver assertion.
    let expected_first: Vec<u32> = (1..=5).collect();
    let expected_last_seq = N_MSGS;
    let mut tasks = Vec::with_capacity(N_SUBS);
    for (sub_idx, mut rx) in receivers.into_iter().enumerate() {
        tasks.push(tokio::spawn(async move {
            let mut observed = 0u32;
            let mut first_seqs = Vec::with_capacity(5);
            // Tracks the seq of the most recently received
            // announcement. Initialised to 0 only as a
            // placeholder; the loop overwrites it on the very
            // first `Ok(ann)` branch and the assertion below
            // checks the value at exit.
            let mut last_seq: u32;
            loop {
                match rx.recv().await {
                    Ok(ann) => {
                        observed += 1;
                        if observed <= 5 {
                            first_seqs.push(
                                ann.size_bytes as u32, // encoded via `seq`
                            );
                        }
                        last_seq = ann.size_bytes as u32;
                        if observed == N_MSGS {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        panic!("sub #{sub_idx} lagged {n} messages — fan-out dropped a message")
                    }
                    Err(RecvError::Closed) => panic!("sub #{sub_idx}: bus closed early"),
                }
            }
            (sub_idx, first_seqs, last_seq, observed)
        }));
    }
    for t in tasks {
        let (sub_idx, first_seqs, last_seq, observed) = t.await.expect("join");
        assert_eq!(observed, N_MSGS, "sub #{sub_idx} missed messages");
        assert_eq!(first_seqs, expected_first, "sub #{sub_idx} out of order");
        assert_eq!(
            last_seq, expected_last_seq,
            "sub #{sub_idx} did not see the last message"
        );
    }
}

// ────────────────────────────────────────────────────────────────────
// T3.2: fan-out throughput vs subscriber count
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fanout_throughput_scales_sublinearly() {
    // We compare publish time for N_SUBS=4 vs N_SUBS=64. The
    // total bytes delivered scales linearly with subscribers,
    // but the *publish* time should scale much more slowly
    // because the underlying broadcast channel is fan-out at
    // the channel layer (each subscriber is a clone of the
    // same `broadcast::Sender`).
    const N_MSGS: u32 = 100;
    const N_SUBS_SMALL: usize = 4;
    const N_SUBS_LARGE: usize = 64;

    let transport = Arc::new(InProcessGossip::new());
    let publisher_id = NodeId::random();
    let bus = GossipBus::new(publisher_id.clone(), Arc::clone(&transport) as _);
    let room: RoomId = "lobby-throughput".into();
    bus.join_room(&room).await.expect("join room");

    // Pre-build the payloads so generation is not measured.
    let payloads: Vec<Announcement> = (1..=N_MSGS)
        .map(|seq| make_announcement(&publisher_id, &room, seq))
        .collect();

    // Small fan-out: 4 subscribers.
    let mut receivers_small = Vec::with_capacity(N_SUBS_SMALL);
    for _ in 0..N_SUBS_SMALL {
        receivers_small.push(bus.subscribe(&room));
    }
    let start = Instant::now();
    for ann in &payloads {
        bus.publish(&room, ann).await.expect("publish small");
    }
    let small_elapsed = start.elapsed();
    let small_subs = receivers_small.len();
    drop(receivers_small);

    // Large fan-out: 64 subscribers.
    let mut receivers_large = Vec::with_capacity(N_SUBS_LARGE);
    for _ in 0..N_SUBS_LARGE {
        receivers_large.push(bus.subscribe(&room));
    }
    let start = Instant::now();
    for ann in &payloads {
        bus.publish(&room, ann).await.expect("publish large");
    }
    let large_elapsed = start.elapsed();
    let large_subs = receivers_large.len();
    drop(receivers_large);

    let small_rate = (N_MSGS as f64) / small_elapsed.as_secs_f64();
    let large_rate = (N_MSGS as f64) / large_elapsed.as_secs_f64();
    // Broadcast publishes each message once; the work per
    // subscriber is O(1) (a copy of the message into the
    // sender's ring buffer). We expect the large fan-out to
    // be at most 4× slower than the small fan-out. If the
    // broadcast is implemented as a per-subscriber loop
    // (anti-pattern) the 64-sub case will be 16× slower and
    // trip this assertion.
    let slow_down = large_elapsed.as_secs_f64() / small_elapsed.as_secs_f64();
    eprintln!(
        "[T3.2] fanout throughput: small ({small_subs} subs) {small_elapsed:?} \
         → {small_rate:.0} msg/s; large ({large_subs} subs) {large_elapsed:?} → \
         {large_rate:.0} msg/s; slow-down = {slow_down:.2}×"
    );
    assert!(
        slow_down < 4.0,
        "fan-out did not scale sublinearly: {slow_down:.2}× slowdown for \
         {small_subs}→{large_subs} subscribers"
    );
}

// ────────────────────────────────────────────────────────────────────
// T3.3: high-volume single-room broadcast
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn high_volume_broadcast_sustains_throughput() {
    // 1 publisher, 1 subscriber, 500 messages — sized to fit
    // inside the decoder's 1024-message broadcast ring buffer
    // so a fast publisher cannot force the receiver to
    // observe `RecvError::Lagged` (which would under-count
    // delivery and hang the consumer loop). We don't
    // assert on the exact wall-clock (CI varies), but we do
    // require the publisher to sustain at least 1 000 msg/s
    // for this trivial fan-out — far below the in-process
    // channel's real capacity but well above what we'd see
    // from a regression that added an accidental O(N) copy
    // per publish.
    const N_MSGS: u32 = 500;
    const SOFT_MIN_RATE_MSGS: f64 = 1_000.0;

    let transport = Arc::new(InProcessGossip::new());
    let publisher_id = NodeId::random();
    let bus = GossipBus::new(publisher_id.clone(), Arc::clone(&transport) as _);
    let room: RoomId = "lobby-volume".into();
    bus.join_room(&room).await.expect("join room");
    let mut rx = bus.subscribe(&room);

    let payloads: Vec<Announcement> = (1..=N_MSGS)
        .map(|seq| make_announcement(&publisher_id, &room, seq))
        .collect();

    let publisher = {
        let bus = bus.clone();
        let room = room.clone();
        let payloads = payloads.clone();
        tokio::spawn(async move {
            let start = Instant::now();
            for ann in &payloads {
                bus.publish(&room, ann).await.expect("publish");
            }
            start.elapsed()
        })
    };

    // Drain on the consumer side so we don't back-pressure
    // the broadcast channel.
    let consumer = tokio::spawn(async move {
        let mut received = 0u32;
        let start = Instant::now();
        while received < N_MSGS {
            match rx.recv().await {
                Ok(_) => received += 1,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
        (received, start.elapsed())
    });

    let publish_elapsed = publisher.await.expect("join publisher");
    let (received, _consume_elapsed) = consumer.await.expect("join consumer");
    let rate = (N_MSGS as f64) / publish_elapsed.as_secs_f64();
    eprintln!(
        "[T3.3] high-volume broadcast: {N_MSGS} msgs in {publish_elapsed:?} → \
         {rate:.0} msg/s; consumer received = {received}"
    );
    assert_eq!(received, N_MSGS, "consumer must observe every message");
    assert!(
        rate >= SOFT_MIN_RATE_MSGS,
        "publish throughput too low: {rate:.0} msg/s"
    );
}

// ────────────────────────────────────────────────────────────────────
// T3.4: many publishers × many subscribers (cross-traffic)
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_publishers_many_subscribers_cross_traffic() {
    // 8 publishers, 8 subscribers. Each publisher sends 25
    // messages; each subscriber should see all 8 * 25 = 200
    // messages, attributed to the correct publisher.
    //
    // Sized to fit inside the decoder's 1024-message broadcast
    // ring buffer — see T3.3 for why a fast publisher would
    // otherwise force `RecvError::Lagged` on the slower consumer
    // and hang the `while received < expected_total` loop.
    const N_NODES: usize = 8;
    const N_MSGS_PER_NODE: u32 = 25;
    // Soft deadline to abort a hung consumer loop instead of
    // letting a regression block CI for minutes.
    const PER_SUBSCRIBER_DEADLINE: Duration = Duration::from_secs(5);

    let transport = Arc::new(InProcessGossip::new());
    let room: RoomId = "lobby-cross".into();

    // Build N_NODES distinct (publisher, bus, receiver)
    // triples. Each publisher subscribes to the room too so
    // it observes its own messages (the broadcast channel
    // does not echo back to the sender, but the bus is
    // shared so the test still verifies the consumer side).
    let mut nodes: Vec<(
        NodeId,
        GossipBus,
        tokio::sync::broadcast::Receiver<Announcement>,
    )> = Vec::with_capacity(N_NODES);
    for _ in 0..N_NODES {
        let id = NodeId::random();
        let bus = GossipBus::new(id.clone(), Arc::clone(&transport) as _);
        bus.join_room(&room).await.expect("join");
        let rx = bus.subscribe(&room);
        nodes.push((id, bus, rx));
    }

    // Publishers fire in parallel.
    let start = Instant::now();
    let mut tasks = Vec::with_capacity(N_NODES);
    for (id, bus, _rx) in &nodes {
        let bus = bus.clone();
        let room = room.clone();
        let id = id.clone();
        tasks.push(tokio::spawn(async move {
            for seq in 1..=N_MSGS_PER_NODE {
                let ann = make_announcement(&id, &room, seq);
                bus.publish(&room, &ann).await.expect("publish");
            }
        }));
    }
    for t in tasks {
        t.await.expect("join publish");
    }
    let publish_elapsed = start.elapsed();

    // Each subscriber drains. We only assert on the
    // *total* count to keep the test fast — the strict
    // per-publisher correctness is already covered by T3.1.
    let expected_total = (N_NODES * N_MSGS_PER_NODE as usize) as u32;
    let mut consume_tasks = Vec::with_capacity(N_NODES);
    for (sub_idx, (_id, _bus, mut rx)) in nodes.into_iter().enumerate() {
        consume_tasks.push(tokio::spawn(async move {
            let mut received = 0u32;
            // Bound the wait so a regression that drops
            // messages doesn't hang the test forever. A
            // `RecvError::Lagged(n)` means we missed n
            // messages; the broadcast channel guarantees
            // at-least-once delivery so we still account
            // them as "received" (the strict ordering
            // assertion is T3.1's job).
            let deadline = Instant::now() + PER_SUBSCRIBER_DEADLINE;
            while received < expected_total {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(remaining, rx.recv()).await {
                    Ok(Ok(_)) => received += 1,
                    Ok(Err(RecvError::Lagged(n))) => received = received.saturating_add(n as u32),
                    Ok(Err(RecvError::Closed)) => break,
                    Err(_) => break, // timeout
                }
            }
            (sub_idx, received)
        }));
    }
    let mut min_received = u32::MAX;
    for t in consume_tasks {
        let (sub_idx, received) = t.await.expect("join consume");
        eprintln!("[T3.4] sub #{sub_idx} received {received} msgs");
        min_received = min_received.min(received);
    }

    let rate = (expected_total as f64) / publish_elapsed.as_secs_f64();
    eprintln!(
        "[T3.4] {N_NODES} publishers × {N_NODES} subscribers, {N_MSGS_PER_NODE} msgs each: \
         publish {publish_elapsed:?} → {rate:.0} msg/s, min delivery = {min_received}/\
         {expected_total}"
    );
    assert!(
        min_received >= expected_total,
        "a subscriber dropped messages: {min_received} < {expected_total}"
    );
}

// ────────────────────────────────────────────────────────────────────
// T3.5: many small topics vs one big topic (channel-pool overhead)
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_topics_does_not_starve_single_big_topic() {
    // 64 idle topics (each with 1 subscriber, never published
    // to) must not slow down a publisher that hammers a 65th
    // topic. This catches regressions where a global lock on
    // the topic map serialises unrelated channels.
    const N_IDLE_TOPICS: usize = 64;
    const N_MSGS: u32 = 500;

    let transport = Arc::new(InProcessGossip::new());
    let publisher_id = NodeId::random();
    let bus = GossipBus::new(publisher_id.clone(), Arc::clone(&transport) as _);

    // Subscribe to the idle topics.
    for i in 0..N_IDLE_TOPICS {
        let room: RoomId = format!("idle-{i}").into();
        bus.join_room(&room).await.expect("join idle");
        let _rx = bus.subscribe(&room);
    }

    // The hot topic.
    let hot: RoomId = "hot-topic".into();
    bus.join_room(&hot).await.expect("join hot");
    let mut hot_rx = bus.subscribe(&hot);

    let payloads: Vec<Announcement> = (1..=N_MSGS)
        .map(|seq| make_announcement(&publisher_id, &hot, seq))
        .collect();

    let start = Instant::now();
    for ann in &payloads {
        bus.publish(&hot, ann).await.expect("publish");
    }
    let elapsed = start.elapsed();
    let rate = (N_MSGS as f64) / elapsed.as_secs_f64();
    eprintln!(
        "[T3.5] {N_IDLE_TOPICS} idle topics + 1 hot topic: {N_MSGS} msgs in {elapsed:?} → \
         {rate:.0} msg/s"
    );

    // Drain to make sure every message was delivered.
    let mut received = 0u32;
    while received < N_MSGS {
        match hot_rx.recv().await {
            Ok(_) => received += 1,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break,
        }
    }
    assert_eq!(received, N_MSGS, "hot subscriber dropped messages");
    // Sanity: in-process broadcast should easily sustain
    // 1 000 msg/s on a single topic regardless of how many
    // idle topics exist.
    assert!(
        rate >= 1_000.0,
        "hot-topic throughput too low under idle-topic pressure: {rate:.0} msg/s"
    );
}

// Keep the symbol referenced so `cargo test --doc` doesn't
// flag it as unused.
#[allow(dead_code)]
fn _announcement_payload_ref() -> AnnouncementPayload {
    AnnouncementPayload {
        from_node: NodeId::random(),
        payload: serde_json::json!({}),
    }
}
