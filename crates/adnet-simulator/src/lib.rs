// SPDX-License-Identifier: MIT OR Apache-2.0
//
// adnet-simulator — Network condition simulator for ADNet.
//
// This crate provides tools for simulating realistic network conditions:
// - Packet loss and corruption
// - Latency and jitter
// - Bandwidth throttling
// - Network partitions
// - Byzantine failures
//
// ## Usage
//
// ```rust
// use adnet_simulator::{NetworkSimulator, NetworkCondition};
//
// let simulator = NetworkSimulator::new()
//     .with_latency_ms(50, 10)  // 50ms ± 10ms jitter
//     .with_packet_loss(0.01);  // 1% packet loss
//
// // Apply conditions to a connection
// let condition = simulator.apply_to_connection(conn).await?;
// ```
//
// ## Testing Scenarios
//
// - Chaos testing: randomly inject failures
// - Performance testing: measure under realistic conditions
// - Resilience testing: verify graceful degradation

pub mod conditions;
pub mod emulator;
pub mod topology;
pub mod scenarios;

pub use conditions::{Bandwidth, Latency, NetworkCondition, PacketLoss, Partition};
pub use emulator::{ConnectionId, ConnectionStats, NetworkEmulator};
pub use scenarios::{Scenario, ScenarioRunner, presets};
pub use topology::{ConnectionConfig, NetworkTopology, NodeRole};
