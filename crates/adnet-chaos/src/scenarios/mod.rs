//! Chaos scenarios for defining complex fault sequences.
//!
//! Scenarios define ordered sequences of faults with timing, targets,
//! and verification conditions.

use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    ChaosError, ExperimentPhase, FaultConfig, Severity,
};
use super::faults::{FaultType, FaultTarget, FaultParameters, NetworkFaultType, NodeFaultType, DataFaultType};

/// A step in a chaos scenario
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioStep {
    /// Unique identifier for this step
    pub id: String,
    
    /// Description of what this step does
    pub description: String,
    
    /// Fault to inject (None = wait only)
    pub fault: Option<FaultConfig>,
    
    /// Duration to wait after injection
    pub wait_duration: Duration,
    
    /// Whether to verify system health after this step
    pub verify_health: bool,
    
    /// Minimum severity required to proceed
    pub min_severity: Option<Severity>,
}

impl ScenarioStep {
    pub fn inject(description: impl Into<String>, fault: FaultConfig) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            description: description.into(),
            fault: Some(fault),
            wait_duration: Duration::from_secs(30),
            verify_health: true,
            min_severity: None,
        }
    }

    pub fn wait(description: impl Into<String>, duration: Duration) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            description: description.into(),
            fault: None,
            wait_duration: duration,
            verify_health: false,
            min_severity: None,
        }
    }

    pub fn with_wait(mut self, duration: Duration) -> Self {
        self.wait_duration = duration;
        self
    }

    pub fn with_verification(mut self, verify: bool) -> Self {
        self.verify_health = verify;
        self
    }
}

/// A complete chaos scenario
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    /// Unique identifier
    pub id: String,
    
    /// Human-readable name
    pub name: String,
    
    /// Description of the scenario
    pub description: String,
    
    /// Steps in the scenario
    pub steps: Vec<ScenarioStep>,
    
    /// Baseline duration before fault injection
    pub baseline_duration: Duration,
    
    /// Recovery duration after faults
    pub recovery_duration: Duration,
    
    /// Whether to run steps sequentially or in parallel
    pub parallel_steps: bool,
    
    /// Tags for categorization
    pub tags: Vec<String>,
}

impl Scenario {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            description: String::new(),
            steps: Vec::new(),
            baseline_duration: Duration::from_secs(60),
            recovery_duration: Duration::from_secs(60),
            parallel_steps: false,
            tags: Vec::new(),
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn with_baseline(mut self, duration: Duration) -> Self {
        self.baseline_duration = duration;
        self
    }

    pub fn with_recovery(mut self, duration: Duration) -> Self {
        self.recovery_duration = duration;
        self
    }

    pub fn add_step(mut self, step: ScenarioStep) -> Self {
        self.steps.push(step);
        self
    }

    pub fn add_parallel_steps(mut self, steps: Vec<ScenarioStep>) -> Self {
        self.parallel_steps = true;
        for step in steps {
            self.steps.push(step);
        }
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }
}

/// Predefined scenarios
impl Scenario {
    /// Network partition scenario
    pub fn network_partition() -> Self {
        Self::new("Network Partition")
            .with_description("Tests system behavior during network partitions")
            .with_baseline(Duration::from_secs(30))
            .add_step(
                ScenarioStep::inject(
                    "Isolate primary node",
                    FaultConfig::new(
                        FaultType::NetworkFault(NetworkFaultType::Partition),
                        FaultTarget::all_nodes(),
                    ),
                )
                .with_wait(Duration::from_secs(60))
                .with_verification(true),
            )
            .with_recovery(Duration::from_secs(60))
    }

    /// Node crash and recovery scenario
    pub fn node_crash_recovery() -> Self {
        Self::new("Node Crash and Recovery")
            .with_description("Tests system resilience to node failures")
            .with_baseline(Duration::from_secs(30))
            .add_step(
                ScenarioStep::inject(
                    "Crash replica node",
                    FaultConfig::new(
                        FaultType::NodeFault(NodeFaultType::Crash),
                        FaultTarget::node("replica-1"),
                    ),
                )
                .with_wait(Duration::from_secs(30))
                .with_verification(true),
            )
            .with_recovery(Duration::from_secs(45))
    }

    /// High latency scenario
    pub fn high_latency() -> Self {
        Self::new("High Latency")
            .with_description("Tests system behavior under increased network latency")
            .with_baseline(Duration::from_secs(20))
            .add_step(
                ScenarioStep::inject(
                    "Inject 500ms latency",
                    FaultConfig::new(
                        FaultType::NetworkFault(NetworkFaultType::Latency),
                        FaultTarget::all_nodes(),
                    )
                    .with_parameters(
                        FaultParameters::new()
                            .with_latency(500),
                    ),
                )
                .with_wait(Duration::from_secs(60))
                .with_verification(true),
            )
            .add_step(
                ScenarioStep::inject(
                    "Increase latency to 1000ms",
                    FaultConfig::new(
                        FaultType::NetworkFault(NetworkFaultType::Latency),
                        FaultTarget::all_nodes(),
                    )
                    .with_parameters(
                        FaultParameters::new()
                            .with_latency(1000),
                    ),
                )
                .with_wait(Duration::from_secs(60))
                .with_verification(true),
            )
            .with_recovery(Duration::from_secs(30))
    }

    /// Packet loss scenario
    pub fn packet_loss() -> Self {
        Self::new("Packet Loss")
            .with_description("Tests system behavior under unreliable network conditions")
            .with_baseline(Duration::from_secs(20))
            .add_step(
                ScenarioStep::inject(
                    "Inject 10% packet loss",
                    FaultConfig::new(
                        FaultType::NetworkFault(NetworkFaultType::PacketLoss),
                        FaultTarget::all_nodes(),
                    )
                    .with_parameters(
                        FaultParameters::new()
                            .with_loss(10.0),
                    ),
                )
                .with_wait(Duration::from_secs(60))
                .with_verification(true),
            )
            .add_step(
                ScenarioStep::inject(
                    "Increase packet loss to 30%",
                    FaultConfig::new(
                        FaultType::NetworkFault(NetworkFaultType::PacketLoss),
                        FaultTarget::all_nodes(),
                    )
                    .with_parameters(
                        FaultParameters::new()
                            .with_loss(30.0),
                    ),
                )
                .with_wait(Duration::from_secs(60))
                .with_verification(true),
            )
            .with_recovery(Duration::from_secs(30))
    }

    /// Data corruption scenario
    pub fn data_corruption() -> Self {
        Self::new("Data Corruption")
            .with_description("Tests system behavior when data corruption occurs")
            .with_baseline(Duration::from_secs(30))
            .add_step(
                ScenarioStep::inject(
                    "Corrupt 5% of data",
                    FaultConfig::new(
                        FaultType::DataFault(DataFaultType::Corruption),
                        FaultTarget::all_nodes(),
                    )
                    .with_parameters(
                        FaultParameters::new()
                            .with_corruption(5.0),
                    ),
                )
                .with_wait(Duration::from_secs(60))
                .with_verification(true),
            )
            .with_recovery(Duration::from_secs(60))
    }

    /// Memory pressure scenario
    pub fn memory_pressure() -> Self {
        Self::new("Memory Pressure")
            .with_description("Tests system behavior under memory stress")
            .with_baseline(Duration::from_secs(30))
            .add_step(
                ScenarioStep::inject(
                    "Apply 50% memory stress",
                    FaultConfig::new(
                        FaultType::NodeFault(NodeFaultType::MemoryStress),
                        FaultTarget::all_nodes(),
                    )
                    .with_parameters(
                        FaultParameters {
                            stress_percentage: Some(50.0),
                            ..Default::default()
                        },
                    ),
                )
                .with_wait(Duration::from_secs(60))
                .with_verification(true),
            )
            .add_step(
                ScenarioStep::inject(
                    "Increase to 80% memory stress",
                    FaultConfig::new(
                        FaultType::NodeFault(NodeFaultType::MemoryStress),
                        FaultTarget::all_nodes(),
                    )
                    .with_parameters(
                        FaultParameters {
                            stress_percentage: Some(80.0),
                            ..Default::default()
                        },
                    ),
                )
                .with_wait(Duration::from_secs(60))
                .with_verification(true),
            )
            .with_recovery(Duration::from_secs(30))
    }

    /// Cascading failure scenario
    pub fn cascading_failure() -> Self {
        Self::new("Cascading Failure")
            .with_description("Simulates a cascade of failures starting from a single point")
            .with_baseline(Duration::from_secs(30))
            .add_step(
                ScenarioStep::inject(
                    "Crash primary node",
                    FaultConfig::new(
                        FaultType::NodeFault(NodeFaultType::Crash),
                        FaultTarget::node("primary"),
                    ),
                )
                .with_wait(Duration::from_secs(10))
                .with_verification(false),
            )
            .add_step(
                ScenarioStep::inject(
                    "Inject latency on remaining nodes",
                    FaultConfig::new(
                        FaultType::NetworkFault(NetworkFaultType::Latency),
                        FaultTarget::all_nodes(),
                    )
                    .with_parameters(
                        FaultParameters::new()
                            .with_latency(200),
                    ),
                )
                .with_wait(Duration::from_secs(30))
                .with_verification(true),
            )
            .with_recovery(Duration::from_secs(120))
    }
}

/// Chaos experiment with hypothesis testing
#[derive(Debug, Clone)]
pub struct ChaosExperiment {
    /// Experiment identifier
    pub id: String,
    
    /// Scenario to run
    pub scenario: Scenario,
    
    /// Hypotheses to verify
    pub hypotheses: Vec<Hypothesis>,
    
    /// Current phase
    pub phase: ExperimentPhase,
    
    /// Current step index
    pub current_step: usize,
    
    /// Experiment start time
    pub started_at: Option<Instant>,
    
    /// Experiment metadata
    pub metadata: std::collections::HashMap<String, String>,
}

impl ChaosExperiment {
    pub fn new(scenario: Scenario) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            scenario,
            hypotheses: Vec::new(),
            phase: ExperimentPhase::Baseline,
            current_step: 0,
            started_at: None,
            metadata: std::collections::HashMap::new(),
        }
    }

    pub fn with_hypothesis(mut self, hypothesis: Hypothesis) -> Self {
        self.hypotheses.push(hypothesis);
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    pub fn start(&mut self) {
        self.started_at = Some(Instant::now());
        self.phase = ExperimentPhase::Baseline;
    }

    pub fn advance_phase(&mut self) {
        self.phase = match self.phase {
            ExperimentPhase::Baseline => ExperimentPhase::FaultInjection,
            ExperimentPhase::FaultInjection => ExperimentPhase::Observation,
            ExperimentPhase::Observation => ExperimentPhase::Recovery,
            ExperimentPhase::Recovery => ExperimentPhase::Completed,
            ExperimentPhase::Completed => ExperimentPhase::Completed,
            ExperimentPhase::Failed => ExperimentPhase::Failed,
        };
    }

    pub fn fail(&mut self) {
        self.phase = ExperimentPhase::Failed;
    }

    pub fn next_step(&mut self) -> bool {
        self.current_step += 1;
        self.current_step < self.scenario.steps.len()
    }
}

/// Hypothesis to verify during experiment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    /// Unique identifier
    pub id: String,
    
    /// Human-readable description
    pub description: String,
    
    /// Type of hypothesis
    pub hypothesis_type: HypothesisType,
    
    /// Threshold for pass/fail
    pub threshold: f64,
    
    /// Whether the hypothesis is currently passing
    pub passing: bool,
    
    /// Latest measurement value
    pub latest_value: Option<f64>,
}

impl Hypothesis {
    pub fn new(description: impl Into<String>, hypothesis_type: HypothesisType, threshold: f64) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            description: description.into(),
            hypothesis_type,
            threshold,
            passing: true,
            latest_value: None,
        }
    }

    pub fn check(&mut self, value: f64) -> bool {
        self.latest_value = Some(value);
        self.passing = self.hypothesis_type.check(value, self.threshold);
        self.passing
    }
}

/// Types of hypotheses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisType {
    /// System should remain available
    Availability,
    /// Latency should not exceed threshold
    LatencyBound,
    /// Error rate should not exceed threshold
    ErrorRateBound,
    /// Throughput should remain above threshold
    MinimumThroughput,
    /// Data consistency should be maintained
    DataConsistency,
    /// Recovery time should be within threshold
    RecoveryTimeBound,
}

impl HypothesisType {
    pub fn check(&self, value: f64, threshold: f64) -> bool {
        match self {
            HypothesisType::Availability => value >= threshold,
            HypothesisType::LatencyBound => value <= threshold,
            HypothesisType::ErrorRateBound => value <= threshold,
            HypothesisType::MinimumThroughput => value >= threshold,
            HypothesisType::DataConsistency => value >= threshold,
            HypothesisType::RecoveryTimeBound => value <= threshold,
        }
    }
}

/// Predefined hypotheses
impl Hypothesis {
    /// System availability should be >= 99%
    pub fn availability_at_least_99() -> Self {
        Self::new(
            "System availability should be at least 99%",
            HypothesisType::Availability,
            0.99,
        )
    }

    /// Error rate should be < 1%
    pub fn error_rate_below_1_percent() -> Self {
        Self::new(
            "Error rate should be below 1%",
            HypothesisType::ErrorRateBound,
            0.01,
        )
    }

    /// Latency should be < 500ms
    pub fn latency_below_500ms() -> Self {
        Self::new(
            "Latency should be below 500ms",
            HypothesisType::LatencyBound,
            500.0,
        )
    }

    /// Throughput should be > 100 req/s
    pub fn minimum_throughput_100() -> Self {
        Self::new(
            "Throughput should remain above 100 req/s",
            HypothesisType::MinimumThroughput,
            100.0,
        )
    }

    /// Data consistency should be 100%
    pub fn data_consistency() -> Self {
        Self::new(
            "Data consistency should be maintained at 100%",
            HypothesisType::DataConsistency,
            1.0,
        )
    }
}
