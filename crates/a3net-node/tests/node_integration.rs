//! Integration tests for a3net-node core functionality.
//!
//! This module tests the integration between different components of a3net-node,
//! including the Node, BlobStore, Bitswap, and DHT integration.

#![cfg(any(feature = "bitswap", feature = "dht"))]

use std::sync::Arc;

use a3net_blobstore::BlobStore;
use a3net_gossip::{GossipBus, InProcessGossip};
use a3net_node::{
    ChecksumReport, ChunkChecksumEntry, DownloadJob, DownloadProgress, NodeBuilder,
    NodeConfig, TransferSettings,
};
#[cfg(feature = "bitswap")]
use a3net_node::{BitswapConfig, BitswapHandle};
#[cfg(feature = "dht")]
use a3net_node::DhtConfig;
use a3net_transport::quic::QuicTransportBuilder;
use a3net_transport::SharedTransport;
use a3net_types::{ContentHash, NodeId};

fn ephemeral_transport() -> (NodeId, SharedTransport) {
    let local_id = NodeId::random();
    let transport = QuicTransportBuilder::new(local_id.clone(), "127.0.0.1:0".parse().unwrap())
        .build()
        .expect("build ephemeral quic transport");
    let id = transport.local_node_id().clone();
    let transport = Arc::new(transport) as SharedTransport;
    (id, transport)
}

fn data_dir() -> std::path::PathBuf {
    let dir = tempfile::tempdir().expect("tempdir");
    dir.path().to_path_buf()
}

fn shared_bus() -> Arc<dyn a3net_gossip::GossipTransport> {
    Arc::new(InProcessGossip::new())
}

// ═══════════════════════════════════════════════════════════════════
//  ChecksumReport Integration Tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn checksum_report_aggregate_from_multiple_sources() {
    // Simulate aggregating checksums from multiple chunk sources
    let mut reports: Vec<ChecksumReport> = Vec::new();

    for chunk_idx in 0..4 {
        let entry = ChunkChecksumEntry {
            index: chunk_idx,
            hash: format!("chunk_hash_{}", chunk_idx),
            size_bytes: 1024 * 1024, // 1MB chunks
        };
        let file_hash_str = format!("file_hash_{}", chunk_idx / 2);
        let report = ChecksumReport::build(
            "blake3",
            file_hash_str.clone(),
            1024 * 1024,
            1,
            vec![entry],
            &file_hash_str,
            0,
        );
        reports.push(report);
    }

    assert_eq!(reports.len(), 4);
    for report in &reports {
        assert!(report.is_clean());
    }
}

#[test]
fn checksum_report_detects_corruption() {
    // Simulate a scenario where one chunk is corrupted
    let chunks = vec![
        ChunkChecksumEntry {
            index: 0,
            hash: "valid_hash_0".to_string(),
            size_bytes: 1024,
        },
        ChunkChecksumEntry {
            index: 1,
            hash: "corrupted_hash".to_string(),
            size_bytes: 1024,
        },
        ChunkChecksumEntry {
            index: 2,
            hash: "valid_hash_2".to_string(),
            size_bytes: 1024,
        },
    ];

    let expected = vec![
        "valid_hash_0".to_string(),
        "expected_hash_1".to_string(), // Different from corrupted
        "valid_hash_2".to_string(),
    ];

    let report = ChecksumReport::build_with_chunk_expectations(
        "blake3",
        "file_hash",
        3072,
        3,
        chunks,
        Some(&expected),
        "file_hash",
        100,
    );

    assert!(!report.is_clean());
    assert_eq!(report.mismatch_chunks, vec![1]);
}

// ═══════════════════════════════════════════════════════════════════
//  DownloadProgress Integration Tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn download_progress_tracks_completion() {
    let hash = ContentHash::from_bytes(b"test-download");

    // Simulate download progress updates
    let total_size: u64 = 10 * 1024 * 1024; // 10MB
    let progress_steps = vec![0, 1024, 5120, 10240, 51200, 102400, 512000, 1024000, 5120000, 10485760];

    for &bytes_done in &progress_steps {
        let progress = DownloadProgress {
            hash: hash.clone(),
            bytes_done,
            bytes_total: total_size,
        };
        assert_eq!(progress.hash, hash);
        assert!(progress.bytes_done <= progress.bytes_total);
    }
}

#[test]
fn download_job_reflects_final_status() {
    let hash = ContentHash::from_bytes(b"completed-download");

    let job = DownloadJob {
        hash: hash.clone(),
        title: "completed.txt".to_string(),
        status: "ok".to_string(),
        bytes_done: 1024,
        bytes_total: 1024,
    };

    assert_eq!(job.status, "ok");
    assert_eq!(job.bytes_done, job.bytes_total);
}

// ═══════════════════════════════════════════════════════════════════
//  TransferSettings Integration Tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn transfer_settings_throttle_scenarios() {
    // Unthrottled
    let unthrottled = TransferSettings::default();
    assert_eq!(unthrottled.throttle_bytes_per_sec, 0);

    // Low bandwidth (dial-up simulation)
    let dialup = TransferSettings {
        throttle_bytes_per_sec: 6000, // ~48kbps
        auto_reconnect: true,
        background_jobs: true,
    };
    assert_eq!(dialup.throttle_bytes_per_sec, 6000);

    // High bandwidth (fiber simulation)
    let fiber = TransferSettings {
        throttle_bytes_per_sec: 100_000_000, // 100MB/s
        auto_reconnect: true,
        background_jobs: true,
    };
    assert_eq!(fiber.throttle_bytes_per_sec, 100_000_000);
}

#[test]
fn transfer_settings_auto_reconnect_behavior() {
    let auto_on = TransferSettings {
        throttle_bytes_per_sec: 0,
        auto_reconnect: true,
        background_jobs: true,
    };
    assert!(auto_on.auto_reconnect);

    let auto_off = TransferSettings {
        throttle_bytes_per_sec: 0,
        auto_reconnect: false,
        background_jobs: true,
    };
    assert!(!auto_off.auto_reconnect);
}

// ═══════════════════════════════════════════════════════════════════
//  NodeConfig Integration Tests
// ═══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_config_round_trip() {
    let node_id = NodeId::random();
    let dir = data_dir();

    let cfg = NodeConfig::new(&dir, node_id.clone());
    assert_eq!(&cfg.node_id, &node_id);
    assert_eq!(cfg.data_dir, dir);
}

#[cfg(feature = "bitswap")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_builder_with_bitswap_config() {
    let (id, transport) = ephemeral_transport();
    let cfg = NodeConfig::new(data_dir(), id.clone());

    let bitswap_cfg = BitswapConfig::default();
    let node = NodeBuilder::new(cfg)
        .with_transport(transport.clone())
        .with_bitswap_config(bitswap_cfg.clone())
        .build_with_bus(GossipBus::new(id, shared_bus()))
        .await
        .expect("build with bitswap config");

    // Verify bitswap is configured
    let bitswap = node.bitswap_handle();
    assert!(bitswap.is_some(), "bitswap handle should be present");

    let _ = node.shutdown().await;
}

#[cfg(feature = "bitswap")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_builder_disabled_bitswap() {
    let (id, transport) = ephemeral_transport();
    let cfg = NodeConfig::new(data_dir(), id.clone());

    let node = NodeBuilder::new(cfg)
        .with_transport(transport.clone())
        .disable_bitswap()
        .build_with_bus(GossipBus::new(id, shared_bus()))
        .await
        .expect("build with bitswap disabled");

    // Verify bitswap is not configured
    let bitswap = node.bitswap_handle();
    assert!(bitswap.is_none(), "bitswap handle should be None when disabled");

    let _ = node.shutdown().await;
}

// ═══════════════════════════════════════════════════════════════════
//  BitswapHandle Integration Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(feature = "bitswap")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bitswap_handle_local_provider_tracking() {
    let dir = tempfile::tempdir().unwrap();
    let blob_store = Arc::new(BlobStore::new(dir.path()).unwrap());
    let node_id = NodeId::random();

    let handle = BitswapHandle::new(
        node_id.clone(),
        blob_store.clone(),
        BitswapConfig::default(),
    )
    .await;

    // Initially no local content
    assert_eq!(handle.local_provider_count(), 0);

    // Add some content to the blob store
    let data1 = b"content-1".to_vec();
    let data2 = b"content-2".to_vec();

    let (hash1, _) = blob_store.put_bytes_sync(&data1).unwrap();
    let (hash2, _) = blob_store.put_bytes_sync(&data2).unwrap();

    // Refresh local providers by calling scan_local_content
    handle.scan_local_content().await;

    assert!(handle.has_block(&hash1));
    assert!(handle.has_block(&hash2));
    assert!(!handle.has_block(&ContentHash::from_bytes(b"unknown")));

    // Provider count should now reflect the added content
    let stats = handle.stats();
    assert!(stats.local_content >= 2);
}

#[cfg(feature = "bitswap")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bitswap_handle_peer_management() {
    let dir = tempfile::tempdir().unwrap();
    let blob_store = Arc::new(BlobStore::new(dir.path()).unwrap());
    let node_id = NodeId::random();

    let handle =
        BitswapHandle::new(node_id.clone(), blob_store, BitswapConfig::default()).await;

    let peer1 = NodeId::random();
    let peer2 = NodeId::random();

    // Add peers
    handle.add_peer(&peer1);
    handle.add_peer(&peer2);

    let stats1 = handle.stats();
    assert!(stats1.connected_peers >= 2);

    // Remove one peer
    handle.remove_peer(&peer1);

    // Query for block should return the remaining peer
    let hash = ContentHash::from_bytes(b"query-test");
    let peers = handle.query_peers_for_block(&hash);
    assert!(peers.contains(&peer2));
}

// ═══════════════════════════════════════════════════════════════════
//  DHT Integration Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(feature = "dht")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dht_handle_provider_announcement() {
    let config = DhtConfig::default();
    let handle = a3net_node::DhtHandle::new(config).await;

    let hash = ContentHash::from_bytes(b"dht-test-content");

    // Announce the content
    handle.provide(&hash).await;

    // Find providers should return the local provider
    let providers = handle.find_providers(&hash).await;
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].provider_id, *handle.local_id());

    // Unknown hash should return empty
    let unknown = ContentHash::from_bytes(b"unknown-content");
    let providers = handle.find_providers(&unknown).await;
    assert!(providers.is_empty());
}

#[cfg(feature = "dht")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dht_handle_external_address() {
    let config = DhtConfig::default();
    let handle = a3net_node::DhtHandle::new(config).await;

    // Initially no external address
    assert!(handle.external_addr().is_none());

    // Set external address
    let addr = "/ip4/203.0.113.1/tcp/4001".to_string();
    handle.set_external_addr(Some(addr.clone()));
    assert_eq!(handle.external_addr().as_deref(), Some(addr.as_str()));

    // Clear external address
    handle.set_external_addr(None);
    assert!(handle.external_addr().is_none());
}

#[cfg(feature = "dht")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dht_metrics_tracking() {
    let config = DhtConfig::default();
    let handle = a3net_node::DhtHandle::new(config).await;

    let hash1 = ContentHash::from_bytes(b"content-a");
    let hash2 = ContentHash::from_bytes(b"content-b");

    // Initial metrics
    let m0 = handle.metrics();
    assert_eq!(m0.provides_total, 0);
    assert_eq!(m0.find_total, 0);

    // Provide content
    handle.provide(&hash1).await;
    handle.provide(&hash2).await;

    let m1 = handle.metrics();
    assert_eq!(m1.provides_total, 2);

    // Find providers (hit)
    let _ = handle.find_providers(&hash1).await;
    // Find providers (miss)
    let _ = handle.find_providers(&ContentHash::from_bytes(b"missing")).await;

    let m2 = handle.metrics();
    assert_eq!(m2.find_total, 2);
    assert_eq!(m2.find_records_total, 1); // Only hash1 was found
    assert_eq!(m2.find_misses_total, 1); // The unknown hash miss
}

// ═══════════════════════════════════════════════════════════════════
//  Swarm Index Integration Tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn swarm_index_multiple_rooms() {
    use a3net_node::SwarmIndex;
    use a3net_types::{Announcement, CdnContentKind, RoomId};
    use chrono::Utc;

    let mut index = SwarmIndex::default();
    let room1 = RoomId::new("room-1");
    let room2 = RoomId::new("room-2");
    let hash1 = ContentHash::from_bytes(b"content-room1");
    let hash2 = ContentHash::from_bytes(b"content-room2");

    let ann1 = Announcement {
        room_id: room1.clone(),
        content_hash: hash1.clone(),
        node_id: NodeId::random(),
        title: "content 1".to_string(),
        kind: CdnContentKind::GenericFile,
        size_bytes: 1024,
        mime_type: None,
        source_url: None,
        ticket: None,
        timestamp: Utc::now(),
        message_id: None,
        ttl_secs: None,
        signer: None,
        signature: None,
    };

    let ann2 = Announcement {
        room_id: room2.clone(),
        content_hash: hash2.clone(),
        node_id: NodeId::random(),
        title: "content 2".to_string(),
        kind: CdnContentKind::GenericFile,
        size_bytes: 2048,
        mime_type: None,
        source_url: None,
        ticket: None,
        timestamp: Utc::now(),
        message_id: None,
        ttl_secs: None,
        signer: None,
        signature: None,
    };

    index.ingest(ann1).unwrap();
    index.ingest(ann2).unwrap();

    assert_eq!(index.total_asset_entries(), 2);

    let feed1 = index.feed_for(&room1);
    assert_eq!(feed1.assets.len(), 1);
    assert_eq!(feed1.assets[0].content_hash, hash1);

    let feed2 = index.feed_for(&room2);
    assert_eq!(feed2.assets.len(), 1);
    assert_eq!(feed2.assets[0].content_hash, hash2);
}

// ═══════════════════════════════════════════════════════════════════
//  Node Handle Accessors Integration Tests
// ═══════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_handle_accessors() {
    let (id, transport) = ephemeral_transport();
    let cfg = NodeConfig::new(data_dir(), id.clone());

    let mut builder = NodeBuilder::new(cfg).with_transport(transport.clone());
    #[cfg(feature = "dht")]
    {
        builder = builder.with_auto_init_dht(DhtConfig::default());
    }
    let node = builder
        .build_with_bus(GossipBus::new(id.clone(), shared_bus()))
        .await
        .expect("build node");

    // Test various accessors
    assert_eq!(node.node_id(), &id);
    assert!(node.transport_dyn().is_some());

    // DHT handle should be present
    #[cfg(feature = "dht")]
    {
        let dht = node.dht_handle().await;
        assert!(dht.is_some());
    }

    let _ = node.shutdown().await;
}
