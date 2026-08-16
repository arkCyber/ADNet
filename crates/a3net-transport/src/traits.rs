//! Transport trait — the abstraction `a3net-node` consumes.
//!
//! Any backend that can carry framed messages between two `NodeId`s can
//! implement this trait. The mesh HTTP server in `a3net-mesh` is the
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

use a3net_types::{NodeAddr, NodeId};
use tokio::sync::mpsc;

use crate::frame::Frame;

/// Connection type indicating how the connection was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionType {
    /// Direct connection (e.g., public IP, LAN).
    Direct,
    /// Connection via relay.
    Relay,
    /// Loopback connection.
    Loopback,
    /// Mixed: some paths are direct, some are relay.
    Mixed,
    /// Connection is closed.
    Closed,
}

impl ConnectionType {
    /// Check if this connection is relay-only.
    pub fn is_relay_only(&self) -> bool {
        matches!(self, ConnectionType::Relay)
    }

    /// Check if this connection has a direct path.
    pub fn has_direct(&self) -> bool {
        matches!(self, ConnectionType::Direct | ConnectionType::Mixed | ConnectionType::Loopback)
    }

    /// Check if this connection uses a relay.
    pub fn has_relay(&self) -> bool {
        matches!(self, ConnectionType::Relay | ConnectionType::Mixed)
    }

    /// Check if this connection is closed.
    pub fn is_closed(&self) -> bool {
        matches!(self, ConnectionType::Closed)
    }
}

impl ConnectionType {
    /// Get a string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            ConnectionType::Direct => "direct",
            ConnectionType::Relay => "relay",
            ConnectionType::Loopback => "loopback",
            ConnectionType::Mixed => "mixed",
            ConnectionType::Closed => "closed",
        }
    }
}

/// Stream priority for outgoing data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamPriority {
    /// Priority level (higher = more urgent).
    pub level: i32,
    /// Whether this is a broadcast (no bandwidth reservation).
    pub broadcast: bool,
}

impl StreamPriority {
    /// High priority constant (Quinn priority = 1).
    pub const High: Self = Self {
        level: 1,
        broadcast: false,
    };

    /// Low priority constant (Quinn priority = -1).
    pub const Low: Self = Self {
        level: -1,
        broadcast: false,
    };

    /// Normal priority constant (Quinn priority = 0, default).
    pub const Normal: Self = Self {
        level: 0,
        broadcast: false,
    };

    /// Critical priority constant (Quinn priority = 2).
    pub const Critical: Self = Self {
        level: 2,
        broadcast: false,
    };

    /// High priority for urgent data.
    pub fn high() -> Self {
        Self::High
    }

    /// Low priority for background data.
    pub fn low() -> Self {
        Self::Low
    }

    /// Normal priority (default).
    pub fn normal() -> Self {
        Self::Normal
    }

    /// Critical priority for highest urgency.
    pub fn critical() -> Self {
        Self::Critical
    }

    /// Convert to Quinn priority value.
    pub fn as_quinn_i32(self) -> i32 {
        if self.broadcast {
            -1 // Lowest priority for broadcast
        } else {
            self.level
        }
    }
}

impl Default for StreamPriority {
    fn default() -> Self {
        Self::normal()
    }
}

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

    /// Get the connection type.
    async fn connection_type(&self) -> ConnectionType {
        ConnectionType::Direct
    }

    /// Set the priority for outgoing data.
    async fn set_priority(&mut self, _priority: StreamPriority) -> TransportResult<()> {
        Ok(())
    }

    /// Get the maximum datagram size for this connection.
    ///
    /// Returns `None` if datagrams are not supported.
    async fn max_datagram_size(&self) -> Option<usize> {
        None
    }
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
    /// over a `NodeAddr` (direct + relay). A3Net's native QUIC impl ignores
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

    /// Perform a health check on the transport.
    fn health_check(&self) -> Result<(), String> {
        Ok(())
    }

    /// Resolve a peer's socket address from the transport's internal registry.
    ///
    /// Returns `None` if the peer is not registered.
    async fn resolve_peer(&self, node: &NodeId) -> Option<std::net::SocketAddr> {
        let _ = node;
        None
    }

    /// Watch for endpoint address changes.
    ///
    /// Returns a stream that yields endpoint addresses when they change.
    /// The iroh endpoint exposes `watch_addr()` which returns a `Watcher`;
    /// we wrap it in a boxed `futures::Stream`. Non-iroh backends return `None`.
    async fn watch_endpoint_addr(
        &self,
    ) -> Option<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = crate::endpoint::EndpointAddr> + Send + Sync + 'static>,
        >,
    > {
        None
    }
}

/// Convenience alias used by higher layers.
pub type SharedTransport = Arc<dyn Transport>;
