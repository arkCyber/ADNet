//! Real-world example — replay a full network scenario using the
//! `ScenarioRunner`, mixing the `NetworkEmulator` + a topology
//! from `presets::relay_topology()` + multiple per-node
//! `NetworkCondition`s. Mimics how `adnet-integration-tests`
//! drives end-to-end relay download tests under a "poor network"
//! preset, but in a single self-contained binary.
//!
//! Run with:
//!   cargo run -p adnet-simulator --example sim_app

use std::sync::Arc;
use std::time::Duration;

use adnet_simulator::{
    scenarios::{presets, Scenario, ScenarioRunner},
    ConnectionId, NetworkEmulator,
};

#[tokio::main]
async fn main() {
    // ─── Build topology + scenarios ─────────────────────────────
    let topology = presets::relay_topology();
    let nodes: Vec<String> = topology
        .nodes()
        .keys()
        .cloned()
        .collect();

    let scenario = Scenario::new(
        "relay-poor-network",
        "Replay relay topology under poor-network conditions",
    )
    .with_duration(Duration::from_millis(50))
    .with_topology(topology.clone())
    .add_node_condition("relay", presets::good_network())
    .add_node_condition("peer-1", presets::moderate_network())
    .add_node_condition("peer-2", presets::poor_network())
    .add_node_condition("peer-3", presets::mobile_network())
    .expect("scenario completed without panic")
    .expect("node traffic reached > 95% delivery")
    .expect("relay processed every peer within SLA");

    let runner = ScenarioRunner::new().add(scenario);

    // ─── Run scenarios, then exercise the emulator per-node ─────
    println!("=== adnet-simulator app example ===");
    let results = runner.run_all().await;
    for result in &results {
        println!(
            "[scenario `{}`] ok={} events={} elapsed={:?}",
            result.scenario,
            result.success,
            result.events.len(),
            result.duration
        );
        for ev in &result.events {
            println!("  {ev:?}");
        }
    }

    // The emulator accepts the same conditions in isolation, so
    // back the same nodes with an in-memory emulator and observe
    // the live stats. The partition updater is spawned in the
    // background so that `intermittent` and `network_partition`
    // presets work end-to-end without a manual `update()` call.
    let emulator = Arc::new(NetworkEmulator::new());
    let _updater = emulator.clone().spawn_partition_updater();

    for (i, node) in nodes.iter().enumerate() {
        let condition = match i {
            0 => presets::good_network(),
            1 => presets::moderate_network(),
            2 => presets::poor_network(),
            _ => presets::mobile_network(),
        };
        let id = ConnectionId(format!("emul-{node}"));
        emulator.add_connection(id.clone(), condition).await;
        for j in 0..5 {
            let payload = format!("{node}-{j}").into_bytes();
            emulator.send(&id, payload).await;
        }
    }

    tokio::time::sleep(Duration::from_millis(120)).await;

    for node in &nodes {
        let id = ConnectionId(format!("emul-{node}"));
        if let Some(stats) = emulator.get_stats(&id).await {
            println!(
                "  {node:>8} → sent={} recv={} dropped={} bytes={}",
                stats.packets_sent,
                stats.packets_received,
                stats.packets_dropped,
                stats.bytes_sent
            );
        }
    }
    println!(
        "total connections managed: {}",
        emulator.connections().await.len()
    );
}
