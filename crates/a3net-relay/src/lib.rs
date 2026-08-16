//! `a3net-relay` — WAN relay HTTP proxy for A3Net mesh traffic.
//!
//! Ported from
//! `Exodus@src-backup/.../wan_relay.rs` and `wan_relay_server.rs`. The
//! relay forwards mesh HTTP requests through a publicly-routable host so
//! nodes behind NAT can still reach each other.
//!
//! Layout:
//! - [`config`]   — [`RelayConfig`] (persistable), [`RelayServerInfo`]
//! - [`client`]   — [`RelayClient`] which builds proxy URLs for the
//!   `/exodus-mesh/fetch?host=...&port=...&path=...` endpoint
//! - [`server`]   — [`RelayServer`] (axum-based) which validates and
//!   forwards requests
//! - [`billing`]  — Optional `billing` feature that adds signed-pledge
//!   acceptance and receipt redemption. Off by default.
//!
//! Crate pillars:
//! - Strict path validation: only `/blobs/...` paths are forwarded (no
//!   path traversal, no arbitrary proxies).
//! - Reuses [`a3net_resilience`] for retry-on-upstream-error behaviour.
//! - Surfaces iroh-style relay URL ergonomics without depending on iroh.
//! - Billing is opt-in: an operator who doesn't enable the `billing`
//!   cargo feature gets the same proxy with zero extra deps.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod billing;
pub mod client;
pub mod config;
pub mod metrics;
pub mod proxy_policy;
pub mod server;

pub use billing::BillingMode;
#[cfg(feature = "billing")]
pub use billing::BillingState;
pub use client::RelayClient;
pub use config::{RelayConfig, RelayServerInfo};
pub use proxy_policy::HostPolicy;
pub use server::{RelayServer, RelayServerHandle, ServerPolicy};
