//! HTTP/JSON sidecar client and server.
//!
//! * [`client::SidecarClient`] — what the Rust harness uses to talk
//!   to an iroh-go / iroh-net sidecar over HTTP.
//! * [`server`] — the reverse channel: sidecars dial back into the
//!   Rust harness to receive gossip events the A3Net side
//!   published. Without this channel, the harness could *publish*
//!   to the sidecar (via [`client::SidecarClient::gossip_publish`])
//!   but the sidecar would have no way to surface events it
//!   observed on the bus back to the harness for assertion.

pub mod client;
pub mod server;

pub use client::SidecarClient;
pub use server::{HarnessServer, HarnessEvent};
