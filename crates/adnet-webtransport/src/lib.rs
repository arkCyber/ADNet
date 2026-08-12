//! `adnet-webtransport` — WebTransport (HTTP/3) transport for ADNet.
//!
//! WebTransport is browser-friendly (Chrome/Firefox/Safari) and gives us:
//! - HTTP/3 + QUIC under the hood (so the cert chain story is the same as
//!   the existing QUIC transport).
//! - True multi-stream semantics, unlike WebRTC DataChannel which is
//!   capped at ~16 concurrent channels.
//! - Fetch-style integration on the JS side, which is what most browser
//!   SDKs already speak.
//!
//! ## Wire model
//!
//! 1. The server binds an HTTPS endpoint (`https://host:port/adnet`) and
//!    accepts WebTransport sessions.
//! 2. Browsers connect with `new WebTransport(url, { headers: ... })` —
//!    the headers carry an HMAC-signed connect-token so we can rate-limit
//!    and reject DOS attempts.
//! 3. After the WebTransport session opens, the **first bidirectional
//!    stream** runs the same Noise_XX handshake as the WebRTC transport
//!    (see `adnet-webrtc::noise_dc`).
//! 4. After Noise completes, additional streams are tagged with the
//!    ADNet channel-id and carry [`Frame`](adnet_types)s.
//!
//! ## Feature flags
//!
//! - `default = []` — zero-cost; only types.
//! - `webtransport = ["dep:wtransport"]` — full runtime.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod config;
pub mod connect_token;
pub mod error;

#[cfg(feature = "webtransport")]
pub mod wt_server;

#[cfg(feature = "webtransport")]
pub mod wt_client;

#[cfg(test)]
mod tests;

pub use config::WebTransportConfig;
pub use connect_token::{ConnectToken, ConnectTokenError};
pub use error::{WebTransportError, WebTransportResult};
