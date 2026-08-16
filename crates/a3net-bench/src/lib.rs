// SPDX-License-Identifier: MIT OR Apache-2.0
//
// a3net-bench — Criterion-based benchmark suite for A3Net.
//
// This crate provides structured, reproducible benchmarks for:
// - Network layer (DHT lookup, gossip fan-out, transport latency)
// - Storage layer (blob import/read, BAO tree operations)
// - Crypto operations (signing, hashing, key derivation)
//
// ## Running Benchmarks
//
// Run all benchmarks (requires nightly for better precision):
//     cargo bench -p a3net-bench
//
// Run specific benchmark group:
//     cargo bench -p a3net-bench -- gossip
//     cargo bench -p a3net-bench -- dht
//     cargo bench -p a3net-bench -- blob
//     cargo bench -p a3net-bench -- crypto
//
// Generate HTML report:
//     cargo bench -p a3net-bench -- --html
//
// ## Benchmark Guidelines
//
// 1. All benchmarks use deterministic inputs (seeded RNG or fixed payloads)
// 2. Warm-up runs are performed automatically by Criterion
// 3. Statistical outlier detection is enabled
// 4. Progress is reported via `indicatif` for long-running benchmarks

pub mod crypto;
pub mod network;
pub mod storage;
pub mod config;
pub mod report;
pub mod bao_large;

pub use config::BenchConfig;
pub use report::BenchReport;
