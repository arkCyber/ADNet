//! Phase 5d: Network partition simulation tests.
//!
//! This module provides utilities for testing how the group sync
//! system behaves under network impairment conditions.
//!
//! ## Impairment Types
//!
//! - **Disconnect**: Complete connection loss
//! - **Latency**: Added round-trip delay
//! - **Packet Loss**: Simulated packet drops
//! - **Reconnect**: Recovery after partition

#![cfg(all(feature = "iroh", feature = "derp"))]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::{mpsc, RwLock};

/// Phase 5d: Network impairment type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpairmentType {
    /// Connection is completely blocked.
    Disconnect,
    /// Added latency per packet (ms).
    Latency(u64),
    /// Packet loss probability (0.0 - 1.0).
    PacketLoss(f32),
    /// Temporary disconnect with auto-reconnect.
    TemporaryDisconnect(Duration),
}

impl std::fmt::Display for ImpairmentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnect => write!(f, "Disconnect"),
            Self::Latency(ms) => write!(f, "Latency({}ms)", ms),
            Self::PacketLoss(p) => write!(f, "PacketLoss({:.1}%)", p * 100.0),
            Self::TemporaryDisconnect(d) => {
                write!(f, "TemporaryDisconnect({:.1}s)", d.as_secs_f64())
            }
        }
    }
}

/// Phase 5d: Network partition state.
#[derive(Debug, Clone)]
pub struct PartitionState {
    /// Whether the partition is currently active.
    pub active: bool,
    /// Type of impairment.
    pub impairment: Option<ImpairmentType>,
    /// When the partition started (if active).
    pub started_at: Option<Instant>,
    /// When the partition is scheduled to end (if applicable).
    pub scheduled_end: Option<Instant>,
}

impl Default for PartitionState {
    fn default() -> Self {
        Self {
            active: false,
            impairment: None,
            started_at: None,
            scheduled_end: None,
        }
    }
}

/// Phase 5d: Controller for network partition simulation.
pub struct PartitionController {
    state: Arc<RwLock<PartitionState>>,
    active: Arc<AtomicBool>,
}

impl PartitionController {
    /// Create a new partition controller.
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(PartitionState::default())),
            active: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start a partition with the given impairment.
    pub async fn start_partition(&self, impairment: ImpairmentType) {
        let mut state = self.state.write().await;
        state.active = true;
        state.impairment = Some(impairment.clone());
        state.started_at = Some(Instant::now());

        // Calculate scheduled end for temporary disconnects
        if let ImpairmentType::TemporaryDisconnect(duration) = impairment {
            state.scheduled_end = Some(Instant::now() + duration);
        }

        self.active.store(true, Ordering::SeqCst);

        tracing::info!("Network partition started: {}", impairment);
    }

    /// End the partition.
    pub async fn end_partition(&self) {
        let mut state = self.state.write().await;
        let duration = state.started_at.map(|s| s.elapsed());

        state.active = false;
        state.impairment = None;
        state.started_at = None;
        state.scheduled_end = None;

        self.active.store(false, Ordering::SeqCst);

        if let Some(d) = duration {
            tracing::info!("Network partition ended after {:?}", d);
        } else {
            tracing::info!("Network partition ended");
        }
    }

    /// Check if the partition is currently active.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Get the current partition state.
    pub async fn get_state(&self) -> PartitionState {
        self.state.read().await.clone()
    }

    /// Wait for partition to end (for temporary disconnects).
    pub async fn wait_for_recovery(&self, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        while self.is_active() {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if start.elapsed() > timeout {
                anyhow::bail!("Partition recovery timeout");
            }
        }
        Ok(())
    }
}

impl Default for PartitionController {
    fn default() -> Self {
        Self::new()
    }
}

/// Phase 5d: Result of a partition test.
#[derive(Debug)]
pub struct PartitionTestResult {
    /// Test description.
    pub description: String,
    /// Whether the test passed.
    pub passed: bool,
    /// Time to detect partition.
    pub detection_time_ms: u64,
    /// Time to recover after partition ended.
    pub recovery_time_ms: u64,
    /// Messages lost during partition.
    pub messages_lost: usize,
    /// Messages delivered after recovery.
    pub messages_recovered: usize,
}

impl PartitionTestResult {
    pub fn print(&self) {
        println!("╔══════════════════════════════════════════╗");
        println!("║     Partition Test Results               ║");
        println!("╠══════════════════════════════════════════╣");
        println!("║ Description:  {:<30} ║", self.description);
        println!("║ Status:       {:<30} ║", if self.passed { "PASSED" } else { "FAILED" });
        println!("║ Detection:    {:>10} ms           ║", self.detection_time_ms);
        println!("║ Recovery:     {:>10} ms           ║", self.recovery_time_ms);
        println!("║ Messages Lost:    {:>10}           ║", self.messages_lost);
        println!("║ Messages Recovered: {:>10}         ║", self.messages_recovered);
        println!("╚══════════════════════════════════════════╝");
    }
}

/// Phase 5d: Scenario for partition testing.
pub struct PartitionScenario {
    pub name: String,
    pub impairment: ImpairmentType,
    pub duration: Duration,
    pub expected_recovery_time: Duration,
}

impl PartitionScenario {
    pub fn disconnect(name: &str, duration_secs: u64) -> Self {
        Self {
            name: name.to_string(),
            impairment: ImpairmentType::TemporaryDisconnect(Duration::from_secs(duration_secs)),
            duration: Duration::from_secs(duration_secs),
            expected_recovery_time: Duration::from_secs(5),
        }
    }

    pub fn latency(name: &str, latency_ms: u64, duration_secs: u64) -> Self {
        Self {
            name: name.to_string(),
            impairment: ImpairmentType::Latency(latency_ms),
            duration: Duration::from_secs(duration_secs),
            expected_recovery_time: Duration::from_secs(1),
        }
    }

    pub fn packet_loss(name: &str, loss_rate: f32, duration_secs: u64) -> Self {
        Self {
            name: name.to_string(),
            impairment: ImpairmentType::PacketLoss(loss_rate),
            duration: Duration::from_secs(duration_secs),
            expected_recovery_time: Duration::from_secs(2),
        }
    }
}

/// Phase 5d: Test result collector for partition scenarios.
pub struct PartitionTestCollector {
    results: Arc<RwLock<Vec<PartitionTestResult>>>,
}

impl PartitionTestCollector {
    pub fn new() -> Self {
        Self {
            results: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn add_result(&self, result: PartitionTestResult) {
        self.results.write().await.push(result);
    }

    pub async fn get_results(&self) -> Vec<PartitionTestResult> {
        self.results.read().await.clone()
    }

    pub async fn print_summary(&self) {
        let results = self.get_results().await;
        let passed = results.iter().filter(|r| r.passed).count();
        let failed = results.len() - passed;

        println!("\n╔══════════════════════════════════════════╗");
        println!("║   Network Partition Test Summary        ║");
        println!("╠══════════════════════════════════════════╣");
        println!("║ Total Tests: {:>25}     ║", results.len());
        println!("║ Passed:      {:>25}     ║", passed);
        println!("║ Failed:      {:>25}     ║", failed);
        println!("╚══════════════════════════════════════════╝");

        for result in &results {
            result.print();
        }
    }
}

impl Default for PartitionTestCollector {
    fn default() -> Self {
        Self::new()
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_partition_controller_start_stop() {
        let controller = PartitionController::new();

        assert!(!controller.is_active());

        controller
            .start_partition(ImpairmentType::Disconnect)
            .await;

        assert!(controller.is_active());

        let state = controller.get_state().await;
        assert!(state.active);
        assert!(matches!(state.impairment, Some(ImpairmentType::Disconnect)));

        controller.end_partition().await;

        assert!(!controller.is_active());
    }

    #[tokio::test]
    async fn test_partition_temporary_disconnect() {
        let controller = PartitionController::new();

        controller
            .start_partition(ImpairmentType::TemporaryDisconnect(
                Duration::from_millis(100),
            ))
            .await;

        assert!(controller.is_active());

        // Wait for auto-recovery
        controller
            .wait_for_recovery(Duration::from_secs(1))
            .await
            .expect("should recover");

        assert!(!controller.is_active());
    }

    #[tokio::test]
    async fn test_partition_latency() {
        let controller = PartitionController::new();

        controller
            .start_partition(ImpairmentType::Latency(500))
            .await;

        let state = controller.get_state().await;
        assert!(state.active);

        if let Some(ImpairmentType::Latency(ms)) = state.impairment {
            assert_eq!(ms, 500);
        } else {
            panic!("Expected latency impairment");
        }

        controller.end_partition().await;
    }

    #[tokio::test]
    async fn test_partition_packet_loss() {
        let controller = PartitionController::new();

        controller
            .start_partition(ImpairmentType::PacketLoss(0.5))
            .await;

        let state = controller.get_state().await;
        assert!(state.active);

        if let Some(ImpairmentType::PacketLoss(rate)) = state.impairment {
            assert!((rate - 0.5).abs() < 0.01);
        } else {
            panic!("Expected packet loss impairment");
        }

        controller.end_partition().await;
    }

    #[test]
    fn test_impairment_display() {
        assert_eq!(ImpairmentType::Disconnect.to_string(), "Disconnect");
        assert_eq!(ImpairmentType::Latency(100).to_string(), "Latency(100ms)");
        assert_eq!(
            ImpairmentType::PacketLoss(0.25).to_string(),
            "PacketLoss(25.0%)"
        );
        assert_eq!(
            ImpairmentType::TemporaryDisconnect(Duration::from_secs(30)).to_string(),
            "TemporaryDisconnect(30.0s)"
        );
    }

    #[tokio::test]
    async fn test_partition_collector() {
        let collector = PartitionTestCollector::new();

        collector
            .add_result(PartitionTestResult {
                description: "Disconnect test".to_string(),
                passed: true,
                detection_time_ms: 100,
                recovery_time_ms: 500,
                messages_lost: 5,
                messages_recovered: 5,
            })
            .await;

        collector
            .add_result(PartitionTestResult {
                description: "Latency test".to_string(),
                passed: false,
                detection_time_ms: 50,
                recovery_time_ms: 2000,
                messages_lost: 10,
                messages_recovered: 8,
            })
            .await;

        let results = collector.get_results().await;
        assert_eq!(results.len(), 2);
        assert!(results[0].passed);
        assert!(!results[1].passed);
    }

    #[test]
    fn test_partition_scenario_disconnect() {
        let scenario = PartitionScenario::disconnect("test", 5);
        assert_eq!(scenario.name, "test");
        assert!(matches!(
            scenario.impairment,
            ImpairmentType::TemporaryDisconnect(Duration) if scenario.duration == Duration::from_secs(5)
        ));
    }

    #[test]
    fn test_partition_scenario_latency() {
        let scenario = PartitionScenario::latency("latency_test", 200, 10);
        assert_eq!(scenario.name, "latency_test");
        assert!(matches!(scenario.impairment, ImpairmentType::Latency(200)));
    }

    #[test]
    fn test_partition_scenario_packet_loss() {
        let scenario = PartitionScenario::packet_loss("loss_test", 0.3, 15);
        assert_eq!(scenario.name, "loss_test");
        assert!(matches!(scenario.impairment, ImpairmentType::PacketLoss(0.3)));
    }
}
