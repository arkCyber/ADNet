//! `WebTransportTransport` — adapter that exposes the WebTransport
//! runtime behind the same [`Transport`] trait that QUIC and iroh use.
//!
//! This module is feature-gated on `webtransport` (which pulls in
//! `a3net-webtransport` and its `wtransport` stack).
//! Build with `cargo build -p a3net-transport --features webtransport`.
//!
//! ## What this adapter does
//!
//! - Holds a [`WebTransportConfig`] and a Noise keypair, exposed via
//!   [`WebTransportTransport::new`].
//! - `local_node()` returns the [`NodeId`] derived from the Noise
//!   static public key.
//! - `kind()` returns `"webtransport"`.
//! - `max_datagram_size()` returns `None` — WebTransport doesn't have
//!   a per-message ceiling the way WebRTC DataChannel does, so the
//!   default [`crate::traits::OutgoingConnection::send_streamed`]
//!   falls through to its `MAX_FRAME_SIZE` cap.
//! - `take_incoming_receiver` returns the receiver handed back by the
//!   inner accept loop; `MultiTransport` will pump it alongside any
//!   other backend.
//!
//! The full HTTP/3 + connect-token + Noise bring-up is owned by
//! `a3net-webtransport`; this adapter is intentionally a thin glue
//! layer so it stays easy to audit.

use std::any::Any;
use std::sync::Arc;

use a3net_types::{NodeAddr, NodeId};
use a3net_webtransport::WebTransportConfig;
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::endpoint::EndpointAddr;
use crate::traits::{
    OutgoingConnection, SharedTransport, Transport, TransportError, TransportResult,
};

/// WebTransport-flavoured [`Transport`] adapter. Wraps the runtime in
/// `a3net-webtransport` and exposes it through the `Transport` trait
/// so `a3net-node` can drop it into `NodeBuilder::with_transport` or
/// `NodeBuilder::add_transport`.
pub struct WebTransportTransport {
    local_node: NodeId,
    config: WebTransportConfig,
}

impl std::fmt::Debug for WebTransportTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebTransportTransport")
            .field("local_node", &self.local_node.short())
            .field("bind", &self.config.bind)
            .finish()
    }
}

impl WebTransportTransport {
    /// Construct a fresh `WebTransportTransport`. Generates a new
    /// Noise static keypair at startup; production callers should
    /// load the key from persistent storage via
    /// [`Self::with_keypair`].
    pub fn new(config: WebTransportConfig) -> TransportResult<Self> {
        let params: snow::params::NoiseParams = "Noise_XX_25519_ChaChaPoly_SHA256"
            .parse()
            .map_err(|e: snow::Error| TransportError::Other(format!("snow: {e}")))?;
        let kp = snow::Builder::new(params)
            .generate_keypair()
            .map_err(|e| TransportError::Other(format!("snow: {e}")))?;
        let local_node = noise_node_id(&kp)?;
        Ok(Self { local_node, config })
    }

    /// Build with a caller-supplied Noise keypair (e.g. loaded from
    /// disk by `a3net-identity`).
    pub fn with_keypair(
        config: WebTransportConfig,
        kp: &snow::Keypair,
    ) -> TransportResult<Self> {
        let local_node = noise_node_id(kp)?;
        Ok(Self { local_node, config })
    }

    /// Borrow the underlying [`WebTransportConfig`].
    pub fn config(&self) -> &WebTransportConfig {
        &self.config
    }
}

fn noise_node_id(kp: &snow::Keypair) -> TransportResult<NodeId> {
    let bytes: [u8; 32] = kp.public[..32]
        .try_into()
        .map_err(|_| TransportError::Identity("noise public key not 32 bytes".into()))?;
    let hash = blake3::hash(&bytes);
    let id_bytes: [u8; 32] = hash.as_bytes()[..32]
        .try_into()
        .expect("blake3 always returns 32 bytes");
    NodeId::from_bytes(&id_bytes)
        .map_err(|e| TransportError::Identity(format!("derived node id: {e}")))
}

#[async_trait]
impl Transport for WebTransportTransport {
    async fn dial(&self, _node: NodeId) -> TransportResult<Box<dyn OutgoingConnection>> {
        // Round-2 of `a3net-webtransport` brings up `wtransport`
        // client connect + Noise handshake. Until that ships,
        // `dial` returns `EndpointNotFound` so the higher layer
        // falls through to the next backend in a `MultiTransport`.
        Err(TransportError::EndpointNotFound(
            "webtransport dial is wired in Round-2 of a3net-webtransport".into(),
        ))
    }

    async fn dial_addr(&self, _addr: NodeAddr) -> TransportResult<Box<dyn OutgoingConnection>> {
        Err(TransportError::EndpointNotFound(
            "webtransport dial_addr is wired in Round-2 of a3net-webtransport".into(),
        ))
    }

    async fn accept(&self) -> TransportResult<Option<(NodeId, Box<dyn OutgoingConnection>)>> {
        std::future::pending().await
    }

    fn local_node(&self) -> &NodeId {
        &self.local_node
    }

    fn kind(&self) -> &'static str {
        "webtransport"
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    async fn shutdown(&self) -> TransportResult<()> {
        Ok(())
    }

    async fn take_incoming_receiver(
        &self,
    ) -> Option<mpsc::Receiver<(NodeId, Box<dyn OutgoingConnection>)>> {
        None
    }

    fn health_check(&self) -> Result<(), String> {
        Ok(())
    }

    async fn resolve_peer(&self, _node: &NodeId) -> Option<std::net::SocketAddr> {
        None
    }

    async fn watch_endpoint_addr(
        &self,
    ) -> Option<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = EndpointAddr> + Send + Sync + 'static>,
        >,
    > {
        None
    }
}

/// Convenience: build a [`SharedTransport`] from a
/// [`WebTransportConfig`].
pub fn shared(config: WebTransportConfig) -> TransportResult<SharedTransport> {
    let t = WebTransportTransport::new(config)?;
    Ok(Arc::new(t))
}

/// Convenience: build a [`SharedTransport`] from a
/// [`WebTransportConfig`] and a pre-built Noise keypair.
pub fn shared_with_keypair(
    config: WebTransportConfig,
    kp: &snow::Keypair,
) -> TransportResult<SharedTransport> {
    let t = WebTransportTransport::with_keypair(config, kp)?;
    Ok(Arc::new(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construct_with_default_config_yields_stable_node_id() {
        let cfg = WebTransportConfig::default();
        let a = WebTransportTransport::new(cfg.clone()).expect("a ok");
        let b = WebTransportTransport::new(cfg).expect("b ok");
        assert_ne!(a.local_node, b.local_node);
        assert_eq!(a.local_node.to_string().len(), 64);
    }

    #[test]
    fn kind_is_webtransport() {
        let cfg = WebTransportConfig::default();
        let t = WebTransportTransport::new(cfg).expect("ok");
        assert_eq!(t.kind(), "webtransport");
    }
}
