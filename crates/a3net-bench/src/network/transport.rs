// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Transport layer benchmarks.

use criterion::{BenchmarkId, Criterion, Throughput};
use std::time::Duration;

/// Benchmark serialization overhead for node IDs.
pub fn bench_node_id_serialization(c: &mut Criterion) {
    use a3net_types::NodeId;

    let mut group = c.benchmark_group("transport/node_id");

    let node_ids: Vec<NodeId> = (0..1000).map(|_| NodeId::random()).collect();

    group.throughput(Throughput::Elements(node_ids.len() as u64));
    group.bench_function("serialize", |b| {
        b.iter(|| {
            for id in &node_ids {
                let _ = serde_json::to_string(id);
            }
        });
    });

    group.bench_function("deserialize", |b| {
        let encoded: Vec<String> = node_ids.iter()
            .map(|id| serde_json::to_string(id).unwrap())
            .collect();
        b.iter(|| {
            for s in &encoded {
                let _: NodeId = serde_json::from_str(s).unwrap();
            }
        });
    });

    group.finish();
}

/// Benchmark connection establishment simulation.
pub fn bench_connection_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("transport/connection");

    // Simulate handshake round trips
    for rounds in [1, 3, 5].iter() {
        group.bench_function(BenchmarkId::from_parameter(rounds), |b| {
            b.iter(|| {
                // Simulate a simplified handshake
                let mut state = 0u8;
                for _ in 0..*rounds {
                    state = state.wrapping_add(1);
                    state = state.wrapping_mul(2);
                }
            });
        });
    }

    group.finish();
}

/// Benchmark stream multiplexing overhead.
pub fn bench_stream_multiplexing(c: &mut Criterion) {
    let mut group = c.benchmark_group("transport/stream_mux");

    for stream_count in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*stream_count as u64));
        group.bench_function(BenchmarkId::from_parameter(stream_count), |b| {
            b.iter(|| {
                let mut next_stream_id = 0u64;
                for _ in 0..*stream_count {
                    let id = next_stream_id;
                    next_stream_id = next_stream_id.wrapping_add(1);
                    // Simulate stream lookup
                    let _ = id ^ 0xDEADBEEF;
                }
            });
        });
    }

    group.finish();
}

/// Benchmark packet fragmentation.
pub fn bench_fragmentation(c: &mut Criterion) {
    let mut group = c.benchmark_group("transport/fragmentation");

    let mtu = 1400; // Typical Ethernet MTU
    for payload_size in [512, 1400, 4096, 16384, 65536].iter() {
        let data = vec![0u8; *payload_size];

        group.throughput(Throughput::Bytes(*payload_size as u64));
        group.bench_function(BenchmarkId::from_parameter(payload_size), |b| {
            b.iter(|| {
                let fragments: Vec<_> = data
                    .chunks(mtu)
                    .map(|chunk| chunk.to_vec())
                    .collect();
                let _ = fragments.len();
            });
        });
    }

    group.finish();
}

/// Benchmark keepalive overhead.
pub fn bench_keepalive(c: &mut Criterion) {
    let mut group = c.benchmark_group("transport/keepalive");

    group.bench_function("ping_interval_check", |b| {
        let last_pong = std::time::Instant::now() - Duration::from_secs(15);
        let timeout = Duration::from_secs(30);

        b.iter(|| {
            let elapsed = last_pong.elapsed();
            let _ = elapsed > timeout;
        });
    });

    group.finish();
}

/// Register all transport benchmarks.
pub fn register(c: &mut Criterion) {
    bench_node_id_serialization(c);
    bench_connection_overhead(c);
    bench_stream_multiplexing(c);
    bench_fragmentation(c);
    bench_keepalive(c);
}
