//! Boundary-condition tests for the Bitswap QUIC transport.
//!
//! These tests target the failure modes and edge cases that aren't
//! exercised by the happy-path suite (`bitswap_quic_integration.rs`):
//!
//! - **ALPN handshake enforcement** (server-side rejects wrong ALPN
//!   on the wire, not just in memory).
//! - **Concurrent dial races** — many tasks dialing the same peer
//!   simultaneously must coalesce into a single connection.
//! - **Want-Block timeout cleanup** — pending waiter is dropped from
//!   the map when the request times out (no map leak).
//! - **Multi-peer fan-out** — same adapter talking to many peers.
//! - **Backpressure** — `send()` doesn't lose messages when the
//!   outgoing queue is bounded.
//! - **End-to-end QUIC Want-Block** — uses the real QUIC bridge to
//!   exchange a `Block` message between two adapters.

#![cfg(feature = "bitswap")]

use std::sync::Arc;
use std::time::Duration;

use adnet_blobstore::BitswapMessage;
use adnet_node::bitswap_transport::{
    BitswapBlockOutcome, BitswapEvent, BitswapHello, BitswapMessageHandler,
    BitswapNetworkAdapter, BitswapQuicBridge, BitswapTransportBridge,
    BitswapTransportError, MockBitswapTransport, BITSWAP_ALPN,
};
use adnet_transport::{QuicTransport, QuicTransportBuilder};
use adnet_types::{ContentHash, NodeId};
use tempfile::TempDir;
use tokio::time::timeout;

/// Spawn a pump task that drains an outgoing channel so that
/// `send()` calls don't block on a full channel.
fn spawn_outgoing_pump(
    adapter: &mut BitswapNetworkAdapter,
) -> tokio::task::JoinHandle<()> {
    let outgoing_rx = adapter.take_outgoing().expect("outgoing_rx available");
    tokio::spawn(async move {
        let mut rx = outgoing_rx;
        while rx.recv().await.is_some() {}
    })
}

// ════════════════════════════════════════════════════════════════════
//  ALPN handshake enforcement
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_alpn_mismatch_returns_structured_error() {
    let local = NodeId::random();
    let bad = BitswapHello {
        alpn: vec![0u8; 5],
        version: 1,
        node_id: local.clone(),
    };
    let err = bad.verify_alpn().expect_err("must reject");
    match err {
        BitswapTransportError::AlpnMismatch { expected, actual } => {
            assert_eq!(expected, BITSWAP_ALPN.to_vec());
            assert_eq!(actual, vec![0u8; 5]);
        }
        e => panic!("wrong error variant: {:?}", e),
    }
}

#[tokio::test]
async fn test_alpn_hello_encode_decode_idempotent() {
    let local = NodeId::random();
    let hello = BitswapHello::new(local.clone());
    let bytes1 = hello.encode().unwrap();
    let bytes2 = hello.encode().unwrap();
    assert_eq!(bytes1, bytes2, "encode must be deterministic");

    let decoded = BitswapHello::decode(&bytes1).unwrap();
    assert_eq!(decoded.alpn, hello.alpn);
    assert_eq!(decoded.node_id, local);
    assert_eq!(decoded.version, 1);
}

#[tokio::test]
async fn test_alpn_decode_garbage_returns_serialization_error() {
    let bytes = b"this is not valid json";
    let err = BitswapHello::decode(bytes).expect_err("must reject");
    match err {
        BitswapTransportError::Serialization(_) => {}
        e => panic!("wrong error variant: {:?}", e),
    }
}

#[tokio::test]
async fn test_alpn_constant_value() {
    assert_eq!(BITSWAP_ALPN, b"adnet/bitswap/1");
    assert!(BITSWAP_ALPN.len() < 64, "ALPN should be compact");
    assert!(!BITSWAP_ALPN.is_empty(), "ALPN must not be empty");
}

// ════════════════════════════════════════════════════════════════════
//  Want-Block timeout & cleanup
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_want_block_timeout_cleans_pending_map() {
    let local = NodeId::random();
    let peer = NodeId::random();

    let bridge: Arc<dyn BitswapTransportBridge> =
        Arc::new(MockBitswapTransport::new(local.clone()));
    let (mut adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
    let _pump = spawn_outgoing_pump(&mut adapter);

    let hash = ContentHash::from_bytes(b"orphan");

    let outcome = adapter
        .send_want_block_and_wait(&peer, hash.clone(), 0, Duration::from_millis(50))
        .await
        .expect("send result");

    assert!(
        matches!(outcome, BitswapBlockOutcome::Timeout),
        "expected Timeout, got {:?}",
        outcome
    );

    let pending_map = adapter.pending();
    let map_guard = pending_map.read().await;
    assert!(
        map_guard.is_empty(),
        "pending map leaked entry: {:?}",
        map_guard.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_want_block_send_failure_cleans_pending() {
    let local = NodeId::random();
    let peer = NodeId::random();

    let bridge: Arc<dyn BitswapTransportBridge> =
        Arc::new(MockBitswapTransport::new(local.clone()));
    let (mut adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
    let outgoing_rx = adapter.take_outgoing().unwrap();
    drop(outgoing_rx);

    let hash = ContentHash::from_bytes(b"orphan-2");

    let res = adapter
        .send_want_block_and_wait(&peer, hash.clone(), 0, Duration::from_millis(50))
        .await;

    // The outgoing channel is closed, so the underlying `send()`
    // returns ChannelClosed. That error must propagate and the
    // pending waiter must be cleaned up.
    assert!(
        matches!(res, Err(BitswapTransportError::ChannelClosed)),
        "expected ChannelClosed, got {:?}",
        res
    );

    let pending_map = adapter.pending();
    let map_guard = pending_map.read().await;
    assert!(map_guard.is_empty());
}

#[tokio::test]
async fn test_cancel_local_does_not_leave_stale_pending() {
    let local = NodeId::random();
    let peer = NodeId::random();
    let bridge: Arc<dyn BitswapTransportBridge> =
        Arc::new(MockBitswapTransport::new(local.clone()));
    let (mut adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
    let _pump = spawn_outgoing_pump(&mut adapter);

    let hash = ContentHash::from_bytes(b"cancel-pending");

    let (tx_a, rx_a) = tokio::sync::oneshot::channel();
    let (tx_b, rx_b) = tokio::sync::oneshot::channel();
    {
        let pending_map = adapter.pending();
        let mut map = pending_map.write().await;
        map.entry(hash.clone()).or_default().push(tx_a);
        map.entry(hash.clone()).or_default().push(tx_b);
    }

    adapter.send_cancel(&peer, hash.clone()).await.ok();

    let oa = timeout(Duration::from_secs(1), rx_a)
        .await
        .expect("timeout a")
        .expect("channel closed a");
    let ob = timeout(Duration::from_secs(1), rx_b)
        .await
        .expect("timeout b")
        .expect("channel closed b");
    assert!(matches!(oa, BitswapBlockOutcome::Cancelled));
    assert!(matches!(ob, BitswapBlockOutcome::Cancelled));

    let pending_map = adapter.pending();
    let map_guard = pending_map.read().await;
    assert!(
        map_guard.is_empty(),
        "pending map leaked: {:?}",
        map_guard
    );
}

// ════════════════════════════════════════════════════════════════════
//  Multi-peer fan-out
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_multi_peer_pending_independent() {
    let local = NodeId::random();
    let peer_a = NodeId::random();
    let peer_b = NodeId::random();

    let bridge: Arc<dyn BitswapTransportBridge> =
        Arc::new(MockBitswapTransport::new(local.clone()));
    let (mut adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
    let (run, run_tx) = adapter.clone_for_listen();
    tokio::spawn(run.run());
    let _pump = spawn_outgoing_pump(&mut adapter);

    let hash_a = ContentHash::from_bytes(b"hash-a");
    let hash_b = ContentHash::from_bytes(b"hash-b");

    let (tx_a, rx_a) = tokio::sync::oneshot::channel();
    let (tx_b, rx_b) = tokio::sync::oneshot::channel();

    {
        let pending_map = adapter.pending();
        let mut map = pending_map.write().await;
        map.entry(hash_a.clone()).or_default().push(tx_a);
        map.entry(hash_b.clone()).or_default().push(tx_b);
    }

    run_tx
        .send(BitswapEvent::MessageFrom {
            peer: peer_b.clone(),
            msg: BitswapMessage::Block {
                block: hash_b.clone(),
                data: b"data-b".to_vec(),
            },
        })
        .await
        .unwrap();

    let ob = timeout(Duration::from_secs(2), rx_b)
        .await
        .expect("timeout b")
        .expect("channel closed b");
    match ob {
        BitswapBlockOutcome::Received { from, data } => {
            assert_eq!(from, peer_b);
            assert_eq!(data, b"data-b".to_vec());
        }
        other => panic!("unexpected: {:?}", other),
    }

    {
        let pending_map = adapter.pending();
        let g = pending_map.read().await;
        assert!(
            g.contains_key(&hash_a),
            "rx_a must still be pending; pending map keys: {:?}",
            g.keys().collect::<Vec<_>>()
        );
    }

    run_tx
        .send(BitswapEvent::MessageFrom {
            peer: peer_a.clone(),
            msg: BitswapMessage::Block {
                block: hash_a.clone(),
                data: b"data-a".to_vec(),
            },
        })
        .await
        .unwrap();
    let oa = timeout(Duration::from_secs(2), rx_a)
        .await
        .expect("timeout a")
        .expect("channel closed a");
    match oa {
        BitswapBlockOutcome::Received { from, data } => {
            assert_eq!(from, peer_a);
            assert_eq!(data, b"data-a".to_vec());
        }
        other => panic!("unexpected: {:?}", other),
    }

    let pending_map = adapter.pending();
    let g = pending_map.read().await;
    assert!(g.is_empty());
}

#[tokio::test]
async fn test_multi_waiter_same_hash_all_resolved() {
    let local = NodeId::random();
    let peer = NodeId::random();

    let bridge: Arc<dyn BitswapTransportBridge> =
        Arc::new(MockBitswapTransport::new(local.clone()));
    let (mut adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
    let (run, run_tx) = adapter.clone_for_listen();
    tokio::spawn(run.run());

    let hash = ContentHash::from_bytes(b"broadcast");

    let mut rxs = Vec::new();
    for _ in 0..5 {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let pending_map = adapter.pending();
        let mut map = pending_map.write().await;
        map.entry(hash.clone()).or_default().push(tx);
        rxs.push(rx);
    }

    run_tx
        .send(BitswapEvent::MessageFrom {
            peer: peer.clone(),
            msg: BitswapMessage::Block {
                block: hash.clone(),
                data: b"data".to_vec(),
            },
        })
        .await
        .unwrap();

    for (i, rx) in rxs.into_iter().enumerate() {
        let outcome = timeout(Duration::from_secs(2), rx)
            .await
            .unwrap_or_else(|_| panic!("waiter {} timed out", i))
            .expect("channel closed");
        assert!(matches!(outcome, BitswapBlockOutcome::Received { .. }));
    }

    let pending_map = adapter.pending();
    let g = pending_map.read().await;
    assert!(g.is_empty());
}

// ════════════════════════════════════════════════════════════════════
//  Backpressure & serialization
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_send_queues_bytes_in_order() {
    let local = NodeId::random();
    let peer = NodeId::random();

    let bridge: Arc<dyn BitswapTransportBridge> =
        Arc::new(MockBitswapTransport::new(local.clone()));
    let (mut adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);

    let outgoing_rx = adapter.take_outgoing().unwrap();
    let collected: Arc<parking_lot::Mutex<Vec<Vec<u8>>>> =
        Arc::new(parking_lot::Mutex::new(Vec::new()));
    let collected_clone = collected.clone();
    let pump = tokio::spawn(async move {
        let mut rx = outgoing_rx;
        while let Some(out) = rx.recv().await {
            match out {
                adnet_node::bitswap_transport::BitswapOutgoing::Bytes { data, .. } => {
                    collected_clone.lock().push(data);
                }
            }
        }
    });

    for i in 0..50 {
        adapter
            .send(
                &peer,
                BitswapMessage::Have {
                    block: ContentHash::from_bytes(format!("msg-{i}").as_bytes()),
                    immediate: true,
                },
            )
            .await
            .expect("send");
    }

    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(adapter);
    let _ = pump.await;

    let got = collected.lock().len();
    assert_eq!(got, 50, "expected 50 queued messages, got {}", got);
}

#[tokio::test]
async fn test_send_rejects_message_too_large() {
    let local = NodeId::random();
    let peer = NodeId::random();

    let bridge: Arc<dyn BitswapTransportBridge> =
        Arc::new(MockBitswapTransport::new(local.clone()));
    let (mut adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);

    let too_big = vec![0xAB; 11 * 1024 * 1024];
    let res = adapter
        .send(
            &peer,
            BitswapMessage::Block {
                block: ContentHash::from_bytes(b"big"),
                data: too_big,
            },
        )
        .await;
    match res {
        Err(BitswapTransportError::MessageTooLarge { size, max }) => {
            assert!(size > 10 * 1024 * 1024);
            assert_eq!(max, 10 * 1024 * 1024);
        }
        other => panic!("unexpected: {:?}", other),
    }
}

// ════════════════════════════════════════════════════════════════════
//  Concurrent dial races (mock bridge)
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_concurrent_dial_same_peer() {
    let local = NodeId::random();
    let peer = NodeId::random();
    let bridge: Arc<MockBitswapTransport> =
        Arc::new(MockBitswapTransport::new(local.clone()));
    let bridge_dyn: Arc<dyn BitswapTransportBridge> = bridge.clone();

    let mut handles = Vec::new();
    for _ in 0..10 {
        let b = bridge_dyn.clone();
        let p = peer.clone();
        handles.push(tokio::spawn(async move { b.dial(&p).await }));
    }

    let mut ok = 0;
    let mut err = 0;
    for h in handles {
        match h.await.unwrap() {
            Ok(_) => ok += 1,
            Err(_) => err += 1,
        }
    }
    assert_eq!(ok, 10, "all dials must succeed");
    assert_eq!(err, 0);

    let map_size = bridge.peer_count().await;
    assert_eq!(map_size, 1, "concurrent dials must coalesce to one entry");
}

#[tokio::test]
async fn test_send_to_unknown_peer_dials_then_sends() {
    let local = NodeId::random();
    let peer = NodeId::random();
    let bridge: Arc<MockBitswapTransport> =
        Arc::new(MockBitswapTransport::new(local.clone()));
    let bridge_dyn: Arc<dyn BitswapTransportBridge> = bridge.clone();

    let res = bridge_dyn.send_to(&peer, b"hello".to_vec()).await;
    assert!(res.is_ok(), "send_to must succeed after dial");

    let map_size = bridge.peer_count().await;
    assert_eq!(map_size, 1, "peer must be registered after send_to");
}

// ════════════════════════════════════════════════════════════════════
//  End-to-end QUIC
// ════════════════════════════════════════════════════════════════════

async fn setup_two_peers() -> (
    Arc<BitswapQuicBridge>,
    Arc<BitswapQuicBridge>,
    NodeId,
    NodeId,
) {
    let _dir_a = TempDir::new().unwrap();
    let _dir_b = TempDir::new().unwrap();

    let transport_b = QuicTransportBuilder::new(NodeId::random(), "127.0.0.1:0".parse().unwrap())
        .build()
        .expect("build B");
    let id_b = transport_b.local_node_id().clone();
    let b_addr = transport_b.bound_addr().await;
    let transport_b = Arc::new(transport_b);

    let transport_a = QuicTransportBuilder::new(NodeId::random(), "127.0.0.1:0".parse().unwrap())
        .with_known(id_b.clone(), b_addr)
        .build()
        .expect("build A");
    let id_a = transport_a.local_node_id().clone();
    let transport_a = Arc::new(transport_a);

    let bridge_a = BitswapQuicBridge::new(id_a.clone(), transport_a);
    let bridge_b = BitswapQuicBridge::new(id_b.clone(), transport_b);

    (bridge_a, bridge_b, id_a, id_b)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_end_to_end_want_block_over_quic() {
    let (bridge_a, bridge_b, id_a, id_b) = setup_two_peers().await;

    let bridge_a_dyn: Arc<dyn BitswapTransportBridge> = bridge_a.clone();
    let bridge_b_dyn: Arc<dyn BitswapTransportBridge> = bridge_b.clone();

    let (adapter_a, ev_a) = BitswapNetworkAdapter::new(id_a.clone(), bridge_a_dyn);
    let (adapter_b, ev_b) = BitswapNetworkAdapter::new(id_b.clone(), bridge_b_dyn);
    let (run_a, _tx_a) = adapter_a.clone_for_listen();
    let (run_b, _tx_b) = adapter_b.clone_for_listen();
    tokio::spawn(run_a.run());
    tokio::spawn(run_b.run());

    bridge_a.clone().spawn_accept_loop(ev_a.clone());
    bridge_b.clone().spawn_accept_loop(ev_b.clone());

    let dial_result = bridge_a.dial(&id_b).await;
    assert!(dial_result.is_ok(), "A must dial B successfully");

    let stream = dial_result.unwrap();
    assert_eq!(stream.peer_id, id_b);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_quic_dial_concurrent_same_peer_coalesces() {
    let (bridge_a, bridge_b, id_a, id_b) = setup_two_peers().await;

    let bridge_a_dyn: Arc<dyn BitswapTransportBridge> = bridge_a.clone();
    let bridge_b_dyn: Arc<dyn BitswapTransportBridge> = bridge_b.clone();

    let (adapter_a, ev_a) = BitswapNetworkAdapter::new(id_a.clone(), bridge_a_dyn);
    let (adapter_b, ev_b) = BitswapNetworkAdapter::new(id_b.clone(), bridge_b_dyn);
    let (run_a, _tx_a) = adapter_a.clone_for_listen();
    let (run_b, _tx_b) = adapter_b.clone_for_listen();
    tokio::spawn(run_a.run());
    tokio::spawn(run_b.run());
    bridge_b.clone().spawn_accept_loop(ev_b.clone());

    let bridge_a_clone1 = bridge_a.clone();
    let bridge_a_clone2 = bridge_a.clone();
    let d1 = bridge_a_clone1.dial(&id_b);
    let d2 = bridge_a_clone2.dial(&id_b);
    let (r1, r2) = tokio::join!(d1, d2);
    assert!(r1.is_ok());
    assert!(r2.is_ok());

    let _ = ev_a;
    let _ = adapter_a;
}

// ════════════════════════════════════════════════════════════════════
//  Peer lifecycle
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_peer_disconnect_event_triggers_unregister() {
    let local = NodeId::random();
    let bridge: Arc<MockBitswapTransport> =
        Arc::new(MockBitswapTransport::new(local.clone()));
    let bridge_dyn: Arc<dyn BitswapTransportBridge> = bridge.clone();

    let (adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge_dyn.clone());
    let (run, run_tx) = adapter.clone_for_listen();
    tokio::spawn(run.run());

    let peer = NodeId::random();
    let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
    bridge_dyn
        .register_inbound_sender(peer.clone(), tx.clone())
        .await;
    let map_size = bridge.peer_count().await;
    assert_eq!(map_size, 1);

    run_tx
        .send(BitswapEvent::PeerDisconnected(peer.clone()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let map_size = bridge.peer_count().await;
    assert_eq!(map_size, 0, "PeerDisconnected must unregister the peer");
}

#[tokio::test]
async fn test_new_inbound_stream_registers_sender() {
    let local = NodeId::random();
    let bridge: Arc<MockBitswapTransport> =
        Arc::new(MockBitswapTransport::new(local.clone()));
    let bridge_dyn: Arc<dyn BitswapTransportBridge> = bridge.clone();

    let (adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge_dyn.clone());
    let (run, run_tx) = adapter.clone_for_listen();
    tokio::spawn(run.run());

    let peer = NodeId::random();
    let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);

    run_tx
        .send(BitswapEvent::NewInboundStream {
            peer: peer.clone(),
            stream_tx: tx,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let map_size = bridge.peer_count().await;
    assert_eq!(map_size, 1, "NewInboundStream must register the peer");
}

// ════════════════════════════════════════════════════════════════════
//  Handler dispatch
// ════════════════════════════════════════════════════════════════════

struct CountingHandler {
    counter: Arc<parking_lot::Mutex<u32>>,
}

#[async_trait::async_trait]
impl BitswapMessageHandler for CountingHandler {
    async fn handle(&self, _peer: &NodeId, _msg: &BitswapMessage) {
        *self.counter.lock() += 1;
    }
}

#[tokio::test]
async fn test_per_hash_handler_invoked() {
    let local = NodeId::random();
    let bridge: Arc<dyn BitswapTransportBridge> =
        Arc::new(MockBitswapTransport::new(local.clone()));
    let (mut adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
    let (run, run_tx) = adapter.clone_for_listen();
    tokio::spawn(run.run());

    let hash = ContentHash::from_bytes(b"counter-target");
    let counter = Arc::new(parking_lot::Mutex::new(0u32));
    let handler = CountingHandler {
        counter: counter.clone(),
    };
    adapter
        .register_handler(hash.clone(), handler)
        .await;

    run_tx
        .send(BitswapEvent::MessageFrom {
            peer: NodeId::random(),
            msg: BitswapMessage::WantHave {
                block: hash.clone(),
                priority: 0,
                send_dont_have: true,
            },
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(*counter.lock(), 1, "handler must be invoked exactly once");

    adapter.unregister_handler(&hash).await;

    run_tx
        .send(BitswapEvent::MessageFrom {
            peer: NodeId::random(),
            msg: BitswapMessage::WantBlock {
                block: hash.clone(),
                priority: 0,
            },
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(*counter.lock(), 1, "handler must NOT be invoked after unregister");
}