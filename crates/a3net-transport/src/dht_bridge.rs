//! QUIC-backed implementation of the DHT [`TransportBridge`] trait.
//!
//! This module bridges [`a3net_dht::transport::TransportBridge`] to a
//! concrete [`QuicTransport`]. The bridge is intentionally minimal:
//!
//! - `dial_and_send` opens a fresh QUIC bidirectional stream, writes
//!   the framed request, reads the single response frame, and hands
//!   the bytes to a caller-supplied response sink. This avoids
//!   re-entrancy in the calling task: the bridge fires the request
//!   off, awaits the response, and only then returns.
//! - `local_node_id` and `get_peer_addr` delegate to the underlying
//!   transport.
//!
//! The dispatcher half of the bridge is built into the
//! [`QuicTransport`] itself: see
//! [`QuicTransportBuilder::with_dht_dispatcher`] and
//! [`quic::DhtInboundDispatcher`].

#[cfg(feature = "dht")]
use std::sync::Arc;
#[cfg(feature = "dht")]
use std::time::Duration;

#[cfg(feature = "dht")]
use a3net_dht::query::QueryError;
#[cfg(feature = "dht")]
use a3net_dht::transport::TransportBridge;
#[cfg(feature = "dht")]
use a3net_types::{NodeAddr, NodeId};

#[cfg(feature = "dht")]
use crate::quic::QuicTransport;

/// Maximum number of bytes the bridge will read from a single
/// DHT response stream. Matches the wire layer's `MAX_VALUE_SIZE`.
#[cfg(feature = "dht")]
const DHT_RESPONSE_READ_LIMIT: usize = 1024 * 1024;

/// Sink for inbound DHT response bytes. The bridge invokes the
/// sink with the raw bytes read from the peer's response stream;
/// the sink (typically backed by a `DhtNetworkSender`) is
/// responsible for parsing, request-id matching, and forwarding
/// to the pending-oneshot map.
#[cfg(feature = "dht")]
pub type DhtResponseSink = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

/// Bridge that connects the DHT module to a real QUIC transport.
///
/// The bridge owns an `Arc<QuicTransport>` plus an optional
/// response sink. When the sink is `Some`, every successful
/// `dial_and_send` reads the response off the same bidirectional
/// stream and forwards the bytes to the sink. When the sink is
/// `None`, the bridge is effectively fire-and-forget — useful for
/// [`AddProvider`](a3net_dht::protocol::DhtWireMessage::AddProvider)
/// which has no response.
#[cfg(feature = "dht")]
pub struct QuicTransportBridge {
    transport: Arc<QuicTransport>,
    response_sink: Option<DhtResponseSink>,
}

#[cfg(feature = "dht")]
impl QuicTransportBridge {
    /// Wrap a `QuicTransport` for use by `a3net-dht` without
    /// response correlation. Equivalent to passing `None` for the
    /// sink.
    pub fn new(transport: Arc<QuicTransport>) -> Self {
        Self {
            transport,
            response_sink: None,
        }
    }

    /// Wrap a `QuicTransport` and install a response sink. The
    /// sink is invoked synchronously on the bridge task when a
    /// response frame arrives.
    pub fn with_sink(transport: Arc<QuicTransport>, sink: DhtResponseSink) -> Self {
        Self {
            transport,
            response_sink: Some(sink),
        }
    }

    /// Borrow the underlying transport. Useful for tests and for
    /// code that needs to access quinn-specific features (e.g.
    /// `local_node_id`) without going through the trait.
    pub fn transport(&self) -> &Arc<QuicTransport> {
        &self.transport
    }
}

#[cfg(feature = "dht")]
#[async_trait::async_trait]
impl TransportBridge for QuicTransportBridge {
    fn local_node_id(&self) -> &NodeId {
        self.transport.local_node_id()
    }

    async fn get_peer_addr(&self, peer: &NodeId) -> Option<NodeAddr> {
        let socket = self.transport.resolve_peer(peer).await?;
        Some(
            NodeAddr::new(peer.clone()).with_direct(a3net_types::Endpoint::new(
                socket.ip().to_string(),
                socket.port(),
            )),
        )
    }

    async fn dial_and_send(&self, peer: &NodeId, msg: Vec<u8>) -> Result<(), QueryError> {
        // Open the stream; failure here means the peer is unknown
        // or the handshake could not complete. Both map to a
        // generic network error so the DHT query engine treats
        // this as a transient failure (it will try the next
        // closest peer on retry).
        let (mut send, mut recv) = self
            .transport
            .dial_for_dht(peer)
            .await
            .map_err(|e| QueryError::Network(e.to_string()))?;

        // Write the request with a short timeout so a stuck
        // peer cannot stall the calling task beyond the
        // configured DHT timeout.
        tokio::time::timeout(Duration::from_secs(5), send.write_all(&msg))
            .await
            .map_err(|_| QueryError::Timeout)?
            .map_err(|e| QueryError::Network(e.to_string()))?;
        let _ = send.finish();

        // If a response sink is installed, drain a single
        // response frame and hand it to the sink. The sink is
        // responsible for parsing + request-id matching.
        if let Some(sink) = &self.response_sink {
            let read = tokio::time::timeout(
                Duration::from_secs(5),
                recv.read_to_end(DHT_RESPONSE_READ_LIMIT),
            )
            .await
            .map_err(|_| QueryError::Timeout)?
            .map_err(|e| QueryError::Network(e.to_string()))?;
            if !read.is_empty() {
                sink(read);
            }
        }

        Ok(())
    }
}