# ADNet Testing & Performance Infrastructure

This document describes the comprehensive testing and performance analysis tools available for the ADNet project.

## Overview

ADNet includes a multi-layered testing infrastructure:

```
┌─────────────────────────────────────────────────────────────────┐
│                     CI/CD Pipeline                              │
├─────────────────────────────────────────────────────────────────┤
│  - GitHub Actions Workflows                                     │
│  - Security Auditing                                            │
│  - Cross-platform Builds                                        │
│  - Performance Regression Detection                             │
└─────────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────────┐
│                  Testing Tools                                  │
├────────────────┬────────────────┬────────────────┬───────────────┤
│  Unit Tests    │ Integration   │  Fuzz Tests   │ Chaos Tests  │
│  (in-crate)   │ Tests         │  (libfuzzer)  │ (simulator)  │
└────────────────┴────────────────┴────────────────┴───────────────┘
                              │
┌─────────────────────────────────────────────────────────────────┐
│                Performance Tools                                 │
├─────────────────────────┬───────────────────────────────────────┤
│   adnet-bench          │        adnet-simulator                 │
│   (Criterion-based)    │    (Network Conditions)                │
└─────────────────────────┴───────────────────────────────────────┘
```

## Quick Start

### Run All Tests

```bash
# Full test suite
./scripts/run_tests.sh

# Quick tests only (unit + doc)
./scripts/run_tests.sh --quick

# Integration tests
./scripts/run_tests.sh --integration
```

### Run Benchmarks

```bash
# Full benchmark suite
./scripts/run_perf_tests.sh

# CI mode (fast)
./scripts/run_perf_tests.sh --ci

# Compare with baseline
./scripts/run_perf_tests.sh --compare
```

## 1. Unit Tests

Unit tests are co-located with the source code in each crate:

```rust
// In crates/adnet-dht/src/bucket.rs
#[cfg(test)]
mod tests {
    #[test]
    fn test_kbucket_insert() {
        // ...
    }
}
```

**Run unit tests:**
```bash
cargo test --workspace --lib
```

## 2. Integration Tests (`adnet-integration-tests`)

Comprehensive integration tests covering network, storage, and chaos scenarios.

**Location:** `crates/adnet-integration-tests/src/`

**Categories:**

| Module | Description |
|--------|-------------|
| `network.rs` | DHT, Gossip, Transport integration |
| `storage.rs` | BlobStore, CAR files, BAO tree |
| `chaos.rs` | Network failures, partitions, recovery |
| `multi_node.rs` | Two-node, multi-node, cluster tests |
| `protocol.rs` | Bitswap, GraphSync, IPNS |

**Run integration tests:**
```bash
cargo test -p adnet-integration-tests
```

## 3. Fuzzing (`adnet-fuzz`)

Coverage-guided fuzzing using `cargo-fuzz` to find parsing vulnerabilities.

**Location:** `crates/adnet-fuzz/fuzz_targets/`

**Fuzz Targets:**

| Target | Description |
|--------|-------------|
| `parse_announcement` | Announcement deserialization |
| `parse_cid` | CID parsing and validation |
| `parse_node_id` | NodeId parsing |
| `parse_dht_message` | DHT wire protocol |
| `parse_graphsync` | GraphSync messages |
| `parse_bitswap` | Bitswap protocol |

**Install and run fuzzing:**
```bash
cargo install cargo-fuzz
cargo fuzz list
cargo fuzz run parse_announcement
```

## 4. Benchmark Suite (`adnet-bench`)

Criterion-based benchmarks for measuring performance.

**Location:** `crates/adnet-bench/src/`

**Benchmark Groups:**

| Group | Metrics |
|-------|---------|
| `blob/*` | Import, read, chunk hashing |
| `bao/*` | Tree build, proof generation |
| `dht/*` | K-bucket, routing table |
| `gossip/*` | Fan-out, throughput |
| `crypto/*` | BLAKE3, Ed25519, Argon2 |
| `transport/*` | Frame encoding, connection overhead |

**Run benchmarks:**
```bash
cargo bench -p adnet-bench
```

## 5. Network Simulator (`adnet-simulator`)

Realistic network condition simulation for testing resilience.

**Location:** `crates/adnet-simulator/src/`

**Capabilities:**

- **Latency simulation** - Fixed, variable, with jitter
- **Packet loss** - Random, burst patterns
- **Bandwidth throttling** - Upload/download limits
- **Network partitions** - Temporary isolation
- **Corruption** - Data corruption simulation

**Usage:**
```rust
use adnet_simulator::{NetworkEmulator, NetworkCondition, Latency, PacketLoss};

let emulator = NetworkEmulator::new();
let mut condition = NetworkCondition::default();
condition.latency = Some(Latency::new(50)); // 50ms latency
condition.packet_loss = Some(PacketLoss::new(0.01)); // 1% loss

emulator.add_connection(conn_id, condition).await;
```

**Preset Scenarios:**
```rust
use adnet_simulator::presets::{good_network, mobile_network, satellite_network};

let condition = good_network();  // 20ms, 0.1% loss
let condition = mobile_network();  // 200ms ± 100ms jitter, 2% loss
let condition = satellite_network();  // 600ms, 1% loss
```

## 6. CI/CD Pipeline

**Location:** `.github/workflows/ci.yml`

**Pipeline Stages:**

| Stage | Description |
|-------|-------------|
| `fmt` | Rustfmt code formatting |
| `clippy` | Clippy linting |
| `test` | Unit + doc tests (stable/beta/nightly) |
| `integration-tests` | Full integration suite |
| `perf-baseline` | Performance measurement |
| `perf-compare` | Regression detection |
| `security-audit` | Cargo-audit vulnerability scan |
| `fuzz-smoke` | Quick fuzzing smoke test |
| `build-cross` | Cross-platform compilation |
| `msrv` | Minimum Rust version check |

**Performance Regression Detection:**

The CI pipeline:
1. Runs benchmarks on every PR to `main`
2. Stores baseline in artifacts
3. Compares new runs against baseline
4. Alerts on >10% regressions

## Test Configuration

**Benchmarks:** `benches/bencher.toml`

**CI Environment Variables:**
```bash
RUST_BACKTRACE=1
RUSTFLAGS="-D warnings"  # Treat warnings as errors
CARGO_TERM_COLOR=always
```

## Coverage Goals

| Metric | Target |
|--------|--------|
| Line Coverage | >80% |
| Branch Coverage | >70% |
| Critical Paths | 100% |

## Best Practices

1. **Write tests before fixing bugs** - Regression tests prevent recurrence
2. **Profile before optimizing** - Use benchmarks to identify bottlenecks
3. **Fuzz critical parsers** - Security vulnerabilities often in parsing code
4. **Test network resilience** - Simulate real-world conditions
5. **Monitor performance** - Set thresholds and alert on regressions

## Troubleshooting

**Benchmarks are too slow for CI:**
```bash
# Use CI mode
./scripts/run_perf_tests.sh --ci
```

**Fuzzing finds crashes:**
```bash
# Reproduce with debug build
cargo fuzz run parse_announcement --fuzz-target-args 'crash-...'
```

**Integration tests timeout:**
```bash
# Increase timeout
RUST_TEST_TIMEOUT=300 cargo test ...
```

## Additional Resources

- [Criterion Documentation](https://bheisner.github.io/criterion.rs/)
- [cargo-fuzz Book](https://rust-fuzz.github.io/book/cargo-fuzz.html)
- [tokio Test Utilities](https://docs.rs/tokio/latest/tokio/test/index.html)
- [Rust Testing Best Practices](https://rust-lang.github.io/mdBook/guide/testing.html)
