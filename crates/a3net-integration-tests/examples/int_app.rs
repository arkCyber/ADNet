//! NOTE: `a3net-integration-tests` is a test-only crate — its helpers
//! (`init_tracing`, `wait_for`, `run_with_timeout`, `temp_dir`,
//! `test_runtime`) are all gated behind `#[cfg(test)]`, so this file
//! is **intentionally not a `cargo`-runnable example**. The
//! `//!` header below is the documentation; copy it into a `#[tokio::test]`
//! function inside the crate to execute it.
//!
//! Pattern: drive a multi-node scenario through `wait_for` + a
//! `NetworkEmulator` from `a3net-simulator`, validating that gossip
//! converges within `timeout_secs` even under simulated packet loss.
//!
//! ```ignore
//! use a3net_integration_tests::{init_tracing, TestConfig, wait_for, run_with_timeout};
//! use a3net_simulator::{ConnectionId, NetworkCondition, NetworkEmulator,
//!                       scenarios::presets};
//! use std::sync::Arc;
//!
//! #[tokio::test]
//! async fn gossip_converges_under_loss() {
//!     init_tracing();
//!     let cfg = TestConfig::default();
//!
//!     let emulator = Arc::new(NetworkEmulator::new());
//!     let _updater = emulator.clone().spawn_partition_updater();
//!
//!     let arrived = Arc::new(tokio::sync::Mutex::new(0u32));
//!     let arrived_clone = arrived.clone();
//!
//!     // Worker: send and record received packets.
//!     let worker = tokio::spawn(async move {
//!         let id = ConnectionId("node-a".into());
//!         emulator.add_connection(id.clone(), presets::moderate_network()).await;
//!         for _ in 0..10 {
//!             if emulator.send(&id, vec![0u8; 64]).await.is_some() {
//!                 let mut g = arrived_clone.lock().await;
//!                 *g += 1;
//!             }
//!             tokio::time::sleep(std::time::Duration::from_millis(50)).await;
//!         }
//!     });
//!
//!     let _ = run_with_timeout(worker, cfg.timeout_secs).await.unwrap();
//!
//!     let reached = wait_for(
//!         || async { *arrived.lock().await >= 5 },
//!         cfg.timeout_secs,
//!     ).await;
//!     assert!(reached, "gossip did not converge within timeout");
//! }
//! ```

fn main() {
    println!("This crate ships no runnable examples; see the `//!` doc above.");
    println!("Copy the pattern into a `#[tokio::test]` inside the crate.");
}
