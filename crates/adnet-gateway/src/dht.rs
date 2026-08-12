//! DHT API for IPFS-compatible DHT operations.
//!
//! This module provides IPFS DHT operations including:
//! - Finding providers for content
//! - Announcing provider records
//! - DHT statistics

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adnet_blobstore::BlobStore;
use adnet_types::ContentHash;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// DHT find providers result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindProvsResult {
    #[serde(rename = "Cid")]
    pub cid: String,
    #[serde(rename = "Providers")]
    pub providers: Vec<ProviderInfo>,
}

/// Provider information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Addrs")]
    pub addrs: Vec<String>,
}

/// DHT provide result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvideResult {
    #[serde(rename = "Cid")]
    pub cid: String,
}

/// DHT find nodes result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindNodesResult {
    #[serde(rename = "Nodes")]
    pub nodes: Vec<NodeInfo>,
}

/// Node information in DHT response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Addrs")]
    pub addrs: Vec<String>,
}

/// DHT API errors.
#[derive(Debug, thiserror::Error)]
pub enum DhtError {
    #[error("peer not connected: {0}")]
    PeerNotConnected(String),

    #[error("query timeout")]
    QueryTimeout,

    #[error("invalid CID: {0}")]
    InvalidCid(String),

    #[error("internal error: {0}")]
    Internal(String),
}

/// Provider record with expiration.
#[derive(Debug, Clone)]
struct ProviderEntry {
    peer_id: String,
    addrs: Vec<String>,
    expires_at: std::time::Instant,
}

/// DHT service for gateway integration with local provider storage.
#[derive(Clone)]
pub struct DhtService {
    /// Local node identifier.
    local_id: String,
    /// Local provider announcements: CID -> Vec<ProviderEntry>
    providers: Arc<RwLock<HashMap<String, Vec<ProviderEntry>>>>,
    /// Provider TTL.
    provider_ttl: Duration,
    /// Blob store for checking content availability.
    #[allow(dead_code)]
    blob_store: Option<Arc<BlobStore>>,
}

impl DhtService {
    /// Create a new DHT service.
    pub fn new(local_id: String, _bootstrap_nodes: Vec<String>) -> Self {
        Self {
            local_id,
            providers: Arc::new(RwLock::new(HashMap::new())),
            provider_ttl: Duration::from_secs(86400), // 24 hours
            blob_store: None,
        }
    }

    /// Create with blob store for content availability checks.
    #[allow(dead_code)]
    pub fn with_blob_store(mut self, blob_store: Arc<BlobStore>) -> Self {
        self.blob_store = Some(blob_store);
        self
    }

    /// Set provider TTL.
    #[allow(dead_code)]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.provider_ttl = ttl;
        self
    }

    /// Find providers for a content CID.
    pub async fn find_providers(&self, cid: &str) -> Result<FindProvsResult, DhtError> {
        // Validate CID format
        let content_hash = ContentHash::from_hex(cid)
            .map_err(|_| DhtError::InvalidCid(cid.to_string()))?;

        let providers = self.get_providers_for_cid(&content_hash).await;
        
        Ok(FindProvsResult {
            cid: cid.to_string(),
            providers,
        })
    }

    /// Announce provider for a content CID.
    pub async fn provide(&self, cid: &str) -> Result<ProvideResult, DhtError> {
        // Validate CID format
        let content_hash = ContentHash::from_hex(cid)
            .map_err(|_| DhtError::InvalidCid(cid.to_string()))?;

        // Add local provider entry
        self.add_provider(content_hash, self.local_id.clone(), vec![]).await;

        Ok(ProvideResult {
            cid: cid.to_string(),
        })
    }

    /// Find closest nodes to a key.
    #[allow(dead_code)]
    pub async fn find_nodes(&self, _key: &str) -> Result<FindNodesResult, DhtError> {
        // TODO: Implement Kademlia find_nodes
        Ok(FindNodesResult {
            nodes: Vec::new(),
        })
    }

    /// Get DHT peer statistics.
    #[allow(dead_code)]
    pub async fn get_peers(&self) -> Result<u32, DhtError> {
        Ok(0)
    }

    /// Get local provider records.
    pub async fn list_local_providers(&self) -> Vec<(String, Vec<ProviderInfo>)> {
        let providers = self.providers.read().await;
        providers.iter()
            .map(|(cid, entries)| {
                let infos: Vec<ProviderInfo> = entries.iter().map(|e| ProviderInfo {
                    id: e.peer_id.clone(),
                    addrs: e.addrs.clone(),
                }).collect();
                (cid.clone(), infos)
            })
            .collect()
    }

    /// Internal: Add a provider entry.
    async fn add_provider(&self, cid: ContentHash, peer_id: String, addrs: Vec<String>) {
        let entry = ProviderEntry {
            peer_id,
            addrs,
            expires_at: std::time::Instant::now() + self.provider_ttl,
        };
        
        let mut providers = self.providers.write().await;
        providers.entry(cid.as_hex().to_string()).or_insert_with(Vec::new).push(entry);
    }

    /// Internal: Get providers for a CID.
    async fn get_providers_for_cid(&self, cid: &ContentHash) -> Vec<ProviderInfo> {
        let providers = self.providers.read().await;
        let now = std::time::Instant::now();
        
        providers.get(&cid.as_hex().to_string())
            .map(|entries| {
                entries.iter()
                    .filter(|e| e.expires_at > now)
                    .map(|e| ProviderInfo {
                        id: e.peer_id.clone(),
                        addrs: e.addrs.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Valid BLAKE3 hash (64 hex characters)
    const TEST_CID: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    #[tokio::test]
    async fn test_find_providers_empty() {
        let service = DhtService::new(
            "test-node".to_string(),
            vec![],
        );
        let result = service.find_providers(TEST_CID).await.unwrap();
        assert_eq!(result.cid, TEST_CID);
        assert!(result.providers.is_empty());
    }

    #[tokio::test]
    async fn test_provide() {
        let service = DhtService::new(
            "test-node".to_string(),
            vec![],
        );
        let result = service.provide(TEST_CID).await.unwrap();
        assert_eq!(result.cid, TEST_CID);
    }

    #[tokio::test]
    async fn test_local_providers() {
        let service = DhtService::new(
            "test-node".to_string(),
            vec![],
        );
        service.provide(TEST_CID).await.unwrap();
        
        let providers = service.list_local_providers().await;
        assert!(!providers.is_empty());
    }
}
