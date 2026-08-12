//! NOTE: `adnet-integration-tests` is a test-only crate — its helpers
//! (`init_tracing`, `wait_for`, `run_with_timeout`, `temp_dir`,
//! `test_runtime`) are all gated behind `#[cfg(test)]`, so this file
//! is **intentionally not a `cargo`-runnable example**. The
//! `//!` header below is the documentation; copy it into a `#[tokio::test]`
//! or `#[test]` function inside the crate to execute it.
//!
//! Pattern: build a `TestConfig`, initialise tracing, then drive a
//! minimal two-node happy path through the helpers.
//!
//! ```ignore
//! use adnet_integration_tests::{init_tracing, TestConfig, run_with_timeout};
//!
//! #[tokio::test]
//! async fn two_node_smoke() {
//!     init_tracing();
//!
//!     let cfg = TestConfig {
//!         num_nodes: 2,
//!         timeout_secs: 15,
//!         setup_time_secs: 2,
//!     };
//!
//!     let ok = run_with_timeout(async move {
//!         // pseudo-work — in real tests this would call into
//!         // adnet-dht, adnet-gossip, adnet-blobstore, etc.
//!         tokio::time::sleep(std::time::Duration::from_millis(100)).await;
//!         true
//!     }, cfg.timeout_secs).await;
//!
//!     assert!(ok.is_ok(), "two_node_smoke timed out");
//! }
//! ```

fn main() {
    println!("This crate ships no runnable examples; see the `//!` doc above.");
    println!("Copy the pattern into a `#[tokio::test]` inside the crate.");
}
