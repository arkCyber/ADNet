//! Transport trait — the abstraction `adnet-node` consumes.
//!
//! Any backend that can carry framed messages between two `NodeId`s can
//! implement this trait. The mesh HTTP server in `adnet-mesh` is the
//! fallback when the transport is offline.
//!
//! Async method style:
//! - [`Transport`] uses **RPITIT** (native `async fn` in trait, Rust 1.75+),
//!   matching the upstream `iroh` crate.
//! - [`OutgoingConnection`] uses `async_trait` because it sits behind a
//!   `Box<dyn ...>` for transport callers.

use std::any::Any;
use std::fmt::Debug;
use std::sync::Arc;

use adnet_types::{NodeAddr, NodeId};
use tokio::sync::mpsc;

use crate::frame::Frame;

/// Errors that can come out of any transport backend.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("connection closed")]
    ConnectionClosed,

    #[error("endpoint not found: {0}")]
    EndpointNotFound(String),

    #[error("frame too large: {0} bytes (max {1})")]
    FrameTooLarge(usize, usize),

    #[error("decode: {0}")]
    Decode(String),

    #[error("peer identity unavailable: {0}")]
    PeerIdentityUnavailable(String),

    #[error("peer identity mismatch: expected {expected}, got {actual}")]
    PeerIdentityMismatch { expected: String, actual: String },

    #[error("identity persistence: {0}")]
    Identity(String),

    #[error("other: {0}")]
    Other(String),
}

pub type TransportResult<T> = Result<T, TransportError>;

/// Outgoing connection handle. `async_trait` because it sits behind
/// `Box<dyn OutgoingConnection>` for transport callers.
#[async_trait::async_trait]
pub trait OutgoingConnection: Send + Sync + Debug + 'static {
    /// Send a single framed message.
    async fn send(&mut self, frame: Frame) -> TransportResult<()>;

    /// Receive the next framed message.
    async fn recv(&mut self) -> TransportResult<Option<Frame>>;

    /// Close the underlying connection gracefully.
    async fn close(self: Box<Self>) -> TransportResult<()>;
}

/// Backend contract.
///
/// `Transport` is `async_trait` so callers can hold `Arc<dyn Transport>`
/// (used as `SharedTransport`). iroh itself exposes its `Endpoint` as a
/// concrete type — we trade a tiny bit of dynamic dispatch for the
/// flexibility of plugging native QUIC, mesh HTTP, or a future iroh-net
/// backend behind the same `Arc<dyn Transport>`.
#[async_trait::async_trait]
pub trait Transport: Send + Sync + 'static {
    /// Open an outgoing connection to a remote node.
    ///
    /// Implementations look up the node in their internal registry; if no
    /// direct endpoint is known, they should fail with
    /// [`TransportError::EndpointNotFound`].
    async fn dial(&self, node: NodeId) -> TransportResult<Box<dyn OutgoingConnection>>;

    /// Open an outgoing connection using a fully-specified [`NodeAddr`].
    ///
    /// This is the entry point iroh uses for dialing when a caller hands
    /// over a `NodeAddr` (direct + relay). ADNet's native QUIC impl ignores
    /// the relay URL for now.
    async fn dial_addr(&self, addr: NodeAddr) -> TransportResult<Box<dyn OutgoingConnection>>;

    /// Accept the next incoming connection. Returns `None` if the listener
    /// has been closed.
    async fn accept(&self) -> TransportResult<Option<(NodeId, Box<dyn OutgoingConnection>)>>;

    /// Local node identifier.
    fn local_node(&self) -> &NodeId;

    /// Short human-readable name of the backend. Used by the REPL
    /// `/transport` command and by telemetry — should be something
    /// like `"quic-native"`, `"quic-iroh"`, or `"loopback"`. The
    /// default is `"unknown"` so backends that do not override it
    /// still surface in the UI.
    fn kind(&self) -> &'static str {
        "unknown"
    }

    /// Upcast to `&dyn Any` so callers can recover the concrete
    /// backend type. The default implementation returns `None`; the
    /// native QUIC backend overrides it so the REPL can print bind
    /// address / certificate fingerprint.
    fn as_any(&self) -> Option<&dyn Any> {
        None
    }

    /// Best-effort shutdown.
    async fn shutdown(&self) -> TransportResult<()>;

    /// Take the receiver that surfaces every accepted incoming
    /// connection. Returns `None` for transports that do not maintain
    /// their own accept loop (the default). Concrete backends such as
    /// [`QuicTransport`](crate::quic::QuicTransport) override this so
    /// higher layers can drive the accept queue themselves.
    async fn take_incoming_receiver(
        &self,
    ) -> Option<mpsc::Receiver<(NodeId, Box<dyn OutgoingConnection>)>> {
        None
    }
}

/// Convenience alias used by higher layers.
pub type SharedTransport = Arc<dyn Transport>;
