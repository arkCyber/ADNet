//! Integration tests for the a3net-chain crate.
//!
//! These tests verify:
//! - Configuration parsing and validation
//! - Node lifecycle (start/stop)
//! - Status transitions
//! - Error handling
//! - Backend trait implementations (mock)

use a3net_chain::{
    ChainError, ChainKind, ChainNode, ChainNodeConfig,
    ChainNodeHandle, ChainRole, ChainStatus,
};
use a3net_chain::types::{ChainBackend, ChainBackendResult};
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};

/// Mock backend for testing the ChainBackend trait.
struct MockBackend {
    running: AtomicBool,
    block_height: u64,
    chain_id: u64,
}

impl MockBackend {
    fn new(block_height: u64, chain_id: u64) -> Self {
        Self {
            running: AtomicBool::new(false),
            block_height,
            chain_id,
        }
    }
}

#[async_trait]
impl ChainBackend for MockBackend {
    fn chain_kind(&self) -> ChainKind {
        ChainKind::Evm
    }

    fn role(&self) -> ChainRole {
        ChainRole::FullNode
    }

    async fn start(&mut self) -> ChainBackendResult<()> {
        self.running.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&mut self) -> ChainBackendResult<()> {
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn status(&self) -> ChainStatus {
        if self.running.load(Ordering::SeqCst) {
            ChainStatus::Synced
        } else {
            ChainStatus::Stopped
        }
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    async fn get_block(&self, height: u64) -> ChainBackendResult<Vec<u8>> {
        if height > self.block_height {
            return Err(ChainError::NotConfigured(format!(
                "Block {} not found, current height is {}",
                height, self.block_height
            )));
        }
        Ok(vec![0u8; 32]) // Mock block data
    }

    async fn get_block_height(&self) -> ChainBackendResult<u64> {
        Ok(self.block_height)
    }

    async fn submit_transaction(&self, _tx_bytes: &[u8]) -> ChainBackendResult<String> {
        if !self.running.load(Ordering::SeqCst) {
            return Err(ChainError::NotConfigured("Backend not running".to_string()));
        }
        Ok(format!("0x{:064x}", 12345u64))
    }

    async fn get_transaction_receipt(&self, _tx_hash: &str) -> ChainBackendResult<Option<Vec<u8>>> {
        if !self.running.load(Ordering::SeqCst) {
            return Err(ChainError::NotConfigured("Backend not running".to_string()));
        }
        Ok(Some(vec![0u8; 64])) // Mock receipt
    }

    async fn get_head_block_hash(&self) -> ChainBackendResult<Vec<u8>> {
        if !self.running.load(Ordering::SeqCst) {
            return Err(ChainError::NotConfigured("Backend not running".to_string()));
        }
        Ok(vec![0u8; 32]) // Mock block hash
    }

    fn chain_id(&self) -> u64 {
        self.chain_id
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Configuration Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_default_config_is_disabled() {
    let config = ChainNodeConfig::default();
    assert!(!config.enabled);
    assert_eq!(config.kind, ChainKind::None);
    assert_eq!(config.role, ChainRole::Observer);
    assert_eq!(config.data_subdir, "chain");
    assert!(config.bind.is_none());
}

#[test]
fn test_enabled_config() {
    let config = ChainNodeConfig::enabled(ChainKind::Evm, ChainRole::Validator);
    assert!(config.enabled);
    assert_eq!(config.kind, ChainKind::Evm);
    assert_eq!(config.role, ChainRole::Validator);
}

#[test]
fn test_config_serialization() {
    let config = ChainNodeConfig::enabled(ChainKind::Substrate, ChainRole::FullNode);
    let json = serde_json::to_string(&config).unwrap();
    let parsed: ChainNodeConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.enabled, config.enabled);
    assert_eq!(parsed.kind, config.kind);
    assert_eq!(parsed.role, config.role);
}

#[test]
fn test_chain_kind_display() {
    assert_eq!(ChainKind::None.to_string(), "none");
    assert_eq!(ChainKind::Evm.to_string(), "evm");
    assert_eq!(ChainKind::Substrate.to_string(), "substrate");
    assert_eq!(ChainKind::Custom("bitcoin".to_string()).to_string(), "bitcoin");
}

#[test]
fn test_chain_kind_default() {
    let kind = ChainKind::default();
    assert_eq!(kind, ChainKind::None);
}

#[test]
fn test_chain_role_default() {
    let role = ChainRole::default();
    assert_eq!(role, ChainRole::Observer);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Node Lifecycle Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_disabled_node_returns_none() {
    let config = ChainNodeConfig::default();
    let node = ChainNode::new(config);
    let handle = node.start().await.unwrap();
    assert!(handle.is_none());
}

#[tokio::test]
async fn test_enabled_node_returns_preview_handle() {
    let config = ChainNodeConfig::enabled(ChainKind::Evm, ChainRole::Observer);
    let node = ChainNode::new(config);
    let handle = node.start().await.unwrap();
    assert!(handle.is_some());
    let handle = handle.unwrap();
    assert_eq!(handle.status(), ChainStatus::Stopped);
}

#[tokio::test]
async fn test_node_with_mock_backend() {
    let config = ChainNodeConfig::enabled(ChainKind::Evm, ChainRole::FullNode);
    let backend = Box::new(MockBackend::new(100, 1));
    let node = ChainNode::with_backend(config, backend);
    let handle = node.start().await.unwrap();
    assert!(handle.is_some());
    let handle = handle.unwrap();
    assert_eq!(handle.status(), ChainStatus::Syncing);
    assert_eq!(handle.chain_id(), Some(1));
}

#[tokio::test]
async fn test_handle_shutdown() {
    let config = ChainNodeConfig::enabled(ChainKind::Evm, ChainRole::Validator);
    let backend = Box::new(MockBackend::new(0, 137));
    let node = ChainNode::with_backend(config, backend);
    let handle = node.start().await.unwrap().unwrap();
    assert!(matches!(handle.status(), ChainStatus::Syncing | ChainStatus::Stopped));
    handle.shutdown();
    assert_eq!(handle.status(), ChainStatus::Stopped);
}

#[tokio::test]
async fn test_preview_handle_get_block_height_fails() {
    let handle = ChainNodeHandle::preview();
    let result = handle.get_block_height().await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, ChainError::NotConfigured(_)));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Backend Trait Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_mock_backend_lifecycle() {
    let mut backend = MockBackend::new(50, 42161);

    // Initially not running
    assert!(!backend.is_running());
    assert_eq!(backend.status().await, ChainStatus::Stopped);

    // Start
    backend.start().await.unwrap();
    assert!(backend.is_running());
    assert_eq!(backend.status().await, ChainStatus::Synced);

    // Stop
    backend.stop().await.unwrap();
    assert!(!backend.is_running());
    assert_eq!(backend.status().await, ChainStatus::Stopped);
}

#[tokio::test]
async fn test_mock_backend_block_operations() {
    let backend = MockBackend::new(100, 1);

    // Get block height
    let height = backend.get_block_height().await.unwrap();
    assert_eq!(height, 100);

    // Get block within range
    let block = backend.get_block(50).await.unwrap();
    assert_eq!(block.len(), 32);

    // Get block out of range
    let result = backend.get_block(200).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_mock_backend_chain_id() {
    let backend = MockBackend::new(0, 137);
    assert_eq!(backend.chain_id(), 137);
}

#[tokio::test]
async fn test_mock_backend_chain_kind() {
    let backend = MockBackend::new(0, 1);
    assert_eq!(backend.chain_kind(), ChainKind::Evm);
    assert_eq!(backend.role(), ChainRole::FullNode);
}

#[tokio::test]
async fn test_mock_backend_transaction_while_running() {
    let mut backend = MockBackend::new(0, 1);
    backend.start().await.unwrap();

    let tx_result = backend.submit_transaction(&[0u8; 32]).await;
    assert!(tx_result.is_ok());
    let tx_hash = tx_result.unwrap();
    assert!(tx_hash.starts_with("0x"));
    assert_eq!(tx_hash.len(), 66); // 0x + 64 hex chars
}

#[tokio::test]
async fn test_mock_backend_transaction_while_stopped() {
    let backend = MockBackend::new(0, 1);
    // Don't start the backend

    let tx_result = backend.submit_transaction(&[0u8; 32]).await;
    assert!(tx_result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Handle Clone Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_preview_handle_is_cloneable() {
    let handle = ChainNodeHandle::preview();
    let handle2 = handle.clone();
    assert_eq!(handle.status(), handle2.status());
    assert_eq!(handle.chain_id(), handle2.chain_id());
}

#[test]
fn test_preview_handle_debug() {
    let handle = ChainNodeHandle::preview();
    let debug_str = format!("{:?}", handle);
    assert!(debug_str.contains("ChainNodeHandle"));
}

#[test]
fn test_node_debug() {
    let config = ChainNodeConfig::enabled(ChainKind::Evm, ChainRole::Observer);
    let node = ChainNode::new(config);
    let debug_str = format!("{:?}", node);
    assert!(debug_str.contains("ChainNode"));
}

#[test]
fn test_config_debug() {
    let config = ChainNodeConfig::enabled(ChainKind::Substrate, ChainRole::Validator);
    let debug_str = format!("{:?}", config);
    assert!(debug_str.contains("ChainNodeConfig"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Status Transition Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_chain_status_variants() {
    // All status variants should be constructable and debug-formattable
    let statuses = [
        ChainStatus::Stopped,
        ChainStatus::Starting,
        ChainStatus::Syncing,
        ChainStatus::Synced,
        ChainStatus::Error,
    ];

    for status in statuses {
        let debug = format!("{:?}", status);
        assert!(!debug.is_empty());
    }
}

#[test]
fn test_chain_role_variants() {
    let roles = [
        ChainRole::Observer,
        ChainRole::FullNode,
        ChainRole::Validator,
    ];

    for role in roles {
        let debug = format!("{:?}", role);
        assert!(!debug.is_empty());
        let json = serde_json::to_string(&role).unwrap();
        assert!(!json.is_empty());
    }
}

#[test]
fn test_chain_kind_variants() {
    let kinds = [
        ChainKind::None,
        ChainKind::Evm,
        ChainKind::Substrate,
        ChainKind::Custom("test".to_string()),
    ];

    for kind in kinds {
        let debug = format!("{:?}", kind);
        assert!(!debug.is_empty());
        let display = format!("{}", kind);
        assert!(!display.is_empty());
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Error Handling Tests
// ═══════════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_chain_error_display() {
        let errors = [
            ChainError::Unimplemented("test"),
            ChainError::NotConfigured("not configured".to_string()),
            ChainError::UnsupportedChain("ethereum".to_string()),
            ChainError::AlreadyRunning,
            ChainError::NotRunning,
        ];

        for err in errors {
            let display = format!("{}", err);
            assert!(!display.is_empty());
        }
    }

#[tokio::test]
async fn test_get_block_height_on_preview_handle() {
    let handle = ChainNodeHandle::preview();
    let result = handle.get_block_height().await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    let display = format!("{}", err);
    assert!(display.contains("No chain backend") || display.contains("configured"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// JSON Serialization Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_chain_kind_serialization() {
    let kinds = vec![
        ChainKind::None,
        ChainKind::Evm,
        ChainKind::Substrate,
        ChainKind::Custom("custom".to_string()),
    ];

    for kind in kinds {
        let json = serde_json::to_string(&kind).unwrap();
        // Verify JSON is valid and contains expected string representation
        assert!(!json.is_empty());
        // Verify we can parse it back
        let parsed: ChainKind = serde_json::from_str(&json).unwrap();
        // Verify kind matches (Custom requires string comparison)
        match (&kind, &parsed) {
            (ChainKind::Custom(s1), ChainKind::Custom(s2)) => assert_eq!(s1, s2),
            _ => assert_eq!(kind, parsed),
        }
    }
}

#[test]
fn test_chain_role_serialization() {
    let roles = vec![
        (ChainRole::Observer, "observer"),
        (ChainRole::FullNode, "full_node"),
        (ChainRole::Validator, "validator"),
    ];

    for (role, expected) in roles {
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, format!("\"{}\"", expected));
        let parsed: ChainRole = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, role);
    }
}

#[test]
fn test_chain_status_serialization() {
    let statuses = vec![
        ChainStatus::Stopped,
        ChainStatus::Starting,
        ChainStatus::Syncing,
        ChainStatus::Synced,
        ChainStatus::Error,
    ];

    for status in statuses {
        let json = serde_json::to_string(&status).unwrap();
        let parsed: ChainStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, status);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Config Roundtrip Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_config_with_all_fields() {
    let config = ChainNodeConfig {
        enabled: true,
        kind: ChainKind::Evm,
        role: ChainRole::Validator,
        data_subdir: "custom_chain_dir".to_string(),
        bind: Some("0.0.0.0:8545".parse().unwrap()),
    };

    let json = serde_json::to_string(&config).unwrap();
    let parsed: ChainNodeConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.enabled, config.enabled);
    assert_eq!(parsed.kind, config.kind);
    assert_eq!(parsed.role, config.role);
    assert_eq!(parsed.data_subdir, config.data_subdir);
    assert_eq!(parsed.bind, config.bind);
}
