//! Predefined chaos profiles for common testing scenarios.
//!
//! Profiles provide ready-to-use experiment configurations for
//! specific testing objectives.

use serde::{Deserialize, Serialize};

use super::scenarios::{ChaosExperiment, Hypothesis, HypothesisType, Scenario};

/// A chaos profile containing predefined scenarios and hypotheses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChaosProfile {
    pub name: String,
    pub description: String,
    pub scenarios: Vec<Scenario>,
    pub hypotheses: Vec<Hypothesis>,
    pub tags: Vec<String>,
}

impl ChaosProfile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            scenarios: Vec::new(),
            hypotheses: Vec::new(),
            tags: Vec::new(),
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn add_scenario(mut self, scenario: Scenario) -> Self {
        self.scenarios.push(scenario);
        self
    }

    pub fn add_hypothesis(mut self, hypothesis: Hypothesis) -> Self {
        self.hypotheses.push(hypothesis);
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }
}

/// Returns all predefined chaos profiles
pub fn predefined_profiles() -> Vec<ChaosProfile> {
    vec![
        network_resilience_profile(),
        data_consistency_profile(),
        performance_degradation_profile(),
        fault_tolerance_profile(),
        disaster_recovery_profile(),
    ]
}

/// Network resilience testing profile
pub fn network_resilience_profile() -> ChaosProfile {
    ChaosProfile::new("Network Resilience")
        .with_description("Tests system behavior under various network fault conditions")
        .add_scenario(Scenario::network_partition())
        .add_scenario(Scenario::high_latency())
        .add_scenario(Scenario::packet_loss())
        .add_hypothesis(Hypothesis::availability_at_least_99())
        .add_hypothesis(Hypothesis::error_rate_below_1_percent())
        .add_hypothesis(Hypothesis::latency_below_500ms())
        .with_tags(vec![
            "network".into(),
            "resilience".into(),
            "production-critical".into(),
        ])
}

/// Data consistency testing profile
pub fn data_consistency_profile() -> ChaosProfile {
    ChaosProfile::new("Data Consistency")
        .with_description("Verifies data integrity and consistency under fault conditions")
        .add_scenario(Scenario::data_corruption())
        .add_scenario(Scenario::network_partition())
        .add_scenario(Scenario::node_crash_recovery())
        .add_hypothesis(Hypothesis::data_consistency())
        .add_hypothesis(Hypothesis::error_rate_below_1_percent())
        .with_tags(vec![
            "data".into(),
            "consistency".into(),
            "integrity".into(),
        ])
}

/// Performance degradation testing profile
pub fn performance_degradation_profile() -> ChaosProfile {
    ChaosProfile::new("Performance Degradation")
        .with_description("Tests system performance under various stress conditions")
        .add_scenario(Scenario::high_latency())
        .add_scenario(Scenario::memory_pressure())
        .add_scenario(Scenario::packet_loss())
        .add_hypothesis(Hypothesis::minimum_throughput_100())
        .add_hypothesis(Hypothesis::latency_below_500ms())
        .with_tags(vec![
            "performance".into(),
            "stress".into(),
            "degradation".into(),
        ])
}

/// Fault tolerance testing profile
pub fn fault_tolerance_profile() -> ChaosProfile {
    ChaosProfile::new("Fault Tolerance")
        .with_description("Tests system ability to handle various failure scenarios")
        .add_scenario(Scenario::node_crash_recovery())
        .add_scenario(Scenario::cascading_failure())
        .add_scenario(Scenario::network_partition())
        .add_hypothesis(Hypothesis::availability_at_least_99())
        .add_hypothesis(Hypothesis::recovery_time_under(30.0))
        .with_tags(vec![
            "fault-tolerance".into(),
            "failover".into(),
            "recovery".into(),
        ])
}

/// Disaster recovery testing profile
pub fn disaster_recovery_profile() -> ChaosProfile {
    ChaosProfile::new("Disaster Recovery")
        .with_description("Tests disaster recovery capabilities and RTO/RPO compliance")
        .add_scenario(Scenario::cascading_failure())
        .add_scenario(Scenario::node_crash_recovery())
        .add_hypothesis(Hypothesis::recovery_time_under(300.0))
        .add_hypothesis(Hypothesis::data_consistency())
        .with_tags(vec![
            "disaster-recovery".into(),
            "rto".into(),
            "rpo".into(),
            "compliance".into(),
        ])
}

/// Extended hypothesis helpers
impl Hypothesis {
    /// Recovery time should be under specified seconds
    pub fn recovery_time_under(seconds: f64) -> Self {
        Self::new(
            format!("Recovery time should be under {} seconds", seconds),
            HypothesisType::RecoveryTimeBound,
            seconds,
        )
    }
}
