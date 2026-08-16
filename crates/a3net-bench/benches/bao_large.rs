// SPDX-License-Identifier: MIT OR Apache-2.0
//
// P0 BaoTree benchmark — this is the regression anchor for the
// hex-dance removal in `bao_tree.rs`. The numbers produced here must
// be compared against the previous `target/criterion/bao_large` HTML
// report to verify the 5–10× speed-up landed.
//
// Run: `cargo bench -p a3net-bench --bench bao_large`

use a3net_blobstore::{BaoTree, ChunkWriter};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::io::Write;

fn make_payload(size: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(size);
    let mut i: usize = 0;
    while buf.len() < size {
        buf.push((i % 251) as u8);
        i += 1;
    }
    buf
}

/// Baseline BaoTree::build on payloads at 1 MiB / 16 MiB / 64 MiB.
/// Use the same inputs as the "hex_dance" baseline below so the two
/// reports can be eyeballed side-by-side in the HTML report.
fn bench_bao_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("bao_large/build");
    for size_mib in [1u64, 16, 64] {
        let size = (size_mib * 1024 * 1024) as usize;
        let data = make_payload(size);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(BenchmarkId::from_parameter(size_mib), |b| {
            b.iter(|| {
                let _tree = BaoTree::build(&data);
            });
        });
    }
    group.finish();
}

/// Reconstruct the old hex-decode-then-concat path the production
/// `bao_tree.rs` used to take when combining two 32-byte hashes into
/// their parent. This is the exact hot loop that P0 removed:
///
/// ```ignore
/// let bytes0 = hex::decode(pair[0].as_hex()).expect("valid hex");
/// let bytes1 = hex::decode(pair[1].as_hex()).expect("valid hex");
/// let combined = [bytes0.as_slice(), bytes1.as_slice()].concat();
/// let parent_hash = ContentHash::from_bytes(&combined);
/// ```
///
/// We keep this gated behind a bench (not behind the lib) so the
/// "before" number is reproducible on demand.
///
/// Run with `--include-ignored` to actually execute this; it is marked
/// `#[ignore]` by default because the only reason to run it is to
/// reproduce the "before" baseline for the PR description.
fn combine_hash_pair_hex_dance(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let hex_a: String = a.iter().map(|x| format!("{:02x}", x)).collect();
    let hex_b: String = b.iter().map(|x| format!("{:02x}", x)).collect();
    let bytes0 = hex::decode(&hex_a).expect("valid hex");
    let bytes1 = hex::decode(&hex_b).expect("valid hex");
    let combined = [bytes0.as_slice(), bytes1.as_slice()].concat();
    let mut out = [0u8; 32];
    let native = blake3::hash(&combined);
    out.copy_from_slice(native.as_bytes());
    out
}

#[ignore]
fn bench_hex_dance_baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("bao_large/hex_dance_baseline");
    for size_mib in [1u64, 16, 64] {
        let size = (size_mib * 1024 * 1024) as usize;
        // 64 KiB chunks → 2 hashes each pair iteration.
        let chunks: Vec<[u8; 32]> = (0..(size / (64 * 1024)))
            .map(|i| blake3::hash(&i.to_le_bytes()).into())
            .collect();

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(BenchmarkId::from_parameter(size_mib), |b| {
            b.iter(|| {
                let mut next: Vec<[u8; 32]> = Vec::with_capacity(chunks.len() / 2 + 1);
                for pair in chunks.chunks(2) {
                    if pair.len() == 2 {
                        next.push(combine_hash_pair_hex_dance(&pair[0], &pair[1]));
                    } else {
                        next.push(pair[0]);
                    }
                }
                let _ = next;
            });
        });
    }
    group.finish();
}

/// Compare `ChunkWriter::new` (one 16 KiB write_all per chunk) with
/// `ChunkWriter::new_buffered` (64 KiB BufWriter around `inner`) on a
/// 64 MiB payload. The output is captured in a `Vec<u8>` so the
/// measurement is dominated by the ChunkWriter logic, not by disk or
/// network latency.
fn bench_chunk_writer_buffered_vs_unbuffered(c: &mut Criterion) {
    let mut group = c.benchmark_group("bao_large/chunk_writer");
    let size = 64 * 1024 * 1024usize;
    let data = make_payload(size);

    group.throughput(Throughput::Bytes(size as u64));

    group.bench_function("unbuffered", |b| {
        b.iter(|| {
            let mut out: Vec<u8> = Vec::with_capacity(size);
            {
                let mut w = ChunkWriter::new(&mut out);
                w.write_all(&data).unwrap();
                let _ = w.finish().unwrap();
            }
            std::hint::black_box(&out);
        });
    });

    group.bench_function("buffered_64kib", |b| {
        b.iter(|| {
            let mut out: Vec<u8> = Vec::with_capacity(size);
            {
                let mut w = ChunkWriter::new_buffered(&mut out);
                w.write_all(&data).unwrap();
                let _ = w.finish().unwrap();
            }
            std::hint::black_box(&out);
        });
    });

    group.finish();
}

/// P1 lazy-mode benchmark — measure the wall-time cost of
/// `BaoTree::parents_cached()` materialisation. The hypothesis is:
///
/// 1. `build_only` (no proof/verify) is the same as the P0 build bench
///    — we measure the eager (root only) path.
/// 2. `build_then_first_proof` adds the parent-chain build cost
///    (~3 MiB of hash allocations for a 1 GiB blob).
/// 3. `build_then_proof_twice` shows the second proof is essentially
///    free because the parents are cached in `OnceCell`.
///
/// All three benchmarks operate on the same 64 MiB payload so the
/// numbers are directly comparable.
fn bench_lazy_parents(c: &mut Criterion) {
    let mut group = c.benchmark_group("bao_large/lazy_parents");
    let size = 64 * 1024 * 1024usize;
    let data = make_payload(size);

    group.throughput(Throughput::Bytes(size as u64));

    group.bench_function("build_only", |b| {
        b.iter(|| {
            // Drop the tree immediately so the OnceCell can never be
            // materialised — root_hash() is metadata-only.
            let tree = BaoTree::build(&data);
            std::hint::black_box(tree.root_hash());
            // Explicitly drop the tree so the OnceCell is never
            // triggered by a destructor.
            drop(tree);
        });
    });

    group.bench_function("build_then_first_proof", |b| {
        b.iter(|| {
            let tree = BaoTree::build(&data);
            let proof = tree.proof_for_range(0, tree.total_len()).unwrap();
            std::hint::black_box(proof);
        });
    });

    group.bench_function("build_then_proof_twice", |b| {
        b.iter(|| {
            let tree = BaoTree::build(&data);
            let p1 = tree.proof_for_range(0, tree.total_len()).unwrap();
            let p2 = tree.proof_for_range(0, tree.total_len()).unwrap();
            std::hint::black_box((p1, p2));
        });
    });

    group.finish();
}

criterion_group!(
    bao,
    bench_bao_build,
    bench_hex_dance_baseline,
    bench_chunk_writer_buffered_vs_unbuffered,
    bench_lazy_parents
);
criterion_main!(bao);

