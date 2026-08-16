//! Configuration for the magic DNS resolver and DNS services.
//!
//! [`ResolverConfig`] is the single knobs object shared by both
//! the pure-name-to-IP [`Resolver`](super::Resolver) and the
//! optional [`DnsServer`](super::DnsServer).

use serde::{Deserialize, Serialize};

/// Default TTL for resolved mesh DNS records (A/AAAA).
pub const DEFAULT_DNS_TTL_SECS: u32 = 300;

/// Default upstream DNS server for non-mesh queries.
pub const DEFAULT_UPSTREAM_DNS: &str = "1.1.1.1:53";

/// Configuration for the magic DNS resolver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolverConfig {
    /// TTL (seconds) written into A/AAAA responses for
    /// resolved mesh hosts.
    pub dns_ttl_secs: u32,

    /// Upstream DNS servers used for non-mesh queries
    /// (e.g. `cloudflare.com`). Each entry is a
    /// `host:port` string.
    ///
    /// The forwarder tries each in order; the first
    /// responsive server is used. Set to `None` to disable
    /// forwarding entirely (only `.ray` / `.a3net` names
    /// will resolve).
    pub upstreams: Vec<String>,

    /// Additional TLDs that are treated the same as `.ray`.
    ///
    /// The resolver already always resolves `.ray`. Setting
    /// this field lets operators add custom TLDs (e.g.
    /// `["a3net", "mesh"]`) so that `alice.a3net` is
    /// equivalent to `alice.ray`.
    ///
    /// The TLD is matched case-insensitively against the
    /// last label of a parsed DNS name.
    #[serde(default)]
    pub extra_tlds: Vec<String>,

    /// Local IPv4 address the mesh VPN claims on the TUN
    /// interface. This is used as the source address of
    /// DNS responses injected into the TUN.
    ///
    /// Defaults to the CGNAT address derived from the local
    /// `NodeId`, but can be overridden here so that the
    /// resolver can be used without a full mesh identity
    /// (e.g. in tests).
    #[serde(skip)]
    pub local_ipv4: Option<std::net::Ipv4Addr>,
}

impl Default for ResolverConfig {
    fn default() -> Self {
        Self {
            dns_ttl_secs: DEFAULT_DNS_TTL_SECS,
            upstreams: vec![DEFAULT_UPSTREAM_DNS.to_string()],
            extra_tlds: Vec::new(),
            local_ipv4: None,
        }
    }
}

impl ResolverConfig {
    /// Set the TTL for resolved DNS records.
    pub fn with_dns_ttl(mut self, ttl_secs: u32) -> Self {
        self.dns_ttl_secs = ttl_secs;
        self
    }

    /// Replace the list of upstream DNS servers.
    pub fn with_upstreams(mut self, upstreams: Vec<String>) -> Self {
        self.upstreams = upstreams;
        self
    }

    /// Add extra TLDs treated as mesh namespace.
    pub fn with_extra_tld(mut self, tld: impl Into<String>) -> Self {
        self.extra_tlds.push(tld.into());
        self
    }

    /// Set the local mesh IPv4 address (used as source of
    /// DNS responses).
    pub fn with_local_ipv4(mut self, ip: std::net::Ipv4Addr) -> Self {
        self.local_ipv4 = Some(ip);
        self
    }

    /// Returns `true` if `tld` (case-insensitive) is a
    /// recognised mesh TLD (`.ray` or `.a3net`, or an extra TLD).
    pub fn is_mesh_tld(&self, tld: &str) -> bool {
        let tld_lower = tld.to_ascii_lowercase();
        if tld_lower == TLD_LOWER || tld_lower == "a3net" {
            return true;
        }
        self.extra_tlds
            .iter()
            .any(|t| t.to_ascii_lowercase() == tld_lower)
    }
}

/// Lowercase canonical TLD suffix ("ray").
const TLD_LOWER: &str = "ray";

/// Returns all recognised mesh TLDs (canonical only).
pub fn all_mesh_tlds() -> Vec<&'static str> {
    vec![TLD_LOWER]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ResolverConfig {
        ResolverConfig::default()
    }

    #[test]
    fn default_ttl_is_300() {
        assert_eq!(cfg().dns_ttl_secs, DEFAULT_DNS_TTL_SECS);
    }

    #[test]
    fn default_upstream_is_cloudflare() {
        assert_eq!(cfg().upstreams, vec![DEFAULT_UPSTREAM_DNS]);
    }

    #[test]
    fn is_mesh_tld_recognises_ray() {
        let c = cfg();
        assert!(c.is_mesh_tld("ray"));
        assert!(c.is_mesh_tld("RAY"));
        assert!(c.is_mesh_tld("Ray"));
    }

    #[test]
    fn is_mesh_tld_recognises_a3net() {
        let c = cfg();
        assert!(c.is_mesh_tld("a3net"));
        assert!(c.is_mesh_tld("ADNET"));
        assert!(c.is_mesh_tld("Adnet"));
    }

    #[test]
    fn is_mesh_tld_recognises_extra_tld() {
        let c = cfg().with_extra_tld("mesh");
        assert!(c.is_mesh_tld("ray"));
        assert!(c.is_mesh_tld("a3net"));
        assert!(c.is_mesh_tld("mesh"));
        assert!(c.is_mesh_tld("MESH"));
    }

    #[test]
    fn is_mesh_tld_rejects_unknown() {
        assert!(!cfg().is_mesh_tld("com"));
        assert!(!cfg().is_mesh_tld("org"));
    }

    #[test]
    fn builder_with_dns_ttl() {
        let c = cfg().with_dns_ttl(600);
        assert_eq!(c.dns_ttl_secs, 600);
    }

    #[test]
    fn builder_with_extra_tld() {
        let c = cfg()
            .with_extra_tld("mesh")
            .with_extra_tld("vpn");
        assert_eq!(c.extra_tlds, vec!["mesh", "vpn"]);
    }

    #[test]
    fn config_serde_roundtrips() {
        let mut c = ResolverConfig::default();
        c.dns_ttl_secs = 120;
        c.upstreams = vec!["8.8.8.8:53".to_string()];
        c.extra_tlds.push("mesh".to_string());
        let json = serde_json::to_string(&c).unwrap();
        let back: ResolverConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.dns_ttl_secs, 120);
        assert_eq!(back.upstreams, vec!["8.8.8.8:53"]);
        assert_eq!(back.extra_tlds, vec!["mesh"]);
    }
}
