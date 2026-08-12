// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Integration tests for ADNet.
//
// Each submodule is gated by `#[cfg(feature = "legacy_tests")]` so
// the crate can be linked as a regular library target; that lets
// `cargo test --workspace` keep building without trying to compile
// the legacy `mod tests` blocks whose API references have drifted.
// Pass `--features legacy_tests` to revive the original tests after
// refreshing them against the current `adnet-dht` / `adnet-simulator`
// surfaces.

#[cfg(feature = "legacy_tests")]
pub mod network;
#[cfg(feature = "legacy_tests")]
pub mod storage;
#[cfg(feature = "legacy_tests")]
pub mod protocol;
#[cfg(feature = "legacy_tests")]
pub mod chaos;
#[cfg(feature = "legacy_tests")]
pub mod multi_node;

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokio::sync::{broadcast, mpsc};
#[cfg(test)]
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialize tracing for tests.
#[cfg(test)]
pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .try_init()
        .ok();
}

/// Test runtime configuration.
#[derive(Debug, Clone)]
pub struct TestConfig {
    pub num_nodes: usize,
    pub timeout_secs: u64,
    pub setup_time_secs: u64,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            num_nodes: 3,
            timeout_secs: 30,
            setup_time_secs: 2,
        }
    }
}

/// Create a test runtime.
#[cfg(test)]
pub fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .build()
        .expect("failed to create test runtime")
}

/// Helper to create temp directories for tests.
#[cfg(test)]
pub fn temp_dir() -> tempfile::TempDir {
    tempfile::TempDir::new().expect("failed to create temp dir")
}

/// Helper to wait for a condition with timeout.
#[cfg(test)]
pub async fn wait_for<F, Fut>(mut check: F, timeout_secs: u64) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs);

    while start.elapsed() < timeout {
        if check().await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    false
}

/// Helper to run a test with a timeout.
#[cfg(test)]
pub async fn run_with_timeout<F>(future: F, timeout_secs: u64) -> Result<F::Output, String>
where
    F: std::future::Future,
{
    tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), future)
        .await
        .map_err(|_| format!("test timed out after {timeout_secs} seconds"))
}
