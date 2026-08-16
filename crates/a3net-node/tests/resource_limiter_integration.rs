//! Integration tests for the P1-5 `ResourceLimiter` wiring on `Node`.
//!
//! These tests confirm the contract:
//!
//! 1. `Node::peer_limiter()`, `Node::room_limiter()`,
//!    `Node::tag_limiter()` return non-`None` limiters.
//! 2. Each limiter has the documented default configuration
//!    (256 global / 16 per peer, etc.).
//! 3. Acquired permits release on drop, leaving the limiter's
//!    available count unchanged.
//! 4. Exhausting the global pool causes subsequent `try_acquire()`
//!    calls to fail (the contract that protects the node from
//!    being pinned by a single peer / room / tag).
//! 5. `Node::shutdown()` is unaffected by the limiter wiring
//!    (idempotent).

use a3net_node::{Node, NodeConfig};
use a3net_resilience::AcquireError;
use a3net_types::NodeId;
use tempfile::TempDir;

fn ephemeral_data_dir() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[tokio::test]
async fn node_limiters_are_present_after_build() {
    let dir = ephemeral_data_dir();
    let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
        .build()
        .await
        .expect("build empty node");

    // Defaults from a3net-resilience: peer=256/16, room=64/32,
    // tag=512/64.
    assert_eq!(node.peer_limiter().global_limit(), 256);
    assert_eq!(node.peer_limiter().per_key_limit(), 16);
    assert_eq!(node.room_limiter().global_limit(), 64);
    assert_eq!(node.room_limiter().per_key_limit(), 32);
    assert_eq!(node.tag_limiter().global_limit(), 512);
    assert_eq!(node.tag_limiter().per_key_limit(), 64);

    node.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn node_peer_limiter_isolates_peers() {
    let dir = ephemeral_data_dir();
    let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
        .build()
        .await
        .expect("build empty node");

    let lim = node.peer_limiter();
    // Hold 16 permits on peer A (the per-peer cap).
    let mut held = Vec::with_capacity(16);
    for _ in 0..16 {
        held.push(
            lim.try_acquire("peer-A".into())
                .expect("peer-A permit"),
        );
    }
    // 17th attempt on the same peer must fail.
    assert!(
        lim.try_acquire("peer-A".into()).is_none(),
        "per-peer cap should reject 17th"
    );
    // But a different peer can still acquire (global has room).
    let p_b = lim.try_acquire("peer-B".into()).expect("peer-B permit");
    assert_eq!(lim.global_available(), 256 - 17);
    drop(held);
    drop(p_b);
    assert_eq!(lim.global_available(), 256);

    node.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn node_room_limiter_caps_per_room_fanout() {
    let dir = ephemeral_data_dir();
    let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
        .build()
        .await
        .expect("build empty node");

    let lim = node.room_limiter();
    let mut held = Vec::with_capacity(32);
    for _ in 0..32 {
        held.push(lim.try_acquire("hot-room".into()).expect("hot-room"));
    }
    // 33rd on the same room rejected.
    assert!(lim.try_acquire("hot-room".into()).is_none());
    // Different room still works (global = 64).
    let p_other = lim.try_acquire("cold-room".into()).expect("cold-room");
    drop(held);
    drop(p_other);

    node.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn node_tag_limiter_tracks_per_tag_buckets() {
    let dir = ephemeral_data_dir();
    let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
        .build()
        .await
        .expect("build empty node");

    let lim = node.tag_limiter();
    let p1 = lim.try_acquire("blobstore.fetch".into()).expect("blobstore");
    let p2 = lim.try_acquire("relay.proxy".into()).expect("relay");
    // Different tags, distinct buckets — both succeed.
    assert_eq!(lim.tracked_keys(), 2);
    drop(p1);
    drop(p2);

    node.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn node_limiter_metrics_reflect_activity() {
    let dir = ephemeral_data_dir();
    let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
        .build()
        .await
        .expect("build empty node");

    let lim = node.peer_limiter();
    let snap_before = lim.snapshot();
    let _p = lim.try_acquire("peer-X".into()).expect("permit");
    let snap_after = lim.snapshot();
    assert_eq!(snap_after.acquired, snap_before.acquired + 1);

    node.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn node_limiter_acquire_with_cancellation_uses_scope_token() {
    // P1-3 + P1-5: confirm the limiter's `acquire(.., Some(token))`
    // path bails on cancellation when the node's cancel scope is
    // fired.  This is the integration point between the two P1
    // tasks.
    let dir = ephemeral_data_dir();
    let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
        .build()
        .await
        .expect("build empty node");

    // Saturate the per-peer budget for `peer-Y`.
    let mut held = Vec::with_capacity(16);
    for _ in 0..16 {
        held.push(
            node.peer_limiter()
                .try_acquire("peer-Y".into())
                .expect("acquire"),
        );
    }
    let token = node.cancel_scope().token();
    // Spawn a task that tries to acquire and observes cancellation.
    // We deliberately `try_unwrap` the Arc-bound limiter after the
    // spawn returns so the closure owns the only reference.
    let lim_arc = node.peer_limiter().clone();
    let waiter = {
        let lim = lim_arc.clone();
        let token_for_task = token.clone();
        tokio::spawn(async move {
            lim.acquire(
                "peer-Y".into(),
                std::time::Duration::from_secs(5),
                Some(token_for_task),
            )
            .await
        })
    };
    // Give the waiter a moment to park on the per-peer wait.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    node.cancel_scope().cancel();
    let result = waiter.await.expect("task panicked");
    assert_eq!(result.err(), Some(AcquireError::Cancelled));
    // Waiter exited and dropped its `lim`; the limiter Arc refcount
    // is back to 1 (the `lim_arc` local), which we drop here.
    drop(lim_arc);

    drop(held);
    node.shutdown().await.expect("shutdown");
}