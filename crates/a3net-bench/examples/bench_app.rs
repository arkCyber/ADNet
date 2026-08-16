//! Real-world example — run a configurable subset of `a3net-bench`
//! groups, collect the resulting metrics and write a markdown + JSON
//! report to `target/bench-report/`.
//!
//! This mirrors the workflow the nightly perf job uses to publish
//! reports to the team wiki:
//!   1. Pick a `BenchConfig` preset.
//!   2. Register the groups you care about.
//!   3. Render results into a `BenchReport`.
//!   4. Write `report.json` + `report.md`.
//!
//! Run with:
//!   cargo run -p a3net-bench --example bench_app --release -- --groups hashing,dht --out target/bench-report
//!
//! The example does not actually *run* Criterion (that takes minutes
//! per group). Instead it shows how the **report assembly** part of
//! the workflow fits together so a CI script can reuse the same data
//! shapes once `cargo bench` produces JSON output.

use std::path::PathBuf;

use a3net_bench::report::{BenchmarkResult, ThroughputMetric};
use a3net_bench::{BenchConfig, BenchReport};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let groups_arg = args
        .iter()
        .position(|a| a == "--groups")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "hashing,dht,gossip".into());
    let out_dir = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/bench-report"));

    let requested: Vec<&str> = groups_arg.split(',').map(str::trim).collect();
    println!("requested groups: {requested:?}");
    println!("output dir       : {out_dir:?}");

    // Use the "thorough" preset for local dev; the CI preset would
    // shrink sample size to keep the example fast.
    let config = BenchConfig::thorough();

    // Register the groups the operator asked for. The harness is
    // optional — calling `register` lets us know the group is wired
    // correctly without actually running the inner benchmark loops.
    let mut criterion = criterion::Criterion::default()
        .sample_size(config.sample_size)
        .measurement_time(config.measurement_time);

    for group in &requested {
        match *group {
            "hashing" => a3net_bench::crypto::hashing::register(&mut criterion),
            "signing" => a3net_bench::crypto::signing::register(&mut criterion),
            "kdf" | "key_derivation" => a3net_bench::crypto::key_derivation::register(&mut criterion),
            "dht" => a3net_bench::network::dht::register(&mut criterion),
            "gossip" => a3net_bench::network::gossip::register(&mut criterion),
            "transport" => a3net_bench::network::transport::register(&mut criterion),
            "bao" => a3net_bench::storage::bao::register(&mut criterion),
            "blob" => a3net_bench::storage::blob::register(&mut criterion),
            other => {
                eprintln!("warning: unknown group '{other}', skipping");
                continue;
            }
        }
        println!("registered: {group}");
    }

    // Simulate a realistic `BenchReport` so the on-disk shape mirrors
    // what `cargo bench --save-baseline` would produce after a real
    // run. Each entry corresponds to one benchmark name you would
    // otherwise see in the Criterion terminal output.
    let results = synthesise_results(&requested);
    let comparisons = synthesise_comparisons(&results);

    let report = BenchReport {
        config,
        results,
        comparisons,
        generated_at: chrono::Utc::now(),
    };

    std::fs::create_dir_all(&out_dir)?;
    let prefix = out_dir.join("report");
    report.write(prefix.to_str().unwrap())?;
    println!("wrote {} (json + md)", prefix.display());

    Ok(())
}

fn synthesise_results(groups: &[&str]) -> Vec<BenchmarkResult> {
    let mut out = Vec::new();
    for group in groups {
        match *group {
            "hashing" => {
                for size in [256usize, 4096, 65536] {
                    out.push(BenchmarkResult {
                        name: format!("blake3/{size}"),
                        group: "crypto/blake3".into(),
                        mean: std::time::Duration::from_nanos(800 * size as u64),
                        median: std::time::Duration::from_nanos(795 * size as u64),
                        std_dev: std::time::Duration::from_nanos(20 * size as u64),
                        min: std::time::Duration::from_nanos(750 * size as u64),
                        max: std::time::Duration::from_nanos(900 * size as u64),
                        samples: 100,
                        throughput: Some(ThroughputMetric {
                            value: size as f64 * 1_000_000_000.0 / (800.0 * size as f64),
                            unit: "bytes/s".into(),
                        }),
                    });
                }
            }
            "dht" => {
                out.push(BenchmarkResult {
                    name: "kbucket_insert/50".into(),
                    group: "dht/kbucket_insert".into(),
                    mean: std::time::Duration::from_micros(420),
                    median: std::time::Duration::from_micros(415),
                    std_dev: std::time::Duration::from_micros(10),
                    min: std::time::Duration::from_micros(400),
                    max: std::time::Duration::from_micros(460),
                    samples: 100,
                    throughput: Some(ThroughputMetric {
                        value: 50.0 / 0.000420,
                        unit: "ops/s".into(),
                    }),
                });
            }
            "gossip" => {
                out.push(BenchmarkResult {
                    name: "single_publisher/1000".into(),
                    group: "gossip/single_publisher".into(),
                    mean: std::time::Duration::from_millis(85),
                    median: std::time::Duration::from_millis(84),
                    std_dev: std::time::Duration::from_millis(3),
                    min: std::time::Duration::from_millis(80),
                    max: std::time::Duration::from_millis(95),
                    samples: 100,
                    throughput: Some(ThroughputMetric {
                        value: 1_000.0 / 0.085,
                        unit: "ops/s".into(),
                    }),
                });
            }
            _ => {}
        }
    }
    out
}

fn synthesise_comparisons(
    results: &[BenchmarkResult],
) -> std::collections::HashMap<String, a3net_bench::report::Comparison> {
    // Pretend every benchmark regressed by ~1.5% — well within the
    // 5 % "critical" threshold from `benches/bencher.toml`. The CI
    // job would treat these as `stable` and pass the PR.
    results
        .iter()
        .map(|r| {
            (
                r.name.clone(),
                a3net_bench::report::Comparison {
                    baseline_mean: r.mean,
                    current_mean: r.mean + std::time::Duration::from_micros(15),
                    change_pct: 1.5,
                    significant: false,
                },
            )
        })
        .collect()
}
