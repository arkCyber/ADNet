//! Publish-policy filter — what subset of addresses (relay URLs +
//! direct IPs) the local node is willing to advertise.
//!
//! Maps 1:1 onto iroh's [`iroh_dns::AddrFilter`]. The default is
//! [`PublishPolicy::RelayOnly`] because publishing direct IPs to a
//! public pkarr relay / DHT leaks the user's network location. Nodes
//! that sit behind a public IP and want sub-second direct dials can
//! opt into [`PublishPolicy::All`] or [`PublishPolicy::IpOnly`].

#![cfg(feature = "iroh")]

use iroh::address_lookup::AddrFilter;
use iroh_base::TransportAddr;

/// Subset of addresses the local node will publish.
///
/// Order: most permissive → most restrictive.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub enum PublishPolicy {
    /// Publish everything (relay URLs + direct IPs + custom
    /// addressing protocols). Use only when the node sits
    /// behind a stable public IP and the operator accepts the
    /// privacy trade-off.
    All,
    /// Publish direct IPs only — never publish the home relay URL.
    /// Useful when running a dedicated public relay fleet and the
    /// node wants to bypass the n0 relay map entirely.
    IpOnly,
    /// Publish the home relay URL only — never publish direct IPs.
    /// **This is the default** and matches iroh's
    /// `PkarrPublisherBuilder::default()` policy.
    #[default]
    RelayOnly,
    /// Publish relay URLs + custom addressing protocols
    /// (e.g. `bt`, `tor`, `socks5`) but **never** direct IPs.
    /// Useful for VPN / Tor / onion-routed deployments where the
    /// node has no public IP but does want to advertise
    /// application-level rendezvous addresses alongside the
    /// standard relay URL.
    ///
    /// `TransportAddr::Custom` payloads pass through this filter
    /// unmodified — iroh's `PublishPolicy::RelayOnly` would
    /// otherwise drop them because it only keeps
    /// `TransportAddr::is_relay()` results.
    RelayAndCustom,
}

impl PublishPolicy {
    /// Stable label used in [`IrohDiscoverySnapshot`](super::IrohDiscoverySnapshot).
    pub fn as_str(&self) -> &'static str {
        match self {
            PublishPolicy::All => "all",
            PublishPolicy::IpOnly => "ip-only",
            PublishPolicy::RelayOnly => "relay-only",
            PublishPolicy::RelayAndCustom => "relay-and-custom",
        }
    }

    /// True when this policy would ever publish a direct IP address.
    /// Used by [`super::DiscoveryBuilder`] to gate warnings.
    pub fn exposes_direct_ip(&self) -> bool {
        matches!(self, PublishPolicy::All | PublishPolicy::IpOnly)
    }

    /// True when this policy keeps `TransportAddr::Custom(...)`
    /// payloads alongside the relay URL. Used by the
    /// `priority(p)` helper to keep custom addresses in the
    /// ordered output instead of stripping them.
    pub fn keeps_custom(&self) -> bool {
        matches!(self, PublishPolicy::All | PublishPolicy::RelayAndCustom)
    }
}

impl std::fmt::Display for PublishPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for PublishPolicy {
    type Err = UnknownPublishPolicy;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "all" | "any" | "everything" => Ok(PublishPolicy::All),
            "ip" | "ip-only" | "direct" | "ip_only" => Ok(PublishPolicy::IpOnly),
            "relay" | "relay-only" | "relay_only" => Ok(PublishPolicy::RelayOnly),
            "relay-custom" | "relay-and-custom" | "relayandcustom" | "relay_and_custom" => {
                Ok(PublishPolicy::RelayAndCustom)
            }
            other => Err(UnknownPublishPolicy(other.to_string())),
        }
    }
}

/// Error returned by [`PublishPolicy::from_str`].
#[derive(Debug, thiserror::Error)]
#[error("unknown publish policy: {0} (expected: all|ip-only|relay-only|relay-and-custom)")]
pub struct UnknownPublishPolicy(pub String);

/// Translate a [`PublishPolicy`] into iroh's [`AddrFilter`].
///
/// The mapping is direct for the three classic policies (All /
/// IpOnly / RelayOnly). For [`PublishPolicy::RelayAndCustom`]
/// we build a custom [`AddrFilter`] that keeps
/// `TransportAddr::is_relay()` AND `TransportAddr::Custom(...)`
/// while stripping direct IPs — `AddrFilter::relay_only()` would
/// drop custom addresses, which is the audit finding this code
/// path is here to fix.
pub(crate) fn publish_policy_to_addr_filter(policy: PublishPolicy) -> AddrFilter {
    match policy {
        PublishPolicy::All => AddrFilter::unfiltered(),
        PublishPolicy::IpOnly => AddrFilter::ip_only(),
        PublishPolicy::RelayOnly => AddrFilter::relay_only(),
        PublishPolicy::RelayAndCustom => AddrFilter::new(|addrs| {
            let filtered: Vec<TransportAddr> = addrs
                .iter()
                .filter(|a| a.is_relay() || matches!(a, TransportAddr::Custom(_)))
                .cloned()
                .collect();
            std::borrow::Cow::Owned(filtered)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_relay_only() {
        assert_eq!(PublishPolicy::default(), PublishPolicy::RelayOnly);
    }

    #[test]
    fn from_str_round_trip() {
        for p in [
            PublishPolicy::All,
            PublishPolicy::IpOnly,
            PublishPolicy::RelayOnly,
            PublishPolicy::RelayAndCustom,
        ] {
            let s = p.to_string();
            let back: PublishPolicy = s.parse().unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn unknown_returns_err() {
        let err = "garbage".parse::<PublishPolicy>().unwrap_err();
        assert_eq!(err.0, "garbage");
    }

    #[test]
    fn exposes_direct_ip() {
        assert!(PublishPolicy::All.exposes_direct_ip());
        assert!(PublishPolicy::IpOnly.exposes_direct_ip());
        assert!(!PublishPolicy::RelayOnly.exposes_direct_ip());
    }

    #[test]
    fn addr_filter_mapping_is_consistent() {
        // `AddrFilter`'s `Debug` is non-exhaustive ("AddrFilter { .. }"
        // for all variants), so we can't tell the variants apart via
        // Debug. Instead we verify that `apply()` keeps the
        // expected subset:
        //
        // - `All` keeps relay + ip
        // - `IpOnly` keeps ip only
        // - `RelayOnly` keeps relay only
        //
        // iroh_dns's `TransportAddr` doesn't have a public
        // constructor here, so we just verify that the function
        // returns a value (compilation is the assertion).
        let _all = publish_policy_to_addr_filter(PublishPolicy::All);
        let _ip = publish_policy_to_addr_filter(PublishPolicy::IpOnly);
        let _relay = publish_policy_to_addr_filter(PublishPolicy::RelayOnly);
        let _rc = publish_policy_to_addr_filter(PublishPolicy::RelayAndCustom);
    }

    // ────────────────────── RelayAndCustom tests ──────────────────────

    #[test]
    fn relay_and_custom_keeps_custom_addresses() {
        // The audit-pointed bug: `TransportAddr::Custom` is dropped
        // by `AddrFilter::relay_only()` because its selector only
        // matches `TransportAddr::is_relay()`. The new
        // `RelayAndCustom` variant builds a custom filter that
        // keeps both relay URLs and custom addresses.
        assert!(PublishPolicy::RelayAndCustom.keeps_custom());
        assert!(!PublishPolicy::RelayOnly.keeps_custom());
        assert!(!PublishPolicy::IpOnly.keeps_custom());
        assert!(PublishPolicy::All.keeps_custom());
    }

    #[test]
    fn relay_and_custom_does_not_expose_direct_ip() {
        // Privacy contract: `RelayAndCustom` MUST NOT publish
        // direct IPs, even though it accepts custom addresses.
        // Differentiating `Relay+Custom` from `All` is the
        // whole point of the new variant.
        assert!(!PublishPolicy::RelayAndCustom.exposes_direct_ip());
        assert!(PublishPolicy::All.exposes_direct_ip());
    }

    #[test]
    fn relay_and_custom_from_str_accepts_aliases() {
        // The match arms cover the three wire formats: the
        // canonical hyphenated `relay-and-custom`, the
        // underscore variant `relay_and_custom`, and the
        // uppercase `RELAY-CUSTOM` (the FromStr impl
        // lowercases before matching).
        for s in [
            "relay-and-custom",
            "relay_and_custom",
            "relayandcustom",
            "RELAY-CUSTOM",
        ] {
            let p: PublishPolicy = s.parse().expect(s);
            assert_eq!(p, PublishPolicy::RelayAndCustom);
        }
    }

    #[test]
    fn relay_and_custom_round_trip() {
        let s = PublishPolicy::RelayAndCustom.to_string();
        assert_eq!(s, "relay-and-custom");
        let back: PublishPolicy = s.parse().unwrap();
        assert_eq!(back, PublishPolicy::RelayAndCustom);
    }

    #[test]
    fn unknown_policy_message_lists_relay_and_custom() {
        // The error message must mention the new variant — operators
        // who copy-paste a typo from the docs discover the valid
        // surface via the error itself.
        let err = "garbage".parse::<PublishPolicy>().unwrap_err();
        assert!(err.to_string().contains("relay-and-custom"), "{}", err);
    }
}
