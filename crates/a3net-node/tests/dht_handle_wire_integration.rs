//! Integration tests for `DhtHandle` — the high-level DHT surface in
//! `a3net-node`.
//!
//! These tests pin down the audit-fix verification for the
//! `DhtHandle`-was-a-placeholder bug:
//!
//!   1. Before the fix, `DhtHandle::new` constructed a `DhtNode` whose
//!      network sender slot stayed `None`, so `provide()` only wrote to
//!      a local store and `find_providers()` returned `Vec::new()`
//!      regardless of routing-table state.
//!   2. After the fix, `DhtHandle::set_transport(bridge)` installs a
//!      real `DhtNetworkSender`, so `provide()` fans `AddProvider` out
//!      to K-closest peers and `find_providers()` issues
//!      `GetProviders` over the bridge when the local store misses.
//!
//! The tests use an in-process `MockBridge` (no QUIC) so they run in
//! CI without flakiness, plus a real-QUIC end-to-end test that drives
//! two `DhtHandle`s over `QuicTransport` and asserts that provider
//! records actually traverse the wire.

#![cfg(feature = "dht")]

use std::sync::Arc;
use std::time::Duration;

use a3net_dht::transport::TransportBridge;
use a3net_dht::{DhtCodec, DynResponseSink};
use a3net_node::dht::{DhtConfig, DhtHandle};
use a3net_transport::{Frame, QuicTransport, QuicTransportBuilder, SharedTransport};
use a3net_types::{ContentHash, NodeId};

/// In-process mock that records every outbound frame and lets the
/// test synthesise a response. Mirrors the production shape: it
/// implements [`TransportBridge`] over a captured set of peer
/// connections.
struct MockBridge {
    local_id: NodeId,
    /// Per-peer outgoing frames captured for assertion.
    sent: parking_lot::Mutex<Vec<(NodeId, Vec<u8>)>>,
    /// Optional responder: when set, return a synthetic response
    /// for the given request bytes.
    responder:
        parking_lot::Mutex<Option<Arc<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync>>>,
    /// Response sink: receives whatever the bridge reads back.
    response_sink: parking_lot::Mutex<Option<DynResponseSink>>,
}

impl MockBridge {
    fn new(local_id: NodeId) -> Self {
        Self {
            local_id,
            sent: parking_lot::Mutex::new(Vec::new()),
            responder: parking_lot::Mutex::new(None),
            response_sink: parking_lot::Mutex::new(None),
        }
    }

    fn set_responder(&self, f: Arc<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync>) {
        *self.responder.lock() = Some(f);
    }

    fn set_response_sink(&self, sink: DynResponseSink) {
        *self.response_sink.lock() = Some(sink);
    }
}

#[async_trait::async_trait]
impl TransportBridge for MockBridge {
    fn local_node_id(&self) -> &NodeId {
        &self.local_id
    }

    async fn get_peer_addr(
        &self,
        _peer: &NodeId,
    ) -> Option<a3net_types::NodeAddr> {
        None
    }

    async fn dial_and_send(
        &self,
        peer: &NodeId,
        msg: Vec<u8>,
    ) -> Result<(), a3net_dht::query::QueryError> {
        self.sent.lock().push((peer.clone(), msg.clone()));

        // Sniff the request shape so we don't run the responder for
        // fire-and-forget messages (AddProvider / Ping) that don't
        // expect a reply.
        let head = &msg[..msg.len().min(80)];
        let fire_and_forget = head
            .windows(b"\"type\":\"AddProvider\"".len())
            .any(|w| w == b"\"type\":\"AddProvider\"")
            || head.windows(b"\"type\":\"Ping\"".len()).any(|w| w == b"\"type\":\"Ping\"");
        if fire_and_forget {
            return Ok(());
        }

        let responder = self.responder.lock().clone();
        if let Some(f) = responder {
            if let Some(resp_bytes) = f(&msg) {
                if let Some(sink) = self.response_sink.lock().clone() {
                    sink(resp_bytes);
                }
            }
        }
        Ok(())
    }
}

fn cfg_for(local_id: NodeId) -> DhtConfig {
    DhtConfig {
        local_id,
        bootstrap_nodes: vec![],
        provider_interval: Duration::from_secs(60),
        refresh_interval: Duration::from_secs(60),
        contact_timeout: Duration::from_secs(60),
        k_bucket_size: 8,
    }
}

/// `DhtHandle::set_transport` actually routes `provide()` over the
/// bridge. Before the fix this short-circuited to "no sender wired".
#[tokio::test]
async fn dht_handle_set_transport_routes_provide_over_wire() {
    let local_id = NodeId::random();
    let handle = DhtHandle::new(cfg_for(local_id.clone())).await;

    let bob_id = NodeId::random();
    handle.inner().add_peer(bob_id.clone(), "127.0.0.1:9999".parse().unwrap()).await;
    handle.set_external_addr(Some("/ip4/127.0.0.1/tcp/7777".into()));

    let bridge = Arc::new(MockBridge::new(local_id.clone()));
    let sender = handle.set_transport(bridge.clone());
    assert_eq!(sender.local_id(), &local_id);

    let hash = ContentHash::from_bytes(b"audit-fix-set-transport");
    handle.provide(&hash).await;

    let sent = bridge.sent.lock();
    assert!(!sent.is_empty(), "no outbound AddProvider was dispatched");
    let (peer, bytes) = &sent[0];
    assert_eq!(peer, &bob_id);
    assert!(
        bytes.windows(b"\"type\":\"AddProvider\"".len()).any(|w| w == b"\"type\":\"AddProvider\""),
        "outbound frame is not an AddProvider: {:?}",
        String::from_utf8_lossy(bytes)
    );
}

/// `DhtHandle::find_providers()` issues a `GetProviders` over the
/// bridge when the local store misses.
#[tokio::test]
async fn dht_handle_find_providers_routes_over_wire() {
    let local_id = NodeId::random();
    let handle = DhtHandle::new(cfg_for(local_id.clone())).await;

    let bob_id = NodeId::random();
    handle.inner().add_peer(bob_id.clone(), "127.0.0.1:9998".parse().unwrap()).await;

    let bridge = Arc::new(MockBridge::new(local_id.clone()));
    let bob_id_for_responder = bob_id.clone();
    bridge.set_responder(Arc::new(move |request_bytes| {
        let msg = match DhtCodec::decode(request_bytes) {
            Ok(m) => m,
            Err(_) => return None,
        };
        let request_id = match msg {
            a3net_dht::protocol::DhtWireMessage::GetProviders(p) => p.request_id,
            _ => return None,
        };
        let resp = a3net_dht::protocol::DhtWireMessage::Providers(
            a3net_dht::protocol::ProvidersPayload {
                request_id,
                providers: vec![a3net_dht::protocol::ProviderRecordWire {
                    provider_id: bob_id_for_responder.clone(),
                    addrs: vec!["127.0.0.1:9998".to_string()],
                    ttl_secs: 3600,
                    signature: None,
                }],
            },
        );
        DhtCodec::encode(&resp).ok()
    }));

    let sender = handle.set_transport(bridge.clone());
    let sink_sender = sender.clone();
    let sink: DynResponseSink = Arc::new(move |bytes| {
        let sender = sink_sender.clone();
        tokio::spawn(async move {
            if let Ok(msg) = DhtCodec::decode(&bytes) {
                sender.handle_response(&msg).await;
            }
        });
    });
    bridge.set_response_sink(sink);

    let hash = ContentHash::from_bytes(b"audit-fix-find-via-wire");
    bridge.sent.lock().clear();

    let providers = handle.find_providers(&hash).await;
    assert!(
        !providers.is_empty(),
        "find_providers should reach Bob via the mock bridge"
    );
    assert_eq!(providers[0].provider_id, bob_id);

    let sent = bridge.sent.lock();
    assert!(!sent.is_empty(), "no outbound GetProviders frame was sent");
    let (_, bytes) = &sent[0];
    assert!(
        bytes.windows(b"\"type\":\"GetProviders\"".len()).any(|w| w == b"\"type\":\"GetProviders\""),
        "outbound frame is not a GetProviders: {:?}",
        String::from_utf8_lossy(bytes)
    );
}

/// `DhtHandle::detach_transport()` reverts to local-only lookups
/// even after a transport has been wired.
#[tokio::test]
async fn dht_handle_detach_transport_reverts_to_local_only() {
    let local_id = NodeId::random();
    let handle = DhtHandle::new(cfg_for(local_id.clone())).await;

    let bob_id = NodeId::random();
    handle.inner().add_peer(bob_id.clone(), "127.0.0.1:9997".parse().unwrap()).await;

    let bridge = Arc::new(MockBridge::new(local_id.clone()));
    let _sender = handle.set_transport(bridge.clone());
    handle.detach_transport();

    let hash = ContentHash::from_bytes(b"audit-fix-detach");
    let providers = handle.find_providers(&hash).await;
    assert!(
        providers.is_empty(),
        "After detach_transport, find_providers must be local-only"
    );
    let sent = bridge.sent.lock();
    assert!(
        sent.is_empty(),
        "After detach, no outbound frame should have been dispatched (got {})",
        sent.len()
    );
}

/// `DhtHandle::set_external_addr` mirrors the address into the
/// inner `DhtNode` so the wire-protocol path publishes the real
/// address, not the placeholder.
#[tokio::test]
async fn dht_handle_set_external_addr_mirrors_to_inner_node() {
    let local_id = NodeId::random();
    let handle = DhtHandle::new(cfg_for(local_id)).await;
    handle.set_external_addr(Some("/ip4/9.9.9.9/tcp/4001".into()));

    let inner_addr = handle.inner().local_addr_str();
    assert_eq!(inner_addr.as_deref(), Some("/ip4/9.9.9.9/tcp/4001"));

    // Clearing the handle's external addr must not retroactively
    // rewrite the inner node's record.
    handle.set_external_addr(None);
    assert!(handle.external_addr().is_none());
}

/// `provide()` patches the locally-cached provider record so the
/// announced `provider_addr` reflects the configured external addr.
#[tokio::test]
async fn dht_handle_provide_publishes_external_addr() {
    let local_id = NodeId::random();
    let handle = DhtHandle::new(cfg_for(local_id.clone())).await;
    let ext = "/ip4/203.0.113.42/tcp/7777".to_string();
    handle.set_external_addr(Some(ext.clone()));

    let hash = ContentHash::from_bytes(b"audit-fix-provider-addr");
    handle.provide(&hash).await;

    let providers = handle.find_providers(&hash).await;
    assert_eq!(providers.len(), 1, "locally-stored provider must be visible");
    assert_eq!(providers[0].provider_id, local_id);
    assert_eq!(providers[0].provider_addr, ext);
}

/// End-to-end over real QUIC. Two `DhtHandle`s talk to each other:
/// Alice's `provide()` arrives at Bob's store, then Alice's
/// `find_providers` for an unrelated hash reaches Bob and returns
/// his record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dht_handle_full_quic_roundtrip() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("a3net_dht=trace,info")),
        )
        .try_init();

    // Build two QUIC transports that know about each other.
    let transport_b = QuicTransportBuilder::new(NodeId::random(), "127.0.0.1:0".parse().unwrap())
        .build()
        .expect("build B");
    let id_b = transport_b.local_node_id().clone();
    let b_addr = transport_b.bound_addr().await;

    let transport_a =
        QuicTransportBuilder::new(NodeId::random(), "127.0.0.1:0".parse().unwrap())
            .with_known(id_b.clone(), b_addr)
            .build()
            .expect("build A");
    let id_a = transport_a.local_node_id().clone();
    let a_addr = transport_a.bound_addr().await;

    // Both transports must know each other's address before either
    // side can dial. `with_known` only taught Alice's transport
    // about Bob; mirror the registry the other way.
    transport_b.register_peer(id_a.clone(), a_addr).await;

    let transport_a: SharedTransport = Arc::new(transport_a);
    let transport_b: SharedTransport = Arc::new(transport_b);

    // Alice and Bob's DHT handles.
    let alice = DhtHandle::new(cfg_for(id_a.clone())).await;
    let bob = DhtHandle::new(cfg_for(id_b.clone())).await;

    alice.inner().add_peer(id_b.clone(), b_addr).await;
    bob.inner().add_peer(id_a.clone(), a_addr).await;

    // Wire Alice via `set_transport`.
    use a3net_node::dht_bridge::DynTransportBridge;
    let alice_bridge: Arc<DynTransportBridge> =
        Arc::new(DynTransportBridge::new(transport_a.clone(), id_a.clone()));
    alice.set_external_addr(Some(format!("/ip4/{}/tcp/{}", a_addr.ip(), a_addr.port())));
    let alice_sender = alice.set_transport(alice_bridge.clone());
    let alice_sink_target = alice_sender.clone();
    alice_bridge.set_response_sink(Arc::new(move |bytes| {
        let sender = alice_sink_target.clone();
        tokio::spawn(async move {
            if let Ok(msg) = DhtCodec::decode(&bytes) {
                sender.handle_response(&msg).await;
            }
        });
    }));

    // Bob's side: also wire a bridge so we can drive both ends.
    let bob_bridge: Arc<DynTransportBridge> =
        Arc::new(DynTransportBridge::new(transport_b.clone(), id_b.clone()));
    let bob_sender = bob.set_transport(bob_bridge.clone());
    let bob_sink_target = bob_sender.clone();
    bob_bridge.set_response_sink(Arc::new(move |bytes| {
        let sender = bob_sink_target.clone();
        tokio::spawn(async move {
            if let Ok(msg) = DhtCodec::decode(&bytes) {
                sender.handle_response(&msg).await;
            }
        });
    }));

    // Spawn inbound handlers so each side actually persists
    // inbound AddProvider frames into its local store. Without
    // these, `provide()` is fire-and-forget over QUIC and Bob's
    // store never reflects Alice's announcement.
    let (_alice_inbound, alice_ready) =
        spawn_dht_inbound(transport_a.clone(), id_a.clone(), alice.inner());
    let (_bob_inbound, bob_ready) =
        spawn_dht_inbound(transport_b.clone(), id_b.clone(), bob.inner());
    // Wait for both inbound accept-loops to register their
    // receivers before triggering any dials — without this the
    // first dial's accept can race the receiver-take and drop
    // the AddProvider on the floor.
    let _ = alice_ready.await;
    let _ = bob_ready.await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Step 1: Alice `provide()`s over the wire. Bob must end up
    // with a provider record for Alice.
    let hash = ContentHash::from_bytes(b"audit-fix-full-quic");
    alice.provide(&hash).await;

    let mut bob_received = false;
    for _ in 0..40 {
        let key = a3net_dht::DhtKey::from_bytes(hash.as_bytes().to_vec());
        let p = bob.inner().find_providers(&key).await;
        if !p.is_empty() && p[0].provider_id == id_a {
            bob_received = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        bob_received,
        "Bob should have received Alice's AddProvider over QUIC"
    );

    // Step 2: Bob pre-publishes a record for an unrelated hash and
    // Alice's `find_providers` must reach Bob and return it.
    let other_hash = ContentHash::from_bytes(b"audit-fix-full-quic-other");
    bob.provide(&other_hash).await;

    let mut found = false;
    for _ in 0..40 {
        let p = alice.find_providers(&other_hash).await;
        if p.iter().any(|r| r.provider_id == id_b) {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        found,
        "Alice's find_providers should reach Bob over QUIC and return his record"
    );
}

/// Spawn an inbound DHT handler task. Reads framed messages off
/// the transport's incoming receiver, dispatches them through a
/// `DhtProtocolHandler`, and writes the response back on the
/// same stream. Without this, `provide()` over QUIC is a
/// fire-and-forget write whose data is never persisted.
///
/// Returns the task's `JoinHandle` and a `oneshot::Receiver`
/// that fires once the inbound accept loop has registered its
/// receiver. Callers should `.await` the receiver (or sleep)
/// before triggering outbound dials so the receiver is in
/// place by the time the dial's accept fires.
fn spawn_dht_inbound(
    transport: SharedTransport,
    local_id: NodeId,
    node: Arc<a3net_dht::DhtNode>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        // The handler is `&mut self`, so it sits behind a Mutex.
        let handler = Arc::new(tokio::sync::Mutex::new(
            a3net_dht::DhtProtocolHandler::new(local_id, node.routing_table(), node.store())
                .0,
        ));
        let mut rx = match transport.take_incoming_receiver().await {
            Some(rx) => rx,
            None => {
                let _ = ready_tx.send(());
                return;
            }
        };
        // Signal that the receiver is in place.
        let _ = ready_tx.send(());
        while let Some((peer_id, mut conn)) = rx.recv().await {
            let handler = handler.clone();
            tokio::spawn(async move {
                let frame = match tokio::time::timeout(Duration::from_secs(5), conn.recv())
                    .await
                {
                    Ok(Ok(Some(f))) => f,
                    Ok(Ok(None)) => return,
                    Ok(Err(_)) => return,
                    Err(_) => return,
                };
                let bytes = frame.into_inner();
                let response = {
                    let mut h = handler.lock().await;
                    h.handle_frame(&bytes).await
                };
                if let Some(resp) = response {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(5),
                        conn.send(Frame(resp)),
                    )
                    .await;
                    // Let quinn flush before close() FINs.
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                let _ = conn.close().await;
                let _ = peer_id;
            });
        }
    });
    (handle, ready_rx)
}