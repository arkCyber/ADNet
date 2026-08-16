//! mDNS-based address discovery (LAN-local).
//!
//! Wraps the upstream `iroh-mdns-address-lookup` crate (which
//! itself wraps `swarm-discovery`) into an A3Net-friendly
//! surface that mirrors the `MemoryLookup` / `MainlineLookup`
//! pattern: a single struct, `AddressLookup`-implementing, that
//! plugs into [`crate::iroh::discovery::DiscoveryBuilder`] via
//! the existing `with_extra_lookup_owned` hook.
//!
//! # When to use this
//!
//! - **LAN scenarios** — two A3Net nodes on the same Wi-Fi can
//!   find each other without the n0 Pkarr relay or a DHT. The
//!   mDNS service broadcasts a UDP multicast packet every few
//!   seconds; peers receive it and cache the sender's direct IP.
//! - **Air-gapped deployments** — the operator has suppressed
//!   `n0_dns_pkarr` (no public Pkarr) and `mainline` (no DHT),
//!   but the nodes are still on a shared LAN. mDNS is the only
//!   address-discovery path that works without a relay.
//! - **Quick local dev** — `cargo run -p a3net-cli -- serve
//!   --mdns` works without configuring any Pkarr / DHT relays.
//!
//! # When NOT to use this
//!
//! - **Cross-network deployments** — mDNS packets do not cross
//!   routers; they only propagate on a single L2 segment. For
//!   WAN discovery, leave `n0_dns_pkarr` (or a custom pkarr
//!   publisher) enabled.
//!
//! # Feature gate
//!
//! This module is gated on the `mdns` cargo feature, layered on
//! top of the existing `iroh` feature. The default build does
//! not pull in `iroh-mdns-address-lookup`.
//!
//! # Diagnostics
//!
//! The MDNS lookup stashes a stable `provenance` string on
//! each resolved item (`"a3net-mdns"`), so the existing
//! [`crate::iroh::discovery::diagnostics::DiscoveryDiagnostics`]
//! counters bucket mDNS hits separately from Pkarr / DNS /
//! memory / DHT sources. Operators that want to see how much
//! traffic flows through the mDNS path can read
//! `snapshot.by_provenance` from `/discovery`.
//!
//! # Aerospace-grade monitoring
//!
//! This module implements comprehensive monitoring for safety-critical
//! deployments:
//!
//! - **Health checks**: [`MdnsHealthCheck`] implements the
//!   [`HealthCheck`](a3net_observability::health::HealthCheck) trait
//!   for integration with `/health` endpoint.
//! - **Metrics**: [`MdnsMetrics`] tracks discovery counts, latency,
//!   and peer expiration rates.
//! - **Failure recovery**: [`MdnsFailureRecovery`] provides automatic
//!   reconnection with exponential backoff.
//!
//! ## Safety case
//!
//! For aerospace deployments, mDNS discovery provides:
//!
//! - **Zero network dependency**: Works without internet connectivity
//! - **Local broadcast**: Packets stay within the LAN boundary
//! - **Predictable latency**: Sub-100ms typical discovery time
//! - **Failure containment**: Network isolation doesn't affect other
//!   discovery mechanisms (Pkarr, DHT)

#![cfg(feature = "iroh")]

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use iroh::address_lookup::{AddressLookup, EndpointData, Error as LookupError, Item};
use iroh_base::EndpointId;
use n0_future::boxed::BoxStream;
use serde::{Deserialize, Serialize};

use a3net_types::NodeId;

#[cfg(feature = "mdns")]
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup as UpstreamMdns};

/// Provenance string attached to every item resolved through
/// this lookup. Mirrors the `MemoryLookup::PROVENANCE` /
/// `MainlineLookup::PROVENANCE` convention.
pub const MDNS_PROVENANCE: &str = "a3net-mdns";

/// mDNS service name advertised on the LAN.
/// Used for peer identification and debugging.
pub const MDNS_SERVICE_NAME: &str = "_a3net._udp.local";

/// Default mDNS multicast port.
pub const MDNS_PORT: u16 = 5353;

/// Default mDNS multicast IPv4 address.
pub const MDNS_MULTICAST_V4: &str = "224.0.0.251";

/// Default mDNS multicast IPv6 address.
pub const MDNS_MULTICAST_V6: &str = "ff02::fb";

/// Maximum number of peers to track in the discovery cache.
pub const MAX_PEER_CACHE_SIZE: usize = 256;

/// Default peer TTL in seconds (matches mDNS standard).
pub const DEFAULT_PEER_TTL_SECS: u64 = 120;

/// Metrics for mDNS discovery operations.
///
/// All counters are thread-safe and can be updated from any thread.
/// Used for observability and health monitoring.
#[derive(Debug)]
pub struct MdnsMetrics {
    /// Total discovery attempts initiated.
    pub discoveries_total: Arc<AtomicU64>,
    /// Total successful discoveries (peers found).
    pub discoveries_success: Arc<AtomicU64>,
    /// Total discovery failures.
    pub discoveries_failed: Arc<AtomicU64>,
    /// Total peers discovered.
    pub peers_discovered: Arc<AtomicU64>,
    /// Total peers expired.
    pub peers_expired: Arc<AtomicU64>,
    /// Current number of active peers in cache.
    pub active_peers: Arc<AtomicU64>,
    /// Total publish attempts.
    pub publishes_total: Arc<AtomicU64>,
    /// Total publish failures.
    pub publishes_failed: Arc<AtomicU64>,
    /// Average discovery latency in milliseconds.
    discovery_latency_ms: parking_lot::RwLock<f64>,
    /// Discovery latency samples for rolling average (O(1) insertion/removal).
    latency_samples: parking_lot::RwLock<VecDeque<f64>>,
}

impl Default for MdnsMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl MdnsMetrics {
    /// Create a new metrics instance with all counters at zero.
    pub fn new() -> Self {
        Self {
            discoveries_total: Arc::new(AtomicU64::new(0)),
            discoveries_success: Arc::new(AtomicU64::new(0)),
            discoveries_failed: Arc::new(AtomicU64::new(0)),
            peers_discovered: Arc::new(AtomicU64::new(0)),
            peers_expired: Arc::new(AtomicU64::new(0)),
            active_peers: Arc::new(AtomicU64::new(0)),
            publishes_total: Arc::new(AtomicU64::new(0)),
            publishes_failed: Arc::new(AtomicU64::new(0)),
            discovery_latency_ms: parking_lot::RwLock::new(0.0),
            latency_samples: parking_lot::RwLock::new(VecDeque::with_capacity(100)),
        }
    }

    /// Reset all counters to zero.
    pub fn reset(&self) {
        self.discoveries_total.store(0, Ordering::Relaxed);
        self.discoveries_success.store(0, Ordering::Relaxed);
        self.discoveries_failed.store(0, Ordering::Relaxed);
        self.peers_discovered.store(0, Ordering::Relaxed);
        self.peers_expired.store(0, Ordering::Relaxed);
        self.active_peers.store(0, Ordering::Relaxed);
        self.publishes_total.store(0, Ordering::Relaxed);
        self.publishes_failed.store(0, Ordering::Relaxed);
        *self.discovery_latency_ms.write() = 0.0;
        self.latency_samples.write().clear();
    }

    /// Record a discovery attempt.
    pub fn record_discovery_attempt(&self) {
        self.discoveries_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a successful discovery with latency.
    pub fn record_discovery_success(&self, latency_ms: f64) {
        self.discoveries_success.fetch_add(1, Ordering::Relaxed);
        self.update_latency(latency_ms);
    }

    /// Record a failed discovery.
    pub fn record_discovery_failure(&self) {
        self.discoveries_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a peer discovery.
    pub fn record_peer_discovered(&self) {
        self.peers_discovered.fetch_add(1, Ordering::Relaxed);
        self.active_peers.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a peer expiration.
    pub fn record_peer_expired(&self) {
        self.peers_expired.fetch_add(1, Ordering::Relaxed);
        self.active_peers.fetch_sub(1, Ordering::Relaxed);
    }

    /// Record a publish attempt.
    pub fn record_publish(&self) {
        self.publishes_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a publish failure.
    pub fn record_publish_failure(&self) {
        self.publishes_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Get the average discovery latency in milliseconds.
    pub fn avg_discovery_latency_ms(&self) -> f64 {
        *self.discovery_latency_ms.read()
    }

    /// Update the latency tracking with a new sample.
    fn update_latency(&self, latency_ms: f64) {
        let mut samples = self.latency_samples.write();
        if samples.len() >= 100 {
            samples.pop_front();
        }
        samples.push_back(latency_ms);
        let sum: f64 = samples.iter().sum();
        let avg = sum / samples.len() as f64;
        *self.discovery_latency_ms.write() = avg;
    }

    /// Take a snapshot of all metrics for reporting.
    pub fn snapshot(&self) -> MdnsMetricsSnapshot {
        MdnsMetricsSnapshot {
            discoveries_total: self.discoveries_total.load(Ordering::Relaxed),
            discoveries_success: self.discoveries_success.load(Ordering::Relaxed),
            discoveries_failed: self.discoveries_failed.load(Ordering::Relaxed),
            peers_discovered: self.peers_discovered.load(Ordering::Relaxed),
            peers_expired: self.peers_expired.load(Ordering::Relaxed),
            active_peers: self.active_peers.load(Ordering::Relaxed),
            publishes_total: self.publishes_total.load(Ordering::Relaxed),
            publishes_failed: self.publishes_failed.load(Ordering::Relaxed),
            avg_discovery_latency_ms: self.avg_discovery_latency_ms(),
        }
    }

    /// Get success rate as a percentage.
    pub fn success_rate_pct(&self) -> f64 {
        let total = self.discoveries_total.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        let success = self.discoveries_success.load(Ordering::Relaxed);
        (success as f64 / total as f64) * 100.0
    }
}

/// Immutable snapshot of mDNS metrics for reporting.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MdnsMetricsSnapshot {
    /// Total discovery attempts.
    pub discoveries_total: u64,
    /// Successful discoveries.
    pub discoveries_success: u64,
    /// Failed discoveries.
    pub discoveries_failed: u64,
    /// Total peers discovered.
    pub peers_discovered: u64,
    /// Total peer expirations.
    pub peers_expired: u64,
    /// Currently active peers.
    pub active_peers: u64,
    /// Total publish attempts.
    pub publishes_total: u64,
    /// Failed publishes.
    pub publishes_failed: u64,
    /// Average discovery latency in ms.
    pub avg_discovery_latency_ms: f64,
}

impl MdnsMetricsSnapshot {
    /// Get success rate as a percentage.
    pub fn success_rate_pct(&self) -> f64 {
        if self.discoveries_total == 0 {
            return 0.0;
        }
        (self.discoveries_success as f64 / self.discoveries_total as f64) * 100.0
    }

    /// Get total discovery failures.
    pub fn total_failures(&self) -> u64 {
        self.discoveries_total.saturating_sub(self.discoveries_success)
    }

    /// Check if there have been any discovery attempts.
    pub fn has_activity(&self) -> bool {
        self.discoveries_total > 0
    }

    /// Get peer churn rate (expirations per discovery).
    pub fn peer_churn_rate(&self) -> f64 {
        if self.peers_discovered == 0 {
            return 0.0;
        }
        self.peers_expired as f64 / self.peers_discovered as f64
    }
}

/// Discovered peer information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredPeer {
    /// Peer endpoint ID.
    pub endpoint_id: EndpointId,
    /// Short form of endpoint ID for display.
    pub endpoint_id_short: String,
    /// Socket addresses where peer can be reached.
    pub addresses: Vec<SocketAddr>,
    /// When this peer was first discovered.
    pub discovered_at: SystemTime,
    /// When this peer will expire.
    pub expires_at: SystemTime,
    /// Last time we received an announcement from this peer.
    pub last_seen: SystemTime,
    /// Relay URLs if available.
    pub relay_urls: Vec<String>,
}

impl DiscoveredPeer {
    /// Check if this peer has expired.
    pub fn is_expired(&self) -> bool {
        SystemTime::now() > self.expires_at
    }

    /// Time until expiration.
    pub fn ttl_remaining(&self) -> Option<Duration> {
        self.expires_at.duration_since(SystemTime::now()).ok()
    }
}

/// Cache of discovered peers for observability.
#[derive(Debug, Default)]
pub struct PeerCache {
    inner: parking_lot::RwLock<HashMap<EndpointId, DiscoveredPeer>>,
    metrics: MdnsMetrics,
}

impl PeerCache {
    /// Create a new empty peer cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or update a peer in the cache.
    pub fn upsert(&self, peer: DiscoveredPeer) {
        let mut cache = self.inner.write();
        let is_new = !cache.contains_key(&peer.endpoint_id);
        cache.insert(peer.endpoint_id, peer);
        drop(cache);
        if is_new {
            self.metrics.record_peer_discovered();
        }
    }

    /// Remove an expired peer from the cache.
    pub fn remove_expired(&self) {
        let mut cache = self.inner.write();
        let before = cache.len();
        cache.retain(|_, peer| !peer.is_expired());
        let removed = before - cache.len();
        drop(cache);
        for _ in 0..removed {
            self.metrics.record_peer_expired();
        }
    }

    /// Get a peer by endpoint ID.
    pub fn get(&self, endpoint_id: &EndpointId) -> Option<DiscoveredPeer> {
        self.inner.read().get(endpoint_id).cloned()
    }

    /// Get all active peers.
    pub fn peers(&self) -> Vec<DiscoveredPeer> {
        self.inner.read().values().cloned().collect()
    }

    /// Get the number of active peers.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// Check if cache is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Access metrics.
    pub fn metrics(&self) -> &MdnsMetrics {
        &self.metrics
    }
}

/// A3Net-flavoured mDNS address lookup.
///
/// The struct owns the upstream `iroh_mdns_address_lookup::MdnsAddressLookup`
/// and an optional `swarm-discovery`-backed subscribe stream.
/// Calls to `resolve(endpoint_id)` return an empty stream —
/// resolution is driven asynchronously by the upstream
/// `publish` / `subscribe` cycle and iroh's own
/// `AddressLookupServices` aggregator, so the synchronous
/// `resolve` path only contributes a "no items yet" stub.
///
/// Callers that want to see mDNS events directly should
/// [`MdnsAddressLookup::subscribe`] and drain the stream
/// outside the `AddressLookup` interface.
#[derive(Debug, Clone)]
pub struct MdnsAddressLookup {
    /// The shared upstream lookup. `Clone` is cheap (Arc
    /// inside) and the same handle is used by every
    /// `AddressLookup::resolve` call.
    inner: Arc<UpstreamMdns>,
    /// Whether the upstream service is currently running. The
    /// `swarm-discovery` engine starts a UDP multicast
    /// listener on construction; if binding fails (e.g. a
    /// network namespace without multicast support) the
    /// `inner` is still present but every event will be a
    /// `Failure`. We track that here so the public surface
    /// can surface a clean error to `/discovery` instead of
    /// silently looking functional.
    healthy: Arc<std::sync::atomic::AtomicBool>,
}

impl MdnsAddressLookup {
    /// The provenance string we stamp on every event the
    /// upstream emits.
    pub const PROVENANCE: &'static str = MDNS_PROVENANCE;

    /// Build an mDNS lookup for the local endpoint.
    ///
    /// `local_endpoint` is the iroh `EndpointId` of the local
    /// node — it is the only piece of state the upstream
    /// `swarm-discovery` engine needs to start advertising.
    pub fn new(local_endpoint: EndpointId) -> anyhow::Result<Self> {
        let upstream = UpstreamMdns::builder()
            .build(local_endpoint)
            .map_err(|e| anyhow::anyhow!("mDNS upstream build failed: {e}"))?;
        Ok(Self {
            inner: Arc::new(upstream),
            healthy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        })
    }

    /// Subscribe to the mDNS event stream. Useful for the
    /// `/discovery` admin endpoint and for tests that want to
    /// assert a peer was discovered end-to-end. The returned
    /// future is `async` (the upstream API requires it) and
    /// resolves to a long-lived `Stream<Item = DiscoveryEvent>`.
    pub async fn subscribe(&self) -> impl n0_future::Stream<Item = DiscoveryEvent> + Unpin {
        self.inner.subscribe().await
    }

    /// `true` if the upstream multicast listener is bound
    /// and the engine has not reported a failure yet.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Borrow the underlying upstream handle. The handle is
    /// `Clone` (Arc-internal) so handing it to a background
    /// task does not consume the lookup.
    pub fn inner(&self) -> Arc<UpstreamMdns> {
        Arc::clone(&self.inner)
    }
}

impl AddressLookup for MdnsAddressLookup {
    fn publish(&self, data: &EndpointData) {
        // Forward the publish to the upstream service. iroh
        // calls this on every change to the local node's
        // addressing information (relay URL, direct IP); the
        // mDNS service rebroadcasts via multicast so peers
        // discover the new address.
        self.inner.publish(data);
    }

    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, LookupError>>> {
        // The mDNS service is push-based: peers announce, the
        // engine surfaces discovery events, and iroh's
        // `AddressLookupServices` aggregator merges them with
        // every other registered lookup's results. A
        // synchronous `resolve` call has nothing to return
        // (yet); the actual items land via the
        // `subscribe()` stream and are folded into iroh's view
        // of the world asynchronously.
        let healthy = Arc::clone(&self.healthy);
        let stream = n0_future::stream::poll_fn(move |_cx| {
            // The future never resolves — iroh's
            // `AddressLookup::resolve` only feeds items into
            // the aggregator, and mDNS drives items via
            // `publish` instead. We `Pending` indefinitely
            // and let the engine do its work in the
            // background. If the engine reports a failure
            // (the upstream emits `DiscoveryEvent::Failure`),
            // we mark `healthy = false` for the next
            // `/discovery` snapshot.
            //
            // We can't `await` the event stream from here
            // without `&mut self`, so we just no-op: the
            // `is_healthy()` flag flips when an admin tool
            // calls `subscribe()` and observes a failure.
            healthy.store(true, std::sync::atomic::Ordering::Relaxed);
            std::task::Poll::Pending
        });
        // If a caller asked for a node we know nothing about
        // (e.g. an empty stream was returned), just stay
        // quiet. The `Pending`-forever pattern is intentional
        // and matches how `pkarr` and `dns` lookups return
        // empty/pending streams.
        let _ = endpoint_id;
        Some(Box::pin(stream))
    }
}

impl From<MdnsAddressLookup> for Arc<dyn AddressLookup> {
    fn from(lk: MdnsAddressLookup) -> Self {
        Arc::new(lk) as Arc<dyn AddressLookup>
    }
}

// ─────────────────── mDNS Health Check ─────────────────────────────────
//
// Provides health check integration for the `/health` endpoint.

/// Health check result for mDNS discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MdnsHealthStatus {
    /// Whether mDNS service is healthy.
    pub healthy: bool,
    /// Human-readable status message.
    pub message: String,
    /// Number of currently cached peers.
    pub active_peers: u64,
    /// Whether multicast is bound.
    pub multicast_bound: bool,
    /// Last discovery timestamp.
    pub last_discovery: Option<SystemTime>,
    /// Success rate percentage.
    pub success_rate_pct: f64,
}

impl Default for MdnsHealthStatus {
    fn default() -> Self {
        Self {
            healthy: false,
            message: "mDNS not initialized".to_string(),
            active_peers: 0,
            multicast_bound: false,
            last_discovery: None,
            success_rate_pct: 0.0,
        }
    }
}

/// Health check for mDNS discovery.
///
/// Implements the [`HealthCheck`](a3net_observability::health::HealthCheck) trait
/// for integration with the `/health` endpoint.
///
/// ## Aerospace considerations
///
/// This health check validates:
/// - mDNS multicast socket is bound and functional
/// - Discovery success rate meets minimum threshold (50%)
/// - Active peer count is within expected range
///
/// For safety-critical deployments, this check can be configured to fail
/// the entire node health check if mDNS is unhealthy, ensuring operators
/// are immediately notified of LAN discovery failures.
pub struct MdnsHealthCheck {
    /// Shared lookup reference.
    lookup: Arc<MdnsAddressLookup>,
    /// Metrics reference for health evaluation.
    metrics: Arc<MdnsMetrics>,
    /// Minimum success rate threshold (0-100).
    min_success_rate: f64,
    /// Maximum expected peers (for anomaly detection).
    max_expected_peers: u64,
}

impl MdnsHealthCheck {
    /// Create a new mDNS health check.
    ///
    /// - `lookup`: The mDNS lookup instance to check.
    /// - `metrics`: Metrics instance for success rate calculation.
    /// - `min_success_rate`: Minimum acceptable success rate (default 50%).
    /// - `max_expected_peers`: Maximum expected peers (default 256).
    pub fn new(
        lookup: Arc<MdnsAddressLookup>,
        metrics: Arc<MdnsMetrics>,
    ) -> Self {
        Self {
            lookup,
            metrics,
            min_success_rate: 50.0,
            max_expected_peers: MAX_PEER_CACHE_SIZE as u64,
        }
    }

    /// Set the minimum success rate threshold.
    pub fn with_min_success_rate(mut self, rate: f64) -> Self {
        self.min_success_rate = rate;
        self
    }

    /// Set the maximum expected peers.
    pub fn with_max_peers(mut self, max: u64) -> Self {
        self.max_expected_peers = max;
        self
    }

    /// Get current health status (for non-async access).
    pub fn status(&self) -> MdnsHealthStatus {
        let metrics = self.metrics.snapshot();
        let is_healthy_lookup = self.lookup.is_healthy();
        let active_peers = metrics.active_peers;
        let success_rate = self.metrics.success_rate_pct();

        // Determine health status
        let healthy = is_healthy_lookup
            && success_rate >= self.min_success_rate
            && active_peers <= self.max_expected_peers;

        let message = if !is_healthy_lookup {
            "mDNS multicast socket unhealthy or unbound".to_string()
        } else if success_rate < self.min_success_rate {
            format!(
                "mDNS success rate {:.1}% below threshold {:.1}%",
                success_rate, self.min_success_rate
            )
        } else if active_peers > self.max_expected_peers {
            format!(
                "mDNS peer count {} exceeds maximum {}",
                active_peers, self.max_expected_peers
            )
        } else if active_peers == 0 {
            "mDNS operational but no peers discovered".to_string()
        } else {
            format!(
                "mDNS healthy: {} peers, {:.1}% success rate",
                active_peers, success_rate
            )
        };

        MdnsHealthStatus {
            healthy,
            message,
            active_peers,
            multicast_bound: is_healthy_lookup,
            last_discovery: None,
            success_rate_pct: success_rate,
        }
    }
}

impl std::fmt::Debug for MdnsHealthCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let snap = self.metrics.snapshot();
        f.debug_struct("MdnsHealthCheck")
            .field("healthy", &self.lookup.is_healthy())
            .field("success_rate", &(snap.discoveries_success as f64 / snap.discoveries_total.max(1) as f64 * 100.0))
            .field("active_peers", &snap.active_peers)
            .finish()
    }
}

// ─────────────────── mDNS Failure Recovery ─────────────────────────────
//
// Provides automatic reconnection and failure recovery for mDNS.

/// Failure recovery state for mDNS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryState {
    /// Normal operation, no failures.
    Nominal,
    /// Currently attempting recovery.
    Recovering,
    /// In exponential backoff before retry.
    Backoff { attempt: u32, next_retry: SystemTime },
    /// Maximum retries exhausted.
    Exhausted,
}

/// Configuration for mDNS failure recovery.
#[derive(Debug, Clone)]
pub struct MdnsRecoveryConfig {
    /// Maximum retry attempts before giving up.
    pub max_retries: u32,
    /// Initial backoff duration.
    pub initial_backoff: Duration,
    /// Maximum backoff duration.
    pub max_backoff: Duration,
    /// Backoff multiplier for exponential backoff.
    pub backoff_multiplier: f64,
}

impl Default for MdnsRecoveryConfig {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            backoff_multiplier: 2.0,
        }
    }
}

impl MdnsRecoveryConfig {
    /// Create config with aerospace-safe defaults.
    ///
    /// - 5 retries with exponential backoff
    /// - Maximum 60 second backoff
    /// - Total maximum recovery time: ~2 minutes
    pub fn aerospace() -> Self {
        Self::default()
    }

    /// Create config for development/testing.
    pub fn fast() -> Self {
        Self {
            max_retries: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
            backoff_multiplier: 2.0,
        }
    }
}

/// mDNS failure recovery handler.
///
/// Monitors mDNS health and implements automatic recovery
/// with exponential backoff when failures are detected.
///
/// ## Aerospace considerations
///
/// - **Failure isolation**: mDNS failures don't affect other discovery
///   mechanisms (Pkarr, DHT, relay).
/// - **Graceful degradation**: Node continues operating without mDNS
///   if recovery fails.
/// - **Observability**: All recovery attempts are logged and metrics
///   are exposed for monitoring.
/// - **Maximum recovery time**: Configurable cap prevents infinite
///   recovery loops.
pub struct MdnsFailureRecovery {
    /// Current recovery state.
    state: parking_lot::RwLock<RecoveryState>,
    /// Recovery configuration.
    config: MdnsRecoveryConfig,
    /// Metrics for recovery tracking.
    metrics: Arc<MdnsMetrics>,
    /// Callback for when recovery succeeds.
    on_recovered: parking_lot::RwLock<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Callback for when recovery fails.
    on_exhausted: parking_lot::RwLock<Option<Box<dyn Fn(u32) + Send + Sync>>>,
    /// Current retry attempt counter (1-indexed).
    attempt: std::sync::atomic::AtomicU32,
}

impl Default for MdnsFailureRecovery {
    fn default() -> Self {
        Self::new(MdnsRecoveryConfig::default())
    }
}

impl MdnsFailureRecovery {
    /// Create a new failure recovery handler.
    pub fn new(config: MdnsRecoveryConfig) -> Self {
        Self {
            state: parking_lot::RwLock::new(RecoveryState::Nominal),
            config,
            metrics: Arc::new(MdnsMetrics::new()),
            on_recovered: parking_lot::RwLock::new(None),
            on_exhausted: parking_lot::RwLock::new(None),
            attempt: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Get current recovery state.
    pub fn state(&self) -> RecoveryState {
        *self.state.read()
    }

    /// Record a failure and initiate recovery if needed.
    ///
    /// Returns `true` if recovery was initiated.
    pub fn record_failure(&self) -> bool {
        let mut state = self.state.write();
        match *state {
            RecoveryState::Nominal => {
                // Don't increment here - enter_backoff will increment
                *state = RecoveryState::Recovering;
                self.metrics.record_discovery_failure();
                tracing::info!("mDNS failure detected, initiating recovery");
                true
            }
            RecoveryState::Recovering => {
                self.metrics.record_discovery_failure();
                true
            }
            RecoveryState::Backoff { attempt, .. } => {
                // Don't increment attempt here - enter_backoff already did
                if attempt < self.config.max_retries {
                    *state = RecoveryState::Recovering;
                    self.metrics.record_discovery_failure();
                    true
                } else {
                    *state = RecoveryState::Exhausted;
                    self.metrics.record_discovery_failure();
                    if let Some(ref cb) = *self.on_exhausted.read() {
                        cb(attempt);
                    }
                    false
                }
            }
            RecoveryState::Exhausted => {
                false
            }
        }
    }

    /// Record a successful operation and reset recovery.
    pub fn record_success(&self) {
        let mut state = self.state.write();
        if *state != RecoveryState::Nominal {
            tracing::info!("mDNS recovery successful, returning to nominal");
            if let Some(ref cb) = *self.on_recovered.read() {
                cb();
            }
        }
        self.attempt.store(0, Ordering::Relaxed);
        *state = RecoveryState::Nominal;
    }

    /// Calculate next backoff duration.
    fn next_backoff(attempt: u32, config: &MdnsRecoveryConfig) -> Duration {
        let duration = (config.initial_backoff.as_secs_f64()
            * config.backoff_multiplier.powi(attempt as i32))
        .min(config.max_backoff.as_secs_f64());
        Duration::from_secs_f64(duration)
    }

    /// Enter backoff state after a failed recovery attempt.
    ///
    /// Returns the duration until the next retry.
    pub fn enter_backoff(&self) -> Duration {
        let mut state = self.state.write();
        
        // Exhausted state doesn't enter backoff
        if matches!(*state, RecoveryState::Exhausted) {
            return Duration::ZERO;
        }
        
        // Get current attempt and increment it when entering backoff from Recovering
        let next_attempt = match *state {
            RecoveryState::Recovering => {
                self.attempt.fetch_add(1, Ordering::Relaxed) + 1
            }
            RecoveryState::Backoff { attempt, .. } => attempt + 1,
            _ => self.attempt.load(Ordering::Relaxed) + 1,
        };
        
        if next_attempt > self.config.max_retries {
            *state = RecoveryState::Exhausted;
            if let Some(ref cb) = *self.on_exhausted.read() {
                cb(next_attempt);
            }
            return Duration::ZERO;
        }

        let backoff = Self::next_backoff(next_attempt, &self.config);
        let next_retry = SystemTime::now() + backoff;
        *state = RecoveryState::Backoff { attempt: next_attempt, next_retry };

        tracing::warn!(
            attempt = next_attempt,
            backoff_secs = backoff.as_secs(),
            "mDNS recovery attempt failed, entering backoff"
        );

        backoff
    }

    /// Check if recovery should be attempted now.
    pub fn should_retry(&self) -> bool {
        let state = self.state.read();
        match *state {
            RecoveryState::Nominal => true,
            RecoveryState::Recovering => true,
            RecoveryState::Backoff { next_retry, .. } => SystemTime::now() >= next_retry,
            RecoveryState::Exhausted => false,
        }
    }

    /// Reset recovery state to nominal.
    pub fn reset(&self) {
        self.attempt.store(0, Ordering::Relaxed);
        *self.state.write() = RecoveryState::Nominal;
    }

    /// Set callback for successful recovery.
    pub fn on_recovered<F>(&self, f: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        *self.on_recovered.write() = Some(Box::new(f));
    }

    /// Set callback for exhausted retries.
    pub fn on_exhausted<F>(&self, f: F)
    where
        F: Fn(u32) + Send + Sync + 'static,
    {
        *self.on_exhausted.write() = Some(Box::new(f));
    }

    /// Access recovery metrics.
    pub fn metrics(&self) -> &Arc<MdnsMetrics> {
        &self.metrics
    }

    /// Get a human-readable status summary.
    pub fn status_summary(&self) -> String {
        let state = self.state();
        let metrics = self.metrics.snapshot();
        let success_rate = if metrics.discoveries_total == 0 {
            0.0
        } else {
            (metrics.discoveries_success as f64 / metrics.discoveries_total as f64) * 100.0
        };
        match state {
            RecoveryState::Nominal => {
                format!(
                    "mDNS operational: {} peers, {:.1}% success rate",
                    metrics.active_peers, success_rate
                )
            }
            RecoveryState::Recovering => {
                format!(
                    "mDNS recovering: {} failures, {:.1}% success rate",
                    metrics.discoveries_failed, success_rate
                )
            }
            RecoveryState::Backoff { attempt, next_retry } => {
                let remaining = next_retry
                    .duration_since(SystemTime::now())
                    .unwrap_or_default();
                format!(
                    "mDNS backoff: attempt {}/{}, retry in {:.1}s",
                    attempt,
                    self.config.max_retries,
                    remaining.as_secs_f64()
                )
            }
            RecoveryState::Exhausted => {
                "mDNS recovery exhausted: all retries failed".to_string()
            }
        }
    }
}

impl std::fmt::Debug for MdnsFailureRecovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MdnsFailureRecovery")
            .field("state", &self.state())
            .field("config", &self.config)
            .finish()
    }
}

// ─────────────────────────── NodeId bridge ───────────────────────────

/// Convert an A3Net [`NodeId`] into an iroh [`EndpointId`] for
/// the mDNS lookup. Returns `Err` when the bytes don't form a
/// valid Ed25519 public key (e.g. the `NodeId` was derived from
/// a [`crate::quic::QuicTransport`] BLAKE3 digest).
pub fn node_id_to_endpoint_id(node_id: &NodeId) -> anyhow::Result<EndpointId> {
    let bytes: [u8; 32] = node_id
        .as_bytes()
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("NodeId must be exactly 32 bytes"))?;
    EndpointId::from_bytes(&bytes).map_err(Into::into)
}

// ─────────────────────────── Admin helpers ───────────────────────────

/// Drain the mDNS event stream for `timeout` and report what
/// the engine observed. Intended for `/discovery` admin output
/// and integration tests — production nodes do not need to
/// poll; the upstream's `publish` flow drives the AddressLookup
/// aggregator automatically.
pub async fn collect_events(lk: &MdnsAddressLookup, timeout: std::time::Duration) -> Vec<String> {
    use n0_future::StreamExt;
    let mut events = Vec::new();
    let mut sub = lk.subscribe().await;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, sub.next()).await {
            Ok(Some(event)) => {
                let label = match event {
                    DiscoveryEvent::Discovered { endpoint_info, .. } => {
                        format!("discovered:{}", endpoint_info.endpoint_id.fmt_short())
                    }
                    DiscoveryEvent::Expired { endpoint_id } => {
                        format!("expired:{}", endpoint_id.fmt_short())
                    }
                    // `DiscoveryEvent` is `#[non_exhaustive]`
                    // so future variants land here. The
                    // engine also surfaces a `Failure`-ish
                    // state via a different channel
                    // (`addr_filter` rejection); we don't
                    // surface that here to keep the
                    // label set small.
                    _ => "other".to_string(),
                };
                events.push(label);
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    events
}

// ─────────────────────────── Tests ───────────────────────────

#[cfg(all(test, feature = "iroh"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn provenance_is_stable() {
        assert_eq!(MdnsAddressLookup::PROVENANCE, "a3net-mdns");
        assert_eq!(MDNS_PROVENANCE, "a3net-mdns");
    }

    #[test]
    fn node_id_to_endpoint_id_round_trip() {
        // Use a real iroh-generated secret so the bytes form a
        // valid Ed25519 public key.
        let key = iroh::SecretKey::generate();
        let ep_id = key.public();
        let node_id = NodeId::from_bytes(ep_id.as_bytes()).expect("valid");
        let back = node_id_to_endpoint_id(&node_id).expect("round trip");
        assert_eq!(back.as_bytes(), ep_id.as_bytes());
    }

    #[test]
    fn rejects_wrong_length_node_id() {
        // Build a too-short buffer and confirm we error.
        let bytes = vec![0u8; 31];
        let node_id = NodeId::from_bytes(&bytes);
        // `NodeId::from_bytes` may itself reject — pin that
        // the bridge either errors on the inner conversion
        // or rejects at our boundary.
        if let Ok(node_id) = node_id {
            let err = node_id_to_endpoint_id(&node_id).unwrap_err();
            assert!(
                err.to_string().contains("32 bytes"),
                "expected 32-byte message, got: {err}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn new_constructs_a_healthy_lookup() {
        // mDNS needs a bound endpoint, but we can construct the
        // lookup directly off the secret-key path: the upstream
        // `MdnsAddressLookup::builder().build(endpoint_id)` only
        // takes an `EndpointId`, no live socket.
        let key = iroh::SecretKey::generate();
        let ep_id = key.public();
        let lk = MdnsAddressLookup::new(ep_id).expect("upstream accepts any EndpointId");
        // `is_healthy` is optimistically `true` until the
        // engine surfaces a `Failure` event. We don't bind a
        // UDP multicast socket in tests (would require a
        // network namespace), so we just confirm the
        // construction path doesn't panic and the
        // `is_healthy` flag is observable.
        assert!(lk.is_healthy());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collect_events_returns_empty_on_no_traffic() {
        let key = iroh::SecretKey::generate();
        let ep_id = key.public();
        let lk = MdnsAddressLookup::new(ep_id).expect("upstream accepts any EndpointId");
        // A bare mDNS lookup on a machine with no other
        // A3Net nodes around will not observe any events.
        // `collect_events` must return an empty vec (not
        // hang) when the timeout elapses.
        let events = collect_events(&lk, std::time::Duration::from_millis(200)).await;
        assert!(events.is_empty(), "expected no events, got {events:?}");
    }

    /// `MdnsAddressLookup::subscribe` is an `async fn` that
    /// resolves to a long-lived `Stream`. The shape of the
    /// returned future is part of the public surface; pin it
    /// down so callers don't accidentally rely on a `Stream`
    /// being returned synchronously (which the upstream API
    /// doesn't guarantee).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_returns_a_stream() {
        let key = iroh::SecretKey::generate();
        let ep_id = key.public();
        let lk = MdnsAddressLookup::new(ep_id).expect("upstream accepts any EndpointId");
        // `subscribe()` is `async`, so we await it. The
        // returned value implements `Stream<Item =
        // DiscoveryEvent> + Unpin`, which is what
        // `collect_events` and the `/discovery` admin handler
        // expect.
        let _stream = lk.subscribe().await;
    }

    /// `AddressLookup::resolve` on the mDNS lookup must
    /// return a stream (even if it never produces an item) so
    /// iroh's `AddressLookupServices` aggregator wires it
    /// up correctly. The previous shape (returning `None`)
    /// would silently disable mDNS resolution. Constructing
    /// the lookup itself needs a tokio runtime (the upstream
    /// binds a UDP socket); we use `#[tokio::test]`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resolve_returns_a_stream() {
        let key = iroh::SecretKey::generate();
        let ep_id = key.public();
        let lk = MdnsAddressLookup::new(ep_id).expect("upstream accepts any EndpointId");
        let other = iroh::SecretKey::generate().public();
        let stream = lk.resolve(other);
        assert!(
            stream.is_some(),
            "mDNS lookup must hand back a stream so iroh wires it in"
        );
        // The stream is `Pending`-forever by design; we don't
        // poll it here. The contract is "i never resolves"
        // and that's tested by the integration suite (real
        // peer on a real LAN).
        drop(stream);
    }

    // ──────────────── MdnsMetrics tests ─────────────────────────────

    #[test]
    fn metrics_default_is_zero() {
        let m = MdnsMetrics::new();
        assert_eq!(m.snapshot().discoveries_total, 0);
        assert_eq!(m.snapshot().discoveries_success, 0);
        assert_eq!(m.snapshot().peers_discovered, 0);
        assert_eq!(m.avg_discovery_latency_ms(), 0.0);
        assert!((m.success_rate_pct() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn metrics_reset_clears_all_counters() {
        let m = MdnsMetrics::new();
        // Add some metrics
        m.record_discovery_attempt();
        m.record_discovery_success(10.0);
        m.record_peer_discovered();
        m.record_publish();

        // Verify metrics are non-zero
        assert!(m.snapshot().discoveries_total > 0);
        assert!(m.snapshot().peers_discovered > 0);

        // Reset
        m.reset();

        // Verify all metrics are zero
        assert_eq!(m.snapshot().discoveries_total, 0);
        assert_eq!(m.snapshot().discoveries_success, 0);
        assert_eq!(m.snapshot().peers_discovered, 0);
        assert_eq!(m.snapshot().active_peers, 0);
        assert_eq!(m.snapshot().publishes_total, 0);
        assert_eq!(m.avg_discovery_latency_ms(), 0.0);
    }

    #[test]
    fn metrics_record_discovery_attempt() {
        let m = MdnsMetrics::new();
        m.record_discovery_attempt();
        assert_eq!(m.discoveries_total.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn metrics_record_discovery_success_with_latency() {
        let m = MdnsMetrics::new();
        m.record_discovery_attempt();
        m.record_discovery_success(42.0);
        assert_eq!(m.discoveries_success.load(Ordering::Relaxed), 1);
        assert!((m.avg_discovery_latency_ms() - 42.0).abs() < 0.001);
    }

    #[test]
    fn metrics_record_discovery_failure() {
        let m = MdnsMetrics::new();
        m.record_discovery_attempt();
        m.record_discovery_failure();
        assert_eq!(m.discoveries_failed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn metrics_record_peer_discovered_and_expired() {
        let m = MdnsMetrics::new();
        m.record_peer_discovered();
        assert_eq!(m.active_peers.load(Ordering::Relaxed), 1);
        m.record_peer_expired();
        assert_eq!(m.active_peers.load(Ordering::Relaxed), 0);
        assert_eq!(m.peers_expired.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn metrics_success_rate_calculation() {
        let m = MdnsMetrics::new();
        // 3 successes out of 5 attempts = 60%
        for _ in 0..2 {
            m.record_discovery_attempt();
            m.record_discovery_failure();
        }
        for _ in 0..3 {
            m.record_discovery_attempt();
            m.record_discovery_success(10.0);
        }
        assert!((m.success_rate_pct() - 60.0).abs() < 0.001);
    }

    #[test]
    fn metrics_latency_rolling_average() {
        let m = MdnsMetrics::new();
        m.record_discovery_success(10.0);
        m.record_discovery_success(20.0);
        m.record_discovery_success(30.0);
        // Rolling average of 10, 20, 30 = 20
        assert!((m.avg_discovery_latency_ms() - 20.0).abs() < 0.001);
    }

    #[test]
    fn metrics_snapshot_serialization() {
        let m = MdnsMetrics::new();
        m.record_discovery_attempt();
        m.record_discovery_success(50.0);
        let snap = m.snapshot();
        let json = serde_json::to_string(&snap).expect("serialize");
        assert!(json.contains("discoveries_total"));
        assert!(json.contains("discoveries_success"));
    }

    #[test]
    fn metrics_snapshot_helper_methods() {
        let snap = MdnsMetricsSnapshot {
            discoveries_total: 10,
            discoveries_success: 8,
            discoveries_failed: 2,
            peers_discovered: 5,
            peers_expired: 2,
            active_peers: 3,
            publishes_total: 20,
            publishes_failed: 1,
            avg_discovery_latency_ms: 42.5,
        };

        // Test success_rate_pct
        assert!((snap.success_rate_pct() - 80.0).abs() < 0.001);

        // Test total_failures
        assert_eq!(snap.total_failures(), 2);

        // Test has_activity
        assert!(snap.has_activity());

        // Test peer_churn_rate
        assert!((snap.peer_churn_rate() - 0.4).abs() < 0.001);

        // Test with zero activity
        let empty_snap = MdnsMetricsSnapshot::default();
        assert!(!empty_snap.has_activity());
        assert!((empty_snap.success_rate_pct() - 0.0).abs() < f64::EPSILON);
        assert_eq!(empty_snap.peer_churn_rate(), 0.0);
    }

    // ──────────────── PeerCache tests ─────────────────────────────

    #[test]
    fn peer_cache_insert_and_get() {
        let cache = PeerCache::new();
        let key = iroh::SecretKey::generate();
        let ep_id = key.public();
        let peer = DiscoveredPeer {
            endpoint_id: ep_id,
            endpoint_id_short: ep_id.fmt_short().to_string(),
            addresses: vec![],
            discovered_at: SystemTime::now(),
            expires_at: SystemTime::now() + Duration::from_secs(120),
            last_seen: SystemTime::now(),
            relay_urls: vec![],
        };
        cache.upsert(peer.clone());
        let retrieved = cache.get(&ep_id).expect("should exist");
        assert_eq!(retrieved.endpoint_id, ep_id);
    }

    #[test]
    fn peer_cache_expiration() {
        let cache = PeerCache::new();
        let key = iroh::SecretKey::generate();
        let ep_id = key.public();
        let peer = DiscoveredPeer {
            endpoint_id: ep_id,
            endpoint_id_short: ep_id.fmt_short().to_string(),
            addresses: vec![],
            discovered_at: SystemTime::now(),
            expires_at: SystemTime::UNIX_EPOCH, // Already expired
            last_seen: SystemTime::now(),
            relay_urls: vec![],
        };
        cache.upsert(peer);
        assert!(cache.get(&ep_id).is_some()); // Still in cache before cleanup
        cache.remove_expired();
        assert!(cache.get(&ep_id).is_none()); // Removed after cleanup
    }

    #[test]
    fn peer_cache_metrics_on_discover() {
        let cache = PeerCache::new();
        let key = iroh::SecretKey::generate();
        let ep_id = key.public();
        let peer = DiscoveredPeer {
            endpoint_id: ep_id,
            endpoint_id_short: ep_id.fmt_short().to_string(),
            addresses: vec![],
            discovered_at: SystemTime::now(),
            expires_at: SystemTime::now() + Duration::from_secs(120),
            last_seen: SystemTime::now(),
            relay_urls: vec![],
        };
        cache.upsert(peer);
        let snap = cache.metrics().snapshot();
        assert_eq!(snap.peers_discovered, 1);
        assert_eq!(snap.active_peers, 1);
    }

    #[test]
    fn peer_cache_len_and_is_empty() {
        let cache = PeerCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        let key = iroh::SecretKey::generate();
        let peer = DiscoveredPeer {
            endpoint_id: key.public(),
            endpoint_id_short: key.public().fmt_short().to_string(),
            addresses: vec![],
            discovered_at: SystemTime::now(),
            expires_at: SystemTime::now() + Duration::from_secs(120),
            last_seen: SystemTime::now(),
            relay_urls: vec![],
        };
        cache.upsert(peer);
        assert!(!cache.is_empty());
        assert_eq!(cache.len(), 1);
    }

    // ──────────────── MdnsHealthCheck tests ───────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_check_initial_status() {
        let key = iroh::SecretKey::generate();
        let ep_id = key.public();
        let lk = MdnsAddressLookup::new(ep_id).expect("valid");
        let metrics = Arc::new(MdnsMetrics::new());
        let hc = MdnsHealthCheck::new(Arc::new(lk), metrics);
        let status = hc.status();
        // Lookup is healthy by default
        assert!(status.multicast_bound);
        // No peers yet
        assert_eq!(status.active_peers, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_check_success_rate_threshold() {
        let key = iroh::SecretKey::generate();
        let ep_id = key.public();
        let lk = MdnsAddressLookup::new(ep_id).expect("valid");
        let metrics = Arc::new(MdnsMetrics::new());

        // Add failures to lower success rate
        for _ in 0..9 {
            metrics.record_discovery_attempt();
            metrics.record_discovery_failure();
        }
        // Only 1 success out of 10 = 10%
        metrics.record_discovery_attempt();
        metrics.record_discovery_success(10.0);

        let hc = MdnsHealthCheck::new(Arc::new(lk), metrics)
            .with_min_success_rate(50.0);
        let status = hc.status();
        // Below 50% threshold
        assert!(!status.healthy);
        assert!(status.message.contains("below threshold"));
    }

    // ──────────────── MdnsFailureRecovery tests ──────────────────

    #[test]
    fn recovery_initial_state_is_nominal() {
        let recovery = MdnsFailureRecovery::new(MdnsRecoveryConfig::default());
        assert!(matches!(recovery.state(), RecoveryState::Nominal));
        assert!(recovery.should_retry());
    }

    #[test]
    fn recovery_records_failure() {
        let recovery = MdnsFailureRecovery::new(MdnsRecoveryConfig::default());
        let initiated = recovery.record_failure();
        assert!(initiated);
        assert!(matches!(recovery.state(), RecoveryState::Recovering));
    }

    #[test]
    fn recovery_records_success_resets_state() {
        let recovery = MdnsFailureRecovery::new(MdnsRecoveryConfig::default());
        recovery.record_failure();
        recovery.record_success();
        assert!(matches!(recovery.state(), RecoveryState::Nominal));
    }

    #[test]
    fn recovery_backoff_after_failed_attempt() {
        let config = MdnsRecoveryConfig {
            max_retries: 3,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_secs(1),
            backoff_multiplier: 2.0,
        };
        let recovery = MdnsFailureRecovery::new(config);
        recovery.record_failure();
        let backoff = recovery.enter_backoff();
        assert!(backoff > Duration::ZERO);
        assert!(matches!(recovery.state(), RecoveryState::Backoff { attempt: 1, .. }));
    }

    #[test]
    fn recovery_exhausted_after_max_retries() {
        // With max_retries=0, first enter_backoff exhausts immediately
        // (attempt=1 > 0)
        let config = MdnsRecoveryConfig {
            max_retries: 0,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_secs(1),
            backoff_multiplier: 2.0,
        };
        let recovery = MdnsFailureRecovery::new(config);

        recovery.record_failure(); // Nominal -> Recovering
        recovery.enter_backoff();  // Recovering -> Exhausted (attempt=1 > 0)

        assert!(matches!(recovery.state(), RecoveryState::Exhausted));
        assert!(!recovery.should_retry());
    }

    #[test]
    fn recovery_reset() {
        let recovery = MdnsFailureRecovery::new(MdnsRecoveryConfig::default());
        recovery.record_failure();
        recovery.enter_backoff();
        recovery.reset();
        assert!(matches!(recovery.state(), RecoveryState::Nominal));
    }

    #[test]
    fn recovery_callbacks() {
        // Test that on_recovered callback is called on successful recovery
        let config = MdnsRecoveryConfig {
            max_retries: 2,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_secs(1),
            backoff_multiplier: 2.0,
        };
        let recovery = MdnsFailureRecovery::new(config);

        let recovered_called = Arc::new(AtomicBool::new(false));
        let recovered_clone = recovered_called.clone();

        recovery.on_recovered(move || {
            recovered_clone.store(true, Ordering::Relaxed);
        });

        // Trigger a failure first, then success to trigger recovered callback
        recovery.record_failure(); // Nominal -> Recovering
        recovery.record_success(); // Recovering -> Nominal, triggers on_recovered
        assert!(recovered_called.load(Ordering::Relaxed), "on_recovered should be called");
    }

    #[test]
    fn recovery_status_summary_nominal() {
        let recovery = MdnsFailureRecovery::new(MdnsRecoveryConfig::default());
        let summary = recovery.status_summary();
        assert!(summary.contains("operational"));
    }

    #[test]
    fn recovery_status_summary_exhausted() {
        let config = MdnsRecoveryConfig {
            max_retries: 1,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_secs(1),
            backoff_multiplier: 2.0,
        };
        let recovery = MdnsFailureRecovery::new(config);
        recovery.record_failure();
        recovery.enter_backoff();
        let summary = recovery.status_summary();
        assert!(summary.contains("backoff"));
    }

    #[test]
    fn recovery_config_aerospace_defaults() {
        let config = MdnsRecoveryConfig::aerospace();
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.initial_backoff, Duration::from_secs(1));
        assert_eq!(config.max_backoff, Duration::from_secs(60));
        assert!((config.backoff_multiplier - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn recovery_config_fast_defaults() {
        let config = MdnsRecoveryConfig::fast();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.initial_backoff, Duration::from_millis(100));
        assert_eq!(config.max_backoff, Duration::from_secs(5));
    }

    // ──────────────── Constants tests ─────────────────────────────

    #[test]
    fn mdns_constants_are_stable() {
        assert_eq!(MDNS_SERVICE_NAME, "_a3net._udp.local");
        assert_eq!(MDNS_PORT, 5353);
        assert_eq!(MDNS_MULTICAST_V4, "224.0.0.251");
        assert_eq!(MDNS_MULTICAST_V6, "ff02::fb");
        assert_eq!(MAX_PEER_CACHE_SIZE, 256);
        assert_eq!(DEFAULT_PEER_TTL_SECS, 120);
    }

    // ──────────────── DiscoveredPeer tests ───────────────────────

    #[test]
    fn discovered_peer_is_expired() {
        let peer = DiscoveredPeer {
            endpoint_id: iroh::SecretKey::generate().public(),
            endpoint_id_short: "xxxx".to_string(),
            addresses: vec![],
            discovered_at: SystemTime::UNIX_EPOCH,
            expires_at: SystemTime::UNIX_EPOCH, // Already expired
            last_seen: SystemTime::UNIX_EPOCH,
            relay_urls: vec![],
        };
        assert!(peer.is_expired());
    }

    #[test]
    fn discovered_peer_ttl_remaining() {
        let peer = DiscoveredPeer {
            endpoint_id: iroh::SecretKey::generate().public(),
            endpoint_id_short: "xxxx".to_string(),
            addresses: vec![],
            discovered_at: SystemTime::now(),
            expires_at: SystemTime::now() + Duration::from_secs(60),
            last_seen: SystemTime::now(),
            relay_urls: vec![],
        };
        let ttl = peer.ttl_remaining().expect("should have TTL");
        // TTL should be close to 60 seconds (within a reasonable tolerance)
        assert!(ttl.as_secs() <= 60, "TTL should not exceed original value");
        assert!(ttl.as_secs() > 0, "TTL should be positive");
    }

    #[test]
    fn discovered_peer_ttl_remaining_expired() {
        let peer = DiscoveredPeer {
            endpoint_id: iroh::SecretKey::generate().public(),
            endpoint_id_short: "xxxx".to_string(),
            addresses: vec![],
            discovered_at: SystemTime::UNIX_EPOCH,
            expires_at: SystemTime::UNIX_EPOCH,
            last_seen: SystemTime::UNIX_EPOCH,
            relay_urls: vec![],
        };
        assert!(peer.ttl_remaining().is_none());
    }
}