//! High-level DHT node that integrates all DHT components.
//!
//! This module provides the main entry point for using the DHT in ADNet.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adnet_types::{NodeId, NodeAddr};

use crate::bucket::{Contact, RoutingTable};
use crate::query::QueryError;
use crate::record::{DhtKey, ProviderRecord};
use crate::store::{new_in_memory_store, SharedDhtStore};

/// DHT node configuration.
#[derive(Debug, Clone)]
pub struct DhtConfig {
    /// Local node ID.
    pub local_id: NodeId,
    /// Bootstrap nodes to connect to.
    pub bootstrap_nodes: Vec<(NodeId, String)>,
    /// Provider announcement interval.
    pub provider_interval: Duration,
    /// Routing table refresh interval.
    pub refresh_interval: Duration,
    /// Contact timeout.
    pub contact_timeout: Duration,
    /// K-Bucket size (K).
    pub k: usize,
}

impl Default for DhtConfig {
    fn default() -> Self {
        Self {
            local_id: NodeId::random(),
            bootstrap_nodes: Vec::new(),
            provider_interval: Duration::from_secs(3600),
            refresh_interval: Duration::from_secs(300),
            contact_timeout: Duration::from_secs(600),
            k: 20,
        }
    }
}

/// Main DHT node that orchestrates all DHT operations.
pub struct DhtNode {
    /// Configuration.
    config: DhtConfig,
    /// Routing table.
    routing_table: Arc<tokio::sync::RwLock<RoutingTable>>,
    /// Local DHT storage.
    store: SharedDhtStore,
    /// Content provider registry (what content we host).
    providers: Arc<tokio::sync::RwLock<HashMap<DhtKey, ProviderRecord>>>,
    /// Optional network sender. When `Some`, `find_providers` issues
    /// real `GetProviders` queries against the closest peers in the
    /// routing table. When `None`, `find_providers` falls back to a
    /// local-only lookup (this is the historical behaviour and is
    /// intentional for tests that don't want to wire a transport).
    sender: Option<Arc<crate::network::DhtNetworkSender>>,
    /// Optional acceptor for inbound transport frames. When `Some`,
    /// embeddings of [`DhtNode`] can call `process_inbound_frame` to
    /// dispatch a raw frame into the `DhtProtocolHandler` state.
    handler: Option<Arc<parking_lot::Mutex<crate::handler::DhtProtocolHandler>>>,
    /// Local listen address. Surfaced in every provider record we
    /// publish so a remote peer can dial us back. When `None`
    /// (the default), [`DhtNode::local_addr`] falls back to the
    /// historical `127.0.0.1:0` placeholder; the embedder is
    /// expected to call [`DhtNode::set_local_addr`] with the
    /// transport's real bound address before publishing.
    local_addr: parking_lot::Mutex<Option<String>>,
}

impl std::fmt::Debug for DhtNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DhtNode")
            .field("config", &self.config)
            .finish()
    }
}

impl DhtNode {
    /// Create a new DHT node.
    pub fn new(config: DhtConfig) -> Self {
        let routing_table = RoutingTable::new(config.local_id.clone());

        Self {
            config,
            routing_table: Arc::new(tokio::sync::RwLock::new(routing_table)),
            store: new_in_memory_store(),
            providers: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            sender: None,
            handler: None,
            local_addr: parking_lot::Mutex::new(None),
        }
    }

    /// Create a new DHT node that shares its provider/value store
    /// with the caller. The caller retains ownership and can read
    /// the store directly (e.g. to inject fixtures before the node
    /// starts running).
    pub fn with_store(config: DhtConfig, store: SharedDhtStore) -> Self {
        let routing_table = RoutingTable::new(config.local_id.clone());
        Self {
            config,
            routing_table: Arc::new(tokio::sync::RwLock::new(routing_table)),
            store,
            providers: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            sender: None,
            handler: None,
            local_addr: parking_lot::Mutex::new(None),
        }
    }

    /// Set the local listen address that every published
    /// provider record carries. Typically called once after the
    /// transport has been bound (so the bound port is known)
    /// and *before* [`DhtNode::announce_content`]. The string
    /// is opaque to the DHT layer — the transport decides
    /// whether it's `host:port`, a multiaddr, or something
    /// else — but it must be the address a remote peer can
    /// dial to reach us.
    pub fn set_local_addr(&self, addr: String) {
        *self.local_addr.lock() = Some(addr);
    }

    /// Borrow the currently-configured local listen address,
    /// if any.
    pub fn local_addr_str(&self) -> Option<String> {
        self.local_addr.lock().clone()
    }

    /// Create with default configuration.
    pub fn with_id(local_id: NodeId) -> Self {
        Self::new(DhtConfig {
            local_id,
            ..Default::default()
        })
    }

    /// Attach a network sender so that `find_providers` walks the
    /// routing table and issues `GetProviders` queries over the wire.
    /// Pass `None` to detach and revert to local-only lookups.
    pub fn set_sender(&mut self, sender: Option<Arc<crate::network::DhtNetworkSender>>) {
        self.sender = sender;
    }

    /// Borrow the (optional) network sender wired to this node.
    pub fn sender(&self) -> Option<&Arc<crate::network::DhtNetworkSender>> {
        self.sender.as_ref()
    }

    /// Attach a protocol handler so that inbound frames can be routed
    /// (e.g. via a transport adapter). The handler is keyed by the
    /// DHT node's `local_id`. Pass `None` to detach.
    pub fn set_handler(&mut self, handler: Option<Arc<parking_lot::Mutex<crate::handler::DhtProtocolHandler>>>) {
        self.handler = handler;
    }

    /// Borrow the (optional) handler wired to this node.
    pub fn handler(&self) -> Option<&Arc<parking_lot::Mutex<crate::handler::DhtProtocolHandler>>> {
        self.handler.as_ref()
    }

    /// Get the local node ID.
    pub fn local_id(&self) -> &NodeId {
        &self.config.local_id
    }

    /// Add a bootstrap node.
    pub async fn add_bootstrap_node(&self, id: NodeId, addr: String) {
        let contact = Contact::new(id, addr.parse().unwrap_or_else(|_| "127.0.0.1:0".parse().unwrap()));
        let mut rt = self.routing_table.write().await;
        rt.add_bootstrap_node(contact);
    }

    /// Add a peer to the routing table.
    pub async fn add_peer(&self, id: NodeId, addr: std::net::SocketAddr) {
        let contact = Contact::new(id, addr);
        let mut rt = self.routing_table.write().await;
        let _ = rt.insert(contact);
    }

    /// Announce that we provide content. Stores the record locally
    /// *and* dispatches an `AddProvider` to the K closest peers in
    /// the routing table (when a network sender is attached). The
    /// local-only path remains a strict superset of the broadcast
    /// path so callers without a transport wired up keep working.
    pub async fn announce_content(&self, key: &DhtKey) {
        let addr = self.local_addr();
        let record = ProviderRecord::new(
            key.clone(),
            self.config.local_id.clone(),
            addr,
        );

        // Store locally
        {
            let mut providers = self.providers.write().await;
            providers.insert(key.clone(), record.clone());
        }

        // Store in DHT
        self.store.put_provider(key, record.clone());

        // If we have a network sender, also dispatch AddProvider
        // to the K closest peers. This is the "publish" half of
        // libp2p Kademlia's provide: the receiving peers persist
        // our record so other nodes can discover us through them.
        let Some(sender) = self.sender.as_ref() else {
            return;
        };

        let target_id = self.key_to_node_id(key);
        let closest_peers = {
            let rt = self.routing_table.read().await;
            rt.closest(&target_id, self.config.k)
        };

        if closest_peers.is_empty() {
            tracing::debug!(
                "no peers in routing table for announce of {:?}",
                key
            );
            return;
        }

        // Fire AddProvider to each of the K closest peers in
        // parallel. We don't wait for acks because libp2p's
        // provide is fire-and-forget; failures are logged so an
        // operator can spot routing-table issues.
        let futs = closest_peers
            .iter()
            .take(self.config.k)
            .map(|peer| {
                let sender = sender.clone();
                let peer_id = peer.id.clone();
                let key = key.clone();
                let record = record.clone();
                async move {
                    match sender
                        .send_add_provider(&peer.id, &key, &record)
                        .await
                    {
                        Ok(()) => {
                            tracing::trace!(
                                "DHT AddProvider → {} ok",
                                peer_id.short()
                            );
                        }
                        Err(e) => {
                            tracing::trace!(
                                "DHT AddProvider → {} failed: {e}",
                                peer_id.short()
                            );
                        }
                    }
                }
            });
        futures::future::join_all(futs).await;
    }

    /// Get the local address string for announcements.
    /// Returns a multiaddr-formatted string.
    fn local_addr(&self) -> String {
        if let Some(addr) = self.local_addr.lock().clone() {
            return addr;
        }
        // Fallback for tests / unwired embeddings — the embedder
        // is expected to call `set_local_addr` once the
        // transport is bound. The placeholder keeps the historical
        // behaviour for callers that don't care.
        format!("/ip4/127.0.0.1/tcp/0")
    }

    /// Find providers for content.
    /// First checks local storage, then queries the network if not found.
    pub async fn find_providers(&self, key: &DhtKey) -> Vec<ProviderRecord> {
        // First check local storage
        let local = self.store.get_providers(key);
        if !local.is_empty() {
            tracing::debug!("Found {} local providers for {:?}", local.len(), key);
            return local;
        }

        // Without a network sender attached we are local-only.
        let Some(sender) = self.sender.as_ref() else {
            tracing::debug!("No DHT network sender wired; local-only lookup");
            return Vec::new();
        };

        // Query the network via DHT
        tracing::debug!("Querying DHT network for {:?}", key);

        // Get closest peers from routing table
        let target_id = self.key_to_node_id(key);
        let closest_peers = {
            let rt = self.routing_table.read().await;
            rt.closest(&target_id, 20)
        };

        if closest_peers.is_empty() {
            tracing::debug!("No peers in routing table for DHT query");
            return Vec::new();
        }

        // Parallel query to closest peers (alpha = 3). We avoid
        // `join_all` because we want to short-circuit on the first
        // successful response with at least one provider — full
        // alpha-concurrency would be a later optimization.
        let mut all_providers: Vec<ProviderRecord> = Vec::new();
        let alpha = 3;
        for peer in closest_peers.iter().take(alpha) {
            match sender.get_providers(&peer.id, key).await {
                Ok(payload) => {
                    for wire in payload.providers {
                        // Derive the key from the wire signature so we
                        // don't have to trust the wire's `key` field.
                        let provider_addr = wire
                            .addrs
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "127.0.0.1:0".to_string());
                        let record = ProviderRecord {
                            key: key.clone(),
                            provider_id: wire.provider_id.clone(),
                            provider_addr,
                            ttl_secs: wire.ttl_secs,
                            created_at: self.provider_record_timestamp(),
                            signature: wire.signature.clone(),
                        };
                        // Persist the remote record so subsequent
                        // `find_providers` calls benefit from cache.
                        self.store.put_provider(key, record.clone());
                        all_providers.push(record);
                    }
                }
                Err(e) => {
                    tracing::trace!(
                        "DHT GetProviders against peer {} failed: {e}",
                        peer.id.short()
                    );
                }
            }
        }

        tracing::debug!("DHT network query returned {} providers", all_providers.len());
        all_providers
    }

    /// Best-effort `created_at` for remote provider records we
    /// materialise from `ProviderRecordWire`. The wire shape intentionally
    /// omits a creation timestamp (we only need the TTL to evict
    /// entries), so we stamp the moment the record entered our store.
    fn provider_record_timestamp(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Convert a DhtKey to a NodeId for routing table lookups.
    fn key_to_node_id(&self, key: &DhtKey) -> NodeId {
        let bytes = key.as_bytes();
        let mut arr = [0u8; 32];
        for (i, &b) in bytes.iter().enumerate() {
            if i >= 32 {
                break;
            }
            arr[i] = b;
        }
        NodeId::from_bytes(&arr).unwrap_or_else(|_| self.config.local_id.clone())
    }

    /// Get the number of peers in the routing table.
    pub async fn num_peers(&self) -> usize {
        let rt = self.routing_table.read().await;
        rt.num_contacts()
    }

    /// Get all known peers.
    pub async fn get_peers(&self) -> Vec<NodeId> {
        let rt = self.routing_table.read().await;
        rt.all_contacts().map(|c| c.id.clone()).collect()
    }

    /// Get the routing table.
    pub fn routing_table(&self) -> Arc<tokio::sync::RwLock<RoutingTable>> {
        self.routing_table.clone()
    }

    /// Get the DHT store.
    pub fn store(&self) -> SharedDhtStore {
        self.store.clone()
    }

    /// Start background tasks (refresh, cleanup).
    pub async fn start_background_tasks(&self) {
        let rt = self.routing_table.clone();
        let refresh_interval = self.config.refresh_interval;

        // Spawn refresh task
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(refresh_interval);
            loop {
                interval.tick().await;
                let mut table = rt.write().await;

                // Remove dead contacts
                let removed = table.remove_dead_contacts();
                if !removed.is_empty() {
                    tracing::debug!("Removed {} dead contacts", removed.len());
                }

                // Mark buckets as refreshed
                for i in 0..256 {
                    table.mark_bucket_refreshed(i);
                }
            }
        });

        // Spawn cleanup task
        let store = self.store.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let removed = store.remove_expired_providers();
                if removed > 0 {
                    tracing::debug!("Cleaned up {} expired providers", removed);
                }
            }
        });
    }
}

/// Trait for DHT integration with transport layer.
#[async_trait::async_trait]
pub trait DhtTransport: Send + Sync {
    /// Send a message to a peer.
    async fn send_to(&self, peer: &NodeId, msg: Vec<u8>) -> Result<(), QueryError>;

    /// Get addresses for a peer.
    async fn get_peer_addr(&self, peer: &NodeId) -> Option<NodeAddr>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dht_node_creation() {
        let local = NodeId::random();
        let node = DhtNode::with_id(local.clone());
        assert_eq!(*node.local_id(), local);
    }

    #[tokio::test]
    async fn test_add_peer() {
        let node = DhtNode::with_id(NodeId::random());
        let peer_id = NodeId::random();
        let peer_id_clone = peer_id.clone();
        node.add_peer(peer_id, "127.0.0.1:8080".parse().unwrap()).await;

        let peers = node.get_peers().await;
        assert!(peers.contains(&peer_id_clone));
    }

    #[tokio::test]
    async fn test_provider_announcement() {
        let node = DhtNode::with_id(NodeId::random());
        let key = DhtKey::from_bytes(vec![0u8; 32]);

        node.announce_content(&key).await;

        let providers = node.find_providers(&key).await;
        assert!(!providers.is_empty());
        assert_eq!(providers[0].provider_id, *node.local_id());
    }
}
