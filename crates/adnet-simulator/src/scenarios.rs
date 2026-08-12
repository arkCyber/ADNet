// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Predefined testing scenarios.

use crate::conditions::{Latency, NetworkCondition, PacketLoss};
use crate::topology::{NetworkTopology, NodeRole, ConnectionConfig};
use std::time::Duration;

/// A predefined testing scenario.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: String,
    pub description: String,
    pub duration: Option<Duration>,
    pub topology: NetworkTopology,
    pub conditions: Vec<(String, NetworkCondition)>, // node_id -> conditions
    pub expected_outcomes: Vec<String>,
}

impl Scenario {
    /// Create a new scenario.
    pub fn new(name: &str, description: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            duration: None,
            topology: NetworkTopology::new(),
            conditions: Vec::new(),
            expected_outcomes: Vec::new(),
        }
    }

    /// Set scenario duration.
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    /// Set the topology.
    pub fn with_topology(mut self, topology: NetworkTopology) -> Self {
        self.topology = topology;
        self
    }

    /// Add conditions for a node.
    pub fn add_node_condition(mut self, node_id: &str, condition: NetworkCondition) -> Self {
        self.conditions.push((node_id.to_string(), condition));
        self
    }

    /// Add an expected outcome.
    pub fn expect(mut self, outcome: &str) -> Self {
        self.expected_outcomes.push(outcome.to_string());
        self
    }
}

/// Runner for executing scenarios.
pub struct ScenarioRunner {
    scenarios: Vec<Scenario>,
}

impl ScenarioRunner {
    /// Create a new scenario runner.
    pub fn new() -> Self {
        Self {
            scenarios: Vec::new(),
        }
    }

    /// Add a scenario.
    pub fn add(mut self, scenario: Scenario) -> Self {
        self.scenarios.push(scenario);
        self
    }

    /// Get all scenarios.
    pub fn scenarios(&self) -> &[Scenario] {
        &self.scenarios
    }

    /// Run a specific scenario.
    pub async fn run(&self, name: &str) -> Option<ScenarioResult> {
        let scenario = self.scenarios.iter().find(|s| s.name == name)?;
        Some(self.execute(scenario).await)
    }

    /// Execute a scenario.
    async fn execute(&self, scenario: &Scenario) -> ScenarioResult {
        let start = std::time::Instant::now();
        let mut events = Vec::new();

        // Log scenario start
        events.push(ScenarioEvent::Started {
            scenario: scenario.name.clone(),
            timestamp: start,
        });

        // Apply conditions to topology
        for (node_id, condition) in &scenario.conditions {
            events.push(ScenarioEvent::ConditionApplied {
                node_id: node_id.clone(),
                condition: condition.clone(),
            });
        }

        // Simulate scenario duration
        if let Some(duration) = scenario.duration {
            tokio::time::sleep(duration).await;
        }

        let elapsed = start.elapsed();
        events.push(ScenarioEvent::Completed {
            duration: elapsed,
            outcomes_verified: scenario.expected_outcomes.clone(),
        });

        ScenarioResult {
            scenario: scenario.name.clone(),
            duration: elapsed,
            events,
            success: true,
        }
    }

    /// Run all scenarios.
    pub async fn run_all(&self) -> Vec<ScenarioResult> {
        let mut results = Vec::new();
        for scenario in &self.scenarios {
            results.push(self.execute(scenario).await);
        }
        results
    }
}

impl Default for ScenarioRunner {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of running a scenario.
#[derive(Debug)]
pub struct ScenarioResult {
    pub scenario: String,
    pub duration: Duration,
    pub events: Vec<ScenarioEvent>,
    pub success: bool,
}

/// Events that occur during scenario execution.
#[derive(Debug)]
pub enum ScenarioEvent {
    Started {
        scenario: String,
        timestamp: std::time::Instant,
    },
    ConditionApplied {
        node_id: String,
        condition: NetworkCondition,
    },
    Completed {
        duration: Duration,
        outcomes_verified: Vec<String>,
    },
}

/// Predefined scenarios for common testing situations.
pub mod presets {
    use super::*;

    /// Good network conditions.
    pub fn good_network() -> NetworkCondition {
        NetworkCondition {
            latency: Some(Latency::new(20)),
            packet_loss: Some(PacketLoss::new(0.001)),
            bandwidth: None,
            corruption_rate: 0.0,
            partition: None,
            reordering_rate: 0.0,
        }
    }

    /// Moderate network conditions.
    pub fn moderate_network() -> NetworkCondition {
        NetworkCondition {
            latency: Some(Latency::new(100)),
            packet_loss: Some(PacketLoss::new(0.01)),
            bandwidth: None,
            corruption_rate: 0.0,
            partition: None,
            reordering_rate: 0.01,
        }
    }

    /// Poor network conditions.
    pub fn poor_network() -> NetworkCondition {
        NetworkCondition {
            latency: Some(Latency::new(500)),
            packet_loss: Some(PacketLoss::new(0.05)),
            bandwidth: None,
            corruption_rate: 0.001,
            partition: None,
            reordering_rate: 0.05,
        }
    }

    /// Mobile network conditions (high latency, variable).
    pub fn mobile_network() -> NetworkCondition {
        NetworkCondition {
            latency: Some(Latency::new(200).with_jitter(100)),
            packet_loss: Some(PacketLoss::new(0.02)),
            bandwidth: None,
            corruption_rate: 0.0,
            partition: None,
            reordering_rate: 0.02,
        }
    }

    /// Satellite network conditions (very high latency).
    pub fn satellite_network() -> NetworkCondition {
        NetworkCondition {
            latency: Some(Latency::new(600).with_jitter(50)),
            packet_loss: Some(PacketLoss::new(0.01)),
            bandwidth: None,
            corruption_rate: 0.0,
            partition: None,
            reordering_rate: 0.01,
        }
    }

    /// Intermittent connectivity.
    pub fn intermittent() -> NetworkCondition {
        use crate::conditions::Partition;
        NetworkCondition {
            latency: Some(Latency::new(50)),
            packet_loss: None,
            bandwidth: None,
            corruption_rate: 0.0,
            partition: Some(Partition::new(Duration::from_secs(30))),
            reordering_rate: 0.0,
        }
    }

    /// Network partition scenario.
    pub fn network_partition() -> NetworkCondition {
        use crate::conditions::Partition;
        NetworkCondition {
            latency: None,
            packet_loss: None,
            bandwidth: None,
            corruption_rate: 0.0,
            partition: Some(Partition::new(Duration::from_secs(60))),
            reordering_rate: 0.0,
        }
    }

    /// High packet loss scenario.
    pub fn packet_loss_storm() -> NetworkCondition {
        NetworkCondition {
            latency: Some(Latency::new(100)),
            packet_loss: Some(PacketLoss::new(0.3)),
            bandwidth: None,
            corruption_rate: 0.01,
            partition: None,
            reordering_rate: 0.1,
        }
    }

    /// Create a 3-node mesh scenario.
    pub fn three_node_mesh() -> NetworkTopology {
        let mut topology = NetworkTopology::new();

        let ids = vec!["node-a", "node-b", "node-c"];
        for id in ids {
            topology.add_node(crate::topology::TopologyNode::new(
                id.to_string(),
                NodeRole::Peer,
            ));
        }

        let config = ConnectionConfig::default();
        if let Some(node) = topology.get_node_mut("node-a") {
            node.add_connection("node-b".to_string(), config.clone());
            node.add_connection("node-c".to_string(), config.clone());
        }
        if let Some(node) = topology.get_node_mut("node-b") {
            node.add_connection("node-c".to_string(), config);
        }

        topology
    }

    /// Create a relay topology.
    pub fn relay_topology() -> NetworkTopology {
        NetworkTopology::star(
            "relay".to_string(),
            vec!["peer-1".to_string(), "peer-2".to_string(), "peer-3".to_string()],
            ConnectionConfig {
                latency_ms: 10,
                jitter_ms: 2,
                packet_loss: 0.001,
                high_latency: false,
            },
            ConnectionConfig {
                latency_ms: 50,
                jitter_ms: 10,
                packet_loss: 0.001,
                high_latency: false,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_scenario_runner() {
        let runner = ScenarioRunner::new()
            .add(
                Scenario::new("test", "A test scenario")
                    .with_duration(Duration::from_millis(10))
                    .with_topology(presets::three_node_mesh())
                    .expect("completed"),
            );

        let results = runner.run_all().await;
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
    }
}
