//! Tests for the chaos engineering framework

use adnet_chaos::{
    ChaosEngine, Scenario, Hypothesis, ExperimentPhase,
    FaultConfig, FaultType, FaultTarget, FaultParameters,
    NetworkFaultType, NodeFaultType, DataFaultType,
};

#[tokio::test]
async fn test_engine_creation() {
    let engine = ChaosEngine::new();
    let status = engine.status().await;
    assert!(status.is_none());
}

#[tokio::test]
async fn test_scenario_creation() {
    let scenario = Scenario::network_partition();
    
    assert_eq!(scenario.name, "Network Partition");
    assert_eq!(scenario.steps.len(), 1);
    assert_eq!(scenario.baseline_duration.as_secs(), 30);
}

#[tokio::test]
async fn test_scenario_builder() {
    let scenario = Scenario::new("Test Scenario")
        .with_description("A test scenario")
        .with_baseline(std::time::Duration::from_secs(10))
        .with_recovery(std::time::Duration::from_secs(5));
    
    assert_eq!(scenario.name, "Test Scenario");
    assert_eq!(scenario.description, "A test scenario");
    assert_eq!(scenario.baseline_duration.as_secs(), 10);
    assert_eq!(scenario.recovery_duration.as_secs(), 5);
}

#[tokio::test]
async fn test_hypothesis_check() {
    let mut hypothesis = Hypothesis::availability_at_least_99();
    
    // Should pass (0.999 >= 0.99)
    assert!(hypothesis.check(0.999));
    
    // Should fail (0.95 < 0.99)
    let mut fail_hypothesis = Hypothesis::availability_at_least_99();
    assert!(!fail_hypothesis.check(0.95));
}

#[tokio::test]
async fn test_fault_config() {
    let config = FaultConfig::new(
        FaultType::NetworkFault(NetworkFaultType::Latency),
        FaultTarget::all_nodes(),
    );
    
    let config_with_duration = config.with_duration(std::time::Duration::from_secs(60));
    assert_eq!(config_with_duration.duration, Some(std::time::Duration::from_secs(60)));
    
    let config_with_params = config_with_duration.with_parameters(
        FaultParameters::new().with_latency(500)
    );
    assert_eq!(config_with_params.parameters.latency_ms, Some(500));
}

#[tokio::test]
async fn test_fault_types() {
    // Network faults
    let latency = FaultType::NetworkFault(NetworkFaultType::Latency);
    assert_eq!(latency.default_severity(), adnet_chaos::Severity::Low);
    
    let partition = FaultType::NetworkFault(NetworkFaultType::Partition);
    assert_eq!(partition.default_severity(), adnet_chaos::Severity::Critical);
    
    // Node faults
    let crash = FaultType::NodeFault(NodeFaultType::Crash);
    assert_eq!(crash.default_severity(), adnet_chaos::Severity::Critical);
    
    // Data faults
    let corruption = FaultType::DataFault(DataFaultType::Corruption);
    assert_eq!(corruption.default_severity(), adnet_chaos::Severity::High);
}

#[tokio::test]
async fn test_fault_target() {
    let node = FaultTarget::node("test-node");
    match node {
        FaultTarget::Node(id) => assert_eq!(id, "test-node"),
        _ => panic!("Expected Node variant"),
    }
    
    let all = FaultTarget::all_nodes();
    match all {
        FaultTarget::AllNodes => {},
        _ => panic!("Expected AllNodes variant"),
    }
}

#[tokio::test]
async fn test_experiment_phases() {
    assert_eq!(ExperimentPhase::Baseline.to_string(), "baseline");
    assert_eq!(ExperimentPhase::FaultInjection.to_string(), "fault_injection");
    assert_eq!(ExperimentPhase::Completed.to_string(), "completed");
    assert_eq!(ExperimentPhase::Failed.to_string(), "failed");
}

#[tokio::test]
async fn test_predefined_scenarios() {
    // Test all predefined scenarios
    let scenarios = vec![
        Scenario::network_partition(),
        Scenario::node_crash_recovery(),
        Scenario::high_latency(),
        Scenario::packet_loss(),
        Scenario::data_corruption(),
        Scenario::memory_pressure(),
        Scenario::cascading_failure(),
    ];
    
    for scenario in scenarios {
        assert!(!scenario.name.is_empty());
        assert!(!scenario.steps.is_empty());
    }
}

#[tokio::test]
async fn test_predefined_hypotheses() {
    // Test all predefined hypotheses
    let hypotheses = vec![
        Hypothesis::availability_at_least_99(),
        Hypothesis::error_rate_below_1_percent(),
        Hypothesis::latency_below_500ms(),
        Hypothesis::minimum_throughput_100(),
        Hypothesis::data_consistency(),
        Hypothesis::recovery_time_under(30.0),
    ];
    
    for hypothesis in hypotheses {
        assert!(!hypothesis.description.is_empty());
        assert!(hypothesis.threshold > 0.0);
    }
}
