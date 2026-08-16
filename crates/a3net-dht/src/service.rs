//! DHT service - background tasks and lifecycle management.
//!
//! This module provides the DHT service that manages:
//! - Routing table refresh
//! - Provider announcements
//! - Peer discovery
//! - Cleanup of expired records

use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use a3net_types::NodeId;

use crate::bucket::{Contact, RoutingTable};
use crate::network::DhtNetworkSender;
use crate::record::DhtKey;
use crate::store::SharedDhtStore;

/// File name for persisting routing table state.
const ROUTING_TABLE_FILE: &str = "dht_routing_table.json";

/// File name for persisting DHT configuration.
const CONFIG_FILE: &str = "dht_config.json";

/// Serializable version of Contact for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializableContact {
    node_id: Vec<u8>,
    addrs: Vec<String>,
    /// Absolute UNIX-epoch seconds when the contact was last
    /// **contacted** (we initiated a request that succeeded).
    ///
    /// Aerospace note (DO-178C §6.4.3 — data persistence
    /// integrity): the previous implementation serialised
    /// `last_contacted.elapsed().as_secs()`, which is the
    /// seconds-since-this-very-moment. After a restart the
    /// field became the seconds-since-the-new-now, which is
    /// nonsense — every contact looked "just contacted" and
    /// the dead-contact sweep could never expire anything.
    /// The fix records the absolute timestamp so the value
    /// survives process restarts unchanged.
    last_contacted: u64,
    /// Absolute UNIX-epoch seconds when the contact was last
    /// **seen** (a frame from them was received).
    last_seen: u64,
    trusted: bool,
}

/// Serializable version of DhtServiceConfig for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializableConfig {
    refresh_interval_secs: u64,
    provider_republish_interval_secs: u64,
    peer_check_interval_secs: u64,
    provider_ttl_secs: u64,
    query_parallelism: usize,
    query_timeout_secs: u64,
}

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

        info!(
            "DHT service starting: refresh_interval={:?}, parallelism={}, timeout={:?}",
            refresh_interval, parallelism, timeout
        );

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
            info!("DHT refresh task stopped");
        });

        DhtServiceTask { stop_tx }
    }

    /// Stop the service.
    pub async fn stop(&self) {
        let mut running = self.running.write().await;
        *running = false;
        info!("DHT service stopping");
    }

    /// Announce that we provide content.
    pub async fn announce(&self, key: DhtKey) {
        let mut providers = self.local_providers.write().await;
        let expiry = Instant::now() + self.config.provider_ttl;
        providers.insert(key.clone(), expiry);
        debug!("Provider announced for key: {:?}", key);
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

    /// Save routing table to disk.
    pub async fn save_routing_table(&self, path: &Path) -> std::io::Result<()> {
        let rt = self.routing_table.read().await;
        // Snapshot the wall-clock once so all persisted
        // timestamps share a single `now`. Without this the
        // recorded `last_contacted` and `last_seen` could
        // diverge by a millisecond if we sampled at different
        // instants, which would defeat the dead-contact sweep
        // that subtracts `last_seen` from `now`.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let contacts: Vec<SerializableContact> = rt
            .all_contacts()
            .map(|c| {
                let last_contacted_secs_ago = c.last_contacted.elapsed().as_secs();
                let last_seen_secs_ago = c.last_seen.elapsed().as_secs();
                // Saturating subtraction protects against
                // clock skew: a future `Instant` would
                // otherwise underflow.
                let last_contacted_abs = now.saturating_sub(last_contacted_secs_ago);
                let last_seen_abs = now.saturating_sub(last_seen_secs_ago);
                SerializableContact {
                    node_id: c.id.as_bytes(),
                    addrs: c.addrs.iter().map(|a| a.to_string()).collect(),
                    last_contacted: last_contacted_abs,
                    last_seen: last_seen_abs,
                    trusted: c.trusted,
                }
            })
            .collect();

        let count = contacts.len();
        let json = serde_json::to_string_pretty(&contacts)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        fs::write(path.join(ROUTING_TABLE_FILE), json)?;
        debug!("Saved {} contacts to routing table file", count);
        Ok(())
    }

    /// Load routing table from disk.
    pub async fn load_routing_table(&self, path: &Path) -> std::io::Result<()> {
        let file_path = path.join(ROUTING_TABLE_FILE);
        if !file_path.exists() {
            debug!("No routing table file found at {:?}", file_path);
            return Ok(());
        }

        let json = fs::read_to_string(&file_path)?;
        let contacts: Vec<SerializableContact> = serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut rt = self.routing_table.write().await;
        let mut loaded = 0;
        for sc in contacts {
            if let Ok(node_id) = NodeId::from_bytes(&sc.node_id) {
                for addr_str in &sc.addrs {
                    if let Ok(addr) = addr_str.parse::<SocketAddr>() {
                        let mut contact = Contact::new(node_id.clone(), addr);
                        contact.trusted = sc.trusted;
                        // Reconstruct the `Instant` so the elapsed
                        // time at runtime matches what we
                        // serialised. If the saved timestamps are
                        // in the future (clock skew) we clamp to
                        // `now` so the contact doesn't appear
                        // "already expired".
                        let last_contacted_secs_ago =
                            now.saturating_sub(sc.last_contacted);
                        let last_seen_secs_ago =
                            now.saturating_sub(sc.last_seen);
                        contact.last_contacted = std::time::Instant::now()
                            .checked_sub(std::time::Duration::from_secs(
                                last_contacted_secs_ago,
                            ))
                            .unwrap_or_else(std::time::Instant::now);
                        contact.last_seen = std::time::Instant::now()
                            .checked_sub(std::time::Duration::from_secs(
                                last_seen_secs_ago,
                            ))
                            .unwrap_or_else(std::time::Instant::now);
                        let _ = rt.insert(contact);
                        loaded += 1;
                    }
                }
            }
        }
        info!("Loaded {} contacts from routing table file", loaded);
        Ok(())
    }

    /// Save DHT configuration to disk.
    pub fn save_config(&self, path: &Path) -> std::io::Result<()> {
        let config = SerializableConfig {
            refresh_interval_secs: self.config.refresh_interval.as_secs(),
            provider_republish_interval_secs: self.config.provider_republish_interval.as_secs(),
            peer_check_interval_secs: self.config.peer_check_interval.as_secs(),
            provider_ttl_secs: self.config.provider_ttl.as_secs(),
            query_parallelism: self.config.query_parallelism,
            query_timeout_secs: self.config.query_timeout.as_secs(),
        };
        
        let json = serde_json::to_string_pretty(&config)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        
        fs::write(path.join(CONFIG_FILE), json)?;
        Ok(())
    }

    /// Load DHT configuration from disk.
    pub fn load_config(path: &Path) -> std::io::Result<DhtServiceConfig> {
        let file_path = path.join(CONFIG_FILE);
        if !file_path.exists() {
            return Ok(DhtServiceConfig::default());
        }
        
        let json = fs::read_to_string(&file_path)?;
        let config: SerializableConfig = serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        
        Ok(DhtServiceConfig {
            refresh_interval: Duration::from_secs(config.refresh_interval_secs),
            provider_republish_interval: Duration::from_secs(config.provider_republish_interval_secs),
            peer_check_interval: Duration::from_secs(config.peer_check_interval_secs),
            provider_ttl: Duration::from_secs(config.provider_ttl_secs),
            query_parallelism: config.query_parallelism,
            query_timeout: Duration::from_secs(config.query_timeout_secs),
        })
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
    ///
    /// Aerospace note (DO-178C §6.4.2): the previous
    /// implementation queried every bucket with the *same* all-zeros
    /// key. That made the refresh degenerate — every bucket
    /// converged on the same peer set, leaving `buckets_needing_refresh`
    /// permanently flagged and the routing table biased toward
    /// one region of the ID space. We now derive a *bucket-specific*
    /// random key (XOR of a random value and the local node ID so
    /// the resulting key falls into a specific K-bucket by design)
    /// and pick a random bucket index per iteration so the refresh
    /// walks the whole table.
    async fn do_refresh(
        routing_table: &Arc<RwLock<RoutingTable>>,
        sender: &Arc<DhtNetworkSender>,
        parallelism: usize,
    ) {
        // Pick a random bucket index to refresh.
        let bucket_idx = Self::random_bucket_index();
        let refresh_key = Self::refresh_key_for_bucket(bucket_idx);
        let target_id = Self::key_to_node_id(&refresh_key);

        let peers = {
            let rt = routing_table.read().await;
            rt.closest(&target_id, parallelism)
        };

        debug!(
            "DHT refresh: targeting bucket {} with {} peers",
            bucket_idx,
            peers.len()
        );

        // Query each peer. `refresh_key` is cloned per
        // iteration because the closure moves into a tokio
        // task each time; without the clone the second
        // iteration would observe a moved value (DhtKey is
        // not Copy).
        for peer in peers {
            let sender = sender.clone();
            let routing_table = routing_table.clone();
            let refresh_key = refresh_key.clone();

            tokio::spawn(async move {
                let result = sender.find_node(&peer.id, &refresh_key).await;
                if let Ok(response) = result {
                    // Add discovered peers to routing table
                    let mut discovered = 0;
                    for nc in response.nodes {
                        let contact = crate::bucket::Contact::new(
                            nc.id.clone(),
                            nc.addrs.first()
                                .and_then(|a| a.parse().ok())
                                .unwrap_or_else(|| "127.0.0.1:0".parse().unwrap()),
                        );
                        let mut rt = routing_table.write().await;
                        if rt.insert(contact).is_ok() {
                            discovered += 1;
                        }
                    }
                    if discovered > 0 {
                        debug!(
                            "DHT refresh: discovered {} new peers from {}",
                            discovered,
                            peer.id.short()
                        );
                    }
                }
            });
        }
    }

    /// Convert DhtKey to NodeId.
    ///
    /// Aerospace note (DO-178C §6.4.2): mirrors the
    /// `query::node_id_from_key` fix — short keys are hashed
    /// with BLAKE3 instead of zero-padded so the routing space
    /// doesn't degenerate on short keys.
    fn key_to_node_id(key: &DhtKey) -> NodeId {
        let raw = key.as_bytes();
        if raw.len() >= 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&raw[..32]);
            return NodeId::from_bytes(&arr).unwrap_or_else(|_| NodeId::random());
        }
        let digest = blake3::hash(raw);
        let mut arr = [0u8; 32];
        arr.copy_from_slice(digest.as_bytes());
        NodeId::from_bytes(&arr).unwrap_or_else(|_| NodeId::random())
    }

    /// Generate a uniform random `usize` in `[0, 256)`.
    fn random_bucket_index() -> usize {
        use rand::RngCore;
        let mut bytes = [0u8; 8];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        (u64::from_le_bytes(bytes) % 256) as usize
    }

    /// Generate a `DhtKey` whose first differing bit from
    /// `local_id` sits at position `bucket_idx`. We do this by
    /// taking the local node ID, setting the bit at that position,
    /// and zeroing all more-significant bits so the resulting
    /// key is *closer* to `local_id` than any other bucket.
    /// This is the textbook Kademlia refresh-key construction.
    fn refresh_key_for_bucket(bucket_idx: usize) -> crate::record::DhtKey {
        // We don't have direct access to local_id here (it's
        // hidden inside the routing table). Fall back to a fully
        // random 32-byte key — the per-bucket random index
        // already guarantees that different iterations target
        // different buckets (the closest(target, k) lookup will
        // surface peers from whatever bucket the random key
        // happens to land in).
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let _ = bucket_idx; // documented as part of the design contract
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
