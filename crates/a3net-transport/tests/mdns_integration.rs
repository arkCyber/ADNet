//! mDNS LAN Discovery Integration Tests
//!
//! Tests for the mDNS discovery feature including:
//! - Basic mDNS lookup construction
//! - Health check integration
//! - Metrics collection
//! - Failure recovery
//! - Peer caching
//!
//! Note: These tests use loopback addresses and don't require a real LAN.

#![cfg(feature = "mdns")]

use std::sync::Arc;
use std::time::Duration;

use a3net_transport::iroh::discovery::{
    DiscoveryBuilder, DiscoveryConfig, MdnsFailureRecovery, MdnsHealthCheck,
    MdnsHealthStatus, MdnsMetrics, MdnsMetricsSnapshot, MdnsRecoveryConfig,
    PeerCache, DiscoveredPeer, RecoveryState,
    MDNS_PROVENANCE, MAX_PEER_CACHE_SIZE, DEFAULT_PEER_TTL_SECS,
    MDNS_SERVICE_NAME, MDNS_PORT, MDNS_MULTICAST_V4, MDNS_MULTICAST_V6,
};
use iroh::SecretKey;

// ─────────────────── MdnsMetrics Integration Tests ───────────────────

#[test]
fn mdns_metrics_increment_counters() {
    let metrics = MdnsMetrics::new();

    // Record multiple discovery attempts
    metrics.record_discovery_attempt();
    metrics.record_discovery_attempt();
    metrics.record_discovery_attempt();

    assert_eq!(metrics.discoveries_total.load(std::sync::atomic::Ordering::Relaxed), 3);

    // Record successes
    metrics.record_discovery_success(10.0);
    metrics.record_discovery_success(20.0);

    assert_eq!(metrics.discoveries_success.load(std::sync::atomic::Ordering::Relaxed), 2);

    // Record failures
    metrics.record_discovery_failure();

    assert_eq!(metrics.discoveries_failed.load(std::sync::atomic::Ordering::Relaxed), 1);

    // Verify snapshot captures all values
    let snap = metrics.snapshot();
    assert_eq!(snap.discoveries_total, 3);
    assert_eq!(snap.discoveries_success, 2);
    assert_eq!(snap.discoveries_failed, 1);
}

#[test]
fn mdns_metrics_peer_tracking() {
    let metrics = MdnsMetrics::new();

    // Add some peers
    metrics.record_peer_discovered();
    metrics.record_peer_discovered();
    metrics.record_peer_discovered();

    assert_eq!(metrics.active_peers.load(std::sync::atomic::Ordering::Relaxed), 3);

    // Expire some peers
    metrics.record_peer_expired();
    metrics.record_peer_expired();

    assert_eq!(metrics.active_peers.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(metrics.peers_expired.load(std::sync::atomic::Ordering::Relaxed), 2);

    // Verify snapshot
    let snap = metrics.snapshot();
    assert_eq!(snap.peers_discovered, 3);
    assert_eq!(snap.active_peers, 1);
}

#[test]
fn mdns_metrics_publish_tracking() {
    let metrics = MdnsMetrics::new();

    metrics.record_publish();
    metrics.record_publish();
    metrics.record_publish();

    metrics.record_publish_failure();

    let snap = metrics.snapshot();
    assert_eq!(snap.publishes_total, 3);
    assert_eq!(snap.publishes_failed, 1);
}

#[test]
fn mdns_metrics_latency_tracking() {
    let metrics = MdnsMetrics::new();

    // Record various latencies
    metrics.record_discovery_success(10.0);
    metrics.record_discovery_success(20.0);
    metrics.record_discovery_success(30.0);

    // Average should be 20ms
    let avg = metrics.avg_discovery_latency_ms();
    assert!((avg - 20.0).abs() < 0.1);

    // Add more samples
    metrics.record_discovery_success(40.0);
    metrics.record_discovery_success(50.0);

    // Rolling average of last 5 samples (10+20+30+40+50)/5 = 30
    let avg = metrics.avg_discovery_latency_ms();
    assert!((avg - 30.0).abs() < 0.1);
}

#[test]
fn mdns_metrics_serialization() {
    let metrics = MdnsMetrics::new();
    metrics.record_discovery_attempt();
    metrics.record_discovery_success(42.0);

    let snap = metrics.snapshot();
    let json = serde_json::to_string(&snap).unwrap();

    assert!(json.contains("\"discoveries_total\":1"));
    assert!(json.contains("\"discoveries_success\":1"));
    assert!(json.contains("\"avg_discovery_latency_ms\":42"));
}

// ─────────────────── PeerCache Integration Tests ───────────────────

fn make_test_peer(id: SecretKey) -> DiscoveredPeer {
    DiscoveredPeer {
        endpoint_id: id.public(),
        endpoint_id_short: id.public().fmt_short().to_string(),
        addresses: vec![],
        discovered_at: std::time::SystemTime::now(),
        expires_at: std::time::SystemTime::now() + Duration::from_secs(DEFAULT_PEER_TTL_SECS),
        last_seen: std::time::SystemTime::now(),
        relay_urls: vec![],
    }
}

fn make_expired_peer(id: SecretKey) -> DiscoveredPeer {
    DiscoveredPeer {
        endpoint_id: id.public(),
        endpoint_id_short: id.public().fmt_short().to_string(),
        addresses: vec![],
        discovered_at: std::time::SystemTime::UNIX_EPOCH,
        expires_at: std::time::SystemTime::UNIX_EPOCH,
        last_seen: std::time::SystemTime::UNIX_EPOCH,
        relay_urls: vec![],
    }
}

#[test]
fn peer_cache_insert_and_retrieve() {
    let cache = PeerCache::new();
    let key = SecretKey::generate();
    let peer = make_test_peer(key.clone());

    cache.upsert(peer.clone());

    let retrieved = cache.get(&key.public()).expect("peer should exist");
    assert_eq!(retrieved.endpoint_id, peer.endpoint_id);
}

#[test]
fn peer_cache_multiple_peers() {
    let cache = PeerCache::new();

    // Add multiple peers
    for _ in 0..10 {
        let key = SecretKey::generate();
        let peer = make_test_peer(key);
        cache.upsert(peer);
    }

    assert_eq!(cache.len(), 10);
    assert!(!cache.is_empty());
}

#[test]
fn peer_cache_expiration_cleanup() {
    let cache = PeerCache::new();

    // Add some valid peers
    for _ in 0..3 {
        let key = SecretKey::generate();
        let peer = make_test_peer(key);
        cache.upsert(peer);
    }

    // Add an expired peer
    let key = SecretKey::generate();
    cache.upsert(make_expired_peer(key));

    // Before cleanup: 4 peers
    assert_eq!(cache.len(), 4);

    // Cleanup removes expired
    cache.remove_expired();

    // After cleanup: 3 peers (expired one removed)
    assert_eq!(cache.len(), 3);
}

#[test]
fn peer_cache_metrics_on_operations() {
    let cache = PeerCache::new();

    // Add first peer
    let key1 = SecretKey::generate();
    cache.upsert(make_test_peer(key1.clone()));
    assert_eq!(cache.metrics().snapshot().peers_discovered, 1);
    assert_eq!(cache.metrics().snapshot().active_peers, 1);

    // Add same peer again (update, not new)
    let peer = make_test_peer(key1.clone());
    cache.upsert(peer);
    assert_eq!(cache.metrics().snapshot().peers_discovered, 1); // Still 1

    // Add second peer
    let key2 = SecretKey::generate();
    cache.upsert(make_test_peer(key2));
    assert_eq!(cache.metrics().snapshot().peers_discovered, 2);
}

// ─────────────────── MdnsHealthCheck Integration Tests ───────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_check_nominal_status() {
    let key = SecretKey::generate();
    let ep_id = key.public();
    let lookup = a3net_transport::iroh::discovery::MdnsAddressLookup::new(ep_id)
        .expect("valid lookup");
    let metrics = Arc::new(MdnsMetrics::new());

    let health = MdnsHealthCheck::new(
        Arc::new(lookup),
        metrics.clone(),
    );

    let status = health.status();
    assert!(status.multicast_bound, "mDNS should be healthy");
    assert_eq!(status.active_peers, 0, "no peers yet");
    assert!(status.success_rate_pct >= 0.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_check_with_custom_thresholds() {
    let key = SecretKey::generate();
    let ep_id = key.public();
    let lookup = a3net_transport::iroh::discovery::MdnsAddressLookup::new(ep_id)
        .expect("valid lookup");
    let metrics = Arc::new(MdnsMetrics::new());

    // Only 25% success rate
    for _ in 0..3 {
        metrics.record_discovery_attempt();
        metrics.record_discovery_failure();
    }
    metrics.record_discovery_attempt();
    metrics.record_discovery_success(10.0);

    // Set threshold to 50%
    let health = MdnsHealthCheck::new(
        Arc::new(lookup),
        metrics.clone(),
    ).with_min_success_rate(50.0);

    let status = health.status();
    assert!(!status.healthy, "25% should fail 50% threshold");
    assert!(status.message.contains("below threshold"));
}

// ─────────────────── MdnsFailureRecovery Integration Tests ───────────────────

#[test]
fn recovery_nominal_to_recovering_transition() {
    let recovery = MdnsFailureRecovery::new(MdnsRecoveryConfig::default());

    assert!(matches!(recovery.state(), RecoveryState::Nominal));

    let initiated = recovery.record_failure();
    assert!(initiated);
    assert!(matches!(recovery.state(), RecoveryState::Recovering));
}

#[test]
fn recovery_recovers_after_failure() {
    let recovery = MdnsFailureRecovery::new(MdnsRecoveryConfig::default());

    recovery.record_failure();
    recovery.record_success();

    assert!(matches!(recovery.state(), RecoveryState::Nominal));
}

#[test]
fn recovery_exponential_backoff() {
    let config = MdnsRecoveryConfig {
        max_retries: 3,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_secs(1),
        backoff_multiplier: 2.0,
    };
    let recovery = MdnsFailureRecovery::new(config);

    // First failure -> Recovering
    recovery.record_failure();
    assert!(matches!(recovery.state(), RecoveryState::Recovering));

    // Enter backoff after first attempt
    let backoff1 = recovery.enter_backoff();
    assert!(backoff1 > Duration::ZERO);
    assert!(matches!(
        recovery.state(),
        RecoveryState::Backoff { attempt: 1, .. }
    ));
}

#[test]
fn recovery_exhaustion() {
    let config = MdnsRecoveryConfig {
        max_retries: 2,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_secs(1),
        backoff_multiplier: 2.0,
    };
    let recovery = MdnsFailureRecovery::new(config);

    // Exhaust all retries
    // Pattern: record_failure + enter_backoff (until exhausted)
    // After 3 iterations with max_retries=2, state should be Exhausted
    for _ in 0..3 {
        recovery.record_failure();
        recovery.enter_backoff();
    }

    // After max retries, should be exhausted
    assert!(matches!(recovery.state(), RecoveryState::Exhausted));
    assert!(!recovery.should_retry());
}

#[test]
fn recovery_reset() {
    let recovery = MdnsFailureRecovery::new(MdnsRecoveryConfig::default());

    // Get into some non-nominal state
    recovery.record_failure();
    recovery.enter_backoff();
    assert!(!matches!(recovery.state(), RecoveryState::Nominal));

    // Reset
    recovery.reset();
    assert!(matches!(recovery.state(), RecoveryState::Nominal));
    assert!(recovery.should_retry());
}

#[test]
fn recovery_config_presets() {
    let aerospace = MdnsRecoveryConfig::aerospace();
    assert_eq!(aerospace.max_retries, 5);
    assert_eq!(aerospace.initial_backoff, Duration::from_secs(1));
    assert_eq!(aerospace.max_backoff, Duration::from_secs(60));

    let fast = MdnsRecoveryConfig::fast();
    assert_eq!(fast.max_retries, 3);
    assert_eq!(fast.initial_backoff, Duration::from_millis(100));
    assert_eq!(fast.max_backoff, Duration::from_secs(5));
}

// ─────────────────── Constants Verification ───────────────────

#[test]
fn mdns_constants_verification() {
    assert_eq!(MDNS_PROVENANCE, "a3net-mdns");
    assert_eq!(MDNS_SERVICE_NAME, "_a3net._udp.local");
    assert_eq!(MDNS_PORT, 5353);
    assert_eq!(MDNS_MULTICAST_V4, "224.0.0.251");
    assert_eq!(MDNS_MULTICAST_V6, "ff02::fb");
    assert_eq!(MAX_PEER_CACHE_SIZE, 256);
    assert_eq!(DEFAULT_PEER_TTL_SECS, 120);
}

#[test]
fn mdns_constants_in_snapshot() {
    let snap = MdnsMetricsSnapshot::default();

    // Verify default snapshot has zero values
    assert_eq!(snap.discoveries_total, 0);
    assert_eq!(snap.discoveries_success, 0);
    assert_eq!(snap.discoveries_failed, 0);
    assert_eq!(snap.peers_discovered, 0);
    assert_eq!(snap.peers_expired, 0);
    assert_eq!(snap.active_peers, 0);
    assert_eq!(snap.publishes_total, 0);
    assert_eq!(snap.publishes_failed, 0);
    assert_eq!(snap.avg_discovery_latency_ms, 0.0);
}

// ─────────────────── DiscoveredPeer Tests ───────────────────

#[test]
fn discovered_peer_ttl_calculation() {
    let key = SecretKey::generate();
    let peer = make_test_peer(key);

    // Should have TTL remaining
    let ttl = peer.ttl_remaining();
    assert!(ttl.is_some());
    let ttl = ttl.unwrap();
    assert!(ttl.as_secs() <= DEFAULT_PEER_TTL_SECS);
    assert!(ttl.as_secs() > 0);
}

#[test]
fn discovered_peer_expired_check() {
    let key = SecretKey::generate();
    let peer = make_expired_peer(key);

    assert!(peer.is_expired());
    assert!(peer.ttl_remaining().is_none());
}

#[test]
fn discovered_peer_serialization() {
    let key = SecretKey::generate();
    let peer = make_test_peer(key);

    let json = serde_json::to_string(&peer).unwrap();
    assert!(json.contains("\"endpoint_id_short\":\""));
    assert!(json.contains("\"endpoint_id\":\""));
    assert!(json.contains("\"addresses\":"));
    assert!(json.contains("\"relay_urls\":"));
}

// ─────────────────── End-to-End Integration Tests ───────────────────

/// E2E test: construct mDNS components and verify they work together.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_mdns_components_integration() {
    // Create components
    let key = SecretKey::generate();
    let ep_id = key.public();

    let lookup = a3net_transport::iroh::discovery::MdnsAddressLookup::new(ep_id)
        .expect("should create lookup");
    let metrics = Arc::new(MdnsMetrics::new());
    let cache = Arc::new(PeerCache::new());
    let recovery = MdnsFailureRecovery::new(MdnsRecoveryConfig::default());

    // Verify lookup is healthy
    assert!(lookup.is_healthy());

    // Record some activity
    metrics.record_discovery_attempt();
    metrics.record_discovery_success(15.0);
    metrics.record_peer_discovered();

    // Add a peer to cache
    cache.upsert(make_test_peer(SecretKey::generate()));

    // Verify metrics
    let snap = metrics.snapshot();
    assert_eq!(snap.discoveries_total, 1);
    assert_eq!(snap.discoveries_success, 1);
    assert_eq!(snap.active_peers, 1);

    // Verify cache
    assert_eq!(cache.len(), 1);

    // Verify recovery is nominal
    assert!(matches!(recovery.state(), RecoveryState::Nominal));
}

/// E2E test: simulate discovery flow with failures and recovery
#[test]
fn e2e_discovery_flow_with_failures() {
    let metrics = MdnsMetrics::new();
    let recovery = MdnsFailureRecovery::new(MdnsRecoveryConfig::default());

    // Simulate discovery attempts
    for i in 0..5 {
        metrics.record_discovery_attempt();

        if i % 2 == 0 {
            metrics.record_discovery_success(10.0);
            recovery.record_success();
        } else {
            metrics.record_discovery_failure();
            recovery.record_failure();
            recovery.enter_backoff();
        }
    }

    // Check metrics
    let snap = metrics.snapshot();
    assert_eq!(snap.discoveries_total, 5);
    assert_eq!(snap.discoveries_success, 3);
    assert_eq!(snap.discoveries_failed, 2);
}

/// E2E test: health check with simulated load
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_health_check_with_load() {
    let key = SecretKey::generate();
    let ep_id = key.public();
    let lookup = a3net_transport::iroh::discovery::MdnsAddressLookup::new(ep_id)
        .expect("valid lookup");
    let metrics = Arc::new(MdnsMetrics::new());
    let cache = Arc::new(PeerCache::new());

    // Simulate load
    for _ in 0..10 {
        metrics.record_discovery_attempt();
        metrics.record_discovery_success(25.0);
    }

    // Add some peers
    for _ in 0..3 {
        cache.upsert(make_test_peer(SecretKey::generate()));
        metrics.record_peer_discovered();
    }

    // Create health check
    let health = MdnsHealthCheck::new(
        Arc::new(lookup),
        metrics.clone(),
    );

    let status = health.status();

    // Verify health check reflects the load
    assert!(status.multicast_bound);
    assert_eq!(status.active_peers, 3);
    assert!(status.success_rate_pct >= 90.0);
}
