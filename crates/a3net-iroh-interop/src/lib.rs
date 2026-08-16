//! `a3net-iroh-interop` — A3Net ↔ iroh-go / iroh-net cross-language interop harness.
//!
//! This crate is the P1 deliverable from `AUDIT_GAP_FINAL.md`. The audit
//! table is paraphrased in the crate-level docs of each sub-module; the
//! short version:
//!
//! | Gap | What this crate does |
//! |-----|----------------------|
//! | A3Net ↔ iroh-go / iroh-net interop harness | HTTP/JSON sidecar protocol + Rust harness driver |
//! | FFI surface extension (blob push / room join / gossip sub) | The harness *exercises* these surfaces; the surfaces themselves live in `a3net-ffi` |
//!
//! ## Architecture
//!
//! ```text
//!   ┌─────────────────┐         HTTP/JSON (port 1)        ┌─────────────────┐
//!   │  Rust harness   │  ───────────────────────────▶   │  iroh-go (or    │
//!   │  (this crate)   │  ◀───────────────────────────   │   any language  │
//!   └─────────────────┘      relay/blobs/gossip wire    │   sidecar)      │
//!            │                                            └─────────────────┘
//!            │
//!            │  iroh 1.0 wire (over QUIC)
//!            ▼
//!     A3Net node (`a3net-node` crate, `iroh` feature on)
//! ```
//!
//! The two sides speak the **same iroh 1.0 wire protocol** (same
//! ALPNs, same BLAKE3-verified chunked streams, same gossip topic
//! encoding). The HTTP/JSON control plane only carries node ids,
//! tickets, and topic names — i.e. the minimum data needed for the
//! Rust harness to know what to dial or subscribe to.
//!
//! ## Why HTTP/JSON?
//!
//! * `iroh-go` doesn't ship a stable Rust-style IPC surface.
//! * stdin/stdout JSON is fragile when the sidecar logs to stderr.
//! * HTTP/JSON is language-agnostic: iroh-go (Go), iroh-py (Python),
//!   iroh-net-internal tools, even a Bash cURL harness can all be
//!   the sidecar.
//!
//! ## Crate layout
//!
//! * [`wire`] — the HTTP/JSON messages (request/response shapes).
//! * [`sidecar`] — client (Rust harness → sidecar) and server (sidecar → Rust harness) HTTP endpoints.
//! * [`driver`] — high-level orchestrator: boots an A3Net node, spawns
//!   a sidecar, runs a test scenario end-to-end.
//! * [`tests`] — Rust integration tests covering the smoke subset
//!   (blob fetch, gossip subscribe) and the comprehensive subset
//!   (DHT, IPNS, docs, relay), gated by env vars and feature flags.

#![doc(test(attr(deny(warnings))))]

pub mod wire;
pub mod sidecar;
pub mod driver;
pub mod ticket_bridge;

pub use wire::{SidecarRequest, SidecarResponse, SidecarError, BlobTicketWire, NodeAddrWire, GossipTopicWire};
pub use driver::{InteropHarness, HarnessConfig, Scenario, ScenarioReport};
pub use ticket_bridge::{IrohBlobTicket, to_a3net_parts, from_a3net_parts, parse_iroh_for_a3net, a3net_to_iroh_ticket};

/// Wire-protocol version. Bumped whenever any [`SidecarRequest`]
/// variant or any field changes shape. The sidecar must echo this
/// version in its [`SidecarResponse::version`] reply or the harness
/// will refuse to proceed.
pub const WIRE_PROTOCOL_VERSION: u32 = 1;
