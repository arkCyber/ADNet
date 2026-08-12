//! Minimal example — assemble a `NetworkCondition` from scratch,
//! then drive a single `NetworkEmulator` connection through its
//! key surface area (send → measure latency → receive).
//!
//! Run with:
//!   cargo run -p adnet-simulator --example sim_basic

use std::time::Instant;

use adnet_simulator::{
    ConnectionId, Latency, NetworkCondition, NetworkEmulator, PacketLoss,
};

#[tokio::main]
async fn main() {
    // ─── Build a deterministic condition ────────────────────────
    // 60 ms base ± 5 ms jitter, 1% packet loss. Both values are
    // clamped by `should_drop()` / `actual_latency()` so even if
    // the rand seed is unlucky, the example stays in roughly the
    // right ballpark.
    let condition = NetworkCondition {
        latency: Some(Latency::new(60).with_jitter(5)),
        packet_loss: Some(PacketLoss::new(0.01)),
        bandwidth: None,
        corruption_rate: 0.0,
        partition: None,
        reordering_rate: 0.0,
    };

    // ─── Wire up the emulator ───────────────────────────────────
    let emulator = NetworkEmulator::new();
    let conn = ConnectionId("basic-conn".into());
    emulator.add_connection(conn.clone(), condition).await;

    // ─── Drive a small send/receive loop ────────────────────────
    let mut sent = 0u64;
    let mut dropped = 0u64;
    let mut total_delay = std::time::Duration::ZERO;
    let start = Instant::now();

    for i in 0..20 {
        let payload = format!("packet-{i}").into_bytes();
        match emulator.send(&conn, payload).await {
            Some(delay) => {
                sent += 1;
                total_delay += delay;
            }
            None => dropped += 1,
        }
    }

    // Wait for the queue to drain so `receive()` has work.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let delivered = emulator.receive(&conn).await;

    let stats = emulator.get_stats(&conn).await.expect("stats");
    println!("=== adnet-simulator basic example ===");
    println!("wall time   : {:?}", start.elapsed());
    println!("sent        : {sent}");
    println!("dropped     : {dropped}");
    println!("delivered   : {}", delivered.len());
    println!(
        "avg delay   : {:?}",
        if sent > 0 {
            total_delay / sent as u32
        } else {
            std::time::Duration::ZERO
        }
    );
    println!(
        "stats       : sent={} recv={} dropped={} bytes_sent={} bytes_recv={}",
        stats.packets_sent,
        stats.packets_received,
        stats.packets_dropped,
        stats.bytes_sent,
        stats.bytes_received,
    );
}
