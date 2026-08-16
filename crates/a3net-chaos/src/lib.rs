//! A3Net Chaos Engineering Framework
//!
//! A comprehensive fault injection framework for testing the resilience of A3Net
//! distributed systems.
//!
//! # Architecture
//!
//! The framework is built on three core concepts:
//! - **Faults**: Individual fault types that can be injected
//! - **Scenarios**: Composed sequences of faults with timing
//! - **Experiments**: Full chaos experiments with hypothesis testing
//!
//! # Quick Start
//!
//! ```rust
//! use a3net_chaos::{ChaosEngine, Scenario};
//!
//! let engine = ChaosEngine::new();
//! let scenario = Scenario::network_partition()
//!     .with_baseline(std::time::Duration::from_secs(30));
//!
//! // Note: In real usage, you would use block_on() or run in async context
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod faults;
pub mod scenarios;
pub mod runner;
pub mod profiles;

use std::time::Duration;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn, error};

pub use faults::{Fault, FaultType, FaultTarget, FaultConfig, FaultParameters, NetworkFaultType, NodeFaultType, DataFaultType};
pub use scenarios::{Scenario, ScenarioStep, ChaosExperiment, Hypothesis, HypothesisType};
pub use runner::{ChaosEngine, ExperimentResult};
pub use profiles::{ChaosProfile, predefined_profiles};

/// Errors that can occur during chaos experiments
#[derive(Debug, Error)]
pub enum ChaosError {
    #[error("Experiment timeout: {0}")]
    ExperimentTimeout(String),

    #[error("Fault injection failed: {0}")]
    FaultInjectionFailed(String),

    #[error("Hypothesis violated: {0}")]
    HypothesisViolation(String),

    #[error("Target not found: {0}")]
    TargetNotFound(String),

    #[error("Experiment cancelled: {0}")]
    ExperimentCancelled(String),

    #[error("System state invalid: {0}")]
    InvalidState(String),
}

/// Severity level for chaos events
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Low impact fault (e.g., minor latency increase)
    Low,
    /// Medium impact fault (e.g., packet loss)
    Medium,
    /// High impact fault (e.g., node crash)
    High,
    /// Critical fault (e.g., network partition)
    Critical,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Phase of a chaos experiment
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExperimentPhase {
    /// Baseline measurement phase
    Baseline,
    /// Fault injection phase
    FaultInjection,
    /// Post-fault observation phase
    Observation,
    /// Recovery phase
    Recovery,
    /// Experiment completed
    Completed,
    /// Experiment failed
    Failed,
}

impl Default for ExperimentPhase {
    fn default() -> Self {
        ExperimentPhase::Baseline
    }
}

impl std::fmt::Display for ExperimentPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ExperimentPhase::Baseline => "baseline",
            ExperimentPhase::FaultInjection => "fault_injection",
            ExperimentPhase::Observation => "observation",
            ExperimentPhase::Recovery => "recovery",
            ExperimentPhase::Completed => "completed",
            ExperimentPhase::Failed => "failed",
        };
        write!(f, "{}", s)
    }
}

/// Statistics collected during an experiment
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExperimentStats {
    pub faults_injected: u64,
    pub faults_recovered: u64,
    pub baseline_measurements: u64,
    pub experiment_measurements: u64,
    pub hypothesis_violations: u64,
    pub duration_ms: u64,
    pub success: bool,
}

impl ExperimentStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration_ms = duration.as_millis() as u64;
        self
    }

    pub fn success_rate(&self) -> f64 {
        if self.baseline_measurements == 0 {
            return 1.0;
        }
        let failures = self.hypothesis_violations;
        let total = self.baseline_measurements + self.experiment_measurements;
        if total == 0 {
            return 1.0;
        }
        (total - failures) as f64 / total as f64
    }
}

/// Chaos event for observability
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ChaosEvent {
    ExperimentStarted {
        experiment_id: String,
        scenario: String,
    },
    PhaseChanged {
        from: ExperimentPhase,
        to: ExperimentPhase,
    },
    FaultInjected {
        fault: String,
        target: String,
        severity: Severity,
    },
    FaultRecovered {
        fault: String,
        target: String,
    },
    HypothesisChecked {
        hypothesis: String,
        passed: bool,
        measurement: f64,
    },
    HypothesisViolated {
        hypothesis: String,
        measurement: f64,
        threshold: f64,
    },
    ExperimentCompleted {
        success: bool,
        stats: ExperimentStats,
    },
}

impl ChaosEvent {
    pub fn log(&self) {
        match self {
            ChaosEvent::ExperimentStarted { experiment_id, scenario } => {
                info!(experiment_id, scenario, "Chaos experiment started");
            }
            ChaosEvent::PhaseChanged { from, to } => {
                info!(from = ?from, to = ?to, "Experiment phase changed");
            }
            ChaosEvent::FaultInjected { fault, target, severity } => {
                warn!(fault, target, severity = %severity, "Fault injected");
            }
            ChaosEvent::FaultRecovered { fault, target } => {
                info!(fault, target, "Fault recovered");
            }
            ChaosEvent::HypothesisChecked { hypothesis, passed, measurement } => {
                info!(hypothesis, passed, measurement, "Hypothesis checked");
            }
            ChaosEvent::HypothesisViolated { hypothesis, measurement, threshold } => {
                error!(
                    hypothesis, 
                    measurement, 
                    threshold, 
                    "HYPOTHESIS VIOLATED"
                );
            }
            ChaosEvent::ExperimentCompleted { success, stats } => {
                info!(
                    success, 
                    faults_injected = stats.faults_injected,
                    duration_ms = stats.duration_ms,
                    "Experiment completed"
                );
            }
        }
    }
}

/// Event emitter trait for chaos events
#[async_trait]
pub trait ChaosEventEmitter: Send + Sync {
    async fn emit(&self, event: ChaosEvent);
}

/// Simple event emitter that logs to tracing
pub struct TracingEventEmitter;

impl TracingEventEmitter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ChaosEventEmitter for TracingEventEmitter {
    async fn emit(&self, event: ChaosEvent) {
        event.log();
    }
}

impl Default for TracingEventEmitter {
    fn default() -> Self {
        Self::new()
    }
}

/// Null event emitter (no-op)
pub struct NullEventEmitter;

#[async_trait]
impl ChaosEventEmitter for NullEventEmitter {
    async fn emit(&self, _event: ChaosEvent) {
        // No-op
    }
}

/// Version constant
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
