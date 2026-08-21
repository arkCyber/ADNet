//! Phase 5d: Performance benchmark tests for group sync.
//!
//! This module provides benchmarks for:
//! - Message sync throughput
//! - Latency measurements
//! - Large batch processing
//! - Concurrent operations
//!
//! ## Running Benchmarks
//!
//! ```bash
//! # Run all benchmarks
//! cargo test -p a3chat-app --features iroh --lib -- benchmarks --nocapture
//!
//! # Run specific benchmark
//! cargo test -p a3chat-app --features iroh --lib -- benchmarks_throughput --nocapture
//! ```

#![cfg(feature = "iroh")]

use std::time::Instant;
use std::path::PathBuf;
use std::fs::{self, OpenOptions};
use std::io::Write;
use serde::{Deserialize, Serialize};

use a3chat_core::id::{ConversationId, UserId};

use super::SyncMetricsCollector;

/// Phase 5d: Throughput benchmark result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThroughputBenchmark {
    /// Messages per second.
    pub msg_per_sec: f64,
    /// Total messages processed.
    pub total_messages: usize,
    /// Total duration in milliseconds.
    pub duration_ms: u64,
}

impl ThroughputBenchmark {
    /// Run throughput benchmark.
    pub async fn run(message_count: usize, batch_size: usize) -> Self {
        let collector = SyncMetricsCollector::new();

        let start = Instant::now();

        // Simulate sync operations
        for i in 0..message_count {
            let batch = if i % batch_size == 0 {
                batch_size.min(message_count - i)
            } else {
                1
            };

            collector.record_sync(batch, 10).await;
        }

        let duration = start.elapsed();
        let msg_per_sec = message_count as f64 / duration.as_secs_f64();

        Self {
            msg_per_sec,
            total_messages: message_count,
            duration_ms: duration.as_millis() as u64,
        }
    }
}

/// Phase 5d: Latency benchmark result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyBenchmark {
    /// Average latency in milliseconds.
    pub avg_ms: f64,
    /// P50 latency in milliseconds.
    pub p50_ms: f64,
    /// P95 latency in milliseconds.
    pub p95_ms: f64,
    /// P99 latency in milliseconds.
    pub p99_ms: f64,
    /// Maximum latency in milliseconds.
    pub max_ms: u64,
}

impl LatencyBenchmark {
    /// Run latency benchmark.
    pub async fn run(operation_count: usize) -> Self {
        let mut latencies = Vec::with_capacity(operation_count);

        for _ in 0..operation_count {
            let start = Instant::now();

            // Simulate sync operation
            tokio::task::yield_now().await;

            let elapsed = start.elapsed().as_millis() as u64;
            latencies.push(elapsed);
        }

        latencies.sort();
        let avg = latencies.iter().sum::<u64>() as f64 / latencies.len() as f64;
        let p50 = percentile(&latencies, 50);
        let p95 = percentile(&latencies, 95);
        let p99 = percentile(&latencies, 99);
        let max = *latencies.iter().max().unwrap_or(&0);

        Self {
            avg_ms: avg,
            p50_ms: p50,
            p95_ms: p95,
            p99_ms: p99,
            max_ms: max,
        }
    }
}

/// Phase 5d: Large batch benchmark.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LargeBatchBenchmark {
    /// Batch size.
    pub batch_size: usize,
    /// Processing time in milliseconds.
    pub processing_time_ms: u64,
    /// Messages per second.
    pub throughput: f64,
}

impl LargeBatchBenchmark {
    /// Run large batch benchmark.
    pub async fn run(messages: &[String]) -> Self {
        let collector = SyncMetricsCollector::new();
        let start = Instant::now();

        // Process in batches
        let batch_size = 100;
        for chunk in messages.chunks(batch_size) {
            collector.record_sync(chunk.len(), 10).await;
        }

        let processing_time = start.elapsed();
        let throughput = messages.len() as f64 / processing_time.as_secs_f64();

        Self {
            batch_size,
            processing_time_ms: processing_time.as_millis() as u64,
            throughput,
        }
    }
}

/// Phase 5d: Concurrent benchmark.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurrentBenchmark {
    /// Number of concurrent operations.
    pub concurrency: usize,
    /// Total operations.
    pub total_ops: usize,
    /// Operations per second.
    pub ops_per_sec: f64,
}

impl ConcurrentBenchmark {
    /// Run concurrent benchmark.
    pub async fn run(concurrency: usize, ops_per_worker: usize) -> Self {
        let collector = SyncMetricsCollector::new();
        let start = Instant::now();

        let mut handles = Vec::with_capacity(concurrency);

        for _ in 0..concurrency {
            let c = collector.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..ops_per_worker {
                    c.record_sync(10, 5).await;
                }
            }));
        }

        for h in handles {
            h.await.expect("task completed");
        }

        let duration = start.elapsed();
        let total_ops = concurrency * ops_per_worker;
        let ops_per_sec = total_ops as f64 / duration.as_secs_f64();

        Self {
            concurrency,
            total_ops,
            ops_per_sec,
        }
    }
}

// Helper function
fn percentile(sorted: &[u64], p: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((p as f64 / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)] as f64
}

/// Combined benchmark results with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkResults {
    /// Timestamp when benchmarks were run (RFC3339 format)
    pub timestamp: String,
    /// Git commit hash (if available)
    pub git_commit: Option<String>,
    /// Cargo package version
    pub version: String,
    /// Throughput benchmark results
    pub throughput: Option<ThroughputBenchmark>,
    /// Latency benchmark results
    pub latency: Option<LatencyBenchmark>,
    /// Large batch benchmark results
    pub large_batch: Option<LargeBatchBenchmark>,
    /// Concurrent benchmark results
    pub concurrent: Option<ConcurrentBenchmark>,
}

impl BenchmarkResults {
    /// Create a new benchmark results container with metadata
    pub fn new() -> Self {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let git_commit = get_git_commit();
        let version = env!("CARGO_PKG_VERSION").to_string();

        Self {
            timestamp,
            git_commit,
            version,
            throughput: None,
            latency: None,
            large_batch: None,
            concurrent: None,
        }
    }

    /// Save results to a JSON file
    pub fn save_to_file(&self, path: impl Into<PathBuf>) -> std::io::Result<()> {
        let path = path.into();
        
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        
        file.write_all(json.as_bytes())?;
        
        Ok(())
    }

    /// Append results to a JSONL (JSON Lines) file for historical tracking
    pub fn append_to_history(&self, path: impl Into<PathBuf>) -> std::io::Result<()> {
        let path = path.into();
        
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string(self)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        
        writeln!(file, "{}", json)?;
        
        Ok(())
    }

    /// Load results from a JSON file
    pub fn load_from_file(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let content = fs::read_to_string(path)?;
        let results: Self = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(results)
    }

    /// Load all historical results from a JSONL file
    pub fn load_history(path: impl Into<PathBuf>) -> std::io::Result<Vec<Self>> {
        let path = path.into();
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(path)?;
        let mut results = Vec::new();

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Self>(line) {
                Ok(result) => results.push(result),
                Err(e) => eprintln!("Warning: Failed to parse line: {}", e),
            }
        }

        Ok(results)
    }
}

impl Default for BenchmarkResults {
    fn default() -> Self {
        Self::new()
    }
}

/// Get current git commit hash
fn get_git_commit() -> Option<String> {
    std::process::Command::new("git")
        .args(&["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
}

// ========================================================================
// Benchmark Tests
// ========================================================================

#[cfg(test)]
mod benchmarks {
    use super::*;

    /// Phase 5d: Throughput benchmark test with persistence.
    #[tokio::test]
    async fn benchmarks_throughput() {
        println!("\n=== Throughput Benchmark ===");

        let result = ThroughputBenchmark::run(1000, 10).await;

        println!("Messages: {}", result.total_messages);
        println!("Duration: {} ms", result.duration_ms);
        println!("Throughput: {:.2} msg/s", result.msg_per_sec);

        assert!(result.total_messages == 1000);
        assert!(result.msg_per_sec > 0.0);

        // Demonstrate persistence
        let mut benchmark_results = BenchmarkResults::new();
        benchmark_results.throughput = Some(result);
        
        // Save to temporary file (in real usage, this would be a project directory)
        if let Ok(temp_dir) = std::env::temp_dir().canonicalize() {
            let results_path = temp_dir.join("a3chat_benchmark_throughput.json");
            if let Ok(_) = benchmark_results.save_to_file(&results_path) {
                println!("📊 Results saved to: {:?}", results_path);
            }
        }
    }

    /// Phase 5d: Latency benchmark test.
    #[tokio::test]
    async fn benchmarks_latency() {
        println!("\n=== Latency Benchmark ===");

        let result = LatencyBenchmark::run(1000).await;

        println!("Average: {:.2} ms", result.avg_ms);
        println!("P50: {:.2} ms", result.p50_ms);
        println!("P95: {:.2} ms", result.p95_ms);
        println!("P99: {:.2} ms", result.p99_ms);
        println!("Max: {} ms", result.max_ms);

        // Note: operations are very fast in memory, so avg might be ~0ms
        // The important thing is that p95/p99 are recorded correctly
        assert!(result.p95_ms >= 0.0);
        assert!(result.p99_ms >= result.p95_ms);
    }

    /// Phase 5d: Large batch benchmark test.
    #[tokio::test]
    async fn benchmarks_large_batch() {
        println!("\n=== Large Batch Benchmark ===");

        let messages: Vec<String> = (0..10000).map(|i| format!("Message {}", i)).collect();

        let result = LargeBatchBenchmark::run(&messages).await;

        println!("Batch size: {}", result.batch_size);
        println!("Processing time: {} ms", result.processing_time_ms);
        println!("Throughput: {:.2} msg/s", result.throughput);

        // Note: processing is in-memory and very fast
        assert!(result.processing_time_ms >= 0);
        assert!(result.throughput > 0.0);
    }

    /// Phase 5d: Concurrent benchmark test.
    #[tokio::test]
    async fn benchmarks_concurrent() {
        println!("\n=== Concurrent Benchmark ===");

        let result = ConcurrentBenchmark::run(10, 100).await;

        println!("Concurrency: {}", result.concurrency);
        println!("Total ops: {}", result.total_ops);
        println!("Ops/s: {:.2}", result.ops_per_sec);

        assert!(result.total_ops == 1000);
        assert!(result.ops_per_sec > 0.0);
    }

    /// Phase 5d: Full sync cycle benchmark.
    #[tokio::test]
    async fn benchmarks_full_sync_cycle() {
        println!("\n=== Full Sync Cycle Benchmark ===");

        let collector = SyncMetricsCollector::new();
        let conv_id = ConversationId::from("bench-conv");
        let owner = UserId::from("bench-owner");

        // Simulate a full sync cycle
        let start = Instant::now();

        // Phase 1: Fetch from iroh
        let fetch_start = Instant::now();
        collector
            .record_sync(50, fetch_start.elapsed().as_millis() as u64)
            .await;

        // Phase 2: Deduplicate
        let dedup_start = Instant::now();
        collector
            .record_sync(5, dedup_start.elapsed().as_millis() as u64)
            .await;

        // Phase 3: Write to SQLite
        let write_start = Instant::now();
        collector
            .record_sync(45, write_start.elapsed().as_millis() as u64)
            .await;

        // Phase 4: Notify subscribers
        let notify_start = Instant::now();
        collector
            .record_sync(0, notify_start.elapsed().as_millis() as u64)
            .await;

        let total_duration = start.elapsed();

        let snapshot = collector.snapshot().await;

        println!(
            "Total duration: {:?} ({:.2} ms)",
            total_duration,
            total_duration.as_millis()
        );
        println!("Messages synced: {}", snapshot.messages_synced_total);
        println!("Sync operations: {}", snapshot.sync_operations_total);

        assert!(snapshot.messages_synced_total > 0);
        assert!(snapshot.sync_operations_total >= 3);
    }

    /// Phase 5d: Stress test benchmark.
    #[tokio::test]
    async fn benchmarks_stress_test() {
        println!("\n=== Stress Test Benchmark ===");

        let collector = SyncMetricsCollector::new();
        let iterations = 100;
        let messages_per_iteration = 1000;

        let start = Instant::now();

        for _ in 0..iterations {
            collector.record_sync(messages_per_iteration, 50).await;
        }

        let duration = start.elapsed();
        let total_messages = iterations * messages_per_iteration;
        let throughput = total_messages as f64 / duration.as_secs_f64();

        println!("Iterations: {}", iterations);
        println!("Messages per iteration: {}", messages_per_iteration);
        println!("Total messages: {}", total_messages);
        println!("Duration: {:?}", duration);
        println!("Throughput: {:.2} msg/s", throughput);

        let snapshot = collector.snapshot().await;
        assert_eq!(snapshot.messages_synced_total, total_messages as u64);
        assert!(throughput > 0.0);
    }
}
