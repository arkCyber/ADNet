//! `adnet-mesh-firewall` — userspace packet filter for the ADNet
//! mesh VPN.
//!
//! ## Threat model & defaults
//!
//! The firewall governs **mesh traffic only**. It sits on top of
//! the host / kernel firewall — a packet has to clear both.
//! The defaults match rayfish / Tailscale SSH: **secure by default**.
//!
//! - **Inbound** (from peer → local node): unsolicited TCP and
//!   UDP are denied. ICMP / ICMPv6 echo (ping) is allowed.
//! - **Outbound** (local node → peer): every protocol is
//!   allowed.
//! - **Conntrack**: when the local node opens a connection to a
//!   peer, the firewall records the (proto, src_port,
//!   peer_node_id) tuple and lets the peer's return packets
//!   back in for the lifetime of the connection. Entries
//!   expire after [`DEFAULT_CONN_TIMEOUT`].
//!
//! ## Rule shape
//!
//! Rules are directional (in / out) and target a (proto, port)
//! pair plus an optional peer filter:
//!
//! ```text
//! rule:     (direction, action, proto, port, peer?)
//! default:  deny TCP/UDP inbound, allow ICMP, allow all outbound
//! ```
//!
//! The order is "first match wins". A rule with
//! [`Action::Allow`] on a matching packet short-circuits the
//! engine; the inverse for [`Action::Deny`]. If no rule
//! matches, the **default policy** applies.
//!
//! ## Operational model
//!
//! The firewall is **stateless across restarts** — rules and
//! conntrack entries are not persisted to disk. Operators that
//! want declarative rules ship a YAML spec and apply it on
//! startup (see [`crate::declarative`]). The mesh stack
//! queries [`FirewallEngine::decide`] synchronously from the
//! packet path; conntrack entries are updated as a side
//! effect of outbound flows.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod conntrack;
pub mod declarative;
pub mod decision;
pub mod engine;
pub mod rule;

pub use conntrack::{
    ConnKey, ConnProto, ConnTracker, ConnTrackerConfig, InboundProbe, DEFAULT_CONN_TIMEOUT,
    MAX_CONN_ENTRIES,
};
pub use decision::{Decision, DecisionReason};
pub use engine::{DefaultPolicy, FirewallConfig, FirewallEngine, FirewallStats, Packet};
pub use rule::{
    Action, Direction, PortRangeError, PortSpec, ProtoSpec, Rule, RuleId, RuleSet, MAX_PORT,
    MAX_RULES,
};
