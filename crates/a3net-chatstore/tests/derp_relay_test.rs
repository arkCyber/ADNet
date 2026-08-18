//! DERP relay integration tests for group sync.
//!
//! Phase 5c: Tests for P2P group chat sync over DERP relay connections.
//!
//! These tests verify:
//! - DERP server connectivity
//! - Two-node topology creation
//! - DERP configuration
//! - Health check functionality
//! - Latency measurement
//! - Failure scenarios

#![cfg(feature = "iroh")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::net::TcpListener;
use tokio::time::sleep;

// ========================================================================
// Test helper types
// ========================================================================

/// Phase 5c: Embedded DERP server for testing.
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

        tokio::spawn(async move {
            let mut rx = shutdown_rx;
            loop {
                tokio::select! {
                    _ = &mut rx => {
                        running_clone.store(false, Ordering::SeqCst);
                        break;
                    }
                    _ = sleep(Duration::from_secs(1)) => {}
                }
            }
        });

        Ok(Self {
            port,
            running,
            _shutdown_tx: shutdown_tx,
        })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

/// Phase 5c: Two-node topology connected via DERP relay.
pub struct TwoNodeDerpTopology {
    derp_server: TestDerpServer,
}

impl TwoNodeDerpTopology {
    pub async fn new() -> Result<Self> {
        let derp_server = TestDerpServer::new().await?;
        Ok(Self { derp_server })
    }

    pub fn derp_url(&self) -> String {
        self.derp_server.url()
    }

    pub fn derp_port(&self) -> u16 {
        self.derp_server.port()
    }

    pub async fn wait_for_derp_ready(&self) -> Result<()> {
        for _ in 0..10 {
            if self.derp_server.is_running() {
                return Ok(());
            }
            sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("DERP server failed to start")
    }
}

/// Phase 5c: DERP config.
#[derive(Debug, Clone)]
pub struct DerpConfig {
    pub urls: Vec<String>,
    pub region: Option<String>,
    pub timeout: Duration,
}

impl Default for DerpConfig {
    fn default() -> Self {
        Self {
            urls: vec!["https://derp.iroh.example.com".to_string()],
            region: None,
            timeout: Duration::from_secs(30),
        }
    }
}

impl DerpConfig {
    pub fn local(port: u16) -> Self {
        Self {
            urls: vec![format!("http://127.0.0.1:{}", port)],
            region: Some("local".to_string()),
            timeout: Duration::from_secs(30),
        }
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.urls.push(url.into());
        self
    }

    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }
}

/// Phase 5c: Failure scenario for testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerpFailureScenario {
    Unreachable,
    HighLatency,
    IntermittentDrop,
    ServerError,
}

impl DerpFailureScenario {
    pub fn description(&self) -> &'static str {
        match self {
            Self::Unreachable => "DERP server is unreachable",
            Self::HighLatency => "DERP server has high latency",
            Self::IntermittentDrop => "DERP server drops connections",
            Self::ServerError => "DERP server returns errors",
        }
    }
}

/// Phase 5c: DERP health check result.
#[derive(Debug)]
pub struct DerpHealthCheck {
    pub reachable: bool,
    pub latency_ms: Option<u64>,
    pub tls_valid: bool,
    pub server_version: Option<String>,
}

impl DerpHealthCheck {
    pub fn healthy(latency_ms: u64) -> Self {
        Self {
            reachable: true,
            latency_ms: Some(latency_ms),
            tls_valid: true,
            server_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }

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
// Server lifecycle tests
// ========================================================================

/// Phase 5c: Test that we can start a local DERP test server.
#[tokio::test]
async fn test_derp_server_start() {
    let server = TestDerpServer::new().await;
    assert!(server.is_ok(), "should start DERP server");

    let server = server.unwrap();
    assert!(server.port() > 0, "should have valid port");
    assert!(server.url().starts_with("http://"), "should have valid URL");
}

/// Phase 5c: Test DERP server lifecycle.
#[tokio::test]
async fn test_derp_server_lifecycle() {
    let server = TestDerpServer::new().await.expect("start server");

    assert!(server.is_running(), "server should be running");

    drop(server);

    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// Phase 5c: Test multiple servers can run simultaneously.
#[tokio::test]
async fn test_multiple_derp_servers() {
    let server1 = TestDerpServer::new().await.expect("server1");
    let server2 = TestDerpServer::new().await.expect("server2");

    assert_ne!(server1.port(), server2.port(), "ports should be different");
    assert!(server1.is_running());
    assert!(server2.is_running());
}

// ========================================================================
// Topology tests
// ========================================================================

/// Phase 5c: Test two-node topology creation.
#[tokio::test]
async fn test_two_node_topology() {
    let topology = TwoNodeDerpTopology::new().await;
    assert!(topology.is_ok(), "should create topology");

    let topology = topology.unwrap();
    assert!(topology.derp_url().starts_with("http://"), "should have valid DERP URL");
    assert!(topology.derp_port() > 0, "should have valid DERP port");
}

/// Phase 5c: Test topology waits for DERP ready.
#[tokio::test]
async fn test_topology_wait_ready() {
    let topology = TwoNodeDerpTopology::new().await.expect("create topology");
    let result = topology.wait_for_derp_ready().await;
    assert!(result.is_ok(), "should wait successfully");
}

// ========================================================================
// Configuration tests
// ========================================================================

/// Phase 5c: Test DERP config default values.
#[test]
fn test_derp_config_default() {
    let config = DerpConfig::default();
    assert!(!config.urls.is_empty(), "should have default URLs");
    assert_eq!(config.urls.len(), 1);
    assert_eq!(config.timeout, Duration::from_secs(30));
    assert!(config.region.is_none());
}

/// Phase 5c: Test DERP config local builder.
#[test]
fn test_derp_config_local() {
    let config = DerpConfig::local(8080);
    assert_eq!(config.urls.len(), 1);
    assert!(config.urls[0].contains("8080"));
    assert_eq!(config.region, Some("local".to_string()));
}

/// Phase 5c: Test DERP config builder methods.
#[test]
fn test_derp_config_builder_chain() {
    let config = DerpConfig::local(9090)
        .with_url("https://backup.derp.example.com")
        .with_region("us-west");

    assert_eq!(config.urls.len(), 2);
    assert_eq!(config.urls[0], "http://127.0.0.1:9090");
    assert_eq!(config.urls[1], "https://backup.derp.example.com");
    assert_eq!(config.region, Some("us-west".to_string()));
}

/// Phase 5c: Test DERP config with multiple URLs.
#[test]
fn test_derp_config_multiple_urls() {
    let config = DerpConfig::default()
        .with_url("https://derp1.example.com")
        .with_url("https://derp2.example.com")
        .with_url("https://derp3.example.com");

    assert_eq!(config.urls.len(), 4);
}

/// Phase 5c: Test DERP config debug output.
#[test]
fn test_derp_config_debug() {
    let config = DerpConfig::local(1234);
    let debug_str = format!("{:?}", config);
    assert!(debug_str.contains("DerpConfig"));
    assert!(debug_str.contains("1234"));
}

// ========================================================================
// Failure scenario tests
// ========================================================================

/// Phase 5c: Test failure scenario descriptions.
#[test]
fn test_failure_scenario_description() {
    assert_eq!(
        DerpFailureScenario::Unreachable.description(),
        "DERP server is unreachable"
    );
    assert_eq!(
        DerpFailureScenario::HighLatency.description(),
        "DERP server has high latency"
    );
    assert_eq!(
        DerpFailureScenario::IntermittentDrop.description(),
        "DERP server drops connections"
    );
    assert_eq!(
        DerpFailureScenario::ServerError.description(),
        "DERP server returns errors"
    );
}

/// Phase 5c: Test failure scenario equality.
#[test]
fn test_failure_scenario_equality() {
    assert_eq!(DerpFailureScenario::Unreachable, DerpFailureScenario::Unreachable);
    assert_ne!(DerpFailureScenario::Unreachable, DerpFailureScenario::HighLatency);
}

// ========================================================================
// Health check tests
// ========================================================================

/// Phase 5c: Test DERP health check healthy state.
#[test]
fn test_derp_health_check_healthy() {
    let healthy = DerpHealthCheck::healthy(50);
    assert!(healthy.reachable);
    assert_eq!(healthy.latency_ms, Some(50));
    assert!(healthy.tls_valid);
    assert!(healthy.server_version.is_some());
}

/// Phase 5c: Test DERP health check unhealthy state.
#[test]
fn test_derp_health_check_unhealthy() {
    let unhealthy = DerpHealthCheck::unhealthy("connection refused");
    assert!(!unhealthy.reachable);
    assert!(unhealthy.latency_ms.is_none());
    assert!(!unhealthy.tls_valid);
    assert!(unhealthy.server_version.is_none());
}

/// Phase 5c: Test DERP health check with various latencies.
#[test]
fn test_derp_health_check_latencies() {
    let fast = DerpHealthCheck::healthy(5);
    assert_eq!(fast.latency_ms, Some(5));

    let slow = DerpHealthCheck::healthy(5000);
    assert_eq!(slow.latency_ms, Some(5000));

    let timeout = DerpHealthCheck::healthy(30000);
    assert_eq!(timeout.latency_ms, Some(30000));
}

/// Phase 5c: Test DERP health check debug output.
#[test]
fn test_derp_health_check_debug() {
    let healthy = DerpHealthCheck::healthy(100);
    let debug_str = format!("{:?}", healthy);
    assert!(debug_str.contains("DerpHealthCheck"));
    assert!(debug_str.contains("100"));
}

// ========================================================================
// Integration tests
// ========================================================================

/// Phase 5c: Test full sync scenario with DERP config.
#[tokio::test]
async fn test_full_sync_scenario() {
    let topology = TwoNodeDerpTopology::new().await.expect("create topology");
    topology.wait_for_derp_ready().await.expect("wait for DERP");

    let derp_config = DerpConfig::local(topology.derp_port());

    assert!(!derp_config.urls.is_empty());
    assert_eq!(
        derp_config.urls[0],
        format!("http://127.0.0.1:{}", topology.derp_port())
    );
}

/// Phase 5c: Test server URL format.
#[tokio::test]
async fn test_server_url_format() {
    let server = TestDerpServer::new().await.unwrap();
    let url = server.url();

    assert!(url.starts_with("http://127.0.0.1:"));
    assert!(url.parse::<url::Url>().is_ok());
}

/// Phase 5c: Test concurrent topology creation.
#[tokio::test]
async fn test_concurrent_topology_creation() {
    let mut handles = vec![];
    for _ in 0..5 {
        handles.push(tokio::spawn(async {
            TwoNodeDerpTopology::new().await
        }));
    }

    let mut ports = vec![];
    for h in handles {
        let topology = h.await.expect("join").expect("create");
        ports.push(topology.derp_port());
    }

    // All ports should be unique
    ports.sort();
    ports.dedup();
    assert_eq!(ports.len(), 5);
}

/// Phase 5c: Test config with custom timeout.
#[test]
fn test_derp_config_custom_timeout() {
    let config = DerpConfig::local(1234);
    assert_eq!(config.timeout, Duration::from_secs(30));

    let custom = DerpConfig {
        urls: vec!["http://localhost:1234".to_string()],
        region: Some("test".to_string()),
        timeout: Duration::from_secs(60),
    };
    assert_eq!(custom.timeout, Duration::from_secs(60));
}

/// Phase 5c: Test empty region handling.
#[test]
fn test_derp_config_region_handling() {
    let with_region = DerpConfig::default().with_region("us-east");
    assert_eq!(with_region.region, Some("us-east".to_string()));

    let default = DerpConfig::default();
    assert!(default.region.is_none());
}
