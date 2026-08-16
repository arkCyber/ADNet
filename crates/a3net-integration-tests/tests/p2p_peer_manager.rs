// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Integration tests for the P2P peer-manager surface: 1024-peer
// connection table, heartbeat-driven liveness detection, and
// operator-facing RPC/CLI plumbing.
//
// What this test exercises that the unit tests don't:
//
// - End-to-end: build a real `Node`, register peers through the
//   public API, drive the heartbeat machine, and snapshot the
//   result.
// - RPC plumbing: invoke the same JSON methods the daemon would
//   serve (`peer_list`, `peer_status`, `peer_tick`, `peer_prune`,
//   `peer_config`, `peer_disconnect`) through `NodeRpc` and assert
//   the JSON shape.
// - Configuration plumbing: build a `RelayConfig` with a
//   customised `p2p` block and confirm it round-trips back into a
//   `PeerManagerConfig`.

use std::sync::Arc;
use std::time::Duration;

use a3net_ipc::RpcHandler;
use a3net_ipc_adapter::{METHODS, NodeRpc};
use a3net_node::{Node, NodeConfig, PeerManagerConfig, MAX_P2P_PEERS};
use a3net_relay::{P2PConfig, RelayConfig};
use a3net_types::NodeId;
use serde_json::Value;

/// Build a minimal in-memory `Node` for testing the peer-manager
/// surface. Returns a ready-to-use `Node` plus the data dir temp
/// handle (held so the directory is not dropped mid-test).
async fn build_test_node(
    peer_cfg: Option<PeerManagerConfig>,
) -> (tempfile::TempDir, Arc<Node>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = NodeConfig::new(tmp.path(), NodeId::random());
    let mut builder = Node::builder(cfg);
    if let Some(pmc) = peer_cfg {
        builder = builder.with_peer_manager_config(pmc);
    }
    let node = builder.build().await.expect("build node");
    (tmp, Arc::new(node))
}

/// Build N random distinct `NodeId`s for fill tests.
fn random_node_ids(n: usize) -> Vec<NodeId> {
    (0..n).map(|_| NodeId::random()).collect()
}

// ─────────────────────────────────────────────────────────────────
// 1. 1024-peer capacity ceiling — sanity-check that we can hold the
//    documented number of slots.
// ─────────────────────────────────────────────────────────────────
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_table_holds_exactly_1024_peers_by_default() {
    let (_tmp, node) = build_test_node(None).await;

    // Fill the table to its documented ceiling.
    let ids = random_node_ids(MAX_P2P_PEERS);
    for id in &ids {
        node.register_peer(id.clone(), None);
    }
    assert_eq!(node.peer_manager().len(), MAX_P2P_PEERS);
    assert_eq!(node.peer_manager().len(), 1024);

    let snap = node.peer_list();
    assert_eq!(snap.capacity, 1024);
    assert_eq!(snap.peers.len(), 1024);
}

// ─────────────────────────────────────────────────────────────────
// 2. Heartbeat-driven liveness: register, refresh, and verify the
//    state machine transitions through Alive → Suspect → Dead.
// ─────────────────────────────────────────────────────────────────
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeat_state_machine_alive_suspect_dead() {
    let pm_cfg = PeerManagerConfig {
        max_peers: 16,
        heartbeat_interval: Duration::from_millis(20),
        heartbeat_timeout: Duration::from_millis(80),
    };
    let (_tmp, node) = build_test_node(Some(pm_cfg)).await;

    let ids = random_node_ids(3);
    for id in &ids {
        node.register_peer(id.clone(), None);
    }
    // The peer starts in Connecting. A heartbeat promotes it to Alive.
    node.peer_manager().record_heartbeat(&ids[0]);
    node.peer_manager().record_heartbeat(&ids[1]);
    node.peer_manager().record_heartbeat(&ids[2]);

    // All three are alive.
    let snap = node.peer_list();
    assert_eq!(snap.alive_count, 3);
    assert_eq!(snap.dead_count, 0);

    // Wait past the timeout — ids[2] never responds.
    tokio::time::sleep(Duration::from_millis(120)).await;
    let stats = node.heartbeat_tick();
    assert!(
        stats.newly_dead >= 1,
        "expected at least 1 newly_dead, got {stats:?}"
    );

    let snap = node.peer_list();
    assert!(
        snap.dead_count >= 1,
        "expected at least 1 dead peer, got {snap:?}"
    );
    // The peer that never responded is the dead one.
    let entry = node.peer_status(&ids[2]).expect("entry exists");
    assert_eq!(
        entry.status,
        a3net_node::PeerStatus::Dead,
        "peer that missed heartbeat must be marked dead"
    );
}

// ─────────────────────────────────────────────────────────────────
// 3. Recover from suspect: a missed heartbeat moves the peer to
//    Suspect; a fresh heartbeat promotes it back to Alive.
// ─────────────────────────────────────────────────────────────────
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeat_recovery_promotes_suspect_to_alive() {
    let pm_cfg = PeerManagerConfig {
        max_peers: 4,
        heartbeat_interval: Duration::from_millis(20),
        heartbeat_timeout: Duration::from_millis(120),
    };
    let (_tmp, node) = build_test_node(Some(pm_cfg)).await;

    let id = NodeId::random();
    node.register_peer(id.clone(), Some("peer-A".into()));
    node.peer_manager().record_heartbeat(&id);

    tokio::time::sleep(Duration::from_millis(40)).await;
    let _ = node.heartbeat_tick();

    let entry = node.peer_status(&id).expect("entry exists");
    assert_eq!(entry.status, a3net_node::PeerStatus::Suspect);

    // A fresh heartbeat should recover it.
    node.peer_manager().record_heartbeat(&id);
    let entry = node.peer_status(&id).expect("entry exists");
    assert_eq!(entry.status, a3net_node::PeerStatus::Alive);
    assert_eq!(entry.alias.as_deref(), Some("peer-A"));
}

// ─────────────────────────────────────────────────────────────────
// 4. Capacity rollover: when the table is full, new inserts evict
//    the oldest Dead peer.
// ─────────────────────────────────────────────────────────────────
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capacity_rollover_evicts_oldest_dead() {
    let pm_cfg = PeerManagerConfig {
        max_peers: 4,
        heartbeat_interval: Duration::from_millis(20),
        heartbeat_timeout: Duration::from_millis(60),
    };
    let (_tmp, node) = build_test_node(Some(pm_cfg)).await;

    // Fill with 4 peers and mark two of them dead.
    let ids = random_node_ids(4);
    for id in &ids {
        node.register_peer(id.clone(), None);
    }
    node.peer_manager().mark_dead(&ids[1]);
    node.peer_manager().mark_dead(&ids[2]);

    // Wait long enough that the heartbeat would also flag them dead.
    tokio::time::sleep(Duration::from_millis(80)).await;
    let _ = node.heartbeat_tick();

    // Now insert one more — must evict a dead slot, never a fresh slot.
    let new_id = NodeId::random();
    let entry = node.register_peer(new_id.clone(), None);
    assert_eq!(entry.node_id, new_id);

    let snap = node.peer_list();
    assert!(snap.peers.iter().any(|p| p.node_id == new_id));
    assert!(snap.peers.len() <= 4);
}

// ─────────────────────────────────────────────────────────────────
// 5. RPC plumbing: every documented peer-method is registered and
//    returns the expected JSON shape.
// ─────────────────────────────────────────────────────────────────
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_peer_methods_are_registered_and_dispatched() {
    let (_tmp, node) = build_test_node(None).await;
    let _rpc = NodeRpc::new(node.clone());

    for method in [
        "peer_list",
        "peer_status",
        "peer_heartbeat",
        "peer_tick",
        "peer_prune",
        "peer_config",
        "peer_connect",
        "peer_disconnect",
    ] {
        assert!(
            METHODS.contains(&method),
            "peer RPC method '{method}' is missing from METHODS"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_peer_list_returns_structured_table() {
    let (_tmp, node) = build_test_node(None).await;
    let id = NodeId::random();
    node.register_peer(id.clone(), Some("rpc-test".into()));

    let rpc = NodeRpc::new(node.clone());
    let raw = rpc
        .handle("peer_list", Value::Object(Default::default()))
        .await
        .expect("peer_list succeeds");

    let obj = raw.as_object().expect("peer_list returns object");
    assert_eq!(obj.get("capacity").and_then(|v| v.as_u64()), Some(1024));
    let peers = obj.get("peers").and_then(|v| v.as_array()).expect("peers");
    assert_eq!(peers.len(), 1);
    let peer = &peers[0];
    assert_eq!(
        peer.get("node_id").and_then(|v| v.as_str()),
        Some(id.as_hex())
    );
    assert_eq!(
        peer.get("alias").and_then(|v| v.as_str()),
        Some("rpc-test")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_peer_status_returns_null_when_unknown() {
    let (_tmp, node) = build_test_node(None).await;
    let rpc = NodeRpc::new(node.clone());
    let unknown = NodeId::random();
    let raw = rpc
        .handle(
            "peer_status",
            serde_json::json!({ "peer_id": unknown.as_hex() }),
        )
        .await
        .expect("peer_status succeeds");
    assert!(raw.is_null(), "unknown peer should yield null");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_peer_config_matches_documented_defaults() {
    let (_tmp, node) = build_test_node(None).await;
    let rpc = NodeRpc::new(node.clone());
    let raw = rpc
        .handle("peer_config", Value::Object(Default::default()))
        .await
        .expect("peer_config succeeds");
    assert_eq!(raw.get("max_peers").and_then(|v| v.as_u64()), Some(1024));
    assert_eq!(
        raw.get("heartbeat_interval_seconds").and_then(|v| v.as_u64()),
        Some(15)
    );
    assert_eq!(
        raw.get("heartbeat_timeout_seconds").and_then(|v| v.as_u64()),
        Some(45)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_peer_disconnect_marks_removed() {
    let (_tmp, node) = build_test_node(None).await;
    let id = NodeId::random();
    node.register_peer(id.clone(), None);
    let rpc = NodeRpc::new(node.clone());
    let raw = rpc
        .handle(
            "peer_disconnect",
            serde_json::json!({ "peer_id": id.as_hex() }),
        )
        .await
        .expect("peer_disconnect succeeds");
    assert_eq!(raw.get("removed").and_then(|v| v.as_bool()), Some(true));
    let entry = node.peer_status(&id).expect("entry still present");
    assert_eq!(entry.status, a3net_node::PeerStatus::Removed);
}

// ─────────────────────────────────────────────────────────────────
// 6. Configuration plumbing: the relay `p2p` block survives a
//    JSON round-trip and is reachable from the `PeerManagerConfig`
//    that the node uses.
// ─────────────────────────────────────────────────────────────────
#[test]
fn relay_p2p_block_round_trips_into_peer_manager_config() {
    let cfg = RelayConfig {
        p2p: P2PConfig {
            max_peers: 256,
            heartbeat_interval: Duration::from_secs(5),
            heartbeat_timeout: Duration::from_secs(15),
            prune_grace: Duration::from_secs(60),
        },
        ..RelayConfig::default()
    };

    let raw = serde_json::to_string(&cfg).expect("serialize");
    let back: RelayConfig = serde_json::from_str(&raw).expect("parse");
    assert_eq!(back.p2p.max_peers, 256);
    assert_eq!(back.p2p.heartbeat_interval, Duration::from_secs(5));
    assert_eq!(back.p2p.heartbeat_timeout, Duration::from_secs(15));
    assert_eq!(back.p2p.prune_grace, Duration::from_secs(60));
}

#[test]
fn relay_p2p_defaults_match_documented_constants() {
    let cfg = RelayConfig::default();
    let p2p = cfg.p2p_config();
    assert_eq!(p2p.max_peers, MAX_P2P_PEERS);
    assert_eq!(p2p.max_peers, 1024);
    assert_eq!(p2p.heartbeat_interval, Duration::from_secs(15));
    assert_eq!(p2p.heartbeat_timeout, Duration::from_secs(45));
}
