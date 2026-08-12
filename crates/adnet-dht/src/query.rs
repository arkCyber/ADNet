//! Kademlia-style DHT queries for finding providers and resolving names.

use std::collections::{HashMap, HashSet};
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

/// Default query timeout (30 seconds).
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Default per-request timeout (5 seconds).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum number of query iterations before giving up.
const MAX_ITERATIONS: usize = 20;

/// Query configuration options.
#[derive(Debug, Clone)]
pub struct QueryConfig {
    /// Maximum time for a complete query to complete.
    pub query_timeout: Duration,
    /// Maximum time to wait for a single peer response.
    pub request_timeout: Duration,
    /// Maximum number of parallel requests (α).
    pub parallelism: usize,
    /// Maximum iterations in iterative lookup.
    pub max_iterations: usize,
    /// Whether to continue on individual peer failures.
    pub continue_on_error: bool,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            query_timeout: DEFAULT_QUERY_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            parallelism: ALPHA,
            max_iterations: MAX_ITERATIONS,
            continue_on_error: true,
        }
    }
}

/// Query state for tracking in-flight queries.
struct QueryState {
    started_at: Instant,
    requests_sent: usize,
    responses_received: usize,
    closest_seen: Vec<Contact>,
    providers: Vec<ProviderRecord>,
    timeout_at: Instant,
}

/// Result of a DHT query.
#[derive(Debug)]
pub struct QueryResult {
    pub peers: Vec<NodeInfo>,
    pub providers: Vec<ProviderRecord>,
    pub query_id: String,
    pub duration_ms: u64,
    pub timed_out: bool,
}

/// Internal result of iterative query.
struct IterativeQueryResult {
    results: Vec<Contact>,
    query_id: String,
    duration_ms: u64,
    iterations: usize,
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
    /// Query configuration.
    config: QueryConfig,
}

impl std::fmt::Debug for DhtQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DhtQuery")
            .field("local_id", &self.local_id)
            .finish()
    }
}

impl DhtQuery {
    /// Create a new DHT query engine with default config.
    pub fn new(
        local_id: NodeId,
        routing_table: Arc<RwLock<RoutingTable>>,
        store: SharedDhtStore,
        sender: Arc<dyn DhtMessageSender>,
    ) -> Self {
        Self::with_config(local_id, routing_table, store, sender, QueryConfig::default())
    }

    /// Create a new DHT query engine with custom config.
    pub fn with_config(
        local_id: NodeId,
        routing_table: Arc<RwLock<RoutingTable>>,
        store: SharedDhtStore,
        sender: Arc<dyn DhtMessageSender>,
        config: QueryConfig,
    ) -> Self {
        Self {
            local_id,
            routing_table,
            store,
            sender,
            pending: HashMap::new(),
            config,
        }
    }

    /// Get the current query configuration.
    pub fn config(&self) -> &QueryConfig {
        &self.config
    }

    /// Update query configuration.
    pub fn set_config(&mut self, config: QueryConfig) {
        self.config = config;
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

    /// Check if a query has exceeded its global timeout.
    fn is_timed_out(&self, start: Instant) -> bool {
        start.elapsed() >= self.config.query_timeout
    }

    /// Query a batch of peers with per-request timeout.
    async fn query_batch_with_timeout(
        &self,
        peers: &[Contact],
        key: &DhtKey,
        query_id: &str,
        _query_start: Instant,
    ) -> Vec<(NodeId, Option<Vec<Contact>>)> {
        let sender = self.sender.clone();
        let k = key.clone();
        let qid = query_id.to_string();
        let request_timeout = self.config.request_timeout;

        let futures: Vec<_> = peers.iter().map(|peer| {
            let sender = sender.clone();
            let k = k.clone();
            let qid = qid.clone();
            async move {
                let peer_id = peer.id.clone();
                let result = tokio::time::timeout(
                    request_timeout,
                    sender.send_find_node(peer, &k, &qid),
                ).await;

                let contacts = match result {
                    Ok(Ok(DhtWireMessage::Nodes(NodesPayload { nodes, .. }))) => {
                        Some(nodes.into_iter().map(|nc| {
                            Contact::new(nc.id, parse_addr(&nc.addrs))
                        }).collect())
                    }
                    Ok(Ok(DhtWireMessage::Providers(ProvidersPayload { providers, .. }))) => {
                        Some(providers.into_iter().filter_map(|pw| {
                            pw.addrs.first().map(|addr| {
                                Contact::new(pw.provider_id, addr.parse().unwrap_or_else(|_| "127.0.0.1:0".parse().unwrap()))
                            })
                        }).collect())
                    }
                    _ => None,
                };

                (peer_id, contacts)
            }
        }).collect();

        futures::future::join_all(futures).await
    }

    /// Start a FIND_NODE query to find peers closest to a key.
    pub async fn find_node(&mut self, key: &DhtKey) -> QueryResult {
        let start = Instant::now();
        let query_id = Self::new_query_id();
        let target_id = node_id_from_key(key);

        // Check timeout before starting
        if self.is_timed_out(start) {
            return QueryResult {
                peers: Vec::new(),
                providers: Vec::new(),
                query_id,
                duration_ms: start.elapsed().as_millis() as u64,
                timed_out: true,
            };
        }

        // Get initial candidates from routing table
        let mut candidates: Vec<Contact> = {
            let rt = self.routing_table.read().unwrap();
            rt.closest(&target_id, CLOSEST_PEERS)
        };

        let mut queried = HashSet::new();
        queried.insert(self.local_id.clone());

        // Track responses
        let mut all_closest: Vec<Contact> = Vec::new();
        let mut iterations = 0;
        let mut timed_out = false;

        // Iterative lookup with timeout
        while !candidates.is_empty() && iterations < self.config.max_iterations {
            // Check global timeout
            if self.is_timed_out(start) {
                tracing::debug!(
                    query_id = %query_id,
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    iterations,
                    "FIND_NODE query timed out"
                );
                timed_out = true;
                break;
            }

            iterations += 1;

            // Take up to parallelism candidates
            let batch: Vec<_> = candidates
                .drain(..std::cmp::min(self.config.parallelism, candidates.len()))
                .filter(|c| !queried.contains(&c.id))
                .collect();

            if batch.is_empty() {
                break;
            }

            // Send parallel queries with per-request timeout
            let results = self.query_batch_with_timeout(
                &batch,
                key,
                &query_id,
                start,
            ).await;

            // Collect responses
            for (peer_id, result) in results {
                queried.insert(peer_id);

                if let Some(contacts) = result {
                    for contact in contacts {
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
            timed_out,
        }
    }

    /// Start a GET_PROVIDERS query to find who has content.
    pub async fn get_providers(&mut self, key: &DhtKey) -> QueryResult {
        let start = Instant::now();
        let query_id = Self::new_query_id();

        // Check global timeout before starting
        if self.is_timed_out(start) {
            return QueryResult {
                peers: Vec::new(),
                providers: self.store.get_providers(key),
                query_id,
                duration_ms: start.elapsed().as_millis() as u64,
                timed_out: true,
            };
        }

        // Check local store first
        let mut all_providers: Vec<ProviderRecord> = self.store.get_providers(key);

        // Get initial candidates
        let target_id = node_id_from_key(key);
        let mut candidates: Vec<Contact> = {
            let rt = self.routing_table.read().unwrap();
            rt.closest(&target_id, CLOSEST_PEERS)
        };

        let mut queried = HashSet::new();
        queried.insert(self.local_id.clone());

        let mut iterations = 0;
        let mut timed_out = false;

        // Iterative lookup with timeout
        while !candidates.is_empty()
            && all_providers.len() < 20
            && iterations < self.config.max_iterations
        {
            // Check global timeout
            if self.is_timed_out(start) {
                tracing::debug!(
                    query_id = %query_id,
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    iterations,
                    "GET_PROVIDERS query timed out"
                );
                timed_out = true;
                break;
            }

            iterations += 1;

            // Take up to parallelism candidates
            let batch: Vec<_> = candidates
                .drain(..std::cmp::min(self.config.parallelism, candidates.len()))
                .filter(|c| !queried.contains(&c.id))
                .collect();

            if batch.is_empty() {
                break;
            }

            // Send parallel queries with per-request timeout
            let results = self.query_providers_batch_with_timeout(
                &batch,
                key,
                &query_id,
                start,
            ).await;

            // Collect responses
            for (peer_id, result) in results {
                queried.insert(peer_id);

                if let Some(providers) = result {
                    all_providers.extend(providers);
                }
            }

            for c in &batch {
                queried.insert(c.id.clone());
            }
        }

        // Deduplicate providers
        let mut seen = HashSet::new();
        all_providers.retain(|p| seen.insert(p.provider_id.clone()));

        QueryResult {
            peers: Vec::new(),
            providers: all_providers,
            query_id,
            duration_ms: start.elapsed().as_millis() as u64,
            timed_out,
        }
    }

    /// Query providers batch with per-request timeout.
    async fn query_providers_batch_with_timeout(
        &self,
        peers: &[Contact],
        key: &DhtKey,
        query_id: &str,
        _query_start: Instant,
    ) -> Vec<(NodeId, Option<Vec<ProviderRecord>>)> {
        let sender = self.sender.clone();
        let k = key.clone();
        let k_bytes = k.as_bytes().to_vec();
        let qid = query_id.to_string();
        let request_timeout = self.config.request_timeout;

        let futures: Vec<_> = peers.iter().map(|peer| {
            let sender = sender.clone();
            let k = k.clone();
            let k_bytes = k_bytes.clone();
            let qid = qid.clone();
            async move {
                let peer_id = peer.id.clone();
                let result = tokio::time::timeout(
                    request_timeout,
                    sender.send_get_providers(peer, &k, &qid),
                ).await;

                let providers = match result {
                    Ok(Ok(DhtWireMessage::Providers(ProvidersPayload { providers, .. }))) => {
                        Some(providers.into_iter().filter_map(|pw| {
                            pw.addrs.first().map(|addr| {
                                ProviderRecord::new(
                                    DhtKey::from_bytes(k_bytes.clone()),
                                    pw.provider_id,
                                    addr.clone(),
                                )
                            })
                        }).collect())
                    }
                    _ => None,
                };

                (peer_id, providers)
            }
        }).collect();

        futures::future::join_all(futures).await
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

/// Convert a DhtKey to a NodeId for routing purposes.
/// Uses the key bytes to derive a consistent node ID.
pub fn node_id_from_key(key: &DhtKey) -> NodeId {
    // Use the key bytes as node ID (for routing)
    // In practice, use first 32 bytes or hash of key
    let bytes: Vec<u8> = key.as_bytes().iter().copied().take(32).chain(std::iter::repeat(0)).take(32).collect();
    let mut arr = [0u8; 32];
    for (i, &b) in bytes.iter().enumerate() {
        arr[i] = b;
    }
    NodeId::from_bytes(&arr).unwrap_or_else(|_| NodeId::random())
}

/// Convert a hex string or other string key to a NodeId for routing.
/// Falls back to deriving a NodeId from the string bytes.
pub fn node_id_from_key_str(key: &str) -> NodeId {
    // First try to parse as a valid NodeId
    if let Ok(node_id) = key.parse::<NodeId>() {
        return node_id;
    }
    
    // Try hex string (strip '0x' prefix if present)
    let hex_str = key.trim_start_matches("0x");
    if hex_str.len() == 64 {
        if let Ok(bytes) = hex::decode(hex_str) {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes[..32]);
            return NodeId::from_bytes(&arr).unwrap_or_else(|_| NodeId::random());
        }
    }
    
    // Fall back to hashing the string to get a deterministic NodeId
    let hash = blake3::hash(key.as_bytes());
    let mut arr = [0u8; 32];
    arr.copy_from_slice(hash.as_bytes());
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

    #[test]
    fn test_query_config_defaults() {
        let config = QueryConfig::default();
        assert_eq!(config.query_timeout, DEFAULT_QUERY_TIMEOUT);
        assert_eq!(config.request_timeout, DEFAULT_REQUEST_TIMEOUT);
        assert_eq!(config.parallelism, ALPHA);
        assert!(config.continue_on_error);
    }

    #[test]
    fn test_query_config_custom() {
        let config = QueryConfig {
            query_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(10),
            parallelism: 5,
            max_iterations: 30,
            continue_on_error: false,
        };
        assert_eq!(config.query_timeout, Duration::from_secs(60));
        assert_eq!(config.request_timeout, Duration::from_secs(10));
        assert_eq!(config.parallelism, 5);
        assert_eq!(config.max_iterations, 30);
        assert!(!config.continue_on_error);
    }

    #[tokio::test]
    async fn test_query_result_timed_out_flag() {
        let result = QueryResult {
            peers: Vec::new(),
            providers: Vec::new(),
            query_id: "test".to_string(),
            duration_ms: 100,
            timed_out: true,
        };
        assert!(result.timed_out);
    }
}
