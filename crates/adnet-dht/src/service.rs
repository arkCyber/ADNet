//! DHT service - background tasks and lifecycle management.
//!
//! This module provides the DHT service that manages:
//! - Routing table refresh
//! - Provider announcements
//! - Peer discovery
//! - Cleanup of expired records

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, RwLock};
use tokio::time::Instant;

use adnet_types::NodeId;

use crate::bucket::RoutingTable;
use crate::network::DhtNetworkSender;
use crate::record::DhtKey;
use crate::store::SharedDhtStore;

/// Service configuration.
#[derive(Debug, Clone)]
pub struct DhtServiceConfig {
    /// How often to refresh the routing table.
    pub refresh_interval: Duration,
    /// How often to republish provider records.
    pub provider_republish_interval: Duration,
    /// How often to check for dead peers.
    pub peer_check_interval: Duration,
    /// Default provider TTL.
    pub provider_ttl: Duration,
    /// Number of closest peers to query.
    pub query_parallelism: usize,
    /// Query timeout.
    pub query_timeout: Duration,
}

impl Default for DhtServiceConfig {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(300),     // 5 minutes
            provider_republish_interval: Duration::from_secs(3600), // 1 hour
            peer_check_interval: Duration::from_secs(60),  // 1 minute
            provider_ttl: Duration::from_secs(86400),      // 24 hours
            query_parallelism: 3,
            query_timeout: Duration::from_secs(5),
        }
    }
}

/// Background task handle.
#[derive(Debug)]
pub struct DhtServiceTask {
    /// Sender to stop the service.
    stop_tx: mpsc::Sender<()>,
}

/// DHT service for managing background tasks.
pub struct DhtService {
    /// Local node ID.
    local_id: NodeId,
    /// Configuration.
    config: DhtServiceConfig,
    /// Routing table.
    routing_table: Arc<RwLock<RoutingTable>>,
    /// Network sender.
    sender: Arc<DhtNetworkSender>,
    /// DHT storage.
    store: SharedDhtStore,
    /// Local provider announcements (key -> expiry time).
    local_providers: Arc<RwLock<HashMap<DhtKey, Instant>>>,
    /// Whether service is running.
    running: Arc<RwLock<bool>>,
}

impl DhtService {
    /// Create a new DHT service.
    pub fn new(
        local_id: NodeId,
        config: DhtServiceConfig,
        routing_table: Arc<RwLock<RoutingTable>>,
        sender: Arc<DhtNetworkSender>,
        store: SharedDhtStore,
    ) -> Self {
        Self {
            local_id,
            config,
            routing_table,
            sender,
            store,
            local_providers: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Start the background service tasks.
    pub async fn start(&self) -> DhtServiceTask {
        let (stop_tx, stop_rx) = mpsc::channel(1);

        // Mark as running
        {
            let mut running = self.running.write().await;
            *running = true;
        }

        // Clone handles for tasks
        let routing_table = self.routing_table.clone();
        let sender = self.sender.clone();
        let store = self.store.clone();
        let running = self.running.clone();
        let config = self.config.clone();

        // Spawn refresh task
        let refresh_interval = self.config.refresh_interval;
        let parallelism = self.config.query_parallelism;
        let timeout = self.config.query_timeout;

        tokio::spawn(async move {
            Self::refresh_task(
                routing_table,
                sender,
                store,
                running,
                refresh_interval,
                parallelism,
                timeout,
                stop_rx,
            ).await;
        });

        DhtServiceTask { stop_tx }
    }

    /// Stop the service.
    pub async fn stop(&self) {
        let mut running = self.running.write().await;
        *running = false;
    }

    /// Announce that we provide content.
    pub async fn announce(&self, key: DhtKey) {
        let mut providers = self.local_providers.write().await;
        let expiry = Instant::now() + self.config.provider_ttl;
        providers.insert(key, expiry);
    }

    /// Get local provider keys.
    pub async fn get_local_providers(&self) -> Vec<DhtKey> {
        let now = Instant::now();
        let providers = self.local_providers.read().await;
        providers
            .iter()
            .filter(|(_, expiry)| **expiry > now)
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Routing table refresh task.
    async fn refresh_task(
        routing_table: Arc<RwLock<RoutingTable>>,
        sender: Arc<DhtNetworkSender>,
        _store: SharedDhtStore,
        running: Arc<RwLock<bool>>,
        interval: Duration,
        parallelism: usize,
        _timeout: Duration,
        mut stop_rx: mpsc::Receiver<()>,
    ) {
        let mut ticker = tokio::time::interval(interval);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let is_running = { *running.read().await };
                    if !is_running {
                        break;
                    }

                    Self::do_refresh(&routing_table, &sender, parallelism).await;
                }
                _ = stop_rx.recv() => {
                    break;
                }
            }
        }
    }

    /// Perform a single refresh of the routing table.
    async fn do_refresh(
        routing_table: &Arc<RwLock<RoutingTable>>,
        sender: &Arc<DhtNetworkSender>,
        parallelism: usize,
    ) {
        // Get a random key to refresh
        let refresh_key = Self::random_refresh_key();
        let target_id = Self::key_to_node_id(&refresh_key);

        let peers = {
            let rt = routing_table.read().await;
            rt.closest(&target_id, parallelism)
        };

        // Query each peer
        for peer in peers {
            let sender = sender.clone();
            let routing_table = routing_table.clone();

            tokio::spawn(async move {
                let key = DhtKey::from_bytes(vec![0u8; 32]);
                let result = sender.find_node(&peer.id, &key).await;
                if let Ok(response) = result {
                    // Add discovered peers to routing table
                    for nc in response.nodes {
                        let contact = crate::bucket::Contact::new(
                            nc.id,
                            nc.addrs.first()
                                .and_then(|a| a.parse().ok())
                                .unwrap_or_else(|| "127.0.0.1:0".parse().unwrap()),
                        );
                        let mut rt = routing_table.write().await;
                        let _ = rt.insert(contact);
                    }
                }
            });
        }
    }

    /// Convert DhtKey to NodeId.
    fn key_to_node_id(key: &DhtKey) -> NodeId {
        let bytes: Vec<u8> = key.as_bytes().iter().copied().take(32).chain(std::iter::repeat(0)).take(32).collect();
        let mut arr = [0u8; 32];
        for (i, &b) in bytes.iter().enumerate() {
            arr[i] = b;
        }
        NodeId::from_bytes(&arr).unwrap_or_else(|_| NodeId::random())
    }

    /// Generate a random key for refresh operations.
    fn random_refresh_key() -> crate::record::DhtKey {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        crate::record::DhtKey::from_bytes(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = DhtServiceConfig::default();
        assert_eq!(config.refresh_interval, Duration::from_secs(300));
        assert_eq!(config.provider_ttl, Duration::from_secs(86400));
    }
}
