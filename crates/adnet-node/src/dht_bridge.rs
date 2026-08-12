//! Transport adapter that connects an `adnet_dht::DhtNetworkSender`
//! to a generic `SharedTransport`.
//!
//! This adapter is the runtime glue that makes the DHT network
//! effect observable. It sits between:
//!
//! - `DhtNetworkSender` (the DHT side), and
//! - `Arc<dyn adnet_transport::Transport>` (the transport side).
//!
//! The bridge is intentionally simple:
//!
//! - [`DynTransportBridge::dial_and_send`] opens a bidi stream via
//!   `transport.dial(peer)`, writes the request as a single
//!   length-prefixed `Frame` and reads the response (also a
//!   length-prefixed `Frame`). It hands the response bytes back to
//!   a caller-supplied response sink which (in production) routes
//!   them to `DhtNetworkSender::handle_response`.
//! - `local_node_id` and `get_peer_addr` are direct delegations to
//!   the underlying transport.
//!
//! The bridge does not implement `TransportDhtSender` directly —
//! instead [`BridgeSenderAdapter`] wraps the bridge to satisfy the
//! trait for the legacy `send_raw` path used by `announce_provider`.

use std::sync::Arc;
use std::time::Duration;

use adnet_dht::query::QueryError;
use adnet_dht::transport::TransportBridge;
use adnet_types::{NodeAddr, NodeId};

use adnet_transport::{Frame, SharedTransport};

/// Sink for inbound DHT response bytes. Invoked synchronously on
/// the bridge task when a response frame arrives. The sink is
/// typically `Arc::new(move |bytes| sender.handle_response(...))`
/// or a thin closure that decodes the wire frame and forwards to
/// `DhtNetworkSender::handle_response`.
pub type DynResponseSink = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

/// Maximum number of bytes the bridge will read from a single
/// DHT response stream. Matches the wire layer's `MAX_VALUE_SIZE`.
const DHT_RESPONSE_READ_LIMIT: usize = 1024 * 1024;

/// Generic transport bridge. Implements `TransportBridge` over
/// `SharedTransport` so the DHT module can be paired with any
/// transport backend (native QUIC, iroh, mesh) that exposes the
/// framed `Transport` trait.
pub struct DynTransportBridge {
    transport: SharedTransport,
    local_id: NodeId,
    response_sink: parking_lot::Mutex<Option<DynResponseSink>>,
}

impl std::fmt::Debug for DynTransportBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynTransportBridge")
            .field("transport", &"<dyn Transport>")
            .field("local_id", &self.local_id)
            .finish()
    }
}

impl DynTransportBridge {
    /// Build a new bridge. The local `NodeId` is provided by the
    /// caller (it matches `transport.local_node()` but exposing it
    /// avoids an extra trait call on every method invocation).
    pub fn new(transport: SharedTransport, local_id: NodeId) -> Self {
        Self {
            transport,
            local_id,
            response_sink: parking_lot::Mutex::new(None),
        }
    }

    /// Install a callback that receives every DHT response frame
    /// the bridge reads. Typically wired to
    /// `DhtNetworkSender::handle_response` after decoding the
    /// frame back into a `DhtWireMessage`.
    pub fn set_response_sink(&self, sink: DynResponseSink) {
        *self.response_sink.lock() = Some(sink);
    }

    /// Resolve the socket address the transport has on file for
    /// `peer`. Returns `None` when the peer is unknown; callers
    /// should treat this as "skip this peer" rather than an
    /// error.
    pub async fn resolve_peer_addr(&self, peer: &NodeId) -> Option<NodeAddr> {
        let socket = self.transport.resolve_peer(peer).await?;
        Some(
            NodeAddr::new(peer.clone()).with_direct(adnet_types::Endpoint::new(
                socket.ip().to_string(),
                socket.port(),
            )),
        )
    }
}

#[async_trait::async_trait]
impl TransportBridge for DynTransportBridge {
    fn local_node_id(&self) -> &NodeId {
        &self.local_id
    }

    async fn get_peer_addr(&self, peer: &NodeId) -> Option<NodeAddr> {
        self.resolve_peer_addr(peer).await
    }

    async fn dial_and_send(&self, peer: &NodeId, msg: Vec<u8>) -> Result<(), QueryError> {
        // Open the connection. A failure here means the peer is
        // unknown or unreachable — map to a network error so the
        // DHT query engine moves on to the next closest peer.
        let mut conn = self
            .transport
            .dial(peer.clone())
            .await
            .map_err(|e| QueryError::Network(e.to_string()))?;

        // Write the request with a timeout so a stuck peer
        // cannot stall the calling task indefinitely.
        //
        // `OutgoingConnection::send` already prepends a length
        // prefix via `FrameCodec::encode`. The bridge must NOT
        // prepend its own — doing so yields
        // `outer-len || inner-len || payload` on the wire, and
        // the receiver's `FrameCodec::decode_stream` would only
        // strip the outer prefix and hand back the inner prefix
        // as the "payload" (i.e. a 4-byte integer), corrupting
        // every DHT message.
        tokio::time::timeout(Duration::from_secs(5), conn.send(Frame(msg)))
            .await
            .map_err(|_| QueryError::Timeout)?
            .map_err(|e| QueryError::Network(e.to_string()))?;

        // If a response sink is installed, read a single
        // response frame and hand it to the sink. The sink is
        // responsible for parsing + request-id matching. We
        // bound the read so a malicious peer cannot fill our
        // memory.
        let sink = self.response_sink.lock().clone();
        if let Some(sink) = sink {
            let recv = tokio::time::timeout(Duration::from_secs(5), conn.recv()).await;
            match recv {
                Ok(Ok(Some(frame))) => {
                    let data = frame.into_inner();
                    if data.len() > DHT_RESPONSE_READ_LIMIT {
                        return Err(QueryError::Network(format!(
                            "dht response too large: {} > {DHT_RESPONSE_READ_LIMIT}",
                            data.len()
                        )));
                    }
                    sink(data);
                }
                Ok(Ok(None)) => {
                    // Peer closed the stream without a reply —
                    // treat as a network error so the DHT
                    // engine retries with the next closest peer.
                    return Err(QueryError::Network(
                        "peer closed stream before reply".into(),
                    ));
                }
                Ok(Err(e)) => return Err(QueryError::Network(e.to_string())),
                Err(_) => return Err(QueryError::Timeout),
            }
        }

        let _ = conn.close().await;
        Ok(())
    }
}

/// Adapter that satisfies `adnet_dht::network::TransportDhtSender`
/// over a `DynTransportBridge`. The trait's `send_to` method is
/// fire-and-forget — it corresponds to `AddProvider` writes where
/// no response is expected. Request/response correlation happens
/// at the `DynTransportBridge` layer instead.
pub struct BridgeSenderAdapter {
    bridge: Arc<dyn TransportBridge>,
}

impl BridgeSenderAdapter {
    pub fn new(bridge: Arc<dyn TransportBridge>) -> Self {
        Self { bridge }
    }
}

#[async_trait::async_trait]
impl adnet_dht::network::TransportDhtSender for BridgeSenderAdapter {
    async fn send_to(&self, peer: &NodeId, data: &[u8]) -> Result<(), QueryError> {
        self.bridge.dial_and_send(peer, data.to_vec()).await
    }

    async fn get_peer_addr(&self, peer: &NodeId) -> Option<String> {
        let addr = self.bridge.get_peer_addr(peer).await?;
        addr.direct.map(|e| format!("{}:{}", e.host(), e.port().unwrap_or(0)))
    }
}