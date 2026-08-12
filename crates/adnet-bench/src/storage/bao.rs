// SPDX-License-Identifier: MIT OR Apache-2.0
//
// BAO (BLAKE3 authenticated output) tree benchmarks.

use adnet_blobstore::{BaoTree, BaoTreeBuilder, BaoProof, BaoLeaf, CHUNK_SIZE};
use adnet_types::ContentHash;
use criterion::{BenchmarkId, Criterion, Throughput};
use std::sync::Arc;

/// Generate test data.
fn make_payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// Benchmark BAO tree construction.
pub fn bench_bao_tree_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("bao/tree_build");

    for size_mb in [1, 4, 16, 64].iter() {
        let size = (size_mb * 1024 * 1024) as u64;
        let data = Arc::new(make_payload(size as usize));

        group.throughput(Throughput::Bytes(size));
        group.bench_function(BenchmarkId::from_parameter(size_mb), |b| {
            b.iter(|| {
                // Use BaoTree::build which takes content bytes directly
                let _tree = BaoTree::build(&data);
            });
        });
    }

    group.finish();
}

/// Benchmark BAO proof generation.
pub fn bench_bao_proof_gen(c: &mut Criterion) {
    let mut group = c.benchmark_group("bao/proof_gen");

    let size = 16 * 1024 * 1024; // 16 MB
    let data = make_payload(size);
    let tree = BaoTree::build(&data);

    for range_count in [1, 10, 100].iter() {
        let ranges: Vec<(u64, u64)> = (0..*range_count)
            .map(|i| {
                let start = (i * (size / *range_count));
                let end = std::cmp::min(start + 4096, size);
                (start as u64, end as u64)
            })
            .collect();

        group.throughput(Throughput::Bytes((size / range_count) as u64));
        group.bench_function(BenchmarkId::from_parameter(range_count), |b| {
            b.iter(|| {
                for (start, end) in &ranges {
                    // Generate proof for range
                    let _ = tree.proof_for_range(*start, *end);
                }
            });
        });
    }

    group.finish();
}

/// Benchmark BAO proof verification.
pub fn bench_bao_proof_verify(c: &mut Criterion) {
    let mut group = c.benchmark_group("bao/proof_verify");

    // Create a proof to verify
    let size = 16 * 1024 * 1024; // 16 MB
    let data = make_payload(size);
    let tree = BaoTree::build(&data);

    // Build a proof for the first 4KB
    let proof = tree.proof_for_range(0, 4096).expect("valid range");

    group.bench_function("single_proof", |b| {
        b.iter(|| {
            // Verify proof against the root
            let _ = proof.verify(&tree.root_hash());
        });
    });

    group.finish();
}

/// Benchmark BAO tree node lookup.
pub fn bench_bao_node_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("bao/node_lookup");

    let size = 64 * 1024 * 1024; // 64 MB
    let data = make_payload(size);
    let tree = BaoTree::build(&data);

    // Get the number of leaves
    let num_leaves = tree.num_leaves();

    // Generate leaf indices to access
    let indices: Vec<usize> = (0..1000)
        .map(|_| rand::random::<usize>() % num_leaves)
        .collect();

    group.throughput(Throughput::Elements(indices.len() as u64));
    group.bench_function("leaf_access", |b| {
        b.iter(|| {
            for idx in &indices {
                let _ = tree.leaf(*idx);
            }
        });
    });

    group.finish();
}

/// Benchmark BAO leaf iteration.
pub fn bench_bao_leaf_iteration(c: &mut Criterion) {
    let mut group = c.benchmark_group("bao/leaf_iteration");

    for size_mb in [1, 4, 16].iter() {
        let size = (size_mb * 1024 * 1024) as usize;
        let data = make_payload(size);
        let tree = BaoTree::build(&data);

        let leaf_count = tree.num_leaves();
        group.throughput(Throughput::Elements(leaf_count as u64));
        group.bench_function(BenchmarkId::from_parameter(size_mb), |b| {
            b.iter(|| {
                let mut count = 0usize;
                for leaf in tree.leaves() {
                    count += 1;
                }
                count
            });
        });
    }

    group.finish();
}

/// Register all BAO benchmarks.
pub fn register(c: &mut Criterion) {
    bench_bao_tree_build(c);
    bench_bao_proof_gen(c);
    bench_bao_proof_verify(c);
    bench_bao_node_lookup(c);
    bench_bao_leaf_iteration(c);
}
