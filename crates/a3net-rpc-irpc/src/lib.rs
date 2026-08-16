//! `a3net-rpc-irpc` — **EXPERIMENTAL** draft of an irpc-based interface
//! mirroring the IPFS-compatible command surface in `a3net-rpc`.
//!
//! # ⚠️ Status
//!
//! This crate is a **design draft, not a production path**:
//!
//! - It is **deliberately not a member of the A3Net workspace** (see
//!   `Cargo.toml` for the reasoning). `cargo check` at the workspace
//!   root will not touch it.
//! - `irpc` itself is pre-1.0 (currently 0.17.0). The crate is pinned
//!   via `irpc = "=0.17.0"`. Bumping requires reading the irpc
//!   CHANGELOG; breaking API changes are routine.
//! - The `default = ["local"]` feature keeps the dependency graph to
//!   `irpc` + `tokio` + `serde`. The `remote` feature would re-pull
//!   `noq`, `postcard`, `n0-future`, `n0-error`, and the full transitive
//!   closure — do not enable it without a deliberate decision.
//!
//! # What it is
//!
//! - A small set of pure-Rust trait / type definitions that document
//!   how the `a3net-rpc` command surface *would* look if expressed as
//!   an [`irpc`] protocol.
//! - A runnable `echo` example that exercises every channel shape used
//!   by the protocol (oneshot, tx-streaming, rx-streaming). It does
//!   *not* open a network connection — the goal is to keep the crate
//!   self-contained for review.
//! - A long-running experiment tracker. See `README.md`'s "Tracking
//!   checklist" section.
//!
//! # What it is not
//!
//! - **Not** a drop-in replacement for `a3net-rpc`. irpc transport is
//!   noq-only (QUIC); a3net-rpc also targets HTTP API / WebSocket /
//!   stdio for the CLI. FFI clients (`a3net-ffi`, `a3net-ffi-js`)
//!   cannot consume irpc — irpc explicitly does not target cross-
//!   language interop.
//! - **Not** a dependency of any other workspace member.
//! - **Not** a stable public API. Types and the protocol enum can and
//!   will change as irpc stabilises.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]
#![warn(unexpected_cfgs)]

pub mod service;

// Re-export irpc-derive's macro re-export + the irpc core at one
// level, so downstream callers can `use a3net_rpc_irpc::irpc::*`
// rather than pulling in `irpc` themselves.
pub use ::irpc;

// Result and Protocol types are reachable via the submodule only;
// we deliberately do not flatten them into the crate root because
// the names (`Protocol`, `GcStats`) would collide with their
// counterparts in `a3net-rpc::results` if both were ever linked
// into the same binary.
pub use service::{
    AdnetRpcMessage, BlockResult, BlockRmResult, BlockStat, DagResult, GcStats, NodeInfo,
    PinAddResult, PinLsResult, PinRmResult, Protocol,
};
