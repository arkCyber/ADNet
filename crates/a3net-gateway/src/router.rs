//! Gateway router for request routing and path handling.
//!
//! This module provides the `GatewayRouter` which handles:
//! - Path normalization and resolution
//! - Request routing to appropriate handlers
//! - Gateway prefix mounting
//! - IPNS resolution coordination

use std::sync::Arc;

use a3net_blobstore::BlobStore;
use a3net_types::ContentHash;

use crate::config::GatewayConfig;
use crate::dag::DagService;
use crate::dht::DhtService;
use crate::handler::{GatewayHandler, GatewayError, IpfsPath};
use crate::ipns::IpnService;
use crate::pin::PinService;

/// Gateway router for handling IPFS-style paths.
#[derive(Clone)]
pub struct GatewayRouter {
    config: Arc<GatewayConfig>,
    blob_store: Arc<BlobStore>,
    dag_service: Arc<DagService>,
    pin_service: Arc<PinService>,
    dht_service: Arc<DhtService>,
    ipns_service: Arc<IpnService>,
}

impl GatewayRouter {
    /// Create a new gateway router.
    pub fn new(
        config: GatewayConfig,
        blob_store: Arc<BlobStore>,
        dag_service: Arc<DagService>,
        pin_service: Arc<PinService>,
        dht_service: Arc<DhtService>,
        ipns_service: Arc<IpnService>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            blob_store,
            dag_service,
            pin_service,
            dht_service,
            ipns_service,
        }
    }

    /// Get the handler for this router.
    pub fn handler(&self) -> GatewayHandler {
        GatewayHandler::new(
            self.config.as_ref().clone(),
            self.blob_store.clone(),
            self.dag_service.clone(),
            self.pin_service.clone(),
            self.dht_service.clone(),
            self.ipns_service.clone(),
        )
    }

    /// Get the route prefix.
    pub fn route_prefix(&self) -> &str {
        &self.config.route_prefix
    }

    /// Start the gateway HTTP server on the configured bind address.
    ///
    /// This delegates to [`crate::handler::start_gateway`] which is the
    /// long-running HTTP loop. It blocks until the process is killed
    /// (or an I/O error occurs on the listener).
    pub async fn serve(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        crate::handler::start_gateway(
            self.config.as_ref().clone(),
            self.blob_store.clone(),
            self.dag_service.clone(),
            self.pin_service.clone(),
            self.dht_service.clone(),
            self.ipns_service.clone(),
        )
        .await
    }

    /// Normalize a gateway path by applying the configured prefix.
    pub fn normalize_path(&self, path: &str) -> String {
        let path = path.trim_end_matches('/');

        // If path already has the gateway prefix, return as-is
        if path.starts_with(&self.config.route_prefix) {
            return path.to_string();
        }

        // If path starts with /ipfs or /ipns, it's already normalized
        if path.starts_with("/ipfs/") || path.starts_with("/ipns/") {
            return path.to_string();
        }

        // Apply the route prefix
        if self.config.route_prefix.is_empty() {
            format!("/ipfs/{}", path)
        } else {
            format!("{}/{}", self.config.route_prefix.trim_end_matches('/'), path)
        }
    }

    /// Resolve an IPFS path to a content hash.
    pub async fn resolve_path(&self, path: &str) -> Result<ResolvedPath, GatewayError> {
        let parsed = IpfsPath::parse(path)?;

        if parsed.is_ipns {
            // Stub: IPNS resolution not available
            return Err(GatewayError::InvalidPath(
                "IPNS resolution not available".to_string()
            ));
        }

        let hash = parsed.to_content_hash()?;

        // Check if content exists
        if !self.blob_store.has_complete(&hash) {
            return Err(GatewayError::NotFound(hash.as_hex().to_string()));
        }

        // Clone hash before borrowing for is_directory check
        let hash_for_dir_check = hash.clone();
        let is_directory = self.dag_service.is_directory(&hash_for_dir_check).await.unwrap_or(false);

        Ok(ResolvedPath {
            hash,
            segments: parsed.segments,
            is_directory,
        })
    }

    /// Check if a content hash exists in the store.
    pub fn has_content(&self, hash: &ContentHash) -> bool {
        self.blob_store.has_complete(hash)
    }

    /// Get content metadata.
    pub fn content_meta(&self, hash: &ContentHash) -> Option<ContentMeta> {
        let hash_clone = hash.clone();
        self.blob_store.meta(&hash_clone).ok().map(|(size, chunks)| ContentMeta {
            hash: hash_clone,
            size_bytes: size,
            chunk_count: chunks,
        })
    }

    /// List pins for a content hash.
    pub async fn list_pins(&self, filter: Option<&ContentHash>) -> Vec<crate::pin::PinInfo> {
        self.pin_service.list_pins(filter).await
    }

    /// Check if content is pinned.
    pub async fn is_pinned(&self, hash: &ContentHash) -> bool {
        self.pin_service.is_pinned(hash).await
    }

    /// Get the DAG links for a content hash.
    pub async fn get_links(&self, hash: &ContentHash) -> Result<Vec<crate::dag::DagLink>, GatewayError> {
        self.dag_service.list_links(hash).await
            .map_err(|e| GatewayError::Internal(e.to_string()))
    }
}

/// Result of resolving an IPFS path.
#[derive(Debug, Clone)]
pub struct ResolvedPath {
    /// The content hash.
    pub hash: ContentHash,
    /// Remaining path segments.
    pub segments: Vec<String>,
    /// Whether the content is a directory.
    pub is_directory: bool,
}

/// Content metadata.
#[derive(Debug, Clone)]
pub struct ContentMeta {
    /// The content hash.
    pub hash: ContentHash,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Number of chunks.
    pub chunk_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_router() -> GatewayRouter {
        let config = GatewayConfig::default();
        let temp_dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            a3net_blobstore::BlobStore::new(temp_dir.path()).unwrap()
        );
        let dag_service = Arc::new(DagService::new(blob_store.clone()));
        let pin_service = Arc::new(PinService::new(
            blob_store.clone(),
            temp_dir.path().to_path_buf()
        ));
        let dht_service = Arc::new(DhtService::new("local".to_string(), vec![]));
        let ipns_service = Arc::new(IpnService::new(
            blob_store.clone(),
            temp_dir.path().to_path_buf(),
            None,
        ));

        GatewayRouter::new(
            config,
            blob_store,
            dag_service,
            pin_service,
            dht_service,
            ipns_service,
        )
    }

    #[test]
    fn test_normalize_path() {
        let router = create_test_router();
        
        // Test path normalization
        assert_eq!(router.normalize_path("/ipfs/QmHash"), "/ipfs/QmHash");
        assert_eq!(router.normalize_path("/ipns/example.com"), "/ipns/example.com");
        assert_eq!(router.normalize_path("QmHash"), "/ipfs/QmHash");
    }
}
