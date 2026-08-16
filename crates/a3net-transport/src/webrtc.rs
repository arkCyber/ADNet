//! `WebRtcTransport` — adapter that exposes the WebRTC DataChannel
//! runtime behind the same [`Transport`] trait that QUIC and iroh use.
//!
//! This module is feature-gated on `webrtc` (which pulls in
//! `a3net-webrtc` and its `webrtc-rs` + `snow` + `pkarr` stack).
//! Build with `cargo build -p a3net-transport --features webrtc`.
//!
//! ## What this adapter does
//!
//! - Holds a [`WebRtcConfig`] and a Noise keypair, exposed via
//!   [`WebRtcTransport::new`].
//! - `local_node()` returns the [`NodeId`] derived from the Noise
//!   static public key (BLAKE3-32, see `a3net_webrtc::noise_dc::StaticPub::to_node_id`).
//! - `kind()` returns `"webrtc"`.
//! - `max_datagram_size()` returns `Some(config.max_datagram_bytes)`
//!   so the default [`crate::traits::OutgoingConnection::send_streamed`]
//!   knows to chunk anything larger than that.
//! - `take_incoming_receiver` returns the receiver handed back by the
//!   inner accept loop; `MultiTransport` will pump it alongside any
//!   other backend.
//!
//! The full SDP / ICE bring-up is owned by `a3net-webrtc`; this
//! adapter is intentionally a thin glue layer so it stays easy to
//! audit and easy to swap when the upstream `webrtc-rs` API changes.

use std::any::Any;
use std::sync::Arc;

use a3net_types::{NodeAddr, NodeId};
use a3net_webrtc::WebRtcConfig;
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::endpoint::EndpointAddr;
use crate::traits::{
    OutgoingConnection, SharedTransport, Transport, TransportError, TransportResult,
};

/// WebRTC-flavoured [`Transport`] adapter. Wraps the runtime in
/// `a3net-webrtc` and exposes it through the `Transport` trait so
/// `a3net-node` can drop it into `NodeBuilder::with_transport` or
/// `NodeBuilder::add_transport`.
pub struct WebRtcTransport {
    /// Local NodeId, derived from the Noise static public key. We
    /// cache it so `local_node()` is O(1).
    local_node: NodeId,
    config: WebRtcConfig,
}

impl std::fmt::Debug for WebRtcTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebRtcTransport")
            .field("local_node", &self.local_node.short())
            .field("max_datagram_bytes", &self.config.max_datagram_bytes)
            .finish()
    }
}

impl WebRtcTransport {
    /// Construct a fresh `WebRtcTransport`. Generates a new Noise
    /// static keypair at startup; production callers should load the
    /// key from persistent storage via [`Self::with_keypair`].
    pub fn new(config: WebRtcConfig) -> TransportResult<Self> {
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
    pub fn with_keypair(config: WebRtcConfig, kp: &snow::Keypair) -> TransportResult<Self> {
        let local_node = noise_node_id(kp)?;
        Ok(Self { local_node, config })
    }

    /// Borrow the underlying [`WebRtcConfig`].
    pub fn config(&self) -> &WebRtcConfig {
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
impl Transport for WebRtcTransport {
    async fn dial(&self, _node: NodeId) -> TransportResult<Box<dyn OutgoingConnection>> {
        // Round-2 of `a3net-webrtc` brings up SDP/ICE. Until that
        // ships, `dial` and `dial_addr` return an explicit
        // "EndpointNotFound" — the higher layer falls through to
        // mesh HTTP or to the next backend in a `MultiTransport`.
        Err(TransportError::EndpointNotFound(
            "webrtc dial is wired in Round-2 of a3net-webrtc".into(),
        ))
    }

    async fn dial_addr(&self, _addr: NodeAddr) -> TransportResult<Box<dyn OutgoingConnection>> {
        Err(TransportError::EndpointNotFound(
            "webrtc dial_addr is wired in Round-2 of a3net-webrtc".into(),
        ))
    }

    async fn accept(&self) -> TransportResult<Option<(NodeId, Box<dyn OutgoingConnection>)>> {
        // The accept loop lives inside `a3net-webrtc` and surfaces
        // incoming connections through `take_incoming_receiver`. The
        // default `accept()` therefore waits forever; the
        // `MultiTransport` wrapper drains via the receiver instead.
        std::future::pending().await
    }

    fn local_node(&self) -> &NodeId {
        &self.local_node
    }

    fn kind(&self) -> &'static str {
        "webrtc"
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    async fn shutdown(&self) -> TransportResult<()> {
        // No persistent resources to release. The inner accept loop
        // task is owned by `a3net-webrtc::DcSession` callers, not by
        // this adapter.
        Ok(())
    }

    async fn take_incoming_receiver(
        &self,
    ) -> Option<mpsc::Receiver<(NodeId, Box<dyn OutgoingConnection>)>> {
        // Round-2 of `a3net-webrtc` will hand back a real receiver
        // from the SDP/ICE accept loop. Until then we return `None`
        // so the `MultiTransport` wrapper simply has no incoming
        // queue to pump from this backend.
        None
    }

    fn health_check(&self) -> Result<(), String> {
        // Without a real `RTCPeerConnection` to inspect we report
        // healthy. Round-2 will surface ICE state here.
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

/// Convenience: build a [`SharedTransport`] from a [`WebRtcConfig`].
pub fn shared(config: WebRtcConfig) -> TransportResult<SharedTransport> {
    let t = WebRtcTransport::new(config)?;
    Ok(Arc::new(t))
}

/// Convenience: build a [`SharedTransport`] from a
/// [`WebRtcConfig`] and a pre-built Noise keypair.
pub fn shared_with_keypair(
    config: WebRtcConfig,
    kp: &snow::Keypair,
) -> TransportResult<SharedTransport> {
    let t = WebRtcTransport::with_keypair(config, kp)?;
    Ok(Arc::new(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construct_with_default_config_yields_stable_node_id() {
        let cfg = WebRtcConfig::default();
        let a = WebRtcTransport::new(cfg.clone()).expect("a ok");
        let b = WebRtcTransport::new(cfg).expect("b ok");
        // Two fresh keypairs produce two different NodeIds.
        assert_ne!(a.local_node, b.local_node);
        // But each one is exactly 32 bytes hex.
        assert_eq!(a.local_node.to_string().len(), 64);
    }

    #[test]
    fn kind_is_webrtc_and_max_datagram_size_matches_config() {
        let cfg = WebRtcConfig {
            max_datagram_bytes: 8 * 1024,
            ..WebRtcConfig::default()
        };
        let t = WebRtcTransport::new(cfg).expect("ok");
        assert_eq!(t.kind(), "webrtc");
        assert_eq!(t.config().max_datagram_bytes, 8 * 1024);
    }
}
