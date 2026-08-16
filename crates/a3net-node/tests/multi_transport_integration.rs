//! Integration tests for `NodeBuilder::add_transport` (MultiTransport
//! wiring).
//!
//! These tests live in their own file (rather than `node_integration.rs`)
//! because the latter is gated behind `bitswap` or `dht` cargo features.
//! The MultiTransport wiring is core node plumbing that should be
//! exercised in default builds too, so this file has no feature
//! requirement.
//!
//! The unit-level MultiTransport fan-out / fall-through semantics are
//! tested in `a3net-transport::multi::tests`. What we cover here is the
//! seam between `NodeBuilder` and `MultiTransport::new`: that
//! `add_transport` actually wraps the backends, that the resulting
//! `transport_dyn()` reports the correct kind and NodeId, and that a
//! mismatched NodeId between backends surfaces as a build error
//! instead of silently routing messages to nobody.

use std::sync::Arc;

use a3net_gossip::{GossipBus, InProcessGossip};
use a3net_node::{NodeBuilder, NodeConfig};
use a3net_transport::quic::{derive_node_id_from_cert, QuicTransportBuilder, TransportIdentity};
use a3net_transport::SharedTransport;
use a3net_types::NodeId;

fn ephemeral_transport() -> (NodeId, SharedTransport) {
    let transport = QuicTransportBuilder::new(NodeId::random(), "127.0.0.1:0".parse().unwrap())
        .build()
        .expect("build ephemeral quic transport");
    let id = transport.local_node_id().clone();
    let transport = Arc::new(transport) as SharedTransport;
    (id, transport)
}

/// Build two `QuicTransport` instances that share the same local
/// NodeId. The id is derived from a single freshly generated QUIC
/// certificate and reused (via `TransportIdentity::clone`) by both
/// transports, so each backend's `derive_node_id_from_cert` returns
/// the same NodeId. MultiTransport's `new` rejects mismatched
/// local_node values; this helper is the canonical way for tests to
/// construct a valid multi-backend set.
fn paired_transports() -> (SharedTransport, SharedTransport, NodeId) {
    let identity = TransportIdentity::generate().expect("gen identity");
    let shared_node = derive_node_id_from_cert(identity.cert_der()).expect("derive node id");
    let make = || -> SharedTransport {
        Arc::new(
            QuicTransportBuilder::new(shared_node.clone(), "127.0.0.1:0".parse().unwrap())
                .with_identity(identity.clone())
                .build()
                .expect("build quic transport"),
        ) as SharedTransport
    };
    (make(), make(), shared_node)
}

fn data_dir() -> std::path::PathBuf {
    let dir = tempfile::tempdir().expect("tempdir");
    dir.path().to_path_buf()
}

fn shared_bus() -> Arc<dyn a3net_gossip::GossipTransport> {
    Arc::new(InProcessGossip::new())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_transport_wraps_in_multi_transport() {
    // Two independent QUIC transports share the same local NodeId
    // (MultiTransport::new requires identical local_node across
    // backends). The wrapped transport_dyn() should report kind ==
    // "multi" and the shared local_node.
    let (primary, secondary, shared_node) = paired_transports();

    let cfg = NodeConfig::new(data_dir(), shared_node.clone());
    let node = NodeBuilder::new(cfg)
        .with_transport(primary)
        .add_transport(secondary)
        .build_with_bus(GossipBus::new(shared_node.clone(), shared_bus()))
        .await
        .expect("build node with multi-transport");

    let t = node
        .transport_dyn()
        .expect("transport should be wired after add_transport");
    assert_eq!(
        t.kind(),
        "multi",
        "add_transport should produce a multi-kind transport"
    );
    assert_eq!(t.local_node(), &shared_node);

    let _ = node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_transport_without_primary_promotes_first_extra() {
    // `add_transport` without a prior `with_transport` is supported:
    // the first added backend is promoted to the primary slot. The
    // transport should still be exposed and have the correct
    // local_node.
    let (id, only) = ephemeral_transport();

    let cfg = NodeConfig::new(data_dir(), id.clone());
    let node = NodeBuilder::new(cfg)
        .add_transport(only)
        .build_with_bus(GossipBus::new(id.clone(), shared_bus()))
        .await
        .expect("build node with single add_transport");

    let t = node
        .transport_dyn()
        .expect("transport should be wired after add_transport only");
    // With one backend the wrapper is left as-is to preserve the
    // legacy single-transport behavior (kind stays as the underlying
    // transport's kind rather than "multi").
    assert_eq!(t.local_node(), &id);

    let _ = node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_transport_rejects_mismatched_local_node() {
    // MultiTransport::new must refuse backends with different
    // local_node values. The builder surfaces this as a build error
    // with an actionable message.
    let (id_a, transport_a) = ephemeral_transport();
    let (id_b, transport_b) = ephemeral_transport();
    assert_ne!(id_a, id_b, "fixture must produce distinct NodeIds");

    let cfg = NodeConfig::new(data_dir(), id_a.clone());
    let result = NodeBuilder::new(cfg)
        .with_transport(transport_a)
        .add_transport(transport_b)
        .build_with_bus(GossipBus::new(id_a, shared_bus()))
        .await;

    let err = match result {
        Ok(_) => panic!("mismatched local_node should cause build failure"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("local node") || msg.contains("node_id"),
        "error should explain the local node mismatch, got: {msg}"
    );
}
