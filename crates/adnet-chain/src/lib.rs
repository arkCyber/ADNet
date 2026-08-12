//! `adnet-chain` — framework for a NAS server to also act as a Web3
//! public blockchain node.
//!
//! ## Motivation
//!
//! An ADNet NAS server already has spare disk, bandwidth and uptime.
//! This crate defines the shape of an *optional* second role for that
//! same node: participating in a public blockchain network (as an
//! archive/observer, a full node, or a validator).
//!
//! ## What exists today
//!
//! - [`ChainNodeConfig`] / [`ChainKind`] / [`ChainRole`] — chain-agnostic
//!   configuration types.
//! - [`ChainNode`] / [`ChainNodeHandle`] — a lifecycle seam
//!   (`start` / `status` / `shutdown`) that callers (e.g. `adnet-node`)
//!   can already build against.
//!
//! ## What this crate does NOT do (yet)
//!
//! - No concrete chain client (no EVM, no Substrate, no Bitcoin-style
//!   node). [`ChainNode::start`] returns
//!   [`ChainError::Unimplemented`](error::ChainError::Unimplemented) when
//!   `enabled = true`.
//! - No consensus, no P2P chain networking, no RPC surface.
//! - No wiring into the NAS blobstore for chain data storage, even
//!   though [`ChainNodeConfig::data_subdir`] reserves a spot for it.
//!
//! These are intentionally deferred; this crate exists to lock in the
//! framework shape (config, roles, lifecycle) ahead of the concrete
//! implementation.
//!
//! ## Layering
//!
//! ```text
//!   adnet-node (optional "chain" feature)
//!                  │
//!                  ▼
//!   adnet-chain              ←  this crate
//!                  │
//!                  ▼
//!   adnet-types (shared primitives only; no chain-specific types yet)
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod config;
pub mod error;
pub mod node;
pub mod types;

pub use config::ChainNodeConfig;
pub use error::{ChainError, ChainResult};
pub use node::{ChainNode, ChainNodeHandle};
pub use types::{ChainKind, ChainRole, ChainStatus};
