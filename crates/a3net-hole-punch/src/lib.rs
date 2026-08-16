//! `a3net-hole-punch` — pluggable hole-punching / address-discovery
//! orchestrator for iroh-based A3Net peers.
//!
//! Two iroh nodes that have never talked before must agree on an
//! `EndpointAddr` before they can exchange a single UDP packet.
//! iroh ships several address-discovery backends — `PkarrPublisher`,
//! `PkarrResolver`, `DnsAddressLookup`, mDNS, Mainline DHT — and
//! A3Net layers an in-memory `MemoryLookup` on top for
//! out-of-band tickets. Operators want one knob to say "any of
//! these is fine" instead of juggling five `EndpointBuilder`
//! annotations.
//!
//! This crate is that knob. It exposes a single
//! [`HolePunchPlanner`](planner::HolePunchPlanner) that races every
//! configured [`HolePunchStrategy`](strategy::HolePunchStrategy)
//! against the same target and returns the first non-empty
//! `EndpointAddr`.
//!
//! ## Strategies
//!
//! | Strategy | Source | Default on | Privacy cost |
//! |----------|--------|------------|--------------|
//! | [`Ticket`](strategy::HolePunchStrategy::Ticket) | out-of-band peer ticket (e.g. peer shared via WeChat) | opt-in | none |
//! | [`Mdns`](strategy::HolePunchStrategy::Mdns) | LAN multicast (`_a3net._udp.local`) | opt-in | none — LAN only |
//! | [`MainlineDht`](strategy::HolePunchStrategy::MainlineDht) | BitTorrent Mainline DHT (`pkarr` signed packet) | opt-in | leaks node id to DHT |
//! | [`PkarrDns`](strategy::HolePunchStrategy::PkarrDns) | `dns.iroh.link` Pkarr relay + DNS | **on** | leaks relay URL + node id |
//! | [`Custom`](strategy::HolePunchStrategy::Custom) | user-supplied [`HolePunchResolver`](strategy::HolePunchResolver) | opt-in | depends |
//!
//! ## Default
//!
//! [`HolePunchConfig::default()`](config::HolePunchConfig::default) is
//! **DNS-first** (`PkarrDns`) to keep stock iroh behaviour: a fresh
//! node with no tickets, no LAN, and no DHT can still find other
//! peers via the public `dns.iroh.link` relay. Operators can
//! downgrade to [`HolePunchConfig::dns_only()`](config::HolePunchConfig::dns_only),
//! or upgrade to
//! [`HolePunchConfig::air_gapped()`](config::HolePunchConfig::air_gapped)
//! (LAN + tickets only) for forensic deployments.
//!
//! ## Race semantics
//!
//! Every configured strategy is launched in parallel. The planner
//! returns the first `EndpointAddr` that resolves to a non-empty
//! address set. Strategies that haven't completed are cancelled
//! (`AbortHandle::abort`) when the winner surfaces, so the planner
//! never wastes work past the first hit. The full per-attempt
//! outcome is captured in [`HolePunchOutcome`](diagnostics::HolePunchOutcome)
//! for `/discovery` admin output.
//!
//! ## Wiring into iroh
//!
//! With the `iroh` feature enabled, [`iroh_bridge::IrohLookupAdapter`]
//! adapts the planner into iroh's `AddressLookup` trait so a bound
//! `Endpoint` reads the planner as a single discovery service.
//! Without the feature, callers embed the planner directly in their
//! higher-level orchestration (see `examples/builder.rs`).
//!
//! ## Why a separate crate?
//!
//! `a3net-nat-traversal` already exists for STUN / TURN / UPnP /
//! UDP hole-punching — the **layer-3** half of NAT traversal.
//! `a3net-hole-punch` is the **layer-7** half: how the peers learn
//! each other's addressing information in the first place. The two
//! crates compose: `a3net-nat-traversal` handles the on-the-wire
//! NAT punching once we know where to punch, and
//! `a3net-hole-punch` handles "how do we know where to punch".
//!
//! ## Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use a3net_hole_punch::{
//!     config::HolePunchConfig, strategy::HolePunchStrategy, planner::HolePunchPlanner,
//! };
//! use a3net_types::NodeId;
//!
//! # async fn demo(target: NodeId) -> anyhow::Result<()> {
//! let cfg = HolePunchConfig::default(); // DNS-first
//! let planner = HolePunchPlanner::new(cfg);
//!
//! let outcome = planner.punch(target).await?;
//! if let Some(addr) = outcome.into_endpoint_addr() {
//!     println!("reached via {:?}: {:?}", addr, outcome.winning_strategy());
//! }
//! # Ok(()) }
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod config;
pub mod diagnostics;
pub mod error;
pub mod planner;
pub mod strategy;

#[cfg(feature = "iroh")]
pub mod iroh_bridge;

// Re-exports — keep the public surface flat so callers can write
// `a3net_hole_punch::HolePunchStrategy` without remembering the
// inner module layout.
pub use config::{HolePunchConfig, HolePunchConfigBuilder, PunchOrdering, TimeoutPolicy};
pub use diagnostics::{HolePunchDiagnostics, HolePunchOutcome, StrategyAttempt};
pub use error::{HolePunchError, HolePunchResult};
pub use planner::HolePunchPlanner;
pub use strategy::{
    CustomResolver, HolePunchResolver, HolePunchStrategy, ResolverCapabilities, ResolvedEndpoint,
};
