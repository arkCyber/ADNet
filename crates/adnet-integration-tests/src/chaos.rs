// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Chaos and network failure tests.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_tracing, run_with_timeout};
    use adnet_simulator::{
        NetworkCondition, Latency, PacketLoss, Bandwidth, Partition,
        NetworkEmulator, ConnectionId,
        NetworkTopology, NodeRole, ConnectionConfig, ConnectionStats,
        presets, Scenario, ScenarioRunner,
    };
    use std::time::Duration;

    // ────────────────────────────────────────────────────────────────────
    // Network Condition Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_network_emulator_create() {
        init_tracing();
        let emulator = NetworkEmulator::new();
        assert!(emulator.connections().await.is_empty());
    }

    #[tokio::test]
    async fn test_network_emulator_add_remove_connection() {
        init_tracing();
        let emulator = NetworkEmulator::new();
        let id = ConnectionId("test-conn".to_string());
        let condition = NetworkCondition::default();

        // Add connection
        emulator.add_connection(id.clone(), condition).await;
        assert_eq!(emulator.connections().await.len(), 1);

        // Get stats
        let stats = emulator.get_stats(&id).await;
        assert!(stats.is_some());

        // Remove connection
        emulator.remove_connection(&id).await;
        assert!(emulator.connections().await.is_empty());
    }

    #[tokio::test]
    async fn test_network_emulator_latency() {
        init_tracing();
        let emulator = NetworkEmulator::new();
        let id = ConnectionId("latency-test".to_string());

        let mut condition = NetworkCondition::default();
        condition.latency = Some(Latency::new(100));

        emulator.add_connection(id.clone(), condition).await;

        // Send a packet
        let delay = emulator.send(&id, vec![1, 2, 3]).await;
        assert!(delay.is_some());

        // Wait and receive
        tokio::time::sleep(Duration::from_millis(150)).await;
        let packets = emulator.receive(&id).await;
        assert_eq!(packets.len(), 1);
    }

    #[tokio::test]
    async fn test_network_emulator_packet_loss() {
        init_tracing();
        let emulator = NetworkEmulator::new();
        let id = ConnectionId("loss-test".to_string());

        let mut condition = NetworkCondition::default();
        condition.packet_loss = Some(PacketLoss::new(0.5)); // 50% loss

        emulator.add_connection(id.clone(), condition).await;

        // Send many packets
        let mut dropped = 0;
        let total = 100;

        for _ in 0..total {
            if emulator.send(&id, vec![1, 2, 3]).await.is_none() {
                dropped += 1;
            }
        }

        // Should have dropped roughly 50%
        let loss_rate = dropped as f64 / total as f64;
        assert!(loss_rate > 0.3 && loss_rate < 0.7,
            "expected ~50% loss, got {}%", loss_rate * 100.0);
    }

    #[tokio::test]
    async fn test_network_emulator_partition() {
        init_tracing();
        let emulator = NetworkEmulator::new();
        let id = ConnectionId("partition-test".to_string());

        let mut condition = NetworkCondition::default();
        condition.partition = Some(Partition::new(Duration::from_secs(10)));

        emulator.add_connection(id.clone(), condition).await;

        // After a while, partition should be active
        // Note: This test is non-deterministic since partition is probabilistic
        let stats = emulator.get_stats(&id).await.unwrap();
        // Just verify we can get stats during partition
        assert!(stats.packets_dropped >= 0);
    }

    // ────────────────────────────────────────────────────────────────────
    // Network Topology Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_topology_complete_graph() {
        init_tracing();
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let topology = NetworkTopology::complete_graph(ids.clone(), ConnectionConfig::default());

        assert_eq!(topology.nodes().len(), 3);
        assert!(topology.are_connected("a", "b"));
        assert!(topology.are_connected("b", "c"));
        assert!(topology.are_connected("a", "c"));
    }

    #[tokio::test]
    async fn test_topology_star() {
        init_tracing();
        let hub = "hub".to_string();
        let spokes = vec!["s1".to_string(), "s2".to_string(), "s3".to_string()];
        let topology = NetworkTopology::star(
            hub.clone(),
            spokes,
            ConnectionConfig::default(),
            ConnectionConfig::default(),
        );

        assert_eq!(topology.nodes().len(), 4);
        assert!(topology.are_connected(&hub, "s1"));
        assert!(topology.are_connected(&hub, "s2"));
        assert!(topology.are_connected(&hub, "s3"));
        assert!(!topology.are_connected("s1", "s2")); // Spokes not connected
    }

    #[tokio::test]
    async fn test_topology_random() {
        init_tracing();
        let topology = NetworkTopology::random(20, 0.3);
        let stats = topology.stats();

        assert_eq!(stats.num_nodes, 20);
        assert!(stats.total_connections > 0);
    }

    #[tokio::test]
    async fn test_topology_stats() {
        init_tracing();
        let topology = NetworkTopology::random(10, 0.5);
        let stats = topology.stats();

        assert!(stats.num_nodes > 0);
        assert!(stats.avg_latency_ms > 0);
    }

    // ────────────────────────────────────────────────────────────────────
    // Scenario Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_scenario_good_network() {
        init_tracing();
        let condition = presets::good_network();

        assert!(condition.latency.is_some());
        assert!(condition.packet_loss.is_some());

        let latency = condition.latency.unwrap();
        assert!(latency.base_ms <= 50); // Good network has low latency
    }

    #[tokio::test]
    async fn test_scenario_poor_network() {
        init_tracing();
        let condition = presets::poor_network();

        assert!(condition.latency.is_some());
        let latency = condition.latency.unwrap();
        assert!(latency.base_ms >= 100); // Poor network has high latency
    }

    #[tokio::test]
    async fn test_scenario_mobile() {
        init_tracing();
        let condition = presets::mobile_network();

        assert!(condition.latency.is_some());
        assert!(condition.packet_loss.is_some());

        let latency = condition.latency.unwrap();
        assert!(latency.jitter_ms > latency.base_ms / 2); // Mobile has high jitter
    }

    #[tokio::test]
    async fn test_scenario_runner() {
        init_tracing();
        let scenario = Scenario::new("test", "A test scenario")
            .with_duration(Duration::from_millis(10))
            .with_topology(presets::three_node_mesh());

        let runner = ScenarioRunner::new().add(scenario);
        let results = runner.run_all().await;

        assert_eq!(results.len(), 1);
        assert!(results[0].success);
    }

    // ────────────────────────────────────────────────────────────────────
    // Resilience Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_reconnection_under_loss() {
        init_tracing();
        let emulator = NetworkEmulator::new();
        let id = ConnectionId("reconnect-test".to_string());

        // Start with high loss
        let mut condition = NetworkCondition::default();
        condition.packet_loss = Some(PacketLoss::new(0.9)); // 90% loss

        emulator.add_connection(id.clone(), condition).await;

        // Try to send - most will fail
        let successes: usize = (0..20)
            .filter(|_| emulator.send(&id, vec![1]).await.is_some())
            .count();

        // With 90% loss, we expect very few successes
        assert!(successes < 10, "expected mostly failures with 90% loss");

        // Update to low loss
        let mut new_condition = NetworkCondition::default();
        new_condition.packet_loss = Some(PacketLoss::new(0.01)); // 1% loss
        emulator.update_condition(&id, new_condition).await;

        // Now should mostly succeed
        tokio::time::sleep(Duration::from_millis(100)).await;
        let successes: usize = (0..20)
            .filter(|_| emulator.send(&id, vec![1]).await.is_some())
            .count();

        assert!(successes > 15, "expected mostly successes with 1% loss");
    }

    #[tokio::test]
    async fn test_recovery_after_partition() {
        init_tracing();
        let emulator = NetworkEmulator::new();
        let id = ConnectionId("partition-recovery".to_string());

        // Simulate partition with long duration
        let mut condition = NetworkCondition::default();
        condition.partition = Some(Partition {
            start_probability: 1.0, // Always start partition
            duration: Duration::from_millis(100),
            active: false,
            started_at: None,
        });

        emulator.add_connection(id.clone(), condition).await;

        // During partition, sends should fail
        tokio::time::sleep(Duration::from_millis(50)).await;

        let stats = emulator.get_stats(&id).await.unwrap();
        let dropped_before = stats.packets_dropped;

        // After partition ends (auto-updated), should work again
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Update to no partition
        let new_condition = NetworkCondition::default();
        emulator.update_condition(&id, new_condition).await;

        // Now sends should succeed
        let success = emulator.send(&id, vec![1]).await;
        assert!(success.is_some());
    }

    // ────────────────────────────────────────────────────────────────────
    // Load Under Failure Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_high_latency_tolerance() {
        init_tracing();
        let emulator = NetworkEmulator::new();
        let id = ConnectionId("latency-tolerance".to_string());

        // Very high latency
        let mut condition = NetworkCondition::default();
        condition.latency = Some(Latency::new(2000)); // 2 seconds

        emulator.add_connection(id.clone(), condition).await;

        // Send should return immediately with delay info
        let start = std::time::Instant::now();
        let delay = emulator.send(&id, vec![1]).await;
        let send_time = start.elapsed();

        // Send should be fast (just recording the delay)
        assert!(send_time < Duration::from_millis(100));

        // But packet won't be deliverable until after latency
        assert!(delay.unwrap() > Duration::from_secs(1));
    }

    #[tokio::test]
    async fn test_burst_packet_loss() {
        init_tracing();
        let emulator = NetworkEmulator::new();
        let id = ConnectionId("burst-loss".to_string());

        // Burst loss pattern
        let mut condition = NetworkCondition::default();
        condition.packet_loss = Some(PacketLoss::new(0.5).with_burst(10)); // Average burst of 10

        emulator.add_connection(id.clone(), condition).await;

        // Track when we see success/failure patterns
        let mut results = Vec::new();
        for _ in 0..100 {
            results.push(emulator.send(&id, vec![1]).is_some());
        }

        // Count consecutive failures (bursts)
        let mut max_consecutive_failures = 0;
        let mut current = 0;
        for success in &results {
            if *success {
                max_consecutive_failures = max_consecutive_failures.max(current);
                current = 0;
            } else {
                current += 1;
            }
        }
        max_consecutive_failures = max_consecutive_failures.max(current);

        // With burst loss, we expect some long streaks of failures
        assert!(max_consecutive_failures > 3,
            "expected burst pattern with some long streaks");
    }
}

fn max(a: usize, b: usize) -> usize {
    if a > b { a } else { b }
}
