//! Phase 5d: Group Sync E2E Integration Tests
//!
//! This module provides test infrastructure for group sync testing.
//!
//! ## Test Types
//!
//! - **Benchmark**: Performance measurement for sync operations
//! - **Partition**: Network partition simulation
//! - **Recovery**: System recovery after failures
//!
//! ## Running Tests
//!
//! ```bash
//! # Run network partition tests
//! cargo test -p a3net-chatstore --features iroh --test derp_relay_test
//!
//! # Run group sync service tests
//! cargo test -p a3chat-app --features iroh -- group_sync_service
//!
//! # Run a3net-relay DERP tests
//! cargo test -p a3net-relay --features derp
//! ```

#![cfg(feature = "chaos_tests")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ========================================================================
// Benchmark Types
// ========================================================================

/// Phase 5d: Benchmark result for sync operations.
#[derive(Debug, Clone)]
pub struct SyncBenchmark {
    /// Total messages in benchmark.
    pub messages: usize,
    /// Total duration in milliseconds.
    pub duration_ms: u64,
    /// Throughput in messages per second.
    pub throughput: f64,
    /// Average latency in milliseconds.
    pub avg_latency_ms: f64,
    /// P50 latency in milliseconds.
    pub p50_latency_ms: f64,
    /// P95 latency in milliseconds.
    pub p95_latency_ms: f64,
    /// P99 latency in milliseconds.
    pub p99_latency_ms: f64,
}

impl SyncBenchmark {
    /// Calculate benchmark from measurements.
    pub fn calculate(messages: usize, duration_ms: u64, latencies: &[u64]) -> Self {
        let throughput = if duration_ms > 0 {
            messages as f64 / (duration_ms as f64 / 1000.0)
        } else {
            0.0
        };

        let avg_latency_ms = if latencies.is_empty() {
            0.0
        } else {
            latencies.iter().sum::<u64>() as f64 / latencies.len() as f64
        };

        let mut sorted = latencies.to_vec();
        sorted.sort();
        let p50 = percentile(&sorted, 50);
        let p95 = percentile(&sorted, 95);
        let p99 = percentile(&sorted, 99);

        Self {
            messages,
            duration_ms,
            throughput,
            avg_latency_ms,
            p50_latency_ms: p50,
            p95_latency_ms: p95,
            p99_latency_ms: p99,
        }
    }

    /// Print benchmark results.
    pub fn print(&self) {
        println!("╔══════════════════════════════════════════╗");
        println!("║     Sync Benchmark Results               ║");
        println!("╠══════════════════════════════════════════╣");
        println!("║ Total Messages:    {:>10}           ║", self.messages);
        println!("║ Duration (ms):      {:>10}           ║", self.duration_ms);
        println!("║ Throughput:        {:>10.2} msg/s   ║", self.throughput);
        println!("║ Avg Latency:        {:>10.2} ms      ║", self.avg_latency_ms);
        println!("║ P50 Latency:        {:>10.2} ms      ║", self.p50_latency_ms);
        println!("║ P95 Latency:        {:>10.2} ms      ║", self.p95_latency_ms);
        println!("║ P99 Latency:        {:>10.2} ms      ║", self.p99_latency_ms);
        println!("╚══════════════════════════════════════════╝");
    }
}

/// Calculate percentile from sorted data.
fn percentile(sorted: &[u64], p: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((p as f64 / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)] as f64
}

// ========================================================================
// Network Partition Types
// ========================================================================

/// Phase 5d: Network impairment type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpairmentType {
    /// Complete disconnection.
    Disconnect,
    /// Added latency (ms).
    Latency(u64),
    /// Packet loss probability (0.0 - 1.0).
    PacketLoss(f32),
    /// Temporary disconnection with auto-reconnect.
    TemporaryDisconnect(Duration),
}

impl std::fmt::Display for ImpairmentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnect => write!(f, "Disconnect"),
            Self::Latency(ms) => write!(f, "Latency({}ms)", ms),
            Self::PacketLoss(p) => write!(f, "PacketLoss({:.1}%)", p * 100.0),
            Self::TemporaryDisconnect(d) => write!(f, "TempDisconnect({:.1}s)", d.as_secs_f64()),
        }
    }
}

/// Phase 5d: Partition controller.
#[derive(Debug)]
pub struct PartitionController {
    is_active: Arc<AtomicBool>,
}

impl PartitionController {
    /// Create a new partition controller.
    pub fn new() -> Self {
        Self {
            is_active: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start a partition.
    pub fn start_partition(&self) {
        self.is_active.store(true, Ordering::SeqCst);
        tracing::info!("Network partition started");
    }

    /// End a partition.
    pub fn end_partition(&self) {
        self.is_active.store(false, Ordering::SeqCst);
        tracing::info!("Network partition ended");
    }

    /// Check if partition is active.
    pub fn is_active(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }
}

impl Default for PartitionController {
    fn default() -> Self {
        Self::new()
    }
}

/// Phase 5d: Partition test result.
#[derive(Debug, Clone)]
pub struct PartitionTestResult {
    /// Test description.
    pub description: String,
    /// Whether test passed.
    pub passed: bool,
    /// Time to detect partition (ms).
    pub detection_time_ms: u64,
    /// Time to recover (ms).
    pub recovery_time_ms: u64,
}

impl PartitionTestResult {
    /// Print the test result.
    pub fn print(&self) {
        println!("╔══════════════════════════════════════════╗");
        println!("║     Partition Test Result              ║");
        println!("╠══════════════════════════════════════════╣");
        println!("║ Description: {:<26} ║", self.description);
        println!("║ Status:      {:<26} ║", if self.passed { "PASSED" } else { "FAILED" });
        println!("║ Detection:   {:>10} ms           ║", self.detection_time_ms);
        println!("║ Recovery:    {:>10} ms           ║", self.recovery_time_ms);
        println!("╚══════════════════════════════════════════╝");
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_benchmark_calculation() {
        let latencies = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        let benchmark = SyncBenchmark::calculate(10, 1000, &latencies);

        assert_eq!(benchmark.messages, 10);
        assert_eq!(benchmark.duration_ms, 1000);
        assert!((benchmark.throughput - 10.0).abs() < 0.01);
        assert!((benchmark.avg_latency_ms - 55.0).abs() < 0.01);
    }

    #[test]
    fn test_percentile() {
        let sorted = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&sorted, 50), 5.0);
        assert_eq!(percentile(&sorted, 95), 10.0);
        assert_eq!(percentile(&sorted, 99), 10.0);
    }

    #[test]
    fn test_empty_percentile() {
        assert_eq!(percentile(&[], 50), 0.0);
    }

    #[test]
    fn test_partition_controller() {
        let controller = PartitionController::new();

        assert!(!controller.is_active());

        controller.start_partition();
        assert!(controller.is_active());

        controller.end_partition();
        assert!(!controller.is_active());
    }

    #[tokio::test]
    async fn test_partition_timing() {
        let controller = PartitionController::new();

        let start = Instant::now();
        controller.start_partition();

        // Simulate some work during partition
        tokio::time::sleep(Duration::from_millis(100)).await;

        controller.end_partition();
        let elapsed = start.elapsed().as_millis() as u64;

        assert!(elapsed >= 100);
        assert!(!controller.is_active());
    }

    #[test]
    fn test_impairment_display() {
        assert_eq!(ImpairmentType::Disconnect.to_string(), "Disconnect");
        assert_eq!(ImpairmentType::Latency(100).to_string(), "Latency(100ms)");
        assert_eq!(ImpairmentType::PacketLoss(0.5).to_string(), "PacketLoss(50.0%)");
    }

    #[test]
    fn test_partition_result() {
        let result = PartitionTestResult {
            description: "Test disconnect".to_string(),
            passed: true,
            detection_time_ms: 50,
            recovery_time_ms: 100,
        };

        assert!(result.passed);
        assert_eq!(result.detection_time_ms, 50);
    }
}
