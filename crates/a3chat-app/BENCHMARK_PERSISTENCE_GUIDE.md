# Benchmark Persistence Usage Guide

## Overview

The benchmark persistence feature allows you to save and track performance metrics over time. This is useful for:
- Comparing performance across different code versions
- Tracking performance regressions
- Creating historical performance reports
- CI/CD integration for performance monitoring

## Basic Usage

### 1. Running Benchmarks and Saving Results

```rust
use a3chat_app::group_sync_service::benchmarks::{
    BenchmarkResults, ThroughputBenchmark
};

#[tokio::main]
async fn main() {
    // Run a benchmark
    let throughput = ThroughputBenchmark::run(1000, 10).await;
    
    // Create results container with metadata
    let mut results = BenchmarkResults::new();
    results.throughput = Some(throughput);
    
    // Save to JSON file
    results.save_to_file("benchmark_results.json")
        .expect("Failed to save results");
    
    // Append to history (JSONL format)
    results.append_to_history("benchmark_history.jsonl")
        .expect("Failed to append history");
}
```

### 2. Loading Historical Data

```rust
use a3chat_app::group_sync_service::benchmarks::BenchmarkResults;

// Load latest results
let latest = BenchmarkResults::load_from_file("benchmark_results.json")
    .expect("Failed to load results");

// Load all historical results
let history = BenchmarkResults::load_history("benchmark_history.jsonl")
    .expect("Failed to load history");

println!("Historical entries: {}", history.len());
```

### 3. Complete Example

```rust
use a3chat_app::group_sync_service::benchmarks::{
    BenchmarkResults,
    ThroughputBenchmark,
    LatencyBenchmark,
    LargeBatchBenchmark,
    ConcurrentBenchmark,
};

#[tokio::main]
async fn main() {
    let mut results = BenchmarkResults::new();
    
    // Run all benchmarks
    results.throughput = Some(ThroughputBenchmark::run(10000, 100).await);
    results.latency = Some(LatencyBenchmark::run(1000).await);
    
    let messages: Vec<String> = (0..10000)
        .map(|i| format!("Message {}", i))
        .collect();
    results.large_batch = Some(LargeBatchBenchmark::run(&messages).await);
    
    results.concurrent = Some(ConcurrentBenchmark::run(10, 100).await);
    
    // Save and track
    results.save_to_file("target/benchmarks/latest.json")?;
    results.append_to_history("target/benchmarks/history.jsonl")?;
    
    Ok(())
}
```

## File Formats

### JSON Format (latest results)
```json
{
  "timestamp": "2026-08-21T06:30:00Z",
  "git_commit": "5f5d118bf",
  "version": "0.1.0",
  "throughput": {
    "msg_per_sec": 1500.0,
    "total_messages": 10000,
    "duration_ms": 6667
  },
  "latency": {
    "avg_ms": 12.5,
    "p50_ms": 10.0,
    "p95_ms": 30.0,
    "p99_ms": 50.0,
    "max_ms": 120
  }
}
```

### JSONL Format (historical tracking)
```jsonl
{"timestamp":"2026-08-20T10:00:00Z","git_commit":"abc123","version":"0.1.0","throughput":{"msg_per_sec":1400.0,...}}
{"timestamp":"2026-08-20T11:00:00Z","git_commit":"def456","version":"0.1.0","throughput":{"msg_per_sec":1500.0,...}}
{"timestamp":"2026-08-21T06:30:00Z","git_commit":"5f5d118","version":"0.1.0","throughput":{"msg_per_sec":1600.0,...}}
```

## CI/CD Integration

### GitHub Actions Example

```yaml
name: Performance Benchmark

on: [push, pull_request]

jobs:
  benchmark:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      
      - name: Run Benchmarks
        run: |
          cargo test --package a3chat-app --features iroh --lib -- benchmarks --nocapture
      
      - name: Upload Results
        uses: actions/upload-artifact@v3
        with:
          name: benchmark-results
          path: target/benchmarks/
```

## Analyzing Historical Data

```rust
use a3chat_app::group_sync_service::benchmarks::BenchmarkResults;

fn analyze_performance_trend() {
    let history = BenchmarkResults::load_history("benchmark_history.jsonl")
        .expect("Failed to load history");
    
    println!("Performance Trend Analysis");
    println!("==========================\n");
    
    for (i, result) in history.iter().enumerate() {
        println!("Run #{}: {}", i + 1, result.timestamp);
        if let Some(tp) = &result.throughput {
            println!("  Throughput: {:.2} msg/s", tp.msg_per_sec);
        }
        if let Some(lat) = &result.latency {
            println!("  P95 Latency: {:.2} ms", lat.p95_ms);
        }
        println!();
    }
    
    // Compare latest vs baseline
    if history.len() >= 2 {
        let baseline = &history[0];
        let latest = history.last().unwrap();
        
        if let (Some(b_tp), Some(l_tp)) = (&baseline.throughput, &latest.throughput) {
            let change = ((l_tp.msg_per_sec - b_tp.msg_per_sec) / b_tp.msg_per_sec) * 100.0;
            println!("Throughput change: {:+.2}%", change);
        }
    }
}
```

## Best Practices

1. **Regular Benchmarking**: Run benchmarks on every commit or at regular intervals
2. **Version Tracking**: The system automatically captures git commit and version
3. **History Management**: Keep JSONL files for long-term trend analysis
4. **CI Integration**: Fail CI if performance degrades beyond threshold
5. **Documentation**: Add comments when significant performance changes occur

## Running Tests

```bash
# Run all benchmark tests
cargo test --package a3chat-app --features iroh --lib -- benchmarks --nocapture

# Run persistence tests
cargo test --package a3chat-app --test benchmark_persistence_test --features iroh -- --nocapture

# Run specific benchmark
cargo test --package a3chat-app --features iroh --lib -- benchmarks_throughput --nocapture
```
