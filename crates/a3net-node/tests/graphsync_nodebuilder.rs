//! Tests that `NodeBuilder::with_graphsync` correctly hooks up the
//! `GraphSyncService` to a `SharedTransport` and a `Node` exposes
//! the resulting handle.

#![cfg(feature = "graphsync")]

use std::sync::Arc;

use a3net_node::graphsync::{GraphSyncConfig, GraphSyncStats};
use a3net_node::{Node, NodeConfig};
use a3net_types::NodeId;
use a3net_transport::SharedTransport;
use a3net_types::NodeAddr;
use async_trait::async_trait;
use tempfile::tempdir;

/// Tiny in-memory `Transport` implementation used only to feed
/// `NodeBuilder::with_transport`. We don't actually drive QUIC here;
/// the `GraphSyncService` accept loop will see `accept()` return
/// `None` immediately and the dispatcher will exit on its own
/// (because `event_rx` is closed by the dispatcher shutdown).
struct EmptyTransport {
    local: NodeId,
}

#[async_trait]
impl a3net_transport::Transport for EmptyTransport {
    fn kind(&self) -> &'static str {
        "empty"
    }
    fn local_node(&self) -> &NodeId {
        &self.local
    }
    async fn dial(
        &self,
        _peer: NodeId,
    ) -> a3net_transport::TransportResult<Box<dyn a3net_transport::OutgoingConnection>>
    {
        Err(a3net_transport::TransportError::Other("empty transport".into()))
    }
    async fn dial_addr(
        &self,
        _addr: NodeAddr,
    ) -> a3net_transport::TransportResult<Box<dyn a3net_transport::OutgoingConnection>>
    {
        Err(a3net_transport::TransportError::Other("empty transport".into()))
    }
    async fn accept(
        &self,
    ) -> a3net_transport::TransportResult<
        Option<(NodeId, Box<dyn a3net_transport::OutgoingConnection>)>,
    > {
        Ok(None)
    }
    async fn take_incoming_receiver(
        &self,
    ) -> Option<tokio::sync::mpsc::Receiver<(NodeId, Box<dyn a3net_transport::OutgoingConnection>)>>
    {
        None
    }
    async fn shutdown(&self) -> a3net_transport::TransportResult<()> {
        Ok(())
    }
}

fn shared_empty(node_id: NodeId) -> SharedTransport {
    Arc::new(EmptyTransport { local: node_id })
}

#[tokio::test]
async fn with_graphsync_wires_handle_into_node() {
    let dir = tempdir().unwrap();
    let node_id = NodeId::random();
    let cfg = NodeConfig::new(dir.path(), node_id.clone());
    let shared: SharedTransport = shared_empty(node_id.clone());

    let handle = Node::builder(cfg)
        .with_transport(shared)
        .with_graphsync(GraphSyncConfig {
            spawn_accept_loop: false,
            ..GraphSyncConfig::default()
        })
        .build()
        .await
        .expect("node with graphsync should build");

    let gs = handle.graphsync_service().expect("graphsync service");
    let stats: GraphSyncStats = gs.stats();
    assert_eq!(stats.requests_sent, 0);
}

#[tokio::test]
async fn graphsync_handle_optional_when_not_configured() {
    let dir = tempdir().unwrap();
    let node_id = NodeId::random();
    let cfg = NodeConfig::new(dir.path(), node_id);
    let node = Node::builder(cfg).build().await.unwrap();
    assert!(node.graphsync_service().is_none());
}

#[tokio::test]
async fn graphsync_handle_shutdown_is_safe() {
    let dir = tempdir().unwrap();
    let node_id = NodeId::random();
    let cfg = NodeConfig::new(dir.path(), node_id.clone());
    let shared = shared_empty(node_id);

    let node = Node::builder(cfg)
        .with_transport(shared)
        .with_graphsync(GraphSyncConfig::default())
        .build()
        .await
        .expect("build should succeed with empty transport + graphsync");

    let _ = node.shutdown().await;
}
