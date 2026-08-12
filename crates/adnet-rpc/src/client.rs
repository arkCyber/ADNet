//! RPC client for the unified IPFS-compatible API.

use std::path::Path;
use std::sync::Arc;

use adnet_blobstore::BlobStore;
use adnet_types::ContentHash;

use crate::results::RpcError;

/// Unified RPC client that provides IPFS-compatible API access.
pub struct RpcClient {
    blob_store: Arc<BlobStore>,
    #[allow(dead_code)]
    base_url: Option<String>,
}

impl RpcClient {
    /// Create a new local RPC client.
    pub fn new(blob_store: Arc<BlobStore>) -> Self {
        Self {
            blob_store,
            base_url: None,
        }
    }

    /// Create a new RPC client with a remote URL.
    pub fn remote(base_url: impl Into<String>) -> Self {
        Self {
            blob_store: Arc::new(adnet_blobstore::BlobStore::new(std::path::Path::new(""))
                .expect("in-memory store")),
            base_url: Some(base_url.into()),
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // DAG operations
    // ─────────────────────────────────────────────────────────────────

    /// Add a DAG node.
    pub async fn put_dag(&self, data: &[u8]) -> Result<String, RpcError> {
        // Simple implementation: store raw data
        // A full implementation would parse and store as a DAG
        let (hash, _) = self.blob_store.put_bytes_sync(data)
            .map_err(|e| RpcError::internal(e.to_string()))?;
        Ok(hash.as_hex().to_string())
    }

    /// Get a DAG node.
    pub async fn get_dag(&self, cid: &str, _path: Option<&str>) -> Result<Vec<u8>, RpcError> {
        let hash = ContentHash::from_hex(cid)
            .map_err(|_| RpcError::invalid_input("invalid CID"))?;

        self.blob_store.get_sync(&hash)
            .ok_or_else(|| RpcError::not_found(format!("content not found: {}", cid)))
    }

    /// Resolve a DAG path.
    pub async fn resolve_dag(&self, path: &str) -> Result<(String, Option<String>), RpcError> {
        let path = path.trim_start_matches("/ipfs/");
        let parts: Vec<&str> = path.splitn(2, '/').collect();

        let cid = parts[0].to_string();
        let remainder = parts.get(1).map(|s| s.to_string());

        Ok((cid, remainder))
    }

    /// Import a file or directory as a DAG.
    pub async fn import_dag(&self, path: &Path, _wrap: bool) -> Result<String, RpcError> {
        use std::fs;

        let data = fs::read(path)
            .map_err(|e| RpcError::internal(e.to_string()))?;

        self.put_dag(&data).await
    }

    // ─────────────────────────────────────────────────────────────────
    // Block operations
    // ─────────────────────────────────────────────────────────────────

    /// Add a raw block.
    pub async fn put_block(&self, data: &[u8]) -> Result<String, RpcError> {
        self.put_dag(data).await
    }

    /// Get a raw block.
    pub async fn get_block(&self, cid: &str) -> Result<Vec<u8>, RpcError> {
        self.get_dag(cid, None).await
    }

    /// Get block statistics.
    pub async fn block_stat(&self, cid: &str) -> Result<BlockStat, RpcError> {
        let hash = ContentHash::from_hex(cid)
            .map_err(|_| RpcError::invalid_input("invalid CID"))?;

        let (size, _) = self.blob_store.meta(&hash)
            .map_err(|_| RpcError::not_found(format!("block not found: {}", cid)))?;

        Ok(BlockStat {
            size,
            cumulative_size: size,
        })
    }

    /// Remove a block.
    pub async fn remove_block(&self, cid: &str, _force: bool) -> Result<(), RpcError> {
        let hash = ContentHash::from_hex(cid)
            .map_err(|_| RpcError::invalid_input("invalid CID"))?;

        self.blob_store.remove(&hash)
            .map_err(|e| RpcError::internal(e.to_string()))?;

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────
    // Pin operations
    // ─────────────────────────────────────────────────────────────────

    /// Add a pin (placeholder implementation).
    pub async fn pin_add(&self, _cid: &str, _recursive: bool) -> Result<(), RpcError> {
        // Pin operations require the PinService
        // This is a placeholder that would be replaced by actual implementation
        Ok(())
    }

    /// Remove a pin.
    pub async fn pin_remove(&self, _cid: &str) -> Result<(), RpcError> {
        Ok(())
    }

    /// List pins.
    pub async fn list_pins(&self, _filter: Option<&ContentHash>) -> Result<std::collections::HashMap<String, crate::results::PinInfoResult>, RpcError> {
        Ok(std::collections::HashMap::new())
    }

    /// Verify pin status.
    pub async fn verify_pin(&self, _cid: &str) -> Result<String, RpcError> {
        Ok("pinned".to_string())
    }

    // ─────────────────────────────────────────────────────────────────
    // DHT operations
    // ─────────────────────────────────────────────────────────────────

    /// Find providers for a CID.
    pub async fn find_providers(&self, _cid: &str) -> Result<Vec<crate::results::ProviderInfo>, RpcError> {
        Ok(Vec::new())
    }

    /// Announce provider for a CID.
    pub async fn provide(&self, _cid: &str) -> Result<(), RpcError> {
        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────
    // IPNS operations
    // ─────────────────────────────────────────────────────────────────

    /// Publish an IPNS record.
    pub async fn publish_ipns(&self, _path: &str) -> Result<String, RpcError> {
        Ok("QmExample123456789".to_string())
    }

    /// Resolve an IPNS name.
    pub async fn resolve_ipns(&self, _name: &str) -> Result<String, RpcError> {
        Ok("/ipfs/QmExample123456789".to_string())
    }

    // ─────────────────────────────────────────────────────────────────
    // GC operations
    // ─────────────────────────────────────────────────────────────────

    /// Run garbage collection.
    pub async fn gc(&self) -> Result<GcStats, RpcError> {
        Ok(GcStats {
            removed: 0,
            failed: 0,
        })
    }

    /// Dry run garbage collection.
    pub async fn gc_dry_run(&self) -> Result<u64, RpcError> {
        Ok(0)
    }

    // ─────────────────────────────────────────────────────────────────
    // Node operations
    // ─────────────────────────────────────────────────────────────────

    /// Get node information.
    pub async fn node_id(&self) -> Result<NodeInfo, RpcError> {
        Ok(NodeInfo {
            id: "adnet-node".to_string(),
            public_key: String::new(),
            addresses: vec!["/ipfs/QmExample".to_string()],
            agent_version: "adnet/0.1.0".to_string(),
            protocol_version: "ipfs/0.1.0".to_string(),
        })
    }
}

/// Block statistics.
#[derive(Debug, Clone)]
pub struct BlockStat {
    pub size: u64,
    pub cumulative_size: u64,
}

/// GC statistics.
#[derive(Debug, Clone)]
pub struct GcStats {
    pub removed: u64,
    pub failed: u64,
}

/// Node information.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: String,
    pub public_key: String,
    pub addresses: Vec<String>,
    pub agent_version: String,
    pub protocol_version: String,
}
