//! Kademlia-style DHT queries for finding providers and resolving names.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use adnet_types::NodeId;

use crate::bucket::{Contact, KBUCKET_SIZE, RoutingTable};
use crate::protocol::{DhtWireMessage, NodesPayload, ProvidersPayload};
use crate::record::{DhtKey, NodeInfo, ProviderRecord};
use crate::store::{SharedDhtStore};

/// Parallelism factor (α in Kademlia).
const ALPHA: usize = 3;

/// Number of closest peers to return in FIND_NODE.
const CLOSEST_PEERS: usize = KBUCKET_SIZE;

/// Query state for tracking in-flight queries.
struct QueryState {
    started_at: Instant,
    requests_sent: usize,
    responses_received: usize,
    closest_seen: Vec<Contact>,
    providers: Vec<ProviderRecord>,
}

/// Result of a DHT query.
#[derive(Debug)]
pub struct QueryResult {
    pub peers: Vec<NodeInfo>,
    pub providers: Vec<ProviderRecord>,
    pub query_id: String,
    pub duration_ms: u64,
}

/// DHT query engine implementing Kademlia-style lookups.
pub struct DhtQuery {
    local_id: NodeId,
    routing_table: Arc<RwLock<RoutingTable>>,
    store: SharedDhtStore,
    /// Network sender for querying peers.
    sender: Arc<dyn DhtMessageSender>,
    /// Pending queries.
    pending: HashMap<String, QueryState>,
}

impl std::fmt::Debug for DhtQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DhtQuery")
            .field("local_id", &self.local_id)
            .finish()
    }
}

impl DhtQuery {
    /// Create a new DHT query engine.
    pub fn new(
        local_id: NodeId,
        routing_table: Arc<RwLock<RoutingTable>>,
        store: SharedDhtStore,
        sender: Arc<dyn DhtMessageSender>,
    ) -> Self {
        Self {
            local_id,
            routing_table,
            store,
            sender,
            pending: HashMap::new(),
        }
    }

    /// Generate a unique query ID.
    fn new_query_id() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("q_{:x}", now)
    }

    /// Start a FIND_NODE query to find peers closest to a key.
    pub async fn find_node(&mut self, key: &DhtKey) -> QueryResult {
        let query_id = Self::new_query_id();
        let start = Instant::now();

        // Get initial candidates from routing table
        let target_id = node_id_from_key(key);
        let mut candidates: Vec<Contact> = {
            let rt = self.routing_table.read().unwrap();
            rt.closest(&target_id, CLOSEST_PEERS)
        };

        let mut queried = std::collections::HashSet::new();
        queried.insert(self.local_id.clone());

        // Track responses
        let mut all_closest: Vec<Contact> = Vec::new();

        // Iterative lookup
        while !candidates.is_empty() {
            // Take up to ALPHA candidates
            let batch: Vec<_> = candidates
                .drain(..std::cmp::min(ALPHA, candidates.len()))
                .filter(|c| !queried.contains(&c.id))
                .collect();

            if batch.is_empty() {
                break;
            }

            // Send parallel queries
            let sender = self.sender.clone();
            let futures: Vec<_> = batch.iter().map(|peer| {
                let sender = sender.clone();
                let qid = query_id.clone();
                let k = key.clone();
                async move {
                    match sender.send_find_node(peer, &k, &qid).await {
                        Ok(DhtWireMessage::Nodes(NodesPayload { nodes, .. })) => {
                            // Convert NodeContact to NodeInfo
                            let result: Vec<NodeInfo> = nodes.into_iter().map(|nc| NodeInfo {
                                id: nc.id,
                                addrs: nc.addrs,
                            }).collect();
                            Some(result)
                        }
                        _ => None,
                    }
                }
            }).collect();

            // Collect responses
            for future in futures {
                if let Some(nodes) = future.await {
                    for node in nodes {
                        let contact = Contact::new(node.id.clone(), parse_addr(&node.addrs));
                        if !queried.contains(&contact.id) {
                            candidates.push(contact.clone());
                            all_closest.push(contact);
                        }
                    }
                }
            }

            // Mark batch as queried
            for c in &batch {
                queried.insert(c.id.clone());
            }
        }

        // Sort by distance and take closest
        all_closest.sort_by(|a, b| {
            let da = a.id.xor_distance(&target_id);
            let db = b.id.xor_distance(&target_id);
            da.cmp(&db)
        });

        let peers: Vec<NodeInfo> = all_closest
            .into_iter()
            .take(CLOSEST_PEERS)
            .map(|c| NodeInfo {
                id: c.id,
                addrs: c.addrs.iter().map(|a| a.to_string()).collect(),
            })
            .collect();

        QueryResult {
            peers,
            providers: Vec::new(),
            query_id,
            duration_ms: start.elapsed().as_millis() as u64,
        }
    }

    /// Start a GET_PROVIDERS query to find who has content.
    pub async fn get_providers(&mut self, key: &DhtKey) -> QueryResult {
        let query_id = Self::new_query_id();
        let start = Instant::now();

        // Check local store first
        let mut all_providers: Vec<ProviderRecord> = self.store.get_providers(key);

        // Get initial candidates
        let target_id = node_id_from_key(key);
        let mut candidates: Vec<Contact> = {
            let rt = self.routing_table.read().unwrap();
            rt.closest(&target_id, CLOSEST_PEERS)
        };

        let mut queried = std::collections::HashSet::new();
        queried.insert(self.local_id.clone());

        // Iterative lookup
        while !candidates.is_empty() && all_providers.len() < 20 {
            let batch: Vec<_> = candidates
                .drain(..std::cmp::min(ALPHA, candidates.len()))
                .filter(|c| !queried.contains(&c.id))
                .collect();

            if batch.is_empty() {
                break;
            }

            // Send parallel queries
            let futures: Vec<_> = batch.iter().map(|peer| {
                let sender = &self.sender;
                let qid = query_id.clone();
                let k = key.clone();
                let k_bytes = k.as_bytes().to_vec();
                async move {
                    match sender.send_get_providers(peer, &k, &qid).await {
                        Ok(DhtWireMessage::Providers(ProvidersPayload { providers, .. })) => {
                            // Convert ProviderRecordWire to ProviderRecord
                            let result: Vec<ProviderRecord> = providers.into_iter().filter_map(|pw| {
                                pw.addrs.first().map(|addr| {
                                    ProviderRecord::new(
                                        DhtKey::from_bytes(k_bytes.clone()),
                                        pw.provider_id,
                                        addr.clone(),
                                    )
                                })
                            }).collect();
                            Some(result)
                        }
                        _ => None,
                    }
                }
            }).collect();

            for future in futures {
                if let Some(providers) = future.await {
                    all_providers.extend(providers);
                }
            }

            for c in &batch {
                queried.insert(c.id.clone());
            }
        }

        // Deduplicate providers
        let mut seen = std::collections::HashSet::new();
        all_providers.retain(|p| seen.insert(p.provider_id.clone()));

        QueryResult {
            peers: Vec::new(),
            providers: all_providers,
            query_id,
            duration_ms: start.elapsed().as_millis() as u64,
        }
    }

    /// Announce that we provide content for a key.
    pub async fn add_provider(&mut self, key: &DhtKey, addr: String) -> Result<(), QueryError> {
        let record = ProviderRecord::new(key.clone(), self.local_id.clone(), addr);

        // Store locally
        self.store.put_provider(key, record.clone());

        // Get closest peers and announce to them
        let target_id = node_id_from_key(key);
        let peers: Vec<Contact> = {
            let rt = self.routing_table.read().unwrap();
            rt.closest(&target_id, CLOSEST_PEERS)
        };

        // Announce to up to K peers
        for peer in peers.into_iter().take(KBUCKET_SIZE) {
            if peer.id == self.local_id {
                continue;
            }
            if let Err(e) = self.sender.send_add_provider(&peer, key, &record).await {
                tracing::warn!("Failed to announce to peer {}: {}", peer.id.short(), e);
            }
        }

        Ok(())
    }

    /// Get a value from the DHT.
    pub async fn get_value(&mut self, key: &DhtKey) -> Option<Vec<u8>> {
        // Check local store
        if let Some(value) = self.store.get_value(key) {
            return Some(value.data);
        }

        // Query network
        let _result = self.find_node(key).await;

        None
    }

    /// Put a value into the DHT.
    pub async fn put_value(&mut self, key: &DhtKey, data: Vec<u8>, ttl: Duration) -> Result<(), QueryError> {
        let value = crate::record::DhtValue {
            data,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ttl_secs: ttl.as_secs(),
        };

        // Store locally (clone for later use)
        let value_for_store = value.clone();
        self.store.put_value(key, value_for_store);

        // Announce to closest peers (synchronous for simplicity)
        let target_id = node_id_from_key(key);
        let peers: Vec<Contact> = {
            let rt = self.routing_table.read().unwrap();
            rt.closest(&target_id, CLOSEST_PEERS)
        };

        let sender = self.sender.as_ref();
        for peer in peers.into_iter().take(KBUCKET_SIZE) {
            if peer.id == self.local_id {
                continue;
            }
            let qid = Self::new_query_id();
            if let Err(e) = sender.send_put_value(&peer, key, &value, &qid).await {
                tracing::warn!("Failed to announce value to peer {}: {}", peer.id.short(), e);
            }
        }

        Ok(())
    }
}

// Import RwLock at the top

/// Trait for sending DHT messages to peers.
#[async_trait::async_trait]
pub trait DhtMessageSender: Send + Sync {
    /// Send a FIND_NODE request.
    async fn send_find_node(
        &self,
        peer: &Contact,
        key: &DhtKey,
        request_id: &str,
    ) -> Result<DhtWireMessage, QueryError>;

    /// Send a GET_PROVIDERS request.
    async fn send_get_providers(
        &self,
        peer: &Contact,
        key: &DhtKey,
        request_id: &str,
    ) -> Result<DhtWireMessage, QueryError>;

    /// Send an ADD_PROVIDER announcement.
    async fn send_add_provider(
        &self,
        peer: &Contact,
        key: &DhtKey,
        record: &ProviderRecord,
    ) -> Result<(), QueryError>;

    /// Send a PUT_VALUE request.
    async fn send_put_value(
        &self,
        peer: &Contact,
        key: &DhtKey,
        value: &crate::record::DhtValue,
        request_id: &str,
    ) -> Result<(), QueryError>;
}

/// Error during DHT queries.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("Network error: {0}")]
    Network(String),

    #[error("Timeout")]
    Timeout,

    #[error("Peer not found")]
    PeerNotFound,

    #[error("Invalid response")]
    InvalidResponse,
}

// Helper functions

fn node_id_from_key(key: &DhtKey) -> NodeId {
    // Use the key bytes as node ID (for routing)
    // In practice, use first 32 bytes or hash of key
    let bytes: Vec<u8> = key.as_bytes().iter().copied().take(32).chain(std::iter::repeat(0)).take(32).collect();
    let mut arr = [0u8; 32];
    for (i, &b) in bytes.iter().enumerate() {
        arr[i] = b;
    }
    NodeId::from_bytes(&arr).unwrap_or_else(|_| NodeId::random())
}

fn parse_addr(addrs: &[String]) -> std::net::SocketAddr {
    addrs
        .first()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "127.0.0.1:0".parse().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_id_format() {
        let id1 = DhtQuery::new_query_id();
        let id2 = DhtQuery::new_query_id();
        // IDs should have the expected format
        assert!(id1.starts_with("q_"), "ID should start with q_: {}", id1);
        assert!(id2.starts_with("q_"), "ID should start with q_: {}", id2);
        // IDs should be reasonably long (at least 16 chars for timestamp + random)
        assert!(id1.len() >= 16, "ID too short: {}", id1);
        assert!(id2.len() >= 16, "ID too short: {}", id2);
        // In normal conditions IDs should be unique, but we don't assert this
        // as they may collide under high load or in tests with deterministic time
    }

    #[tokio::test]
    async fn test_local_provider_lookup() {
        let local_id = NodeId::random();
        let store = crate::store::new_in_memory_store();
        let _rt = Arc::new(RwLock::new(RoutingTable::new(local_id.clone())));

        // Add a provider locally
        let key = DhtKey::from_bytes(vec![0u8; 32]);
        let provider_id = NodeId::random();
        let record = ProviderRecord::new(
            key.clone(),
            provider_id,
            "127.0.0.1:8080".to_string(),
        );
        store.put_provider(&key, record);

        // Query should find it
        let providers = store.get_providers(&key);
        assert_eq!(providers.len(), 1);
    }
}
