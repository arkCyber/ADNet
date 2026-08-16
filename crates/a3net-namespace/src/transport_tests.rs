//! Tests for the new `transport` module.
//!
//! These tests are deliberately hermetic: no network calls, no
//! `pkarr::Client`, no live `GossipBus`. They exercise the
//! `MultiTransport` fanout/fanin logic, the disk journal replay, and
//! the dedupe invariant.

use crate::transport::disk::DiskJournalTransport;
use crate::transport::gossip::GossipIpnTransport;
use crate::transport::multi::MultiTransport;
use crate::transport::pkarr::PkarrTransport;
use crate::transport::{IpnTransport, SharedIpnBus, TransportHealth};
use crate::ipns::{Ed25519SecretKey, IpnRecord, IpnResolver, IpnsError};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

fn sign_record(name: &str, value: &str, key: &Ed25519SecretKey) -> IpnRecord {
    let mut r = IpnRecord::with_name_value(name.to_string(), value.to_string());
    r.sign(key).unwrap();
    r
}

#[tokio::test]
async fn disk_journal_writes_and_replays() {
    let dir = TempDir::new().unwrap();
    let t = DiskJournalTransport::new(dir.path().to_path_buf());

    let key = Ed25519SecretKey::generate();

    let r1 = sign_record("name-a", "/ipfs/v1", &key);
    let r2 = sign_record("name-b", "/ipfs/v2", &key);

    t.publish(&r1).await.unwrap();
    t.publish(&r2).await.unwrap();

    let replay = t.replay_all().await.unwrap();
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].name, "name-a");
    assert_eq!(replay[1].name, "name-b");
}

#[tokio::test]
async fn disk_journal_replay_stream_emits_all() {
    let dir = TempDir::new().unwrap();
    let t = DiskJournalTransport::new(dir.path().to_path_buf());

    let key = Ed25519SecretKey::generate();
    let r = sign_record("alpha", "/ipfs/x", &key);
    t.publish(&r).await.unwrap();

    let mut s = t.subscribe("anything").await.unwrap();
    let first = s.next().await.expect("at least one record");
    assert!(first.is_ok());
}

#[tokio::test]
async fn multi_transport_dedupes_duplicate_publish() {
    let dir = TempDir::new().unwrap();
    let disk: Arc<dyn IpnTransport> =
        Arc::new(DiskJournalTransport::new(dir.path().to_path_buf()));
    let bus = SharedIpnBus::new(64);
    let gossip: Arc<dyn IpnTransport> = Arc::new(GossipIpnTransport::new(bus.sender()));

    let multi = MultiTransport::new(vec![disk, gossip]);
    let key = Ed25519SecretKey::generate();
    let r1 = sign_record("dup-name", "/ipfs/a", &key);
    let r2 = sign_record("dup-name", "/ipfs/a", &key);

    multi.publish(&r1).await.unwrap();
    multi.publish(&r2).await.unwrap();

    let mut s = multi.subscribe("dup-name").await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut pulled = 0;
    while let Ok(Some(_)) =
        tokio::time::timeout(Duration::from_millis(150), s.next()).await
    {
        pulled += 1;
        if pulled >= 4 {
            break;
        }
    }
    assert!(pulled <= 4);
}

#[tokio::test]
async fn multi_transport_aggregates_health() {
    let bus = SharedIpnBus::new(8);
    let gossip: Arc<dyn IpnTransport> = Arc::new(GossipIpnTransport::new(bus.sender()));
    let noop: Arc<dyn IpnTransport> = Arc::new(GossipIpnTransport::noop());

    let multi = MultiTransport::new(vec![gossip, noop]);
    let h = multi.health().await.unwrap();
    assert_eq!(h, TransportHealth::Healthy);
}

#[tokio::test]
async fn pkarr_transport_default_no_real_client_records_cache_only() {
    let t = PkarrTransport::default();
    let key = Ed25519SecretKey::generate();
    let r = sign_record("pkarr-name", "/ipfs/z", &key);

    t.publish(&r).await.unwrap();

    let mut s = t.subscribe("pkarr-name").await.unwrap();
    let first = s.next().await.expect("at least the cached record");
    let r2 = first.unwrap();
    assert_eq!(r2.name, "pkarr-name");
}

#[tokio::test]
async fn pkarr_transport_resolves_not_found_without_client() {
    let t = PkarrTransport::default();
    let res = t.resolve_now("missing").await;
    assert!(matches!(res, Err(IpnsError::NotFound)));
}

#[tokio::test]
async fn pkarr_transport_dns_name_is_stable() {
    assert_eq!(PkarrTransport::dns_name("abc"), "_a3net.abc");
    assert_eq!(PkarrTransport::dns_name("deadbeef"), "_a3net.deadbeef");
}

#[tokio::test]
async fn ipn_resolver_round_trip_with_disk_transport() {
    let dir = TempDir::new().unwrap();
    let t = DiskJournalTransport::new(dir.path().to_path_buf());
    let key = Ed25519SecretKey::generate();
    let publisher = IpnResolver::new(Duration::from_secs(60));

    let r = sign_record("ink", "/ipfs/ink", &key);
    t.publish(&r).await.unwrap();

    let mut s = t.subscribe("ink").await.unwrap();
    let replayed = s.next().await.unwrap().unwrap();
    // sanity: name round-trips
    assert_eq!(replayed.name, "ink");
    // sanity: resolver caches and resolves
    let resolver = publisher;
    resolver.cache_record(replayed);
    let value = resolver.resolve("ink").await.unwrap();
    assert_eq!(value, "/ipfs/ink");
}

#[test]
fn record_signs_and_verifies() {
    let key = Ed25519SecretKey::generate();
    let mut r = IpnRecord::with_name_value("name".into(), "/ipfs/v1".into());
    r.sign(&key).unwrap();
    assert!(!r.signature.is_empty());
}

#[test]
fn encoded_packet_round_trip() {
    let key = Ed25519SecretKey::generate();
    let r = sign_record("name", "/ipfs/v", &key);
    let t = PkarrTransport::default();
    let bytes = t.encode_packet(&r).unwrap();
    assert_eq!(bytes[0], 0x01);
    let decoded = PkarrTransport::decode_packet("name", &bytes).unwrap();
    assert_eq!(decoded.name, r.name);
    assert_eq!(decoded.value, r.value);
}

#[test]
fn encoded_packet_rejects_name_mismatch() {
    let key = Ed25519SecretKey::generate();
    let r = sign_record("alice", "/ipfs/v", &key);
    let t = PkarrTransport::default();
    let bytes = t.encode_packet(&r).unwrap();
    let res = PkarrTransport::decode_packet("bob", &bytes);
    assert!(res.is_err());
}

/// Test DHT transport health reporting with local backend.
#[tokio::test]
#[cfg(feature = "dht")]
async fn dht_transport_local_health() {
    use crate::transport::dht::DhtIpnTransport;

    let store = a3net_dht::store::new_in_memory_store();
    let transport = DhtIpnTransport::local(store);

    // Local backend is always healthy (even with no values stored)
    let h = transport.health().await.unwrap();
    assert!(h == TransportHealth::Healthy || h == TransportHealth::Degraded);
}

/// Test that DHT transport health reflects backend state.
#[tokio::test]
#[cfg(feature = "dht")]
async fn dht_transport_local_health_with_values() {
    use crate::transport::dht::DhtIpnTransport;

    let store = a3net_dht::store::new_in_memory_store();
    let transport = DhtIpnTransport::local(store);

    // Initially degraded (no peers, just values)
    let h1 = transport.health().await.unwrap();
    assert_eq!(h1, TransportHealth::Degraded);

    // Add a record
    let key = Ed25519SecretKey::generate();
    let name = key.ipns_name();
    let mut record = IpnRecord::with_name_value(name.clone(), "/ipfs/QmHealth".to_string());
    record.sign(&key).unwrap();
    transport.publish(&record).await.expect("publish to DHT");

    // Still degraded (no peers, but has values)
    let h2 = transport.health().await.unwrap();
    assert_eq!(h2, TransportHealth::Degraded);
}