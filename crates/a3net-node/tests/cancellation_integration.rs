//! Integration tests for the P1-3 `CancellationScope` wiring on `Node`.
//!
//! These tests don't try to refactor every internal `tokio::spawn`
//! site — that's a much larger refactor.  Instead they confirm the
//! end-to-end contract:
//!
//! 1. `Node::cancel_scope()` returns a non-`None` scope.
//! 2. Tasks registered via `scope.spawn()` observe cancellation.
//! 3. `Node::shutdown()` flips the scope's token (so post-shutdown
//!    registrations see the cancelled flag).
//! 4. `Node::shutdown()` remains safe to call twice (idempotent).
//!
//! The integration tests live here (rather than in
//! `crates/a3net-resilience/tests/`) because they exercise the
//! `a3net-node` wiring, not the cancellation primitive directly.
//! Primitive-level tests live in `a3net-resilience/src/cancellation.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use a3net_node::{Node, NodeConfig};
use a3net_resilience::CancellationToken;
use a3net_types::NodeId;
use tempfile::TempDir;

fn ephemeral_data_dir() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[tokio::test]
async fn node_cancel_scope_is_present_after_build() {
    let dir = ephemeral_data_dir();
    let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
        .build()
        .await
        .expect("build empty node");

    let scope = node.cancel_scope();
    assert!(
        !scope.is_cancelled(),
        "freshly built node should have an un-cancelled scope"
    );
    assert_eq!(
        scope.spawn_count(),
        0,
        "scope starts with zero tracked tasks"
    );

    node.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn node_cancel_scope_token_observable_from_outside() {
    let dir = ephemeral_data_dir();
    let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
        .build()
        .await
        .expect("build empty node");

    let token: CancellationToken = node.cancel_scope().token();
    assert!(!token.is_cancelled());

    // `Node::shutdown` must flip the token.  We can't observe the
    // flip before shutdown returns (the scope joins synchronously
    // inside shutdown), so we check after.
    node.shutdown().await.expect("shutdown");

    assert!(
        token.is_cancelled(),
        "shutdown() must propagate cancellation to all token clones"
    );
}

#[tokio::test]
async fn node_cancel_scope_spawn_observes_shutdown_signal() {
    let dir = ephemeral_data_dir();
    let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
        .build()
        .await
        .expect("build empty node");

    // Hand-register a tracked task on the node's scope.  In real
    // code this would be a background refresh loop or tracing
    // exporter; here we just verify the contract.
    let observed = std::sync::Arc::new(AtomicBool::new(false));
    let observed_clone = observed.clone();
    let token = node.cancel_scope().token();
    node.cancel_scope().spawn(Some("test-observer"), async move {
        token.cancelled().await;
        observed_clone.store(true, Ordering::SeqCst);
    });

    // Give the spawned task a moment to park on cancelled().
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!observed.load(Ordering::SeqCst));

    node.shutdown().await.expect("shutdown");
    assert!(
        observed.load(Ordering::SeqCst),
        "tracked task must observe the shutdown signal"
    );
}

#[tokio::test]
async fn node_shutdown_is_idempotent_with_cancel_scope() {
    let dir = ephemeral_data_dir();
    let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
        .build()
        .await
        .expect("build empty node");

    // First shutdown flips the scope.
    node.shutdown().await.expect("first shutdown");
    assert!(node.cancel_scope().is_cancelled());

    // Second shutdown must be a clean no-op.  The scope's join() is
    // already-drained so it should return instantly with
    // completed=0 / aborted=0.
    let started = std::time::Instant::now();
    node.shutdown().await.expect("second shutdown");
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "second shutdown should be near-instant"
    );
}

#[tokio::test]
async fn node_cancel_scope_join_summary_clean_on_empty_node() {
    let dir = ephemeral_data_dir();
    let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
        .build()
        .await
        .expect("build empty node");

    node.shutdown().await.expect("shutdown");
    let summary = node
        .cancel_scope()
        .join(Duration::from_secs(1))
        .await;
    assert_eq!(summary.completed, 0);
    assert_eq!(summary.aborted, 0);
    assert!(summary.is_clean());
}
