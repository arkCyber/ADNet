//! Chaos experiment runner and engine.
//!
//! The runner executes chaos experiments, manages fault injection,
//! and collects results.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, broadcast};
use tracing::{info, warn};
use serde::{Serialize, Deserialize};

use super::{ChaosError, ChaosEvent, ChaosEventEmitter, ExperimentPhase, ExperimentStats, Severity, TracingEventEmitter};
use super::faults::{Fault, FaultConfig, FaultType};
use super::scenarios::{ChaosExperiment, Hypothesis, HypothesisType, Scenario, ScenarioStep};

/// The main chaos engine
pub struct ChaosEngine {
    /// Active faults
    active_faults: Arc<RwLock<Vec<Fault>>>,
    
    /// Event emitter
    event_emitter: Arc<dyn ChaosEventEmitter>,
    
    /// Cancel signal sender
    cancel_tx: Arc<RwLock<Option<broadcast::Sender<()>>>>,
    
    /// Current experiment
    current_experiment: Arc<RwLock<Option<ChaosExperiment>>>,
}

impl ChaosEngine {
    /// Create a new chaos engine
    pub fn new() -> Self {
        Self {
            active_faults: Arc::new(RwLock::new(Vec::new())),
            event_emitter: Arc::new(TracingEventEmitter::new()),
            cancel_tx: Arc::new(RwLock::new(None)),
            current_experiment: Arc::new(RwLock::new(None)),
        }
    }

    /// Create with custom event emitter
    pub fn with_emitter<E: ChaosEventEmitter + 'static>(emitter: E) -> Self {
        Self {
            active_faults: Arc::new(RwLock::new(Vec::new())),
            event_emitter: Arc::new(emitter),
            cancel_tx: Arc::new(RwLock::new(None)),
            current_experiment: Arc::new(RwLock::new(None)),
        }
    }

    /// Run a chaos scenario
    pub async fn run(&self, scenario: Scenario) -> Result<ExperimentResult, ChaosError> {
        self.run_with_hypotheses(scenario, vec![]).await
    }

    /// Run a chaos scenario with hypotheses
    pub async fn run_with_hypotheses(
        &self,
        scenario: Scenario,
        hypotheses: Vec<Hypothesis>,
    ) -> Result<ExperimentResult, ChaosError> {
        let mut experiment = ChaosExperiment::new(scenario);
        experiment.hypotheses = hypotheses;
        experiment.start();

        // Store current experiment
        {
            let mut current = self.current_experiment.write().await;
            *current = Some(experiment.clone());
        }

        // Emit start event
        self.emit(ChaosEvent::ExperimentStarted {
            experiment_id: experiment.id.clone(),
            scenario: experiment.scenario.name.clone(),
        }).await;

        let result = self.execute_experiment(experiment).await;

        // Emit completion event
        match &result {
            Ok(result) => {
                let stats = result.stats.clone();
                self.emit(ChaosEvent::ExperimentCompleted {
                    success: result.success,
                    stats,
                }).await;
            }
            Err(e) => {
                // Emit failure event
                let stats = ExperimentStats::default();
                self.emit(ChaosEvent::ExperimentCompleted {
                    success: false,
                    stats,
                }).await;
                let error = ChaosError::ExperimentCancelled(e.to_string());
                return Err(error);
            }
        }

        // Clear current experiment
        {
            let mut current = self.current_experiment.write().await;
            *current = None;
        }

        result
    }

    /// Execute a single experiment
    async fn execute_experiment(
        &self,
        mut experiment: ChaosExperiment,
    ) -> Result<ExperimentResult, ChaosError> {
        let start_time = Instant::now();
        let mut stats = ExperimentStats::new();
        let mut hypotheses_state: Vec<(bool, Vec<f64>)> = experiment
            .hypotheses
            .iter()
            .map(|h| (true, Vec::new()))
            .collect();

        // Phase 1: Baseline measurement
        self.emit(ChaosEvent::PhaseChanged {
            from: ExperimentPhase::Baseline,
            to: ExperimentPhase::Baseline,
        }).await;

        let baseline_duration = experiment.scenario.baseline_duration;
        info!("Running baseline measurement for {:?}", baseline_duration);
        
        let baseline_start = Instant::now();
        while baseline_start.elapsed() < baseline_duration {
            // Collect baseline measurements
            for (i, hypothesis) in experiment.hypotheses.iter().enumerate() {
                let measurement = self.measure_hypothesis(hypothesis).await;
                hypotheses_state[i].1.push(measurement);
                stats.baseline_measurements += 1;
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        // Phase 2: Fault injection
        self.emit(ChaosEvent::PhaseChanged {
            from: ExperimentPhase::Baseline,
            to: ExperimentPhase::FaultInjection,
        }).await;

        experiment.phase = ExperimentPhase::FaultInjection;

        for (step_idx, step) in experiment.scenario.steps.iter().enumerate() {
            info!("Executing step {}: {}", step_idx, step.description);

            // Inject fault if present
            if let Some(fault_config) = &step.fault {
                let fault = self.inject_fault(fault_config).await?;
                stats.faults_injected += 1;

                self.emit(ChaosEvent::FaultInjected {
                    fault: format!("{:?}", fault_config.fault_type),
                    target: format!("{:?}", fault_config.target),
                    severity: fault_config.severity,
                }).await;
            }

            // Wait for step duration
            let step_start = Instant::now();
            while step_start.elapsed() < step.wait_duration {
                // Measure hypotheses during fault
                for (i, hypothesis) in experiment.hypotheses.iter().enumerate() {
                    let measurement = self.measure_hypothesis(hypothesis).await;
                    hypotheses_state[i].1.push(measurement);
                    stats.experiment_measurements += 1;

                    // Check hypothesis
                    if !hypothesis.hypothesis_type.check(measurement, hypothesis.threshold) {
                        hypotheses_state[i].0 = false;
                        stats.hypothesis_violations += 1;

                        self.emit(ChaosEvent::HypothesisViolated {
                            hypothesis: hypothesis.description.clone(),
                            measurement,
                            threshold: hypothesis.threshold,
                        }).await;
                    }
                }

                tokio::time::sleep(Duration::from_secs(1)).await;
            }

            // Verify health if required
            if step.verify_health {
                if !self.verify_health().await {
                    warn!("Health check failed during step {}", step_idx);
                }
            }
        }

        // Phase 3: Observation
        self.emit(ChaosEvent::PhaseChanged {
            from: ExperimentPhase::FaultInjection,
            to: ExperimentPhase::Observation,
        }).await;

        experiment.phase = ExperimentPhase::Observation;

        // Collect observation data
        tokio::time::sleep(Duration::from_secs(10)).await;

        // Phase 4: Recovery
        self.emit(ChaosEvent::PhaseChanged {
            from: ExperimentPhase::Observation,
            to: ExperimentPhase::Recovery,
        }).await;

        experiment.phase = ExperimentPhase::Recovery;

        // Recover all faults
        self.recover_all_faults().await?;

        // Wait for recovery duration
        tokio::time::sleep(experiment.scenario.recovery_duration).await;

        experiment.phase = ExperimentPhase::Completed;

        stats.duration_ms = start_time.elapsed().as_millis() as u64;
        stats.faults_recovered = stats.faults_injected;

        let success = hypotheses_state.iter().all(|(passing, _)| *passing);

        Ok(ExperimentResult {
            experiment_id: experiment.id,
            success,
            stats,
            hypothesis_results: experiment
                .hypotheses
                .iter()
                .zip(hypotheses_state.into_iter())
                .map(|(h, (passed, measurements))| HypothesisResult {
                    hypothesis: h.clone(),
                    passed,
                    measurements,
                })
                .collect(),
            logs: Vec::new(),
        })
    }

    /// Inject a fault
    async fn inject_fault(&self, config: &FaultConfig) -> Result<Fault, ChaosError> {
        let fault = Fault::new(config.clone());
        let mut faults = self.active_faults.write().await;
        faults.push(fault.clone());
        Ok(fault)
    }

    /// Recover all active faults
    async fn recover_all_faults(&self) -> Result<(), ChaosError> {
        let mut faults = self.active_faults.write().await;
        for fault in faults.drain(..) {
            self.emit(ChaosEvent::FaultRecovered {
                fault: format!("{:?}", fault.config.fault_type),
                target: format!("{:?}", fault.config.target),
            }).await;
        }
        Ok(())
    }

    /// Measure a hypothesis
    async fn measure_hypothesis(&self, hypothesis: &Hypothesis) -> f64 {
        // In a real implementation, this would query actual metrics
        // For now, return a simulated value
        match hypothesis.hypothesis_type {
            HypothesisType::Availability => 0.999,
            HypothesisType::LatencyBound => 45.0,
            HypothesisType::ErrorRateBound => 0.001,
            HypothesisType::MinimumThroughput => 150.0,
            HypothesisType::DataConsistency => 1.0,
            HypothesisType::RecoveryTimeBound => 5.0,
        }
    }

    /// Verify system health
    async fn verify_health(&self) -> bool {
        // In a real implementation, this would check actual system health
        true
    }

    /// Emit an event
    async fn emit(&self, event: ChaosEvent) {
        self.event_emitter.emit(event).await;
    }

    /// Get current experiment status
    pub async fn status(&self) -> Option<ExperimentPhase> {
        let current = self.current_experiment.read().await;
        current.as_ref().map(|e| e.phase)
    }

    /// Cancel current experiment
    pub async fn cancel(&self) -> Result<(), ChaosError> {
        let tx = self.cancel_tx.read().await;
        if let Some(sender) = tx.as_ref() {
            sender.send(()).map_err(|_| ChaosError::ExperimentCancelled("Failed to cancel".into()))?;
        }
        Ok(())
    }

    /// Get active faults
    pub async fn active_faults(&self) -> Vec<Fault> {
        self.active_faults.read().await.clone()
    }
}

impl Default for ChaosEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a chaos experiment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentResult {
    pub experiment_id: String,
    pub success: bool,
    pub stats: ExperimentStats,
    pub hypothesis_results: Vec<HypothesisResult>,
    pub logs: Vec<LogEntry>,
}

/// Result of hypothesis verification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HypothesisResult {
    pub hypothesis: Hypothesis,
    pub passed: bool,
    pub measurements: Vec<f64>,
}

/// A log entry from an experiment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub level: String,
    pub message: String,
    pub metadata: Option<std::collections::HashMap<String, String>>,
}

impl LogEntry {
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            timestamp: chrono::Utc::now(),
            level: "INFO".into(),
            message: message.into(),
            metadata: None,
        }
    }

    pub fn warn(message: impl Into<String>) -> Self {
        Self {
            timestamp: chrono::Utc::now(),
            level: "WARN".into(),
            message: message.into(),
            metadata: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            timestamp: chrono::Utc::now(),
            level: "ERROR".into(),
            message: message.into(),
            metadata: None,
        }
    }
}
