//! DHT (Distributed Hash Table) and IPNS integration for a3net-node.
//!
//! This module wraps the underlying `a3net_dht` DHT node with the
//! `a3net-node` configuration shape and provider-record mapping that
//! `BitswapHandle` consumes.
//!
//! DO-178C traceability:
//! - DHT-1: `provide()` must announce content via `DhtNode::announce_content`
//!   and track the record locally so callers can introspect it.
//! - DHT-2: `find_providers()` must consult both the local DHT store and the
//!   in-process routing table, returning `Vec<ProviderRecord>` so the Bitswap
//!   side can initiate downloads.
//! - DHT-3: announced `provider_addr` reflects the node's real listen address
//!   (set via [`DhtHandle::set_external_addr`]) so peers can dial us back.
//! - IPNS-1: `IpnHandle::publish` requires an Ed25519 publisher and refuses
//!   to sign records with `NotAuthorized` when no keypair is wired.
//! - IPNS-2: `IpnHandle::cleanup` drops expired cache entries so the
//!   resolver never returns stale content.

use std::sync::Arc;
use std::time::Duration;

use a3net_dht::{
    DhtKey, DhtNode, DhtNetworkSender, ProviderRecord as DhtProviderRecord, TransportDhtSender,
};
use a3net_namespace::{IpnsError, IpnPublisher, IpnResolver};
use a3net_types::{ContentHash, NodeId};

#[cfg(feature = "pubsub")]
use a3net_namespace::PubsubSubscription;

use crate::dht_bridge::BridgeSenderAdapter;

/// Public alias for the IPNS-over-PubSub subscription handle. Only
/// available when the `pubsub` feature of `a3net-namespace` is on
/// (which the `a3net-node` `dht` feature forces).
#[cfg(feature = "pubsub")]
pub type IpnPubsubSubscription = PubsubSubscription;
/// No-op fallback when `pubsub` is off. We still expose the type so
/// `Node`'s `Option<IpnPubsubSubscription>` field compiles cleanly
/// under both feature combinations.
#[cfg(not(feature = "pubsub"))]
pub type IpnPubsubSubscription = ();

// ════════════════════════════════════════════════════════════════════
//  Configuration
// ════════════════════════════════════════════════════════════════════

/// DHT configuration wrapper for a3net-node.
///
/// Field shape mirrors `a3net_dht::DhtConfig` so a `.into()` conversion is
/// a 1:1 mapping.
#[derive(Debug, Clone)]
pub struct DhtConfig {
    pub local_id: NodeId,
    pub bootstrap_nodes: Vec<(NodeId, String)>,
    pub provider_interval: Duration,
    pub refresh_interval: Duration,
    pub contact_timeout: Duration,
    pub k_bucket_size: usize,
}

impl Default for DhtConfig {
    fn default() -> Self {
        Self {
            local_id: NodeId::random(),
            bootstrap_nodes: Vec::new(),
            provider_interval: Duration::from_secs(3600),
            refresh_interval: Duration::from_secs(300),
            contact_timeout: Duration::from_secs(600),
            k_bucket_size: 20,
        }
    }
}

impl From<DhtConfig> for a3net_dht::DhtConfig {
    fn from(config: DhtConfig) -> Self {
        Self {
            local_id: config.local_id,
            bootstrap_nodes: config.bootstrap_nodes,
            provider_interval: config.provider_interval,
            refresh_interval: config.refresh_interval,
            contact_timeout: config.contact_timeout,
            k: config.k_bucket_size,
        }
    }
}

/// IPNS configuration.
#[derive(Debug, Clone)]
pub struct IpnConfig {
    /// Resolver cache TTL — entries older than this are dropped on `cleanup`.
    pub cache_ttl_secs: u64,
    /// Default TTL applied to records when none is provided.
    pub record_ttl_secs: u64,
}

impl Default for IpnConfig {
    fn default() -> Self {
        Self {
            cache_ttl_secs: 3600,
            record_ttl_secs: 3600,
        }
    }
}

// ════════════════════════════════════════════════════════════════════
//  Provider record
// ════════════════════════════════════════════════════════════════════

/// Provider record returned to `BitswapHandle::find_providers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRecord {
    /// Hex-encoded DHT key (the content hash, since `DhtKey` wraps raw bytes).
    pub key: String,
    pub provider_id: NodeId,
    pub provider_addr: String,
    pub ttl_secs: u64,
    pub created_at: u64,
    pub signature: Option<Vec<u8>>,
}

impl From<DhtProviderRecord> for ProviderRecord {
    fn from(r: DhtProviderRecord) -> Self {
        Self {
            key: r.key.as_hex(),
            provider_id: r.provider_id,
            provider_addr: r.provider_addr,
            ttl_secs: r.ttl_secs,
            created_at: r.created_at,
            signature: r.signature,
        }
    }
}

/// DHT error type.
#[derive(Debug, thiserror::Error)]
pub enum DhtError {
    #[error("DHT operation failed: {0}")]
    Operation(String),
    #[error("Invalid configuration: {0}")]
    Config(String),
    #[error("Content hash parse failed: {0}")]
    HashParse(String),
}

impl From<a3net_dht::QueryError> for DhtError {
    fn from(e: a3net_dht::QueryError) -> Self {
        DhtError::Operation(e.to_string())
    }
}

// ════════════════════════════════════════════════════════════════════
//  DHT handle
// ════════════════════════════════════════════════════════════════════

/// DHT handle for a3net-node integration.
///
/// Owns the underlying `a3net_dht::DhtNode` (shared via `Arc` so multiple
/// callers can `provide()` concurrently). The Iroh runtime, Bitswap, and
/// RPC layers all consume this handle.
pub struct DhtHandle {
    node: Arc<DhtNode>,
    local_id: NodeId,
    /// Multiaddr the local node is reachable at; surfaced in
    /// `provider_addr` of every announce we make.
    external_addr: parking_lot::RwLock<Option<String>>,
    /// Lightweight counters — enough for the `/diagnostics` surface
    /// without taking a dependency on `a3net-observability`.
    metrics: parking_lot::RwLock<DhtMetrics>,
}

/// Atomic counters for DHT operations. Read with [`DhtHandle::metrics`].
#[derive(Debug, Default, Clone, Copy)]
pub struct DhtMetrics {
    /// Number of `provide()` calls successfully completed.
    pub provides_total: u64,
    /// Number of `find_providers()` calls completed (any result size).
    pub find_total: u64,
    /// Number of provider records returned by `find_providers()`.
    pub find_records_total: u64,
    /// Number of `find_providers()` calls that returned zero records.
    pub find_misses_total: u64,
    /// Last `find_providers()` call's latency (microseconds).
    pub last_find_latency_us: u64,
}

impl std::fmt::Display for DhtMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "provides={} finds={} (records={}, misses={}) last_find_us={}",
            self.provides_total,
            self.find_total,
            self.find_records_total,
            self.find_misses_total,
            self.last_find_latency_us,
        )
    }
}

impl std::fmt::Debug for DhtHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DhtHandle")
            .field("local_id", &self.local_id)
            .field("providers", &"<DhtNode>")
            .finish()
    }
}

impl DhtHandle {
    /// Build a handle around a freshly-created `DhtNode`.
    ///
    /// Bootstrap nodes registered in `config.bootstrap_nodes` are added
    /// to the routing table immediately so the first `find_providers`
    /// call has at least one candidate peer. Callers that need a
    /// long-running refresh / cleanup loop should invoke
    /// [`DhtHandle::start_background_tasks`] explicitly.
    pub async fn new(config: DhtConfig) -> Self {
        let local_id = config.local_id.clone();
        let node = Arc::new(DhtNode::new(config.clone().into()));
        let handle = Self {
            node,
            local_id,
            external_addr: parking_lot::RwLock::new(None),
            metrics: parking_lot::RwLock::new(DhtMetrics::default()),
        };

        // Bootstrap registration is asynchronous and best-effort. The
        // background refresh loop retries unreachable nodes.
        for (id, addr) in &config.bootstrap_nodes {
            if let Ok(socket) = addr.parse() {
                handle.node.add_peer(id.clone(), socket).await;
            }
        }
        handle
    }

    /// Build a handle with a pre-existing `Arc<DhtNode>`.
    pub fn from_node(node: Arc<DhtNode>) -> Self {
        let local_id = node.local_id().clone();
        Self {
            node,
            local_id,
            external_addr: parking_lot::RwLock::new(None),
            metrics: parking_lot::RwLock::new(DhtMetrics::default()),
        }
    }

    /// Wire a transport bridge into the DHT so that `provide()`
    /// and `find_providers()` actually go over the wire.
    ///
    /// This is the audit-fix entry point: before this method exists,
    /// the DHT was a placeholder — `DhtHandle::new` constructed a
    /// `DhtNode` whose `sender` slot stayed `None`, so
    /// `announce_content` only stored locally and `find_providers`
    /// short-circuited to "no sender wired; local-only".
    ///
    /// After this call, `provide()` fans `AddProvider` out to the K
    /// closest peers, and `find_providers()` issues `GetProviders`
    /// against those peers when the local store misses. The caller
    /// is responsible for installing any response sink on the bridge
    /// before passing it in (via `DynTransportBridge::set_response_sink`).
    ///
    /// Returns the shared sender so callers (tests, RPC layers) can
    /// drive requests directly when needed.
    pub fn set_transport(
        &self,
        bridge: Arc<dyn a3net_dht::transport::TransportBridge>,
    ) -> Arc<DhtNetworkSender> {
        // Build a `TransportDhtSender` adapter over the bridge for
        // the legacy `send_raw` path used by `announce_provider`.
        let adapter: Arc<dyn TransportDhtSender> =
            Arc::new(crate::dht_bridge::BridgeSenderAdapter::new(bridge));

        // Build the network sender and install it on the inner node.
        let routing_table = self.node.routing_table();
        let sender = Arc::new(DhtNetworkSender::new(
            self.local_id.clone(),
            adapter,
            routing_table,
        ));

        self.node.attach_sender(Some(sender.clone()));

        // Mirror the configured external address to the inner node
        // so the wire-protocol broadcast path carries it.
        if let Some(addr) = self.external_addr.read().clone() {
            self.node.set_local_addr(addr);
        }

        sender
    }

    /// Detach the network sender (revert to local-only `find_providers`).
    ///
    /// Useful for tests that want to verify the local-only path
    /// even after wiring a transport.
    pub fn detach_transport(&self) {
        self.node.attach_sender(None);
    }

    /// Borrow the (optional) network sender wired to this handle.
    pub fn sender(&self) -> Option<Arc<DhtNetworkSender>> {
        self.node.sender()
    }

    /// Borrow the underlying DHT node.
    pub fn inner(&self) -> Arc<DhtNode> {
        Arc::clone(&self.node)
    }

    /// Get local node ID.
    pub fn local_id(&self) -> &NodeId {
        &self.local_id
    }

/// Set the multiaddr the local node is reachable at.
///
/// Subsequent `provide()` calls will surface this address in
/// `ProviderRecord::provider_addr` so peers can dial us back.
/// Pass `None` to clear.
pub fn set_external_addr(&self, addr: Option<String>) {
    *self.external_addr.write() = addr.clone();
    // Mirror `Some` to the inner `DhtNode` so the wire-protocol
    // broadcast path (which calls `node.announce_content` directly)
    // carries the same address. We intentionally skip the `None`
    // branch — clearing the handle's external addr must not
    // retroactively rewrite records already on the wire (covered by
    // the dedicated `dht_handle_clear_external_addr_preserves_records`
    // test).
    if let Some(a) = addr {
        self.node.set_local_addr(a);
    }
}

    /// Get the currently-configured external address, if any.
    pub fn external_addr(&self) -> Option<String> {
        self.external_addr.read().clone()
    }

    /// Announce that we are a provider for `hash`.
    ///
    /// After the underlying [`DhtNode::announce_content`] runs we
    /// patch the local provider record so its `provider_addr` reflects
    /// the value set via [`DhtHandle::set_external_addr`] (when one is
    /// configured). This keeps the public `provide()` / `find_providers()`
    /// contract honest: callers see the address we told them to publish,
    /// not the placeholder `/ip4/127.0.0.1/tcp/0` the DHT layer uses.
    pub async fn provide(&self, hash: &ContentHash) {
        let key: DhtKey = hash.into();
        self.node.announce_content(&key).await;

        // Patch the locally-cached provider record so subsequent
        // `find_providers` calls reflect the configured external
        // address.
        let external_addr = self.external_addr.read().clone();
        if let Some(addr) = external_addr {
            self.rewrite_local_provider_addr(&key, addr).await;
        }
        // Single-write path: take the lock once, mutate, drop.
        let mut m = self.metrics.write();
        m.provides_total = m.provides_total.saturating_add(1);
    }

    /// Replace the `provider_addr` of the locally-cached provider
    /// record for `key`, if one exists.
    async fn rewrite_local_provider_addr(&self, key: &DhtKey, addr: String) {
        // The `DhtNode` owns the in-memory provider map; we patch the
        // entry through the public `DhtStorage` trait.
        let store = self.node.store();
        let mut providers = store.get_providers(key);
        if let Some(record) = providers.first_mut() {
            record.provider_addr = addr;
            store.put_provider(key, record.clone());
        }
    }

/// Find providers for `hash`.
///
/// First checks the local DHT store (in-memory `SharedDhtStore`); falls
/// back to walking the routing table once routing-table integration is
/// complete.
    pub async fn find_providers(&self, hash: &ContentHash) -> Vec<ProviderRecord> {
        let start = std::time::Instant::now();
        let key: DhtKey = hash.into();
        let result = self
            .node
            .find_providers(&key)
            .await
            .into_iter()
            .map(ProviderRecord::from)
            .collect::<Vec<_>>();

        let elapsed_us = start.elapsed().as_micros() as u64;
        let mut m = self.metrics.write();
        m.find_total = m.find_total.saturating_add(1);
        m.find_records_total = m.find_records_total.saturating_add(result.len() as u64);
        if result.is_empty() {
            m.find_misses_total = m.find_misses_total.saturating_add(1);
        }
        m.last_find_latency_us = elapsed_us;

        result
    }

    /// Snapshot the current DHT metrics counters. Useful for
    /// `/diagnostics` endpoints and tests.
    pub fn metrics(&self) -> DhtMetrics {
        *self.metrics.read()
    }

    /// Number of contacts currently in the routing table.
    pub async fn num_peers(&self) -> usize {
        self.node.num_peers().await
    }

    /// List all peer IDs known to the routing table.
    pub async fn known_peers(&self) -> Vec<NodeId> {
        self.node.get_peers().await
    }

    /// Spawn the DHT's background refresh + cleanup loops.
    pub async fn start_background_tasks(&self) {
        self.node.start_background_tasks().await;
    }

    /// Get DHT statistics.
    pub fn stats(&self) -> DhtStats {
        DhtStats {
            local_id: self.local_id.clone(),
            external_addr: self.external_addr.read().clone(),
            metrics: *self.metrics.read(),
        }
    }

    /// Get a value from the DHT store.
    pub fn get_value(&self, key: &DhtKey) -> Option<a3net_dht::record::DhtValue> {
        self.node.get_value(key)
    }

    /// Put a value into the DHT store.
    pub fn put_value(&self, key: &DhtKey, value: a3net_dht::record::DhtValue) {
        self.node.put_value(key, value);
    }

    /// Get all known peers from the routing table.
    pub async fn get_peers(&self) -> Vec<NodeId> {
        self.node.get_peers().await
    }

    /// Run an iterative FIND_NODE query for the peers closest to
    /// `target`. Returns the query's full result so callers can
    /// inspect the candidate set, elapsed time, and timed-out
    /// flag.
    pub async fn find_node(
        &self,
        key: &DhtKey,
    ) -> a3net_dht::query::QueryResult {
        self.node.find_node(key).await
    }
}

/// DHT statistics.
#[derive(Debug, Clone)]
pub struct DhtStats {
    pub local_id: NodeId,
    /// Currently-configured external multiaddr (see [`DhtHandle::set_external_addr`]).
    pub external_addr: Option<String>,
    pub metrics: DhtMetrics,
}

impl std::fmt::Display for DhtStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DHT(local_id={}, external_addr={:?}, {})",
            self.local_id, self.external_addr, self.metrics
        )
    }
}

// ════════════════════════════════════════════════════════════════════
//  IPNS handle
// ════════════════════════════════════════════════════════════════════

/// IPNS handle for mutable name resolution.
///
/// Composes an `IpnResolver` (cache + lookup) with an optional
/// `IpnPublisher` (used when this node owns an Ed25519 keypair and wants
/// to publish its own records).
pub struct IpnHandle {
    resolver: Arc<IpnResolver>,
    publisher: Option<Arc<IpnPublisher>>,
    local_id: NodeId,
    /// Default TTL applied to records when `publish` is called without
    /// an explicit TTL.
    default_record_ttl: Duration,
    /// Resolver cache TTL.
    cache_ttl: Duration,
}

impl std::fmt::Debug for IpnHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpnHandle")
            .field("local_id", &self.local_id)
            .field("publisher", &self.publisher.is_some())
            .field("default_record_ttl", &self.default_record_ttl)
            .field("cache_ttl", &self.cache_ttl)
            .finish()
    }
}

impl IpnHandle {
    /// Build a read-only handle with the cache TTL from `config`.
    pub fn new(config: IpnConfig, node_id: NodeId) -> Self {
        let cache_ttl = Duration::from_secs(config.cache_ttl_secs);
        Self {
            resolver: Arc::new(IpnResolver::new(cache_ttl)),
            publisher: None,
            local_id: node_id,
            default_record_ttl: Duration::from_secs(config.record_ttl_secs),
            cache_ttl,
        }
    }

    /// Attach a publisher (used when this node signs records).
    pub fn with_publisher(mut self, publisher: Arc<IpnPublisher>) -> Self {
        self.publisher = Some(publisher);
        self
    }

    /// Get local node ID.
    pub fn local_node_id(&self) -> &NodeId {
        &self.local_id
    }

    /// Borrow the resolver.
    pub fn resolver(&self) -> &IpnResolver {
        &self.resolver
    }

    /// Clone the [`Arc<IpnResolver>`] so external subsystems (e.g.
    /// the [`crate::pubsub::PubsubIpnsResolver`]) can ingest records
    /// into the same cache that [`IpnHandle::resolve`] consults.
    pub fn resolver_arc(&self) -> Arc<IpnResolver> {
        self.resolver.clone()
    }


    /// Borrow the publisher, if any.
    pub fn publisher(&self) -> Option<&IpnPublisher> {
        self.publisher.as_ref().map(|p| p.as_ref())
    }

    /// Default TTL applied to records when `publish` is called without an
    /// explicit TTL.
    pub fn default_record_ttl(&self) -> Duration {
        self.default_record_ttl
    }

    /// Resolve `name` to a `ContentHash`-shaped value.
    ///
    /// `IpnResolver::resolve` returns the raw `value` string stored in the
    /// record. We try to parse it as a `ContentHash` (lowercase hex).
    /// Non-hex values surface as [`DhtError::HashParse`] so callers can
    /// decide whether to interpret them differently.
    pub async fn resolve(&self, name: &str) -> Result<ContentHash, DhtError> {
        let value = self
            .resolver
            .resolve(name)
            .await
            .map_err(|e| DhtError::Operation(e.to_string()))?;
        ContentHash::from_hex(&value).map_err(|e| DhtError::HashParse(e.to_string()))
    }

    /// Publish a new value under `name`.
    pub async fn publish(
        &self,
        name: &str,
        value: String,
        ttl: Duration,
    ) -> Result<(), IpnsError> {
        let publisher = self
            .publisher
            .as_ref()
            .ok_or_else(|| IpnsError::NotAuthorized)?;
        publisher.publish(name, value, ttl).await?;
        Ok(())
    }

    /// Publish a new value under `name` using the default TTL from
    /// [`IpnConfig::record_ttl_secs`].
    pub async fn publish_default(
        &self,
        name: &str,
        value: String,
    ) -> Result<(), IpnsError> {
        self.publish(name, value, self.default_record_ttl).await
    }

    /// Drop expired entries from the resolver cache.
    pub fn cleanup(&self) {
        self.resolver.clear_expired();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_namespace::ipns::SecretKey as Sk;
    use a3net_namespace::{Ed25519SecretKey, IpnRecord};
    use a3net_types::content::ContentHash;

    fn hash_of(bytes: &[u8]) -> ContentHash {
        ContentHash::from_bytes(bytes)
    }

    #[tokio::test]
    async fn dht_handle_provide_announces_locally() {
        let config = DhtConfig::default();
        let handle = DhtHandle::new(config).await;

        let hash = hash_of(b"hello-dht");
        handle.provide(&hash).await;

        // After `provide` the local provider registry should have an
        // entry. `find_providers` consults the in-memory store first.
        let providers = handle.find_providers(&hash).await;
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].provider_id, *handle.local_id());
    }

    #[tokio::test]
    async fn dht_handle_unknown_hash_yields_empty() {
        let handle = DhtHandle::new(DhtConfig::default()).await;
        let providers = handle.find_providers(&hash_of(b"nothing")).await;
        assert!(providers.is_empty());
    }

    #[tokio::test]
    async fn dht_stats_display_includes_node_id() {
        let handle = DhtHandle::new(DhtConfig::default()).await;
        let stats = handle.stats();
        let display = format!("{stats}");
        assert!(display.contains("DHT("));
        assert!(display.contains(&handle.local_id().to_string()));
    }

    #[tokio::test]
    async fn dht_stats_external_addr_round_trips() {
        let handle = DhtHandle::new(DhtConfig::default()).await;
        assert!(handle.external_addr().is_none());
        handle.set_external_addr(Some("/ip4/1.2.3.4/tcp/4001".to_string()));
        assert_eq!(
            handle.external_addr().as_deref(),
            Some("/ip4/1.2.3.4/tcp/4001")
        );
        let display = format!("{}", handle.stats());
        assert!(display.contains("/ip4/1.2.3.4/tcp/4001"));

        handle.set_external_addr(None);
        assert!(handle.external_addr().is_none());
    }

    #[tokio::test]
    async fn dht_config_converts_to_internal() {
        let mut cfg = DhtConfig::default();
        cfg.k_bucket_size = 42;
        cfg.local_id = NodeId::from_bytes(&[7u8; 32]).unwrap();
        let internal: a3net_dht::DhtConfig = cfg.clone().into();
        assert_eq!(internal.k, 42);
        assert_eq!(internal.local_id, cfg.local_id);
    }

    #[tokio::test]
    async fn dht_handle_from_node_shares_inner() {
        let config = DhtConfig::default();
        let handle_a = DhtHandle::new(config).await;
        let inner = handle_a.inner();
        let handle_b = DhtHandle::from_node(inner.clone());

        assert_eq!(handle_a.local_id(), handle_b.local_id());
        // Inner Arc count: handle_a + handle_b + inner = 3
        assert!(Arc::strong_count(&inner) >= 2);

        // Provide via one handle, find via the other.
        let hash = hash_of(b"shared");
        handle_a.provide(&hash).await;
        let providers = handle_b.find_providers(&hash).await;
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].provider_id, *handle_a.local_id());
    }

    #[tokio::test]
    async fn dht_provide_reflects_external_addr_in_records() {
        let handle = DhtHandle::new(DhtConfig::default()).await;
        handle.set_external_addr(Some("/ip4/1.2.3.4/tcp/4001".to_string()));

        let hash = hash_of(b"with-external-addr");
        handle.provide(&hash).await;

        let providers = handle.find_providers(&hash).await;
        assert_eq!(providers.len(), 1);
        // The placeholder `/ip4/127.0.0.1/tcp/0` that the DHT layer
        // writes by default must have been overwritten with the
        // configured external address.
        assert_eq!(
            providers[0].provider_addr,
            "/ip4/1.2.3.4/tcp/4001".to_string(),
        );
    }

    #[tokio::test]
    async fn dht_provide_without_external_addr_keeps_placeholder() {
        let handle = DhtHandle::new(DhtConfig::default()).await;

        let hash = hash_of(b"placeholder-addr");
        handle.provide(&hash).await;

        let providers = handle.find_providers(&hash).await;
        assert_eq!(providers.len(), 1);
        // No external address set → the DHT layer's placeholder is
        // preserved (callers downstream know to substitute it).
        assert_eq!(providers[0].provider_addr, "/ip4/127.0.0.1/tcp/0");
    }

    #[tokio::test]
    async fn dht_handle_clear_external_addr_preserves_records() {
        let handle = DhtHandle::new(DhtConfig::default()).await;
        handle.set_external_addr(Some("/ip4/9.9.9.9/tcp/9999".to_string()));

        let hash = hash_of(b"clear-addr");
        handle.provide(&hash).await;

        // Clearing the external addr must not retroactively rewrite
        // already-stored records. They keep the addr we patched.
        handle.set_external_addr(None);
        let providers = handle.find_providers(&hash).await;
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].provider_addr, "/ip4/9.9.9.9/tcp/9999");
    }

    #[tokio::test]
    async fn ipn_handle_resolve_without_publisher_returns_error() {
        // A read-only handle has no publisher, so resolving a name not
        // already cached must fail (NotFound).
        let handle = IpnHandle::new(IpnConfig::default(), NodeId::random());
        let result = handle.resolve("missing-name").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn ipn_handle_resolve_rejects_non_hex_value() {
        let handle = IpnHandle::new(IpnConfig::default(), NodeId::random());
        let record = IpnRecord::with_name_value("name".to_string(), "not-hex".to_string());
        handle.resolver().cache_record(record);
        let err = handle.resolve("name").await.unwrap_err();
        assert!(matches!(err, DhtError::HashParse(_)));
    }

    #[tokio::test]
    async fn ipn_handle_publish_requires_publisher() {
        let handle = IpnHandle::new(IpnConfig::default(), NodeId::random());
        let err = handle
            .publish("name", "value".to_string(), Duration::from_secs(60))
            .await
            .unwrap_err();
        assert!(matches!(err, IpnsError::NotAuthorized));
    }

    #[tokio::test]
    async fn ipn_handle_publish_default_uses_configured_ttl() {
        let secret = Arc::new(Ed25519SecretKey::generate());
        let name = secret.ipns_name();
        let publisher = Arc::new(IpnPublisher::new(secret.clone()));
        let cfg = IpnConfig {
            cache_ttl_secs: 3600,
            record_ttl_secs: 120,
        };
        let handle = IpnHandle::new(cfg, NodeId::random())
            .with_publisher(publisher.clone());

        let hash = hash_of(b"ipns-content");
        handle
            .publish_default(&name, hash.as_hex().to_string())
            .await
            .expect("publish_default");

        let record = publisher.get_local(&name).expect("record present");
        assert_eq!(record.ttl_secs, 120);
        // expires should be created + ttl (within a small drift).
        assert!(record.expires >= record.created + 120);
    }

    #[tokio::test]
    async fn ipn_handle_publisher_round_trip() {
        let secret = Arc::new(Ed25519SecretKey::generate());
        let name = secret.ipns_name();
        let publisher = Arc::new(IpnPublisher::new(secret.clone()));

        let handle = IpnHandle::new(IpnConfig::default(), NodeId::random())
            .with_publisher(publisher.clone());

        // Publish a content hash under the IPNS name.
        let hash = hash_of(b"ipns-content");
        handle
            .publish(
                &name,
                hash.as_hex().to_string(),
                Duration::from_secs(300),
            )
            .await
            .expect("publish");

        // Pre-seed the resolver cache with the same record so resolve works.
        let record = publisher.get_local(&name).expect("record present");
        handle.resolver().cache_record(record);

        let resolved = handle.resolve(&name).await.expect("resolve");
        assert_eq!(resolved, hash);
    }

    #[tokio::test]
    async fn ipn_record_signing_is_verifiable() {
        let secret = Ed25519SecretKey::generate();
        let pubkey_bytes: [u8; 32] = secret.public_key_bytes().as_slice().try_into().unwrap();
        let verifier = a3net_namespace::ipns::Ed25519Verifier::from_bytes(&pubkey_bytes).unwrap();

        let mut record = IpnRecord::with_name_value("hello".into(), hash_of(b"data").as_hex().to_string());
        record.sign(&secret).unwrap();
        assert!(record.verify_signature(&verifier));
    }

    #[tokio::test]
    async fn ipn_cleanup_drops_expired_entries() {
        let handle = IpnHandle::new(IpnConfig::default(), NodeId::random());

        // Build a record that's already expired.
        let mut record = IpnRecord::with_name_value("n".into(), hash_of(b"x").as_hex().to_string());
        record.created = 0;
        record.expires = 1;
        handle.resolver().cache_record(record);
        // cleanup should drop the entry because expires < now.
        handle.cleanup();
        assert!(handle.resolver().get_cached("n").is_none());
    }

    #[tokio::test]
    async fn ipn_handle_default_record_ttl_reflects_config() {
        let cfg = IpnConfig {
            cache_ttl_secs: 60,
            record_ttl_secs: 600,
        };
        let handle = IpnHandle::new(cfg, NodeId::random());
        assert_eq!(
            handle.default_record_ttl(),
            Duration::from_secs(600)
        );
    }

    // ──────────────── Metrics / observability tests ────────────────

    #[tokio::test]
    async fn dht_metrics_initial_values_are_zero() {
        let handle = DhtHandle::new(DhtConfig::default()).await;
        let m = handle.metrics();
        assert_eq!(m.provides_total, 0);
        assert_eq!(m.find_total, 0);
        assert_eq!(m.find_records_total, 0);
        assert_eq!(m.find_misses_total, 0);
        assert_eq!(m.last_find_latency_us, 0);
    }

    #[tokio::test]
    async fn dht_metrics_provide_counter_increments() {
        let handle = DhtHandle::new(DhtConfig::default()).await;
        handle.provide(&hash_of(b"a")).await;
        handle.provide(&hash_of(b"b")).await;
        handle.provide(&hash_of(b"c")).await;
        assert_eq!(handle.metrics().provides_total, 3);
    }

    #[tokio::test]
    async fn dht_metrics_find_records_hit_and_miss() {
        let handle = DhtHandle::new(DhtConfig::default()).await;
        let known = hash_of(b"known");
        handle.provide(&known).await;

        // Hit (1 record back).
        let _ = handle.find_providers(&known).await;
        // Miss.
        let _ = handle.find_providers(&hash_of(b"missing")).await;

        let m = handle.metrics();
        assert_eq!(m.find_total, 2);
        assert_eq!(m.find_records_total, 1);
        assert_eq!(m.find_misses_total, 1);
        // Latency captured (instant on the local store).
        assert!(m.last_find_latency_us < 1_000_000);
    }

    #[tokio::test]
    async fn dht_metrics_display_is_human_readable() {
        let handle = DhtHandle::new(DhtConfig::default()).await;
        handle.provide(&hash_of(b"x")).await;
        let _ = handle.find_providers(&hash_of(b"x")).await;
        let stats = handle.stats();
        let display = format!("{stats}");
        // Display must mention the metric fields the operator cares about.
        assert!(display.contains("provides="));
        assert!(display.contains("finds="));
    }

    #[tokio::test]
    async fn dht_error_display_distinguishes_variants() {
        let op = DhtError::Operation("boom".to_string());
        assert!(format!("{op}").contains("boom"));
        let cfg = DhtError::Config("bad".to_string());
        assert!(format!("{cfg}").contains("bad"));
        let hp = DhtError::HashParse("xyz".to_string());
        assert!(format!("{hp}").contains("xyz"));
    }
}
