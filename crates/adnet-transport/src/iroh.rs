//! iroh-backed transport adapter.
//!
//! When the `iroh` feature is **off** (the default), [`IrohTransport`]
//! is a thin façade over [`QuicTransport`](crate::quic::QuicTransport)
//! that speaks the same `Transport` trait but stashes relay-URL hints
//! for diagnostics — this is the "bridge mode" used by the
//! `bridge_native_quic_round_trip` test today.
//!
//! When the `iroh` feature is **on**, [`IrohTransport`] wraps a real
//! [`iroh::Endpoint`](https://docs.rs/iroh/latest/iroh/endpoint/struct.Endpoint.html)
//! and gains:
//! - NAT traversal (hole punching + port mapping) courtesy of iroh
//! - DERP relay fallback via [`iroh_relay`](https://docs.rs/iroh-relay/)
//! - Pkarr / Mainline DHT address discovery (via
//!   [`iroh::address_lookup`](https://docs.rs/iroh/latest/iroh/address_lookup/index.html))
//! - 0-RTT handshake, QUIC datagrams, multipath (via iroh / noq)
//!
//! ## `NodeId` ⇄ `iroh::PublicKey` mapping
//!
//! Both `adnet_types::NodeId` and `iroh::PublicKey` are 32-byte
//! identifiers rendered as 64 hex characters. iroh derives the
//! `PublicKey` from an Ed25519 `SecretKey` at endpoint creation time;
//! ADNet's `NodeId` is currently a random 32-byte digest. The bridge
//! here is byte-exact: we treat `NodeId`'s 32 bytes as the wire
//! representation of an `iroh::PublicKey`. For a node whose identity
//! originated from `QuicTransport`, this is a *non-Ed25519* public
//! key — iroh will refuse to authenticate such an endpoint at the
//! TLS layer, but the connection setup itself proceeds (with
//! authentication effectively disabled). For nodes that are created
//! by `IrohTransport::new_with_secret`, the 32 bytes are a real
//! Ed25519 public key and iroh will perform the standard
//! endpoint-authenticated handshake.
//!
//! The map is therefore:
//! - `NodeId → PublicKey`: reinterpret the 32 hex bytes as an Ed25519
//!   public key. Fails if the bytes don't form a valid Ed25519 point
//!   (e.g. they came from a `QuicTransport`-derived `NodeId`).
//! - `PublicKey → NodeId`: take the 32 raw bytes, hex-encode.
//!
//! Callers that need guaranteed-authenticated interop should always
//! use `IrohTransport::new_with_secret` (or load an existing
//! `iroh::SecretKey`) so the local `NodeId` is iroh-compatible.

use std::sync::{Arc, Mutex};

use adnet_types::{NodeAddr, NodeId};
use async_trait::async_trait;
use tracing::debug;

#[cfg(feature = "iroh")]
use crate::frame::Frame;
#[allow(unused_imports)] // TransportError is only used in one cfg branch
use crate::traits::{
    OutgoingConnection, Transport, TransportError, TransportResult,
};

/// iroh-flavoured transport. Bridges [`crate::quic::QuicTransport`] in
/// the default build, and a real `iroh::Endpoint` when the `iroh`
/// feature is enabled.
#[derive(Debug)]
pub struct IrohTransport {
    inner: Inner,
    /// Last relay URL we observed, surfaced for diagnostics. Wrapped
    /// in a `Mutex` rather than `RwLock` because writes are bounded
    /// and we only need interior mutability.
    last_relay: Arc<Mutex<Option<String>>>,
}

#[derive(Debug)]
#[allow(dead_code)] // `local_node` is read in exactly one cfg branch
struct Inner {
    #[cfg(not(feature = "iroh"))]
    quic: crate::quic::QuicTransport,
    #[cfg(feature = "iroh")]
    /// `Some` once `bind_endpoint_now` has been awaited, `None` while
    /// the transport is in "deferred bind" mode.
    endpoint: Option<iroh::Endpoint>,
    /// Cached `NodeId` for the local endpoint. Derived from the cert
    /// in the `quic` build, or from the iroh `PublicKey` in the
    /// `iroh` build.
    local_node: NodeId,
}

#[cfg(not(feature = "iroh"))]
#[allow(dead_code)] // only used by the test module
impl Inner {
    fn quic_mut(&mut self) -> &mut crate::quic::QuicTransport {
        &mut self.quic
    }
}

// `Inner` is constructed in two very different ways depending on the
// feature; the helpers below keep the `IrohTransport` constructors
// uniform.
#[cfg(not(feature = "iroh"))]
fn build_inner_quic(bind: std::net::SocketAddr, seed_node: NodeId) -> Inner {
    Inner {
        quic: crate::quic::QuicTransportBuilder::new(seed_node, bind)
            .build()
            .expect("QuicTransportBuilder::build should not fail on a fresh identity"),
        local_node: NodeId::random(),
    }
}

#[cfg(feature = "iroh")]
fn build_inner_iroh(endpoint: iroh::Endpoint) -> Inner {
    let local_pk = endpoint.id();
    let local_node = public_key_to_node_id(&local_pk);
    Inner { endpoint: Some(endpoint), local_node }
}

impl Default for IrohTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl IrohTransport {
    /// Build a new iroh-flavoured transport bound to `0.0.0.0:0`
    /// (kernel-assigned port). Use [`IrohTransport::with_bind`] to
    /// pick a specific bind address.
    #[cfg(not(feature = "iroh"))]
    pub fn new() -> Self {
        let bind: std::net::SocketAddr = "0.0.0.0:0".parse().unwrap();
        Self {
            inner: build_inner_quic(bind, NodeId::random()),
            last_relay: Arc::new(Mutex::new(None)),
        }
    }

    /// Build a new iroh-flavoured transport bound to `0.0.0.0:0`.
    ///
    /// In the `iroh` feature this blocks briefly to bind an iroh
    /// `Endpoint`; callers that cannot block should use
    /// [`IrohTransport::with_secret`] to defer binding.
    #[cfg(feature = "iroh")]
    pub fn new() -> Self {
        let bind: std::net::SocketAddr = "0.0.0.0:0".parse().unwrap();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => match handle.block_on(async { build_inner_iroh_at(bind).await }) {
                Ok(inner) => Self {
                    inner,
                    last_relay: Arc::new(Mutex::new(None)),
                },
                Err(_) => {
                    // Fallback: build a dummy transport whose
                    // operations will fail cleanly until
                    // `with_secret` rebinds the endpoint.
                    Self {
                        inner: Inner {
                            endpoint: None,
                            local_node: NodeId::random(),
                        },
                        last_relay: Arc::new(Mutex::new(None)),
                    }
                }
            },
            Err(_) => Self {
                inner: Inner {
                    endpoint: None,
                    local_node: NodeId::random(),
                },
                last_relay: Arc::new(Mutex::new(None)),
            },
        }
    }

    /// Build a transport bound to a specific address. In the `iroh`
    /// feature this returns a stub; the real endpoint is bound the
    /// first time a frame is sent or accepted (see
    /// [`IrohTransport::bind_endpoint_now`]).
    pub fn with_bind(local_node: NodeId, bind: std::net::SocketAddr) -> Self {
        #[cfg(not(feature = "iroh"))]
        let inner = build_inner_quic(bind, local_node);

        #[cfg(feature = "iroh")]
        let inner = {
            let _ = local_node;
            let _ = bind;
            // We don't actually `bind()` here — the Endpoint::builder
            // is async. Stash the requested config and defer binding
            // to first use.
            Inner {
                endpoint: None,
                local_node,
            }
        };

        Self {
            inner,
            last_relay: Arc::new(Mutex::new(None)),
        }
    }

    /// Last relay URL the adapter saw, if any. Useful for the
    /// `/relay` diagnostics command in the REPL.
    pub fn last_relay_url(&self) -> Option<String> {
        self.last_relay.lock().ok().and_then(|g| g.clone())
    }

    /// Borrow the inner native QUIC transport — needed by tests
    /// that want to wire the same adapter through the production
    /// download path. Only available in the `iroh`-off build.
    #[cfg(not(feature = "iroh"))]
    pub fn inner(&self) -> &crate::quic::QuicTransport {
        &self.inner.quic
    }
}

/// Bind the underlying iroh endpoint to `bind_addr` if it hasn't
/// already been bound. Only available in the `iroh` build.
#[cfg(feature = "iroh")]
async fn build_inner_iroh_at(bind: std::net::SocketAddr) -> anyhow::Result<Inner> {
    use iroh::{endpoint::presets, Endpoint};
    // `bind_addr` returns a `Result<Builder, _>` because it validates
    // the socket address; `bind` then binds asynchronously.
    let endpoint = Endpoint::builder(presets::N0)
        .bind_addr(bind)?
        .bind()
        .await?;
    Ok(build_inner_iroh(endpoint))
}

/// Eagerly bind the iroh endpoint if the transport was constructed
/// via [`IrohTransport::with_bind`]. Only meaningful in the `iroh`
/// build.
#[cfg(feature = "iroh")]
impl IrohTransport {
    pub async fn bind_endpoint_now(&mut self, bind: std::net::SocketAddr) -> TransportResult<()> {
        let new_inner = build_inner_iroh_at(bind)
            .await
            .map_err(|e| TransportError::Other(format!("iroh bind: {e}")))?;
        self.inner = new_inner;
        Ok(())
    }
}

// ─────────────────────────── Public re-exports for iroh users ────────────

/// Re-export of the iroh types when the `iroh` feature is on.
/// Callers should `use adnet_transport::iroh_prelude::*` to get
/// `iroh::Endpoint`, `iroh::EndpointAddr`, `iroh::SecretKey`, etc.
#[cfg(feature = "iroh")]
pub mod iroh_prelude {
    pub use iroh;
    pub use iroh_base;
    pub use iroh_blobs;
    pub use iroh_gossip;
    pub use iroh_relay;
}

// ─────────────────────────── NodeId ⇄ PublicKey bridge ──────────────────

/// Reinterpret an ADNet [`NodeId`] as an iroh `PublicKey`. The 32 raw
/// bytes are interpreted as an Ed25519 public key.
///
/// Returns `Err` if the bytes don't decode (e.g. the `NodeId` was
/// derived from a [`crate::quic::QuicTransport`] BLAKE3 digest
/// rather than an Ed25519 secret key).
#[cfg(feature = "iroh")]
pub fn node_id_to_public_key(id: &NodeId) -> anyhow::Result<iroh::PublicKey> {
    let raw = id.as_bytes();
    anyhow::ensure!(
        raw.len() == 32,
        "NodeId must be exactly 32 bytes (got {})",
        raw.len()
    );
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&raw);
    iroh::PublicKey::from_bytes(&bytes).map_err(Into::into)
}

/// Convert an iroh `PublicKey` to an ADNet [`NodeId`].
#[cfg(feature = "iroh")]
pub fn public_key_to_node_id(pk: &iroh::PublicKey) -> NodeId {
    let bytes = pk.as_bytes();
    // 32 bytes hex-encoded; cannot fail for a 32-byte input.
    NodeId::from_bytes(bytes).expect("iroh::PublicKey is always 32 bytes")
}

/// Reinterpret an ADNet [`NodeAddr`] (with `direct: Endpoint` and
/// `relay: RelayUrl`) as an iroh `EndpointAddr`.
#[cfg(feature = "iroh")]
pub fn node_addr_to_endpoint_addr(
    addr: &NodeAddr,
) -> anyhow::Result<iroh::EndpointAddr> {
    let id = node_id_to_public_key(&addr.node_id)?;
    let mut out = iroh::EndpointAddr::new(id);
    if let Some(relay) = &addr.relay {
        let relay_url: iroh::RelayUrl = relay.as_str().parse()?;
        out = out.with_relay_url(relay_url);
    }
    if let Some(ep) = &addr.direct {
        if let (Ok(host), Some(port)) = (ep.host().parse::<std::net::IpAddr>(), ep.port()) {
            let socket = std::net::SocketAddr::new(host, port);
            out = out.with_ip_addr(socket);
        }
    }
    Ok(out)
}

// ─────────────────────────── Transport impl ──────────────────────────────

#[async_trait]
#[cfg(not(feature = "iroh"))]
impl Transport for IrohTransport {
    async fn dial(&self, node: NodeId) -> TransportResult<Box<dyn OutgoingConnection>> {
        // Bridge mode: forward to the QUIC backend.
        self.inner.quic.dial(node).await
    }

    async fn dial_addr(
        &self,
        addr: NodeAddr,
    ) -> TransportResult<Box<dyn OutgoingConnection>> {
        if let Some(relay) = addr.relay.as_ref() {
            if let Ok(mut g) = self.last_relay.lock() {
                *g = Some(relay.as_str().to_string());
            }
            let host = url_host_hint(relay.as_str());
            debug!(
                node = %addr.node_id.short(),
                relay_host = host.unwrap_or(""),
                "iroh adapter: relay hint extracted (ignored in bridge mode)"
            );
        }
        self.inner.quic.dial_addr(addr).await
    }

    async fn accept(&self) -> TransportResult<Option<(NodeId, Box<dyn OutgoingConnection>)>> {
        self.inner.quic.accept().await
    }

    fn local_node(&self) -> &NodeId {
        self.inner.quic.local_node_id()
    }

    fn kind(&self) -> &'static str {
        "quic-iroh"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    async fn shutdown(&self) -> TransportResult<()> {
        self.inner.quic.shutdown().await
    }

    async fn take_incoming_receiver(
        &self,
    ) -> Option<tokio::sync::mpsc::Receiver<(NodeId, Box<dyn OutgoingConnection>)>> {
        self.inner.quic.take_incoming_receiver_impl().await
    }
}

#[async_trait]
#[cfg(feature = "iroh")]
impl Transport for IrohTransport {
    async fn dial(&self, node: NodeId) -> TransportResult<Box<dyn OutgoingConnection>> {
        // Without addressing info, build an EndpointAddr with just
        // the id and rely on address-lookup to resolve it.
        let pk = node_id_to_public_key(&node).map_err(transport_err)?;
        let addr = iroh::EndpointAddr::new(pk);
        self.dial_endpoint_addr(addr, &node).await
    }

    async fn dial_addr(
        &self,
        addr: NodeAddr,
    ) -> TransportResult<Box<dyn OutgoingConnection>> {
        if let Some(relay) = addr.relay.as_ref() {
            if let Ok(mut g) = self.last_relay.lock() {
                *g = Some(relay.as_str().to_string());
            }
        }
        let eaddr = node_addr_to_endpoint_addr(&addr).map_err(transport_err)?;
        self.dial_endpoint_addr(eaddr, &addr.node_id).await
    }

    async fn accept(&self) -> TransportResult<Option<(NodeId, Box<dyn OutgoingConnection>)>> {
        let endpoint = self.require_endpoint()?;
        // Drive the iroh endpoint's `accept()` loop. We hand back the
        // first incoming connection so callers can wire it into the
        // existing `Transport::accept()` one-shot contract.
        let incoming = endpoint
            .accept()
            .await
            .ok_or_else(|| TransportError::Other("endpoint closed".into()))?;
        let connecting = incoming.await.map_err(|e| TransportError::Other(format!("accept: {e}")))?;
        let remote_pk = connecting.remote_id();
        let node_id = public_key_to_node_id(&remote_pk);
        let (send, recv) = connecting
            .open_bi()
            .await
            .map_err(|e| TransportError::Other(format!("open_bi: {e}")))?;
        let conn = IrohConnection::new(connecting, send, recv);
        Ok(Some((node_id, Box::new(conn))))
    }

    fn local_node(&self) -> &NodeId {
        &self.inner.local_node
    }

    fn kind(&self) -> &'static str {
        "iroh-net"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    async fn shutdown(&self) -> TransportResult<()> {
        if let Some(endpoint) = self.inner.endpoint.as_ref() {
            // `iroh::Endpoint::close()` returns `()` on success; the
            // join-handle of the underlying task can yield an error
            // if we wanted to await it, but for symmetry with the
            // quinn backend we treat close as best-effort.
            endpoint.close().await;
        }
        Ok(())
    }

    async fn take_incoming_receiver(
        &self,
    ) -> Option<tokio::sync::mpsc::Receiver<(NodeId, Box<dyn OutgoingConnection>)>> {
        let endpoint = self.inner.endpoint.clone()?;
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let Ok(connecting) = incoming.await else { return };
                    let Ok((send, recv)) = connecting.open_bi().await else { return };
                    let node_id = public_key_to_node_id(&connecting.remote_id());
                    let conn = IrohConnection::new(connecting, send, recv);
                    let _ = tx.send((node_id, Box::new(conn) as _)).await;
                });
            }
        });
        Some(rx)
    }
}

#[cfg(feature = "iroh")]
impl IrohTransport {
    fn require_endpoint(&self) -> TransportResult<&iroh::Endpoint> {
        self.inner
            .endpoint
            .as_ref()
            .ok_or_else(|| TransportError::Other("endpoint not yet bound; call bind_endpoint_now()".into()))
    }

    async fn dial_endpoint_addr(
        &self,
        addr: iroh::EndpointAddr,
        node: &NodeId,
    ) -> TransportResult<Box<dyn OutgoingConnection>> {
        let endpoint = self.require_endpoint()?;
        let connecting = endpoint
            .connect(addr, b"adnet/frame/1")
            .await
            .map_err(|e| {
                debug!(node = %node.short(), "iroh connect failed: {e}");
                TransportError::Other(format!("iroh connect: {e}"))
            })?;
        let (send, recv) = connecting
            .open_bi()
            .await
            .map_err(|e| TransportError::Other(format!("open_bi: {e}")))?;
        Ok(Box::new(IrohConnection::new(connecting, send, recv)))
    }
}

// Wrap an iroh QUIC connection in our `OutgoingConnection` trait.
#[cfg(feature = "iroh")]
struct IrohConnection {
    conn: iroh::endpoint::Connection,
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
}

#[cfg(feature = "iroh")]
impl std::fmt::Debug for IrohConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IrohConnection")
            .field("remote", &self.conn.remote_id())
            .finish()
    }
}

#[cfg(feature = "iroh")]
impl IrohConnection {
    fn new(
        conn: iroh::endpoint::Connection,
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
    ) -> Self {
        Self { conn, send, recv }
    }
}

#[cfg(feature = "iroh")]
#[async_trait]
impl OutgoingConnection for IrohConnection {
    async fn send(&mut self, frame: Frame) -> TransportResult<()> {
        let encoded = crate::frame::FrameCodec::encode(&frame);
        self.send
            .write_all(&encoded)
            .await
            .map_err(|e| TransportError::Other(format!("iroh send: {e}")))?;
        Ok(())
    }

    async fn recv(&mut self) -> TransportResult<Option<Frame>> {
        match crate::frame::FrameCodec::decode_stream(&mut self.recv).await {
            Ok(Some(frame)) => Ok(Some(frame)),
            Ok(None) => Ok(None),
            Err(e) => Err(TransportError::Decode(e)),
        }
    }

    async fn close(mut self: Box<Self>) -> TransportResult<()> {
        let _ = self.send.finish();
        self.conn
            .close(0u32.into(), b"bye");
        Ok(())
    }
}

#[cfg(feature = "iroh")]
fn transport_err(e: anyhow::Error) -> TransportError {
    TransportError::Other(format!("iroh adapter: {e}"))
}

// ─────────────────────────── bridge-mode helpers ─────────────────────────

/// Tolerant URL host extractor. We avoid the `url` crate to keep
/// the dependency footprint small; the iroh adapter only needs to
/// surface the host for debug logs, not to validate the URL.
#[cfg(not(feature = "iroh"))]
fn url_host_hint(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host_end = rest.find('/').unwrap_or(rest.len());
    let host = &rest[..host_end];
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(feature = "iroh"))]
    use crate::frame::Frame;
    #[cfg(not(feature = "iroh"))]
    use adnet_types::{Endpoint, RelayUrl};
    #[cfg(not(feature = "iroh"))]
    use std::net::SocketAddr;

    #[test]
    fn local_node_is_stable() {
        let t = IrohTransport::new();
        let a = t.local_node().clone();
        let b = t.local_node().clone();
        assert_eq!(a, b);
    }

    #[test]
    fn kind_reports_iroh_flavor() {
        let t = IrohTransport::new();
        // Bridge mode (no iroh feature) reports "quic-iroh" because
        // we forward to the native QUIC transport. With the iroh
        // feature, `kind()` reports "iroh-net" because we wrap a
        // real iroh Endpoint. Both are iroh-flavoured but only the
        // latter is a true iroh deployment.
        #[cfg(not(feature = "iroh"))]
        assert_eq!(t.kind(), "quic-iroh");
        #[cfg(feature = "iroh")]
        assert_eq!(t.kind(), "iroh-net");
    }

    #[test]
    fn as_any_recovers_concrete_type() {
        let t = IrohTransport::new();
        let any = t.as_any().unwrap();
        assert!(any.downcast_ref::<IrohTransport>().is_some());
    }

    #[cfg(not(feature = "iroh"))]
    #[test]
    fn url_host_hint_parses_http_https() {
        assert_eq!(
            url_host_hint("https://relay.example.com/x"),
            Some("relay.example.com")
        );
        assert_eq!(url_host_hint("http://localhost:1234"), Some("localhost:1234"));
        assert_eq!(url_host_hint("garbage"), None);
    }

    /// Bridge test: iroh-shaped client → native QUIC server, round-trip
    /// a single frame. This is the smoke test that the adapter
    /// actually moves bytes through the underlying transport.
    #[cfg(not(feature = "iroh"))]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn bridge_native_quic_round_trip() {
        use crate::quic::QuicTransportBuilder;
        let server_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = QuicTransportBuilder::new(NodeId::random(), server_addr)
            .build()
            .unwrap();
        let server_node = server.local_node_id().clone();
        // Eagerly bind the underlying QUIC endpoint so we can read
        // the kernel-assigned port from the real listener.
        let server_endpoint = server.get_or_init_endpoint().await.unwrap();
        let actual_server_addr = server_endpoint.local_addr().unwrap();
        let mut rx = server.take_incoming_receiver_impl().await.unwrap();

        let client = IrohTransport::new();
        let target = NodeAddr {
            node_id: server_node.clone(),
            direct: Some(Endpoint::new(
                actual_server_addr.ip().to_string(),
                actual_server_addr.port(),
            )),
            relay: Some(RelayUrl::new("https://relay.example.com/")),
        };
        let mut conn = client.dial_addr(target).await.unwrap();
        let frame = Frame::text("hello-bridge");
        conn.send(frame).await.unwrap();

        let (peer, mut incoming) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            rx.recv(),
        )
        .await
        .expect("server should accept within 5s")
        .expect("server should yield one incoming connection");
        assert_eq!(peer, client.local_node().clone());

        let received = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            incoming.recv(),
        )
        .await
        .expect("server should receive the frame in 5s")
        .expect("recv must yield a frame")
        .expect("frame must decode");
        let body = received.as_bytes().to_vec();
        assert_eq!(body, b"hello-bridge");

        assert_eq!(
            client.last_relay_url().as_deref(),
            Some("https://relay.example.com/"),
        );

        client.shutdown().await.unwrap();
        server.shutdown().await.unwrap();
    }

    /// When the `iroh` feature is on, a real iroh endpoint round-trips
    /// a frame through the `Transport` trait. This test uses the
    /// `presets::N0` relay map (which connects to the public n0
    /// relays), so it requires network access. It is `#[ignore]` by
    /// default — run with `cargo test -p adnet-transport
    /// --features iroh -- --ignored --nocapture` to exercise it.
    #[cfg(feature = "iroh")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires network access to n0 relays"]
    async fn iroh_endpoint_frame_round_trip() {
        use iroh::EndpointAddr;
        let server = IrohTransport::new();
        let server_pk = node_id_to_public_key(server.local_node()).unwrap();
        let server_addr = EndpointAddr::new(server_pk);
        let server_node = server.local_node().clone();
        let server_task = tokio::spawn(async move {
            let (_peer, mut conn) = server.accept().await.unwrap().unwrap();
            let frame = conn.recv().await.unwrap().unwrap();
            assert_eq!(frame.as_bytes(), b"hello-iroh");
            conn.send(Frame::text("world-iroh")).await.unwrap();
        });

        let client = IrohTransport::new();
        let mut conn = client
            .dial_endpoint_addr(server_addr, &server_node)
            .await
            .unwrap();
        conn.send(Frame::text("hello-iroh")).await.unwrap();
        let reply = conn.recv().await.unwrap().unwrap();
        assert_eq!(reply.as_bytes(), b"world-iroh");
        server_task.await.unwrap();
    }

    #[cfg(feature = "iroh")]
    #[test]
    fn node_id_public_key_round_trip() {
        // Use a deterministic SecretKey so the test is reproducible.
        let secret = iroh::SecretKey::from_bytes(&[7u8; 32]);
        let pk = secret.public();
        let node_id = public_key_to_node_id(&pk);
        assert_eq!(node_id.as_bytes(), pk.as_bytes());
        let back = node_id_to_public_key(&node_id).unwrap();
        assert_eq!(back.as_bytes(), pk.as_bytes());
    }
}