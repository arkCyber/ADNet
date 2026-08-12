//! `adnet-exit-node` — gateway / exit-node routing for the
//! ADNet mesh VPN.
//!
//! ## Two roles
//!
//! - **Gateway** — a mesh member that *offers* its
//!   Internet connectivity to the rest of the mesh.
//!   `Gateway::allow` adds the local node as a gateway
//!   candidate; `Gateway::revoke` removes it. The gateway
//!   advertises its availability on the network's gossip
//!   topic so other members can pick it.
//!
//! - **Client** — a mesh member that *uses* a gateway to
//!   reach the public Internet. `Client::use_gateway`
//!   sets the active gateway; `Client::unset` clears it.
//!   The client's outbound packets — addressed to
//!   non-mesh destinations — are forwarded via the
//!   gateway instead of dropped.
//!
//! ## Routing decision
//!
//! [`Router`] is the routing-decision engine. It takes an
//! [`IpAddr`](std::net::IpAddr) (the packet's destination)
//! and returns a [`RouteAction`]: forward-to-mesh,
//! forward-to-gateway, or drop. The decision is purely a
//! function of (destination, current routing table,
//! current gateway), so it can be unit-tested without
//! real packet forwarding.
//!
//! ## What this crate does NOT do
//!
//! - It does **not** open the kernel IP forwarding switch
//!   or manipulate iptables. Those are operator
//!   responsibilities (rayfish shells out to `iptables`
//!   / `nftables` on Linux).
//! - It does **not** perform any NAT. The gateway's
//!   masquerade is the host's responsibility.
//!
//! ## Layering
//!
//! ```text
//!   adnet-tun          ← packet capture
//!   adnet-mesh-firewall ← per-packet decision
//!   adnet-exit-node     ← (this crate) gateway routing
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod bandwidth;
pub mod billing;
pub mod client;
pub mod error;
pub mod exit_handler;
pub mod gateway;
pub mod router;
pub mod transit;
pub mod transit_gossip;

pub use bandwidth::{
    BandwidthSnapshot, BandwidthStats, ClientMeter, ExitNodeMeter, GlobalBandwidthLimit,
    RateLimitConfig, RateLimitResult, TrafficDirection,
};
pub use billing::{
    BillingEngine, BillingStatus, Invoice, InvoiceStatus, LineItem, PricingModel,
    PricingTier, RateCard, UsageRecord,
};
pub use client::{Client, ClientConfig, ClientState};
#[allow(unused_imports)]
pub use error::{ExitError, ExitResult};
pub use exit_handler::{
    AsyncExitHandler, ExitEvent, ExitHandler, ExitHandlerConfig, ExitHandlerSnapshot,
    PacketAction, PacketRecord, PacketResult, TrafficKind,
};
pub use gateway::{Gateway, GatewayAdvert, GatewayState};
#[allow(unused_imports)]
pub use router::{is_mesh_address, RouteAction, Router, RouterConfig, RouterSnapshot};
#[allow(unused_imports)]
pub use transit::{
    StaticTopology, TransitCapability, TransitConfig, TransitDecision, TransitHop,
    TransitRouter, TransitSnapshot, TransitTopology,
};
#[allow(unused_imports)]
pub use transit_gossip::{GossipApplyError, GossipFederatedTopology};
