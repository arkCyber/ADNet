// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Gossip benchmarks.

use adnet_gossip::{GossipBus, InProcessGossip};
use adnet_types::{Announcement, CdnContentKind, ContentHash, NodeId, RoomId};
use criterion::{BenchmarkId, Criterion, Throughput};
use chrono::Utc;
use std::sync::Arc;

/// Generate test announcements.
fn make_announcements(count: usize) -> Vec<Announcement> {
    let node_id = NodeId::random();
    let room_id: RoomId = "bench-room".into();

    (0..count)
        .map(|seq| Announcement {
            room_id: room_id.clone(),
            content_hash: ContentHash::from_bytes(format!("payload-{seq}").as_bytes()),
            node_id: node_id.clone(),
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
        })
        .collect()
}

/// Benchmark single publisher, single subscriber throughput.
pub fn bench_single_publisher_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("gossip/single_publisher");

    for msg_count in [100, 1_000, 10_000].iter() {
        let payloads = make_announcements(*msg_count);
        let publisher_id = NodeId::random();
        let transport = Arc::new(InProcessGossip::new());
        let bus = GossipBus::new(publisher_id.clone(), Arc::clone(&transport) as _);
        let room: RoomId = "bench".into();

        group.throughput(Throughput::Elements(*msg_count as u64));
        group.bench_function(BenchmarkId::from_parameter(msg_count), |b| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                bus.join_room(&room).await.expect("join");
                b.iter(|| {
                    let bus = bus.clone();
                    let room = room.clone();
                    rt.block_on(async {
                        for ann in &payloads {
                            bus.publish(&room, ann).await.expect("publish");
                        }
                    });
                });
            });
        });
    }

    group.finish();
}

/// Benchmark subscription overhead.
pub fn bench_subscription(c: &mut Criterion) {
    let mut group = c.benchmark_group("gossip/subscription");

    let publisher_id = NodeId::random();
    let transport = Arc::new(InProcessGossip::new());
    let bus = GossipBus::new(publisher_id.clone(), Arc::clone(&transport) as _);
    let room: RoomId = "sub-bench".into();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        bus.join_room(&room).await.expect("join");

        group.bench_function("subscribe_overhead", |b| {
            let bus = bus.clone();
            let room = room.clone();
            b.iter(|| {
                // subscribe is synchronous, no await needed
                let _rx = bus.subscribe(&room);
            });
        });
    });

    group.finish();
}

/// Register all gossip benchmarks.
pub fn register(c: &mut Criterion) {
    bench_single_publisher_throughput(c);
    bench_subscription(c);
}
