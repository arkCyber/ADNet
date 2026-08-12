//! `adnet-webrtc` — WebRTC DataChannel transport for ADNet.
//!
//! Symmetric: it lets a native ADNet node speak to another native ADNet node
//! over WebRTC, and lets a browser running JS speak to a native ADNet node
//! over the same path. The browser gets a small JS shim in
//! `crates/adnet-ffi/js/` that ports the Noise + Frame primitives.
//!
//! ## Why WebRTC?
//!
//! NAT traversal is hard. WebRTC ICE gives us a turnkey solution: STUN for
//! reflexive candidates, TURN for relayed, and per-peer DTLS + SRTP for
//! transport security. The browser already supports it natively; on the
//! native side, `webrtc-rs` (a Pion port) gives us a pure-Rust implementation
//! with no system OpenSSL dependency.
//!
//! ## Wire model
//!
//! Each ADNet node has a long-term Noise static key (32 bytes, Ed25519,
//! persisted via [`adnet_identity`]). When two peers meet over WebRTC:
//!
//! 1. They exchange SDP via the signaling channel (pkarr or in-process for
//!    tests). The SDP carries a fingerprint of the peer identity.
//! 2. ICE candidate exchange happens over the same channel.
//! 3. Once ICE reaches `Connected` state, a single DataChannel labeled
//!    `"adnet/0"` is opened in ordered+reliable mode.
//! 4. We run Noise_XX on top of the DC: three messages (`e`, `e, ee, s, es`,
//!    `s, se`), then both sides derive an authenticated cipher state.
//! 5. After Noise completes, the channel is bound to a [`NodeId`] derived
//!    from the remote static key and we hand off to the regular ADNet frame
//!    codec.
//!
//! Frames larger than ~16 KiB (the SCTP message-size ceiling) are routed
//! through a "chunked stream" that uses multiple DC messages with explicit
//! sequence numbers. See [`frame_codec::chunked`] (forthcoming).
//!
//! ## Feature flags
//!
//! - `default = []` — zero-cost stub. The trait surface is exported so
//!   downstream code can hold a `WebRtcConfig` without pulling the heavy
//!   deps.
//! - `webrtc` — pulls in `webrtc-rs`, `snow`, and `pkarr`. This is what
//!   production builds enable.
//! - `signaling` — only the signaling helpers + Noise handshake, no
//!   `webrtc-rs` runtime. Useful for embedders that pin their own
//!   `webrtc-rs` version.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod config;
pub mod error;

#[cfg(feature = "signaling")]
pub mod noise_dc;

#[cfg(feature = "webrtc")]
pub mod rtc_engine;

#[cfg(feature = "webrtc")]
pub mod dc_session;

#[cfg(feature = "webrtc")]
pub mod frame_codec;

#[cfg(feature = "webrtc")]
pub mod signaling;

#[cfg(test)]
mod tests;

pub use config::WebRtcConfig;
pub use error::{WebRtcError, WebRtcResult};
