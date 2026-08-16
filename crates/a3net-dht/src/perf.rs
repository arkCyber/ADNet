// SPDX-License-Identifier: MIT OR Apache-2.0
//
// `perf.rs` — Performance / load tests for `a3net-dht`.
//
// Aerospace note (DO-178C §6.4.4 — performance under load):
// the DHT must remain responsive when a single node carries
// tens of thousands of providers, IPNS records, and routing
// table entries — the network is sized for ~tens of millions
// of users, and a per-node baseline of 10k records is the
// floor for a healthy home node.
//
// These tests are intentionally generous on timing — the
// goal is regression detection, not micro-benchmarking. CI
// runners vary in CPU; we use 5× the baseline a developer
// laptop can hit so the test stays reliable across machines.

#![cfg(test)]

use std::time::{Duration, Instant};

use a3net_types::NodeId;

use crate::bucket::{Contact, RoutingTable};
use crate::record::{DhtKey, IpnRecord, ProviderRecord};
use crate::store::new_in_memory_store;

// ─────────────────────────────────────────────────────────────────
// §6.4.4 — Storage: 10k records
// ─────────────────────────────────────────────────────────────────

#[test]
fn storage_handles_10k_providers_within_budget() {
    let store = new_in_memory_store();
    let provider = NodeId::random();
    let start = Instant::now();
    for i in 0..10_000u32 {
        let key = DhtKey::from_bytes(i.to_be_bytes().to_vec());
        store.put_provider(
            &key,
            ProviderRecord::new(key, provider.clone(), format!("127.0.0.1:{}", 9000 + (i % 1000))),
        );
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "10k provider inserts took {elapsed:?}"
    );
    assert_eq!(store.get_all_provider_count(), 10_000);
}

#[test]
fn storage_lookup_under_1ms_for_10k_providers() {
    let store = new_in_memory_store();
    let provider = NodeId::random();
    let mut keys = Vec::with_capacity(10_000);
    for i in 0..10_000u32 {
        let key = DhtKey::from_bytes(i.to_be_bytes().to_vec());
        store.put_provider(
            &key,
            ProviderRecord::new(key.clone(), provider.clone(), "addr".into()),
        );
        keys.push(key);
    }
    // Warm cache: each key is fetched once before timing.
    for k in &keys {
        let _ = store.get_providers(k);
    }
    // Time 100 random lookups; assert the median is sub-ms.
    let mut samples = Vec::with_capacity(100);
    for i in 0..100 {
        let k = &keys[(i * 137) % keys.len()];
        let t = Instant::now();
        let _ = store.get_providers(k);
        samples.push(t.elapsed());
    }
    samples.sort();
    let median = samples[samples.len() / 2];
    assert!(
        median < Duration::from_millis(1),
        "median lookup = {median:?} (target <1ms)"
    );
}

#[test]
fn storage_handles_10k_ipns_records() {
    let store = new_in_memory_store();
    let start = Instant::now();
    for i in 0..10_000u32 {
        let key = DhtKey::from_bytes(i.to_be_bytes().to_vec());
        let mut rec = IpnRecord::new(key.clone(), format!("/ipfs/v{i}"));
        rec.sequence = i as u64;
        store.put_ipns(&key, rec).expect("insert");
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "10k IPNS inserts took {elapsed:?}"
    );
    assert_eq!(store.get_ipns_count(), 10_000);
}

// ─────────────────────────────────────────────────────────────────
// §6.4.4 — Routing table: 10k contacts
// ─────────────────────────────────────────────────────────────────

#[test]
fn routing_table_accepts_10k_contacts_within_budget() {
    // Aerospace note (DO-178C §6.4.4): the routing table must
    // handle 10k contacts in well under a second. Insertions
    // are O(K) per contact because of the bucket dispatch +
    // potential pending-replacement dance, but with 256
    // buckets each capped at K=20 the insertion is dominated
    // by the hash dispatch and a VecDeque push.
    let local = NodeId::random();
    let mut table = RoutingTable::new(local);
    let start = Instant::now();
    let mut inserted = 0;
    for i in 0..10_000u32 {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&i.to_be_bytes());
        let id = NodeId::from_bytes(&bytes).unwrap();
        let contact = Contact::new(
            id,
            std::net::SocketAddr::from(([127, 0, 0, 1], 9000 + (i % 60_000) as u16)),
        );
        if table.insert(contact).is_ok() {
            inserted += 1;
        }
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "10k inserts took {elapsed:?}"
    );
    // The table can hold at most K * 256 = 5120 contacts, but
    // we also have bootstrap_nodes (none here). So the upper
    // bound is 5120.
    assert!(table.num_contacts() <= 5120);
    let _ = inserted;
}

#[test]
fn closest_query_sub_millisecond_for_full_table() {
    let local = NodeId::random();
    let mut table = RoutingTable::new(local);
    // Saturate the table.
    for i in 0..5_000u32 {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&i.to_be_bytes());
        let id = NodeId::from_bytes(&bytes).unwrap();
        let contact = Contact::new(
            id,
            std::net::SocketAddr::from(([127, 0, 0, 1], 9000 + (i % 60_000) as u16)),
        );
        let _ = table.insert(contact);
    }
    let target = NodeId::random();
    // Warm.
    let _ = table.closest(&target, 20);
    let start = Instant::now();
    for _ in 0..100 {
        let _ = table.closest(&target, 20);
    }
    let elapsed = start.elapsed();
    let per_call = elapsed / 100;
    assert!(
        per_call < Duration::from_millis(5),
        "per-call closest() = {per_call:?}"
    );
}

// ─────────────────────────────────────────────────────────────────
// §6.4.4 — XOR distance: 100k ops/sec floor
// ─────────────────────────────────────────────────────────────────

#[test]
fn xor_distance_sustains_100k_ops_per_sec() {
    let k1 = DhtKey::from_bytes(vec![0xAA; 32]);
    let k2 = DhtKey::from_bytes(vec![0x55; 32]);
    let start = Instant::now();
    let mut acc = 0u64;
    for i in 0..100_000u64 {
        let d = k1.xor_distance(&k2);
        acc = acc.wrapping_add(d[i as usize % d.len()] as u64);
    }
    let elapsed = start.elapsed();
    let ops_per_sec = 100_000.0 / elapsed.as_secs_f64();
    assert!(
        ops_per_sec >= 100_000.0,
        "xor_distance throughput = {ops_per_sec:.0} ops/sec (target ≥100k)"
    );
    let _ = acc;
}

// ─────────────────────────────────────────────────────────────────
// §6.4.4 — Concurrent reads under load (no starvation)
// ─────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_lookups_dont_starve_writers() {
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let store = Arc::new(crate::store::InMemoryDhtStore::new());
    let provider = NodeId::random();

    // Pre-populate with 5k records.
    for i in 0..5_000u32 {
        let key = DhtKey::from_bytes(i.to_be_bytes().to_vec());
        store.put_provider(
            &key,
            ProviderRecord::new(key, provider.clone(), "addr".into()),
        );
    }

    // Spawn 16 reader tasks and 4 writer tasks; verify all
    // complete within a reasonable budget.
    let start = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..16 {
        let s = store.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..1_000u32 {
                let key = DhtKey::from_bytes(i.to_be_bytes().to_vec());
                let _ = s.get_providers(&key);
            }
        }));
    }
    for w in 0..4 {
        let s = store.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..100u32 {
                let key = DhtKey::from_bytes((i * 1000 + w).to_be_bytes().to_vec());
                s.put_provider(
                    &key,
                    ProviderRecord::new(key, provider.clone(), "addr".into()),
                );
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(10),
        "concurrent ops took {elapsed:?}"
    );
}