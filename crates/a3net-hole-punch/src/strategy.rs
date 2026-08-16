//! Hole-punch strategies — the actual "how do we find a peer?"
//! implementations.
//!
//! Two pieces:
//!
//! 1. [`HolePunchStrategy`] — a tagged enum that names which
//!    backend the planner should spawn. The built-in variants are
//!    [`Ticket`](HolePunchStrategy::Ticket),
//!    [`Mdns`](HolePunchStrategy::Mdns),
//!    [`MainlineDht`](HolePunchStrategy::MainlineDht),
//!    [`PkarrDns`](HolePunchStrategy::PkarrDns), and
//!    [`Custom`](HolePunchStrategy::Custom). The enum is what
//!    `HolePunchConfig` stores.
//!
//! 2. [`HolePunchResolver`] — the trait every strategy implements
//!    so the planner can run them uniformly. Each strategy carries
//!    its own `resolve(...)` future that the planner awaits
//!    concurrently with the others.
//!
//! ## Custom backends
//!
//! Operators that want to plug their own backend (e.g. a private
//! Redis rendezvous, a SCION address broker) implement
//! [`HolePunchResolver`] and register it via either
//! [`HolePunchStrategy::Custom`] (single record) or
//! [`HolePunchConfig::with_extra_resolver`](super::config::HolePunchConfig::with_extra_resolver)
//! (extra list). The planner treats both paths identically.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use a3net_types::NodeId;

use crate::error::{HolePunchError, HolePunchResult};

/// The "thing the planner wants to know" — one specific peer's
/// addressing information. Mirrors iroh's `EndpointAddr` shape
/// (NodeId + relay URLs + direct addresses) without taking a hard
/// dependency on iroh.
///
/// The planner doesn't care about the wire-level iroh types
/// because the resolver is allowed to return whatever struct it
/// has on hand — the iroh 1.0 bridge lives in `iroh_bridge.rs` and
/// is the only place that owns the iroh-type conversion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedEndpoint {
    /// The peer's NodeId. The planner validates this matches the
    /// request's `target`; resolvers MUST return the same NodeId
    /// they were asked to resolve (some Pkarr relay implementations
    /// silently rewrite the id during read; we reject that).
    pub node_id: NodeId,
    /// Relay URLs the peer is reachable at. Empty when the peer
    /// is direct-only.
    #[serde(default)]
    pub relay_urls: Vec<String>,
    /// Direct IP addresses (host + port) the peer is reachable at.
    /// Empty when the peer is relay-only.
    #[serde(default)]
    pub direct_addresses: Vec<DirectAddress>,
    /// Optional raw user-data payload (the same `user-data=` TXT
    /// attribute that Pkarr carries). Lets the operator attach
    /// application-layer metadata (node role, version tag) to the
    /// resolution result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_data: Option<String>,
}

impl ResolvedEndpoint {
    /// Empty placeholder — used by resolvers that need to allocate
    /// the type before falling through to a real answer. The
    /// planner treats endpoints with no relays and no direct
    /// addresses as "no addresses" and falls through to the next
    /// strategy.
    pub fn empty(node_id: NodeId) -> Self {
        Self {
            node_id,
            relay_urls: Vec::new(),
            direct_addresses: Vec::new(),
            user_data: None,
        }
    }

    /// `true` when at least one relay URL or direct address is
    /// present. The planner stops racing once a winning strategy
    /// returns a non-empty endpoint.
    pub fn has_any_address(&self) -> bool {
        !self.relay_urls.is_empty() || !self.direct_addresses.is_empty()
    }

    /// Number of addresses the endpoint carries (relay + direct).
    pub fn address_count(&self) -> usize {
        self.relay_urls.len() + self.direct_addresses.len()
    }
}

/// A simple `(host, port)` pair — the same wire format that iroh
/// uses for `TransportAddr::Ip`. We keep our own type so the
/// planner compiles without the iroh feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectAddress {
    pub host: String,
    pub port: u16,
}

impl DirectAddress {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

impl std::fmt::Display for DirectAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

/// Capabilities a resolver advertises. The planner uses this to
/// short-circuit impossible resolution paths (e.g. no reason to
/// race an mDNS lookup against a non-LAN target).
///
/// Resolvers that support ALL capabilities should return
/// [`ResolverCapabilities::all`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResolverCapabilities {
    /// True when the resolver can return addresses for a target
    /// that is on the same L2 segment as the local node (LAN).
    /// mDNS is the canonical example.
    pub lan: bool,
    /// True when the resolver can return addresses for a target
    /// anywhere on the public internet (i.e. via a relay or
    /// signed-pkarr relay). Pkarr/DNS is the canonical example.
    pub wan: bool,
    /// True when the resolver requires **no** prior
    /// knowledge of the target (i.e. the resolver canonicalises
    /// from scratch). Mainline DHT is the canonical example.
    pub zero_knowledge: bool,
    /// True when the resolver's address book is owned by the
    /// local node (tickets, gossip). MemoryLookup is the canonical
    /// example.
    pub out_of_band: bool,
}

impl ResolverCapabilities {
    /// Firewall-on convenience: a resolver that covers LAN +
    /// WAN + zero-knowledge + out-of-band. Used by the default
    /// strategies that genuinely cover everything.
    pub const fn all() -> Self {
        Self {
            lan: true,
            wan: true,
            zero_knowledge: true,
            out_of_band: true,
        }
    }

    /// Capabilities the stock `PkarrDns` resolver advertises.
    pub const fn pkarr_dns() -> Self {
        Self {
            lan: false,
            wan: true,
            zero_knowledge: true,
            out_of_band: false,
        }
    }

    /// Capabilities the stock `Mdns` resolver advertises.
    pub const fn mdns() -> Self {
        Self {
            lan: true,
            wan: false,
            zero_knowledge: false,
            out_of_band: false,
        }
    }

    /// Capabilities the stock `MainlineDht` resolver advertises.
    pub const fn mainline_dht() -> Self {
        Self {
            lan: false,
            wan: true,
            zero_knowledge: true,
            out_of_band: false,
        }
    }

    /// Capabilities the stock `Ticket` resolver advertises.
    pub const fn ticket() -> Self {
        Self {
            lan: false,
            wan: false,
            zero_knowledge: false,
            out_of_band: true,
        }
    }
}

/// A single configurable hole-punch strategy.
///
/// The variants are the "names" an operator sees in the config;
/// the actual implementations live in the planner's body. Custom
/// resolvers are wrapped in `Arc<dyn HolePunchResolver>` so the
/// planner can store them in a uniform `Vec`.
#[derive(Debug, Clone)]
pub enum HolePunchStrategy {
    /// Out-of-band ticket (e.g. peer shared via WeChat). The
    /// planner's `Ticket` resolver is backed by an in-memory
    /// address book loaded by the operator at start-up.
    Ticket,
    /// LAN-only mDNS discovery. The `Mdns` resolver wraps
    /// `iroh-mdns-address-lookup` and only finds peers on the same
    /// L2 segment.
    Mdns,
    /// BitTorrent Mainline DHT. The resolver asks the `pkarr`
    /// crate's DHT backend to locate the target's signed packet.
    MainlineDht,
    /// n0 DNS / Pkarr relay. The resolver asks `dns.iroh.link`
    /// for the target's packet and falls back to plain DNS.
    PkarrDns,
    /// Operator-supplied resolver. The planner invokes the inner
    /// `HolePunchResolver` together with the built-in strategies.
    /// Capabilities are advertised by the resolver itself — the
    /// planner doesn't override them.
    Custom(Arc<dyn HolePunchResolver>),
}

impl HolePunchStrategy {
    /// Stable label for logs and `/discovery` output. Mirrors the
    /// naming convention used by iroh's own
    /// `MemoryLookup::PROVENANCE` / `MainlineLookup::PROVENANCE`.
    pub fn label(&self) -> &'static str {
        match self {
            HolePunchStrategy::Ticket => "a3net-ticket",
            HolePunchStrategy::Mdns => "a3net-mdns",
            HolePunchStrategy::MainlineDht => "a3net-mainline-dht",
            HolePunchStrategy::PkarrDns => "a3net-pkarr-dns",
            HolePunchStrategy::Custom(r) => r.label(),
        }
    }

    /// Capabilities this strategy advertises. The planner exposes
    /// the same value on the per-attempt outcome so the
    /// operator can see "this strategy can't reach LAN" without
    /// reading the resolver source.
    pub fn capabilities(&self) -> ResolverCapabilities {
        match self {
            HolePunchStrategy::Ticket => ResolverCapabilities::ticket(),
            HolePunchStrategy::Mdns => ResolverCapabilities::mdns(),
            HolePunchStrategy::MainlineDht => ResolverCapabilities::mainline_dht(),
            HolePunchStrategy::PkarrDns => ResolverCapabilities::pkarr_dns(),
            HolePunchStrategy::Custom(r) => r.capabilities(),
        }
    }

    /// Downcast to a `Custom` resolver, if any. Used by the
    /// planner when it needs the concrete type to forward
    /// diagnostics; downstream callers can ignore this.
    pub fn as_custom(&self) -> Option<&Arc<dyn HolePunchResolver>> {
        match self {
            HolePunchStrategy::Custom(r) => Some(r),
            _ => None,
        }
    }

    /// True when this strategy is a `Custom` variant.
    pub fn is_custom(&self) -> bool {
        matches!(self, HolePunchStrategy::Custom(_))
    }

    /// Cheap equality test for the non-Custom variants. Useful for
    /// config-dedup logic (e.g. "don't enable PkarrDns twice").
    pub fn same_kind(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (HolePunchStrategy::Ticket, HolePunchStrategy::Ticket)
                | (HolePunchStrategy::Mdns, HolePunchStrategy::Mdns)
                | (HolePunchStrategy::MainlineDht, HolePunchStrategy::MainlineDht)
                | (HolePunchStrategy::PkarrDns, HolePunchStrategy::PkarrDns)
        )
    }
}

/// The trait every strategy must implement.
///
/// The planner spawns one tokio task per strategy and `await`s the
/// returned future. The future should abort as soon as
/// [`tokio::sync::oneshot::Receiver`] lane is closed (the planner
/// uses a `tokio::sync::Notify` race to cancel siblings when a
/// winner surfaces).
#[async_trait]
pub trait HolePunchResolver: Send + Sync + std::fmt::Debug + 'static {
    /// Resolve `target` to a `ResolvedEndpoint`. The future must
    /// respect the supplied `budget` — when the budget elapses the
    /// planner cancels the task via an abort channel, but the
    /// resolver is also expected to surface a result-or-error
    /// promptly so the cancellation is a fast no-op rather than a
    /// hard kill.
    ///
    /// `cancel` is the planner's `tokio::sync::Notify` handle —
    /// the resolver's future should `tokio::select!` on it so the
    /// race is cooperative rather than reliant on the abort
    /// channel.
    async fn resolve(
        &self,
        target: NodeId,
        budget: Duration,
        cancel: Arc<tokio::sync::Notify>,
    ) -> HolePunchResult<ResolvedEndpoint>;

    /// Stable label for telemetry. The planner uses this as the
    /// `provenance` field on every per-attempt outcome and as the
    /// `strategy` field on the chosen winner.
    fn label(&self) -> &'static str;

    /// What the resolver can and cannot do. The planner uses this
    /// to short-circuit impossible branches (e.g. skipping mDNS
    /// when the target is not known to be on the LAN).
    fn capabilities(&self) -> ResolverCapabilities {
        ResolverCapabilities::all()
    }
}

/// Convenience alias for `Arc<dyn HolePunchResolver>`. Used by
/// `HolePunchStrategy::Custom` and by
/// `HolePunchConfig::with_extra_resolver`.
pub type CustomResolver = Arc<dyn HolePunchResolver>;

/// A resolver that intentionally returns an empty `ResolvedEndpoint`
/// for the built-in `HolePunchStrategy` variants that haven't been
/// filled in by the `iroh_bridge` module.
///
/// The planner spawns one of these per built-in strategy so the
/// race loop has *something* to await. When the `iroh` feature is
/// enabled, `iroh_bridge` exposes replacement resolvers
/// (`TicketResolver`, `MdnsResolver`, `MainlineDhtResolver`,
/// `PkarrDnsResolver`) that provide real answers; the planner
/// still uses these as the default strategies, but operators that
/// want the real resolver should mount them via
/// `HolePunchStrategy::Custom(...)`.
///
/// The hollow resolver never errors — it always returns
/// `Ok(ResolvedEndpoint::empty(target))` — so the planner sees
/// every built-in strategy as an "empty" attempt rather than a
/// hard failure. This keeps the diagnostics counters honest
/// (no spurious `errors` from unpiped variants).
#[derive(Debug, Clone)]
pub struct HollowResolver {
    label: &'static str,
    capabilities: ResolverCapabilities,
}

impl HollowResolver {
    /// Build a hollow resolver that mirrors the matching
    /// built-in strategy's provenance labels + capabilities.
    /// The returned resolver is `Clone` so the planner can hold
    /// it cheaply.
    pub fn from_strategy(strategy: &HolePunchStrategy) -> Self {
        match strategy {
            HolePunchStrategy::Ticket => Self {
                label: "a3net-ticket",
                capabilities: ResolverCapabilities::ticket(),
            },
            HolePunchStrategy::Mdns => Self {
                label: "a3net-mdns",
                capabilities: ResolverCapabilities::mdns(),
            },
            HolePunchStrategy::MainlineDht => Self {
                label: "a3net-mainline-dht",
                capabilities: ResolverCapabilities::mainline_dht(),
            },
            HolePunchStrategy::PkarrDns => Self {
                label: "a3net-pkarr-dns",
                capabilities: ResolverCapabilities::pkarr_dns(),
            },
            HolePunchStrategy::Custom(_) => Self {
                // A Custom strategy should never reach the
                // hollow resolver — the planner routes Custom
                // through `Arc::clone(r)` directly. We use
                // the same fallback label so the planner
                // doesn't crash if a future refactor
                // accidentally routes Custom through here.
                label: "a3net-custom",
                capabilities: ResolverCapabilities::all(),
            },
        }
    }
}

#[async_trait]
impl HolePunchResolver for HollowResolver {
    async fn resolve(
        &self,
        target: NodeId,
        _budget: Duration,
        _cancel: Arc<Notify>,
    ) -> HolePunchResult<ResolvedEndpoint> {
        // The hollow resolver never errors and never hits —
        // it simply surfaces the target with no addresses so
        // the planner can record it as an empty attempt.
        Ok(ResolvedEndpoint::empty(target))
    }

    fn label(&self) -> &'static str {
        self.label
    }

    fn capabilities(&self) -> ResolverCapabilities {
        self.capabilities
    }
}

/// Marker error returned by `HolePunchResolver::resolve` when
/// the planner cancels the in-flight task via the shared
/// `Notify`. The planner treats this as a `Cancelled` attempt
/// outcome rather than a hard error.
pub fn cancellation_marker() -> HolePunchError {
    HolePunchError::Cancelled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_endpoint_has_any_address_distinguishes_empty() {
        let id = NodeId::from_bytes(&[0u8; 32]).expect("32 bytes is valid");
        let empty = ResolvedEndpoint::empty(id.clone());
        assert!(!empty.has_any_address());
        assert_eq!(empty.address_count(), 0);

        let mut with_relay = empty.clone();
        with_relay.relay_urls.push("https://relay.iroh.link".into());
        assert!(with_relay.has_any_address());
        assert_eq!(with_relay.address_count(), 1);

        let mut with_direct = empty;
        with_direct
            .direct_addresses
            .push(DirectAddress::new("192.168.1.5", 42000));
        assert!(with_direct.has_any_address());
        assert_eq!(with_direct.address_count(), 1);
    }

    #[test]
    fn direct_address_display() {
        let a = DirectAddress::new("203.0.113.4", 7777);
        assert_eq!(a.to_string(), "203.0.113.4:7777");
    }

    #[test]
    fn strategy_labels_are_stable() {
        assert_eq!(HolePunchStrategy::Ticket.label(), "a3net-ticket");
        assert_eq!(HolePunchStrategy::Mdns.label(), "a3net-mdns");
        assert_eq!(
            HolePunchStrategy::MainlineDht.label(),
            "a3net-mainline-dht"
        );
        assert_eq!(HolePunchStrategy::PkarrDns.label(), "a3net-pkarr-dns");
    }

    #[test]
    fn strategy_capabilities_match_expectations() {
        let t = HolePunchStrategy::Ticket.capabilities();
        assert!(t.out_of_band);
        assert!(!t.lan);
        assert!(!t.wan);

        let m = HolePunchStrategy::Mdns.capabilities();
        assert!(m.lan);
        assert!(!m.wan);

        let d = HolePunchStrategy::MainlineDht.capabilities();
        assert!(d.wan);
        assert!(d.zero_knowledge);

        let p = HolePunchStrategy::PkarrDns.capabilities();
        assert!(p.wan);
        assert!(p.zero_knowledge);
        assert!(!p.out_of_band);
    }

    #[test]
    fn same_kind_distinguishes_built_in_variants() {
        assert!(HolePunchStrategy::Ticket.same_kind(&HolePunchStrategy::Ticket));
        assert!(HolePunchStrategy::Mdns.same_kind(&HolePunchStrategy::Mdns));
        assert!(!HolePunchStrategy::Ticket.same_kind(&HolePunchStrategy::Mdns));
    }
}
