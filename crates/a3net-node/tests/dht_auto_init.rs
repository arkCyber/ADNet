#![cfg(feature = "dht")]

//! P0-D integration tests: DHT and IPNS are auto-initialised by
//! `NodeBuilder::build_with_bus` when the corresponding
//! `with_auto_init_*` setters are used.
//!
//! These exercise the seams closed by P0-D:
//! - `Node::init_dht` wires the supplied transport into a fresh
//!   `DhtHandle` and exposes it via `dht_handle()`.
//! - `Node::init_ipns` (read-only, no keypair) builds an `IpnHandle`
//!   backed by the DHT query, and exposes it via `ipn_handle()`.
//! - `NodeBuilder::with_auto_init_dht` / `with_auto_init_ipns` invoke
//!   the above without the operator having to call them manually.
//! - `Node::dht_handle()` / `Node::ipn_handle()` are visible after
//!   construction (callers don't have to wait for a second init
//!   call).

use std::time::Duration;

use a3net_node::{Node, NodeBuilder, NodeConfig};
use a3net_transport::{SharedTransport, Transport, quic::QuicTransportBuilder};
use a3net_types::NodeId;

fn ephemeral_transport() -> (NodeId, SharedTransport) {
    let local_id = NodeId::random();
    let transport = QuicTransportBuilder::new(local_id.clone(), "127.0.0.1:0".parse().unwrap())
        .build()
        .expect("build ephemeral quic transport");
    let id = transport.local_node_id().clone();
    let transport = std::sync::Arc::new(transport) as SharedTransport;
    (id, transport)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builder_with_auto_init_dht_wires_handle() {
    let (_, transport) = ephemeral_transport();
    let cfg = NodeConfig::default();
    let node = NodeBuilder::new(cfg)
        .with_transport(transport)
        .with_auto_init_dht(a3net_node::DhtConfig::default())
        .build_with_bus(a3net_gossip::GossipBus::default())
        .await
        .expect("build with auto_init_dht");

    let handle = node
        .dht_handle()
        .await
        .expect("dht_handle must be Some after auto_init_dht");
    assert!(
        !handle.find_providers(&sample_hash()).await.is_empty(),
        "auto-initialised DHT should have local provider records"
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builder_without_auto_init_dht_leaves_handle_none() {
    let (_, transport) = ephemeral_transport();
    let cfg = NodeConfig::default();
    let node = NodeBuilder::new(cfg)
        .with_transport(transport)
        .build_with_bus(a3net_gossip::GossipBus::default())
        .await
        .expect("build without auto_init_dht");

    assert!(
        node.dht_handle().await.is_none(),
        "DHT handle must stay None when auto_init_dht is not requested"
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builder_with_auto_init_ipns_sets_up_resolver() {
    let (_, transport) = ephemeral_transport();
    let cfg = NodeConfig::default();
    let node = NodeBuilder::new(cfg)
        .with_transport(transport)
        .with_auto_init_dht(a3net_node::DhtConfig::default())
        .with_auto_init_ipns(a3net_node::IpnConfig::default())
        .build_with_bus(a3net_gossip::GossipBus::default())
        .await
        .expect("build with auto_init_dht + auto_init_ipns");

    let handle = node
        .ipn_handle()
        .await
        .expect("ipn_handle must be Some after auto_init_ipns");
    let stats = format!("{:?}", handle);
    assert!(
        stats.contains("publisher: false"),
        "read-only IPNS handle must report no publisher; got: {stats}"
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_init_dht_then_init_ipns_round_trip() {
    let (_, transport) = ephemeral_transport();
    let cfg = NodeConfig::default();
    let node = NodeBuilder::new(cfg)
        .with_transport(transport)
        .build_with_bus(a3net_gossip::GossipBus::default())
        .await
        .expect("build");

    // First init DHT explicitly.
    node.init_dht(a3net_node::DhtConfig::default())
        .await
        .expect("init_dht");
    assert!(node.dht_handle().await.is_some());

    // Then init IPNS read-only (no keypair).
    node.init_ipns(a3net_node::IpnConfig::default(), None)
        .await
        .expect("init_ipns");
    let handle = node.ipn_handle().await.expect("ipn_handle populated");
    assert_eq!(handle.local_node_id(), node.node_id());

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_ipns_without_init_dht_errors() {
    let (_, transport) = ephemeral_transport();
    let cfg = NodeConfig::default();
    let node = NodeBuilder::new(cfg)
        .with_transport(transport)
        .build_with_bus(a3net_gossip::GossipBus::default())
        .await
        .expect("build");

    let err = node
        .init_ipns(a3net_node::IpnConfig::default(), None)
        .await
        .expect_err("init_ipns without init_dht must error");
    assert!(
        err.to_string().contains("init_ipns"),
        "error message should mention init_ipns; got: {err}"
    );

    node.shutdown().await;
}

fn sample_hash() -> a3net_types::ContentHash {
    a3net_types::ContentHash::from_bytes(b"p0-d-auto-init")
}
