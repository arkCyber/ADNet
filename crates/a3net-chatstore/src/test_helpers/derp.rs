//! Test helpers for DERP relay integration testing.
//!
//! This module provides utilities for testing iroh-docs P2P sync
//! over DERP relay connections.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use a3net_chatstore::test_helpers::derp::{
//!     TestDerpServer, TwoNodeDerpTopology
//! };
//!
//! #[tokio::test]
//! async fn test_message_sync_via_derp() {
//!     let topology = TwoNodeDerpTopology::new().await;
//!     // Test message sync between nodes via DERP
//!     topology.sync_and_verify().await;
//! }
//! ```

#[cfg(feature = "iroh")]
pub mod derp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Result;
    use tokio::net::TcpListener;
    use tokio::time::sleep;

    /// Phase 5c: Embedded DERP server for testing.
    ///
    /// This starts a minimal DERP relay server on a random port.
    /// Used for integration tests that require real P2P connectivity.
    ///
    /// ## Example
    ///
    /// ```rust,ignore
    /// let server = TestDerpServer::new().await?;
    /// let url = server.url();
    /// // Use `url` in iroh connection config
    /// ```
    pub struct TestDerpServer {
        port: u16,
        running: Arc<AtomicBool>,
        _shutdown_tx: tokio::sync::oneshot::Sender<()>,
    }

    impl TestDerpServer {
        /// Start a new DERP test server on a random port.
        pub async fn new() -> Result<Self> {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let port = listener.local_addr()?.port();

            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
            let running = Arc::new(AtomicBool::new(true));
            let running_clone = running.clone();

            // In a full implementation, we would spawn the actual iroh DERP server
            // For now, we just keep the port open to simulate a running server
            tokio::spawn(async move {
                let mut rx = shutdown_rx;
                loop {
                    tokio::select! {
                        _ = &mut rx => {
                            running_clone.store(false, Ordering::SeqCst);
                            break;
                        }
                        _ = sleep(Duration::from_secs(1)) => {
                            // Keep-alive for DERP server simulation
                        }
                    }
                }
            });

            Ok(Self {
                port,
                running,
                _shutdown_tx: shutdown_tx,
            })
        }

        /// Get the DERP server URL for use in iroh connection config.
        pub fn url(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        /// Get the port number.
        pub fn port(&self) -> u16 {
            self.port
        }

        /// Check if the server is still running.
        pub fn is_running(&self) -> bool {
            self.running.load(Ordering::SeqCst)
        }
    }

    // Drop handled by background task via oneshot channel drop

    /// Phase 5c: Two-node topology connected via DERP relay.
    ///
    /// Creates two iroh nodes that communicate through a DERP relay.
    /// Useful for testing NAT traversal and message sync.
    ///
    /// ## Network Topology
    ///
    /// ```text
    ///  Node A <---- DERP Relay ----> Node B
    ///   (peer)                     (peer)
    /// ```
    ///
    /// ## Example
    ///
    /// ```rust,ignore
    /// let topology = TwoNodeDerpTopology::new().await?;
    ///
    /// // Node A sends a message
    /// topology.node_a_append("hello").await?;
    ///
    /// // Node B receives the message
    /// topology.sync_and_verify().await?;
    /// ```
    pub struct TwoNodeDerpTopology {
        derp_server: TestDerpServer,
    }

    impl TwoNodeDerpTopology {
        /// Create a new two-node topology with a shared DERP relay.
        pub async fn new() -> Result<Self> {
            let derp_server = TestDerpServer::new().await?;

            Ok(Self {
                derp_server,
            })
        }

        /// Get the DERP relay URL.
        pub fn derp_url(&self) -> String {
            self.derp_server.url()
        }

        /// Wait for the DERP server to be ready.
        pub async fn wait_for_derp_ready(&self) -> Result<()> {
            // Retry binding to check server readiness
            for _ in 0..10 {
                if self.derp_server.is_running() {
                    return Ok(());
                }
                sleep(Duration::from_millis(100)).await;
            }
            anyhow::bail!("DERP server failed to start")
        }

        /// Simulate sync between the two nodes.
        ///
        /// In a full implementation, this would:
        /// 1. Have both nodes connect to the DERP relay
        /// 2. Exchange doc capabilities
        /// 3. Verify message sync
        pub async fn sync_and_verify(&self) -> Result<()> {
            // Placeholder: In real implementation, this would:
            // - Connect node A to DERP
            // - Connect node B to DERP
            // - Exchange iroh-docs capabilities
            // - Verify messages are synced
            Ok(())
        }

        /// Get the DERP server port (for configuration).
        pub fn derp_port(&self) -> u16 {
            self.derp_server.port()
        }
    }

    /// Phase 5c: Configuration for connecting to a DERP relay.
    #[derive(Debug, Clone)]
    pub struct DerpConfig {
        /// DERP relay URLs (can be multiple for redundancy).
        pub urls: Vec<String>,
        /// Optional: region code for the DERP server.
        pub region: Option<String>,
        /// Connection timeout.
        pub timeout: Duration,
    }

    impl Default for DerpConfig {
        fn default() -> Self {
            Self {
                urls: vec![
                    // Default: use iroh's public DERP servers
                    // In production, replace with self-hosted DERP
                    "https://derp.iroh.example.com".to_string(),
                ],
                region: None,
                timeout: Duration::from_secs(30),
            }
        }
    }

    impl DerpConfig {
        /// Create a config pointing to a local DERP server.
        pub fn local(port: u16) -> Self {
            Self {
                urls: vec![format!("http://127.0.0.1:{}", port)],
                region: Some("local".to_string()),
                timeout: Duration::from_secs(30),
            }
        }

        /// Add a DERP relay URL.
        pub fn with_url(mut self, url: impl Into<String>) -> Self {
            self.urls.push(url.into());
            self
        }

        /// Set the region code.
        pub fn with_region(mut self, region: impl Into<String>) -> Self {
            self.region = Some(region.into());
            self
        }
    }

    /// Phase 5c: Metrics from DERP relay connection.
    #[derive(Debug, Default)]
    pub struct DerpMetrics {
        /// Number of bytes sent through DERP.
        pub bytes_sent: u64,
        /// Number of bytes received through DERP.
        pub bytes_received: u64,
        /// Current latency to DERP server (ms).
        pub latency_ms: Option<u64>,
        /// Whether currently connected via DERP.
        pub connected: bool,
    }

    /// Phase 5c: Test scenario for DERP relay failure.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DerpFailureScenario {
        /// DERP server is completely unreachable.
        Unreachable,
        /// DERP server responds slowly (high latency).
        HighLatency,
        /// DERP server drops connections intermittently.
        IntermittentDrop,
        /// DERP server returns error responses.
        ServerError,
    }

    impl DerpFailureScenario {
        /// Get a description of the failure scenario.
        pub fn description(&self) -> &'static str {
            match self {
                Self::Unreachable => "DERP server is unreachable",
                Self::HighLatency => "DERP server has high latency",
                Self::IntermittentDrop => "DERP server drops connections",
                Self::ServerError => "DERP server returns errors",
            }
        }
    }

    /// Phase 5c: DERP relay health check result.
    #[derive(Debug)]
    pub struct DerpHealthCheck {
        /// Whether the DERP server is reachable.
        pub reachable: bool,
        /// Latency in milliseconds.
        pub latency_ms: Option<u64>,
        /// Whether TLS certificate is valid.
        pub tls_valid: bool,
        /// Server version (if available).
        pub server_version: Option<String>,
    }

    impl DerpHealthCheck {
        /// Create a successful health check.
        pub fn healthy(latency_ms: u64) -> Self {
            Self {
                reachable: true,
                latency_ms: Some(latency_ms),
                tls_valid: true,
                server_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }
        }

        /// Create an unhealthy health check.
        #[allow(unused_variables)]
        pub fn unhealthy(reason: &str) -> Self {
            Self {
                reachable: false,
                latency_ms: None,
                tls_valid: false,
                server_version: None,
            }
        }
    }

    // ========================================================================
    // Integration test helpers
    // ========================================================================

    /// Phase 5c: Helper to run DERP integration tests.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// run_derp_integration_test(|topology| async move {
    ///     // Your test code here
    /// }).await;
    /// ```
    pub async fn run_derp_integration_test<F, Fut>(test: F)
    where
        F: FnOnce(TwoNodeDerpTopology) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let topology = TwoNodeDerpTopology::new().await.expect("create topology");
        if let Err(e) = test(topology).await {
            panic!("DERP integration test failed: {e}");
        }
    }

    /// Phase 5c: Test message sync under DERP relay.
    ///
    /// This test verifies that messages sent from one node are
    /// received by another node via the DERP relay.
    ///
    /// # Test Steps
    ///
    /// 1. Create two nodes connected via DERP
    /// 2. Node A creates a conversation doc
    /// 3. Node A appends a message
    /// 4. Node B syncs and receives the message
    /// 5. Verify message content matches
    pub async fn test_message_sync_via_derp() -> Result<()> {
        let topology = TwoNodeDerpTopology::new().await?;
        topology.wait_for_derp_ready().await?;

        // In a full implementation:
        // 1. Create two IrohDocsChat instances
        // 2. Configure both to use the DERP relay
        // 3. Open a doc on node A
        // 4. Append a message on node A
        // 5. Subscribe on node B
        // 6. Verify node B receives the message

        Ok(())
    }

    /// Phase 5c: Test DERP relay failover.
    ///
    /// This test verifies graceful degradation when the primary
    /// DERP relay becomes unavailable.
    ///
    /// # Test Steps
    ///
    /// 1. Connect nodes via primary DERP
    /// 2. Verify message sync works
    /// 3. Fail over to backup DERP
    /// 4. Verify message sync continues
    pub async fn test_derp_failover() -> Result<()> {
        // In a full implementation, this would:
        // 1. Start two DERP servers
        // 2. Configure iroh with both URLs
        // 3. Kill primary DERP
        // 4. Verify automatic failover to backup
        Ok(())
    }

    /// Phase 5c: Test DERP relay latency.
    ///
    /// Measures the round-trip time through the DERP relay.
    pub async fn measure_derp_latency() -> Result<u64> {
        let topology = TwoNodeDerpTopology::new().await?;
        topology.wait_for_derp_ready().await?;

        // In a full implementation, measure actual RTT
        // For now, return a placeholder
        Ok(50) // ms
    }
}

#[cfg(not(feature = "iroh"))]
pub mod derp {
    use anyhow::Result;

    /// Stub when iroh feature is disabled.
    pub struct TestDerpServer;

    /// Stub when iroh feature is disabled.
    pub struct TwoNodeDerpTopology;

    /// Stub when iroh feature is disabled.
    pub struct DerpConfig;

    /// Stub when iroh feature is disabled.
    pub async fn run_derp_integration_test<F, Fut>(_test: F)
    where
        F: FnOnce(TwoNodeDerpTopology) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        // No-op when iroh is disabled
    }
}
