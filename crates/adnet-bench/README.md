# adnet-bench

> Criterion-based benchmark suite for ADNet — measure network, storage and crypto hot paths.

## 概览 (Overview)

`adnet-bench` is the **performance regression suite** for the ADNet workspace. It uses
[Criterion.rs](https://github.com/bheisler/criterion.rs) to produce statistically
rigorous measurements for the layers that determine user-visible latency and
throughput: DHT routing-table operations, gossip fan-out, transport framing,
BAO tree construction, blob import/read, BLAKE3 hashing, Ed25519 signing and
key derivation.

The suite is driven by `src/main.rs` which registers every benchmark group
under a single `criterion::Criterion` instance. Each group can be run in
isolation by passing its name as a filter (`cargo bench -- gossip`). The
companion `benches/bencher.toml` defines the **critical** benchmarks that
must not regress on PRs and the **extended** suite run nightly.

## 特性 (Features)

- **Network benchmarks** (`network::dht`, `network::gossip`, `network::transport`):
  - K-bucket insert / lookup / remove (10, 50, 100 contacts)
  - Single-publisher gossip throughput at 100 / 1k / 10k messages
  - NodeId JSON serialisation round-trips
  - MTU-sized packet fragmentation
  - Simulated connection handshake + stream multiplexing
- **Storage benchmarks** (`storage::bao`, `storage::blob`):
  - BAO tree construction on 1 / 4 / 16 / 64 MiB blobs
  - BAO proof generation + verification (single + per-range)
  - BAO leaf lookup and full-tree iteration
  - Chunk hashing throughput (1 KiB / 4 KiB / 16 KiB)
- **Crypto benchmarks** (`crypto::hashing`, `crypto::signing`, `crypto::key_derivation`):
  - BLAKE3 hashing from 64 B to 1 MiB
  - BLAKE3 streaming hash + parallel hash on multi-MiB inputs
  - SHA-256 hashing (1 KiB … 64 KiB)
  - Ed25519 sign / verify single + batched
  - Announcement signing full-pipeline (JSON encode → sign → wrap)
- **Configuration presets** (`BenchConfig`):
  - `BenchConfig::default()` — 100 samples, 10 s measurement
  - `BenchConfig::ci()` — 10 samples, 3 s measurement, single-threaded
  - `BenchConfig::thorough()` — 200 samples, 30 s measurement
- **Report generation** (`BenchReport`): serialises results to JSON and
  markdown, computes baseline-vs-current deltas, tags regressions as
  *SIGNIFICANT* above 5 % drift.

## 安装 (Installation)

`adnet-bench` is a workspace-internal crate — it is **not** published. Add it
as a path dependency from another workspace crate (e.g. a future
`xtask`) if you want to call `BenchReport` programmatically:

```toml
[dev-dependencies]
adnet-bench = { workspace = true }
```

The Criterion binary entrypoint lives at `src/main.rs` (`adnet-bench`); run
it directly via `cargo bench`.

## 使用 (Usage)

Run the whole suite:

```bash
cargo bench -p adnet-bench
```

Run a single benchmark group:

```bash
cargo bench -p adnet-bench -- gossip
cargo bench -p adnet-bench -- dht
cargo bench -p adnet-bench -- blob
cargo bench -p adnet-bench -- crypto
```

Generate an HTML report and save the markdown summary:

```bash
cargo bench -p adnet-bench -- --html
```

Use the benchmark library from Rust (e.g. inside a test harness or
nightly-runner):

```rust
use adnet_bench::{BenchConfig, BenchReport};
use criterion::Criterion;

let cfg = BenchConfig::ci();
let mut c = Criterion::default()
    .sample_size(cfg.sample_size)
    .measurement_time(cfg.measurement_time);

adnet_bench::crypto::hashing::register(&mut c);
adnet_bench::network::dht::register(&mut c);
c.final_summary();
```

Render a comparison report:

```rust
use adnet_bench::{BenchConfig, BenchReport, report::BenchmarkResult};

let cfg = BenchConfig::default();
let mut report = BenchReport {
    config: cfg,
    results: Vec::<BenchmarkResult>::new(),
    comparisons: Default::default(),
    generated_at: chrono::Utc::now(),
};

report.write("target/bench")?; // writes bench.json + bench.md
```

## 应用案例 (Use Cases / Examples)

- **CI regression gate** — keep `benches/bencher.toml`'s `critical` group
  on the `bench-critical` job. Any benchmark that drifts >5 % blocks the
  PR with a comment linking the rendered markdown report.
- **Pre-release performance audit** — invoke `BenchConfig::thorough()`
  from a release checklist script, write `BenchReport` to `reports/v0.x/`
  and diff against the previous tag's `bench.md`.
- **Cross-layer correlation study** — combine `storage::bao` results
  with `network::transport::fragmentation` numbers to model how a 16 MiB
  BAO blob is transported over a 1500 B MTU link, end-to-end.
- **Hardware tier sizing** — run `cargo bench -- crypto/blake3_parallel/16`
  on candidate edge devices to validate that BLAKE3 saturates enough
  cores to meet the 1 Gbps bitswap target.
- **Crypto-bench triage** — when a signing-related PR lands, isolate it
  with `cargo bench -p adnet-bench -- ed25519_sign` and look at the
  `BenchmarkReport::markdown_summary` to confirm the announcement pipeline
  hasn't introduced an extra JSON serialisation round-trip.

## 许可

MIT OR Apache-2.0
