//! Minimal example — register a single benchmark group and print a
//! `BenchReport` so you can see the configuration presets end-to-end.
//!
//! Run with:
//!   cargo run -p adnet-bench --example bench_basic --release

use adnet_bench::crypto::hashing;
use adnet_bench::{BenchConfig, report::BenchmarkResult};

fn main() {
    // Pick the configuration that matches your environment. The CI preset
    // is intentionally tiny (10 samples, 3 s measurement) so it doesn't
    // stall the workflow.
    let config = BenchConfig::ci();

    println!("=== adnet-bench basic example ===");
    println!(
        "config: sample_size={}, measurement_time={:?}, threads={}",
        config.sample_size, config.measurement_time, config.threads
    );

    // Register the hashing group against a Criterion harness. The library
    // never owns the harness — pass one in. For a non-criterion quick
    // smoke test we just enumerate the registered benches.
    let mut criterion = criterion::Criterion::default()
        .sample_size(config.sample_size)
        .measurement_time(config.measurement_time)
        .warm_up_time(config.warm_up_time)
        .noise_threshold(config.noise_threshold);

    println!("registering hashing benchmarks...");
    hashing::register(&mut criterion);
    println!("registration complete (sample_size={})", config.sample_size);

    // Build a fake BenchmarkResult so we can exercise BenchReport's
    // markdown summary path without actually running Criterion (which
    // would take real wall-clock time).
    let results = vec![BenchmarkResult {
        name: "crypto/blake3/4096".into(),
        group: "crypto/blake3".into(),
        mean: std::time::Duration::from_micros(950),
        median: std::time::Duration::from_micros(945),
        std_dev: std::time::Duration::from_micros(15),
        min: std::time::Duration::from_micros(900),
        max: std::time::Duration::from_micros(1100),
        samples: config.sample_size,
        throughput: Some(adnet_bench::report::ThroughputMetric {
            value: 4_000_000.0 / 950.0,
            unit: "MiB/s".into(),
        }),
    }];

    let report = adnet_bench::BenchReport {
        config,
        results,
        comparisons: Default::default(),
        generated_at: chrono::Utc::now(),
    };

    println!("\n{}", report.markdown_summary());
}
