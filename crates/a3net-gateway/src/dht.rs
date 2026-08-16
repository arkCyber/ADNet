//! DHT API for IPFS-compatible DHT operations.
//!
//! This module provides IPFS DHT operations including:
//! - Finding providers for content
//! - Finding closest nodes (Kademlia FIND_NODE)
//! - Announcing provider records
//! - DHT statistics

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use a3net_blobstore::BlobStore;
use a3net_types::ContentHash;
use a3net_dht::bucket::RoutingTable;
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
    /// Kademlia routing table for find_nodes queries.
    routing_table: Option<Arc<RwLock<RoutingTable>>>,
    /// Bootstrap nodes for initial peer discovery.
    bootstrap_nodes: Vec<String>,
}

impl DhtService {
    /// Create a new DHT service.
    pub fn new(local_id: String, bootstrap_nodes: Vec<String>) -> Self {
        Self {
            local_id,
            providers: Arc::new(RwLock::new(HashMap::new())),
            provider_ttl: Duration::from_secs(86400), // 24 hours
            blob_store: None,
            routing_table: None,
            bootstrap_nodes,
        }
    }

    /// Create with routing table for Kademlia queries.
    pub fn with_routing_table(mut self, routing_table: Arc<RwLock<RoutingTable>>) -> Self {
        self.routing_table = Some(routing_table);
        self
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

    /// Get the number of peers in the routing table.
    pub async fn num_peers(&self) -> usize {
        if let Some(ref rt) = self.routing_table {
            let rt = rt.read().await;
            rt.num_contacts()
        } else {
            0
        }
    }

    /// Add a peer to the routing table.
    pub async fn add_peer(&self, peer_id: &str, addr: &str) -> Result<(), DhtError> {
        if let Some(ref rt) = self.routing_table {
            let node_id: a3net_types::NodeId = peer_id.parse()
                .map_err(|_| DhtError::Internal(format!("Invalid peer ID: {}", peer_id)))?;
            let socket_addr: std::net::SocketAddr = addr.parse()
                .map_err(|_| DhtError::Internal(format!("Invalid address: {}", addr)))?;
            
            let contact = a3net_dht::Contact::new(node_id, socket_addr);
            let mut rt = rt.write().await;
            rt.insert(contact)
                .map_err(|e| DhtError::Internal(format!("Failed to add peer: {:?}", e)))?;
            Ok(())
        } else {
            Err(DhtError::Internal("Routing table not initialized".to_string()))
        }
    }

    /// Find closest nodes to a key using Kademlia routing.
    pub async fn find_nodes(&self, key: &str) -> Result<FindNodesResult, DhtError> {
        // Parse the key as a node ID target
        let target: a3net_types::NodeId = key.parse()
            .unwrap_or_else(|_: <a3net_types::NodeId as std::str::FromStr>::Err| {
                // If not a valid node ID, derive one from the key bytes
                a3net_dht::node_id_from_key_str(key)
            });

        if let Some(ref rt) = self.routing_table {
            let rt = rt.read().await;
            // Get K closest peers from the routing table
            let contacts = rt.closest(&target, 20);
            
            let nodes: Vec<NodeInfo> = contacts.into_iter().map(|c| {
                NodeInfo {
                    id: c.id.to_string(),
                    addrs: c.addrs.iter().map(|a| a.to_string()).collect(),
                }
            }).collect();

            Ok(FindNodesResult { nodes })
        } else {
            // No routing table - return bootstrap nodes as candidates
            let nodes: Vec<NodeInfo> = self.bootstrap_nodes.iter()
                .filter_map(|addr| {
                    // Try to parse as node_id@addr format or just addr
                    if let Some((peer_id, peer_addr)) = addr.split_once('@') {
                        Some(NodeInfo {
                            id: peer_id.to_string(),
                            addrs: vec![peer_addr.to_string()],
                        })
                    } else {
                        // Just use the address as a bootstrap node
                        None
                    }
                })
                .collect();

            if nodes.is_empty() {
                // Return empty result with info
                tracing::debug!("find_nodes called but no routing table or bootstrap nodes available");
                Ok(FindNodesResult { nodes: Vec::new() })
            } else {
                Ok(FindNodesResult { nodes })
            }
        }
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
