// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Blob storage benchmarks.

use adnet_blobstore::{BaoTree, CHUNK_SIZE};
use criterion::{BenchmarkId, Criterion, Throughput};

/// Generate deterministic test data.
fn make_payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// Benchmark chunk hashing throughput.
pub fn bench_chunk_hashing(c: &mut Criterion) {
    let mut group = c.benchmark_group("blob/chunk_hash");

    for chunk_size_kb in [1, 4, 16].iter() {
        let chunk_size = chunk_size_kb * 1024;
        let data = make_payload(chunk_size);
        let chunks = 1000; // Number of chunks to hash

        group.throughput(Throughput::Bytes((chunks * chunk_size) as u64));
        group.bench_function(BenchmarkId::from_parameter(chunk_size_kb), |b| {
            b.iter(|| {
                for _ in 0..chunks {
                    let _ = blake3::hash(&data);
                }
            });
        });
    }

    group.finish();
}

/// Benchmark BAO tree building.
pub fn bench_bao_tree_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("blob/bao_build");

    for size_mb in [1, 4, 16].iter() {
        let size = size_mb * 1024 * 1024;
        let data = make_payload(size);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(BenchmarkId::from_parameter(size_mb), |b| {
            b.iter(|| {
                let _tree = BaoTree::build(&data);
            });
        });
    }

    group.finish();
}

/// Benchmark BAO proof generation.
pub fn bench_bao_proof_gen(c: &mut Criterion) {
    let mut group = c.benchmark_group("blob/bao_proof");

    let size = 16 * 1024 * 1024;
    let data = make_payload(size);
    let tree = BaoTree::build(&data);

    group.throughput(Throughput::Bytes(size as u64));
    group.bench_function("16mb_proof", |b| {
        b.iter(|| {
            let _ = tree.proof_for_range(0, 4096);
        });
    });

    group.finish();
}

/// Register all blob benchmarks.
pub fn register(c: &mut Criterion) {
    bench_chunk_hashing(c);
    bench_bao_tree_build(c);
    bench_bao_proof_gen(c);
}
