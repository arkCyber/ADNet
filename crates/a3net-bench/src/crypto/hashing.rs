// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Hashing operation benchmarks.

use criterion::{BenchmarkId, Criterion, Throughput};

/// Generate test data.
fn make_payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// Benchmark BLAKE3 hashing.
pub fn bench_blake3(c: &mut Criterion) {
    let mut group = c.benchmark_group("crypto/blake3");

    for size in [64, 256, 1024, 4096, 65536, 1048576].iter() {
        let data = make_payload(*size);

        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_function(BenchmarkId::from_parameter(size), |b| {
            b.iter(|| {
                let _ = blake3::hash(&data);
            });
        });
    }

    group.finish();
}

/// Benchmark SHA-256 hashing using Digest trait.
pub fn bench_sha256(c: &mut Criterion) {
    let mut group = c.benchmark_group("crypto/sha256");

    for size in [64, 256, 1024, 4096, 65536].iter() {
        let data = make_payload(*size);

        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_function(BenchmarkId::from_parameter(size), |b| {
            b.iter(|| {
                use sha2::{Sha256, Digest};
                let mut hasher = Sha256::new();
                Digest::update(&mut hasher, &data);
                let _ = hasher.finalize();
            });
        });
    }

    group.finish();
}

/// Benchmark BLAKE3 multi-threaded hashing (large data).
pub fn bench_blake3_parallel(c: &mut Criterion) {
    let mut group = c.benchmark_group("crypto/blake3_parallel");

    for size_mb in [1, 4, 16, 64].iter() {
        let size = size_mb * 1024 * 1024;
        let data = make_payload(size);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(BenchmarkId::from_parameter(size_mb), |b| {
            b.iter(|| {
                let _ = blake3::hash(&data);
            });
        });
    }

    group.finish();
}

/// Benchmark BLAKE3 streaming hash.
pub fn bench_blake3_streaming(c: &mut Criterion) {
    let mut group = c.benchmark_group("crypto/blake3_streaming");

    let total_size = 16 * 1024 * 1024; // 16 MB
    let chunk_size = 64 * 1024; // 64 KB chunks

    group.throughput(Throughput::Bytes(total_size as u64));
    group.bench_function("streaming_hash", |b| {
        b.iter(|| {
            let mut hasher = blake3::Hasher::new();
            let mut offset = 0;
            while offset < total_size {
                let chunk = make_payload(chunk_size);
                hasher.update(&chunk);
                offset += chunk_size;
            }
            let _ = hasher.finalize();
        });
    });

    group.finish();
}

/// Register all hashing benchmarks.
pub fn register(c: &mut Criterion) {
    bench_blake3(c);
    bench_sha256(c);
    bench_blake3_parallel(c);
    bench_blake3_streaming(c);
}
