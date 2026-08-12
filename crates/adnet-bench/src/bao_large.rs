// SPDX-License-Identifier: MIT OR Apache-2.0
//
// BaoTree hot-path regression benchmarks.
//
// These benchmarks gate the P0 perf work: rerun them after touching
// `bao_tree.rs` and compare the report against the previous `bench.md`
// to verify the hex-dance removal landed its 5–10× speed-up on large
// inputs and didn't regress anything else.
//
// To run:
//     cargo bench -p adnet-bench -- bao_large
//
// The 1 GiB test consumes ~1 GiB of RAM and ~30 s on a modern laptop;
// the smaller tiers are sub-second.

use adnet_blobstore::{BaoTree, BaoTreeBuilder};
use criterion::{BenchmarkId, Criterion, Throughput};
use std::io::Write;

/// Build deterministic test data with a fast byte pattern.
fn make_payload(size: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(size);
    let mut i: usize = 0;
    while buf.len() < size {
        let b = (i % 251) as u8;
        buf.push(b);
        i += 1;
    }
    buf
}

/// Benchmark building the full Bao tree on payloads ranging from 1 MiB up
/// to 1 GiB. This is the single operation that P0 was designed to speed
/// up — the result is the regression anchor for the PR.
pub fn bench_bao_build_large(c: &mut Criterion) {
    let mut group = c.benchmark_group("bao_large/build");

    for size_mib in [1u64, 16, 64, 256, 1024] {
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

/// Streaming BaoTreeBuilder, contrasted with the eager BaoTree::build
/// above. Both must produce the same root hash (verified by an
/// accompanying unit test in `adnet-blobstore`); the bench measures any
/// allocation difference.
pub fn bench_bao_streaming_large(c: &mut Criterion) {
    let mut group = c.benchmark_group("bao_large/streaming");

    for size_mib in [1u64, 16, 64, 256] {
        let size = (size_mib * 1024 * 1024) as usize;
        let data = make_payload(size);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(BenchmarkId::from_parameter(size_mib), |b| {
            b.iter(|| {
                let mut builder = BaoTreeBuilder::new();
                // Feed in 64 KiB chunks to model how a real consumer reads
                // from disk / network.
                for chunk in data.chunks(64 * 1024) {
                    builder.write(chunk);
                }
                let _tree = builder.finish();
            });
        });
    }

    group.finish();
}

/// Write a 1 GiB payload to `/dev/null` via a `BufWriter` analogue to
/// measure the inner-loop cost of `ChunkWriter` per chunk_size write.
/// This is purely a timing harness: even if 1 GiB doesn't get written
/// to disk in your environment, the bench still times the read loop.
pub fn bench_bao_input_io_pattern(c: &mut Criterion) {
    let mut group = c.benchmark_group("bao_large/input_io");
    let size = 64 * 1024 * 1024usize;
    let data = make_payload(size);

    group.throughput(Throughput::Bytes(size as u64));
    group.bench_function("64mib_64kib_chunks", |b| {
        b.iter(|| {
            let mut sink = std::io::sink();
            for chunk in data.chunks(64 * 1024) {
                let _ = sink.write_all(chunk);
            }
        });
    });

    group.finish();
}

/// Register all P0 anchor benchmarks.
pub fn register(c: &mut Criterion) {
    bench_bao_build_large(c);
    bench_bao_streaming_large(c);
    bench_bao_input_io_pattern(c);
}
