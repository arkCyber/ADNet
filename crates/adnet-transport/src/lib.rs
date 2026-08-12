//! `adnet-transport` — abstract transport layer.
//!
//! Public trait [`Transport`] defines the operations `adnet-node` consumes:
//! open outgoing connections, accept incoming ones, send/receive framed
//! messages.
//!
//! Today's [`quic::QuicTransport`] is a wiring-ready stub: the full
//! QUIC handshake + certificate generation is queued as a follow-up. The
//! mesh HTTP layer in `adnet-mesh` remains the always-on fallback transport.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod blob_proto;
pub mod endpoint;
pub mod frame;
pub mod metrics;
pub mod quic;
pub mod traits;

pub use blob_proto::{
    JsonValue, MAX_CHUNK_PAYLOAD, Message, error_frame, error_value, fetch_blob_over_transport,
    looks_like_message, serve_blob_request,
};
pub use endpoint::EndpointAddr;
pub use frame::{Frame, FrameCodec};
pub use quic::{QuicTransport, QuicTransportBuilder, TransportIdentity, derive_node_id_from_cert};
pub use traits::{ConnectionType, OutgoingConnection, SharedTransport, StreamPriority, Transport, TransportError, TransportResult};

pub mod iroh;
#[cfg(feature = "iroh")]
pub use iroh::IrohTransport;
