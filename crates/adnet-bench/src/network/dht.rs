// SPDX-License-Identifier: MIT OR Apache-2.0
//
// DHT benchmarks.

use adnet_dht::{KBucket, Contact};
use adnet_types::NodeId;
use criterion::{BenchmarkId, Criterion, Throughput};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

/// Generate N random contacts for benchmarking.
fn generate_contacts(n: usize) -> Vec<Contact> {
    (0..n)
        .map(|i| {
            let id = NodeId::random();
            let addr = SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::new(127, 0, 0, 1),
                10000 + i as u16,
            ));
            Contact::new(id, addr)
        })
        .collect()
}

/// Benchmark KBucket insert operations.
pub fn bench_kbucket_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("dht/kbucket_insert");

    for size in [10, 50, 100].iter() {
        let contacts = generate_contacts(*size);

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_function(BenchmarkId::from_parameter(size), |b| {
            b.iter(|| {
                let mut bucket = KBucket::new();
                for contact in &contacts {
                    let _ = bucket.insert(contact.clone());
                }
            });
        });
    }

    group.finish();
}

/// Benchmark KBucket lookup by ID.
pub fn bench_kbucket_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("dht/kbucket_lookup");

    // Populate a bucket
    let contacts = generate_contacts(50);
    let mut bucket = KBucket::new();
    for contact in &contacts {
        let _ = bucket.insert(contact.clone());
    }

    // Generate target IDs to look up
    let targets: Vec<NodeId> = (0..100).map(|_| NodeId::random()).collect();

    group.throughput(Throughput::Elements(targets.len() as u64));
    group.bench_function("xor_distance_lookup", |b| {
        b.iter(|| {
            for target in &targets {
                let _ = bucket.find(target);
            }
        });
    });

    group.finish();
}

/// Benchmark KBucket removal.
pub fn bench_kbucket_remove(c: &mut Criterion) {
    let mut group = c.benchmark_group("dht/kbucket_remove");

    // Populate a bucket
    let contacts = generate_contacts(50);
    let mut bucket = KBucket::new();
    for contact in &contacts {
        let _ = bucket.insert(contact.clone());
    }

    // Generate IDs to remove
    let to_remove: Vec<NodeId> = contacts.iter().take(25).map(|c| c.id.clone()).collect();

    group.throughput(Throughput::Elements(to_remove.len() as u64));
    group.bench_function("remove_contacts", |b| {
        b.iter(|| {
            let mut bucket = bucket.clone();
            for id in &to_remove {
                let _ = bucket.remove(id);
            }
        });
    });

    group.finish();
}

/// Register all DHT benchmarks.
pub fn register(c: &mut Criterion) {
    bench_kbucket_insert(c);
    bench_kbucket_lookup(c);
    bench_kbucket_remove(c);
}
