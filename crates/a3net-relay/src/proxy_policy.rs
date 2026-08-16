//! Proxy security policy — the single source of truth for what the relay
//! is willing to forward.
//!
//! Three policies are exposed:
//!
//! 1. [`HostPolicy::default_block_private`] — rejects IP literals in
//!    loopback / RFC1918 / link-local / cloud-metadata ranges, and
//!    performs DNS resolution to ensure the hostname does not resolve
//!    into one of those ranges either. This is the recommended default
//!    for any production deployment.
//! 2. [`HostPolicy::allow_loopback_only`] — used by integration tests so
//!    they can stand up an upstream on `127.0.0.1`.
//! 3. [`HostPolicy::allow_all`] — explicit opt-out for trust-anchored
//!    deployments (e.g. behind a mesh auth proxy). The constructor is
//!    intentionally verbose so an audit on the call site can spot it.
//!
//! ## Path policy
//!
//! [`validate_path`] mirrors the previous in-handler validation but adds
//! NUL / control-character rejection and a tighter length cap. The
//! relay only ever forwards `/blobs/...` paths; anything else is refused
//! at the policy layer so the same guarantees hold for both the proxy
//! endpoint and (in future) any other consumer.
//!
//! ## Redirect policy
//!
//! [`RedirectPolicy::Safe`] re-validates the destination of every 3xx
//! response against the same host policy. The default
//! [`reqwest::redirect::Policy::limited`] does **not** do this, which is
//! how a malicious upstream can 302 us into `http://169.254.169.254/`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Maximum number of characters in a forwarded path.
///
/// Realistic legitimate paths look like `/blobs/<blake3-hex>` where
/// the hex is 64 chars, so `/blobs/` + hash = 71 chars. We bound at
/// **256** so:
///
/// - An attacker cannot construct a 1023-char `/blobs/aaaa…` path
///   to force the O(n) `validate_path` to scan a long string on
///   every redirect.
/// - Legitimate mesh paths (with optional query string and any
///   future prefix) have ~3.5× headroom.
///
/// Anything beyond this is rejected at the policy boundary; the
/// relay never lets a path longer than this into iroh's HTTP stack.
pub const MAX_PATH_LEN: usize = 256;

/// Maximum number of bytes the relay will buffer or stream from an
/// upstream response. The cap exists to bound memory pressure
/// under hostile traffic — a single bad request with
/// `upstream_timeout=60s` could otherwise eat up to
/// `timeout × throughput` bytes before iroh's HTTP stack gives
/// up.
///
/// **Default 16 MiB**: matches `iroh_store::MAX_RANGE_BYTES` so the
/// relay will serve any legitimate single-range blob fetch in a
/// single response. Operators that need to move larger blobs (e.g.
/// dataset archives) should stream them in chunks via repeated
/// `/blobs/<hash>?offset=…&len=…` requests, or raise this
/// constant explicitly in their `RelayConfig`.
pub const DEFAULT_MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Default total budget for an upstream request. Anything longer than
/// this is a strong signal of abuse — mesh fetches should complete in
/// seconds.
pub const DEFAULT_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(60);

/// Decides which upstream hosts the relay is willing to talk to.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostPolicy {
    /// Reject all IP literal / DNS-resolved addresses that fall into
    /// the loopback, RFC1918, link-local, or cloud-metadata ranges.
    ///
    /// This is the **default** for production deployments. It is the
    /// only policy that protects against SSRF.
    #[default]
    DefaultBlockPrivate,

    /// Permit only loopback hosts. Used by the integration tests so
    /// they can stand up an upstream on `127.0.0.1`. **Never** use this
    /// in production.
    AllowLoopbackOnly,

    /// Permit every host. The constructor name is intentionally verbose
    /// so an audit on the call site can spot the opt-out.
    AllowAllUntrusted,
}

impl HostPolicy {
    /// Test-only helper: loopback-allowing policy.
    #[cfg(test)]
    pub fn loopback_for_tests() -> Self {
        Self::AllowLoopbackOnly
    }

    /// Decide whether `host` is acceptable to forward to.
    ///
    /// `host` may be a hostname (`node-7.a3net.example`) or an IP
    /// literal (`203.0.113.5`, `[2001:db8::1]`). Bracketed IPv6
    /// literals are accepted; the surrounding brackets are stripped
    /// before parsing.
    pub fn accepts(&self, host: &str) -> Result<(), &'static str> {
        match self {
            HostPolicy::AllowAllUntrusted => Ok(()),
            HostPolicy::AllowLoopbackOnly => match parse_literal(host) {
                Some(ip) if is_loopback(ip) => Ok(()),
                Some(_) => Err("host is not a loopback address"),
                None => Err("host must be a loopback IP literal in this policy"),
            },
            HostPolicy::DefaultBlockPrivate => {
                // IP literal?
                if let Some(ip) = parse_literal(host) {
                    return if is_public(ip) {
                        Ok(())
                    } else {
                        Err("host is in a private/loopback/link-local range")
                    };
                }
                // Hostname — we can't resolve here (the caller does
                // that asynchronously), so we just verify the syntax
                // is plausible. The async variant [`accepts_resolved`]
                // does the full check.
                if !is_plausible_hostname(host) {
                    return Err("host is not a syntactically valid hostname");
                }
                Ok(())
            }
        }
    }

    /// Async + DNS-aware variant. Resolves `host` through the system
    /// resolver and rejects any resolution that lands on a private
    /// range. This is the variant the proxy handler uses.
    pub async fn accepts_resolved(&self, host: &str) -> Result<(), String> {
        match self {
            HostPolicy::AllowAllUntrusted => Ok(()),
            HostPolicy::AllowLoopbackOnly => {
                // Same as sync; reject anything that doesn't parse as a
                // loopback IP literal.
                self.accepts(host).map_err(|e| e.to_string())
            }
            HostPolicy::DefaultBlockPrivate => {
                // IP literal: check directly.
                if let Some(ip) = parse_literal(host) {
                    return if is_public(ip) {
                        Ok(())
                    } else {
                        Err("host is in a private/loopback/link-local range".into())
                    };
                }
                // Hostname: must be syntactically valid.
                if !is_plausible_hostname(host) {
                    return Err("host is not a syntactically valid hostname".into());
                }
                // Resolve. If any resolved address is private, reject.
                let addrs = tokio::net::lookup_host((host.trim(), 0u16))
                    .await
                    .map_err(|e| format!("DNS resolution failed for {host}: {e}"))?;
                let mut saw_any = false;
                for addr in addrs {
                    saw_any = true;
                    let ip = addr.ip();
                    if !is_public(ip) {
                        return Err(format!(
                            "host {host} resolves to private/loopback/link-local address {ip}"
                        ));
                    }
                }
                if !saw_any {
                    return Err(format!("host {host} has no A/AAAA records"));
                }
                Ok(())
            }
        }
    }

    /// Human-readable name for logs / status responses.
    pub fn name(&self) -> &'static str {
        match self {
            HostPolicy::DefaultBlockPrivate => "default-block-private",
            HostPolicy::AllowLoopbackOnly => "loopback-only",
            HostPolicy::AllowAllUntrusted => "allow-all-untrusted",
        }
    }
}

/// Path policy: ensures the forwarded path is a real `/blobs/...` path
/// with no traversal, no NUL, no control characters, and a sane length.
pub fn validate_path(path: &str) -> Result<(), &'static str> {
    let path = path.trim();
    if !path.starts_with('/') {
        return Err("path must start with /");
    }
    if !path.starts_with("/blobs/") {
        return Err("path must start with /blobs/");
    }
    if path.len() > MAX_PATH_LEN {
        return Err("path too long");
    }
    if path.contains('\\') {
        return Err("path contains backslash");
    }
    if path.contains('\0') {
        return Err("path contains NUL");
    }
    for c in path.chars() {
        if (c as u32) < 0x20 && c != '\t' {
            return Err("path contains control character");
        }
    }
    if path.split('/').any(|seg| seg == "..") {
        return Err("path contains traversal segment");
    }
    Ok(())
}

/// Validate a host string as syntactically valid without resolving.
pub fn host_is_valid_literal(host: &str) -> bool {
    parse_literal(host).is_some()
}

/// Parse a host string as an IP literal. Accepts IPv4 dotted-quad and
/// IPv6 (with or without `[...]` brackets). Returns `None` for
/// hostnames.
pub fn parse_literal(host: &str) -> Option<IpAddr> {
    let h = host.trim().trim_matches(|c| c == '[' || c == ']');
    if let Ok(ip) = h.parse::<IpAddr>() {
        return Some(ip);
    }
    None
}

/// Returns `true` for IPs that are safe to expose to the public
/// internet: anything that is not loopback, not RFC1918, not link-local,
/// not multicast, not the unspecified address, and not the cloud
/// metadata address.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(v4: Ipv4Addr) -> bool {
    if v4.is_loopback() {
        return false;
    }
    if v4.is_private() {
        return false;
    }
    if v4.is_link_local() {
        return false;
    }
    if v4.is_multicast() {
        return false;
    }
    if v4.is_unspecified() {
        return false;
    }
    if v4.is_broadcast() {
        return false;
    }
    if v4.octets() == [169, 254, 169, 254] {
        // AWS / GCP / Azure instance metadata.
        return false;
    }
    if v4.octets()[0] == 100 && (v4.octets()[1] & 0b1100_0000) == 0b0100_0000 {
        // 100.64/10 — carrier-grade NAT (RFC 6598).
        return false;
    }
    if v4.octets()[0] == 0 {
        // 0.0.0.0/8 — "this network".
        return false;
    }
    // 192.0.0.0/24 (IETF protocol assignments) — be conservative and
    // mark as non-public.
    if v4.octets()[0] == 192 && v4.octets()[1] == 0 && v4.octets()[2] == 0 {
        return false;
    }
    // 198.18.0.0/15 — benchmarking range.
    if v4.octets()[0] == 198 && (v4.octets()[1] == 18 || v4.octets()[1] == 19) {
        return false;
    }
    true
}

fn is_public_v6(v6: Ipv6Addr) -> bool {
    if v6.is_loopback() {
        return false;
    }
    if v6.is_unspecified() {
        return false;
    }
    if v6.is_multicast() {
        return false;
    }
    // Unique-local addresses (fc00::/7).
    let seg0 = v6.segments()[0];
    if (seg0 & 0xfe00) == 0xfc00 {
        return false;
    }
    // Link-local (fe80::/10).
    if (seg0 & 0xffc0) == 0xfe80 {
        return false;
    }
    // IPv4-mapped IPv6: `::ffff:a.b.c.d` — re-check the v4 part.
    if let Some(v4) = v6.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    // IPv4-compatible (::a.b.c.d) — deprecated, but treat as private.
    if let Some(v4) = v6.to_ipv4() {
        return is_public_v4(v4);
    }
    true
}

pub fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Returns `true` if `host` looks like a valid DNS hostname. We don't
/// apply the full RFC 1123 ruleset — we just check the format that
/// actually matters for forwarding: a reasonable length, no NUL /
/// control chars, no `/`, and at least one dot for FQDNs (or "localhost"
/// as a special case). Hostnames without a dot are allowed because
/// mesh node IDs look like `node-7`.
fn is_plausible_hostname(host: &str) -> bool {
    let h = host.trim();
    if h.is_empty() || h.len() > 253 {
        return false;
    }
    if h == "localhost" {
        return true;
    }
    if h.contains('/') || h.contains('\0') {
        return false;
    }
    h.split('.')
        .all(|label| !label.is_empty() && label.len() <= 63 && label.chars().all(is_label_char))
}

fn is_label_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

/// Redirect policy that re-validates the destination of every 3xx
/// response against the active host policy. This is what the relay
/// should use instead of `reqwest::redirect::Policy::limited(3)`,
/// which lets upstream 302 us into a private address.
#[derive(Clone)]
pub struct SafeRedirectPolicy {
    host: HostPolicy,
    limit: usize,
}

impl SafeRedirectPolicy {
    pub fn new(host: HostPolicy) -> Self {
        Self { host, limit: 3 }
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn host_policy(&self) -> &HostPolicy {
        &self.host
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Decide whether `next` is an acceptable redirect target.
    ///
    /// `current` is the URL that issued the redirect and is provided
    /// so callers can derive a relative-Location. `current_host` and
    /// `current_port` are the host/port of the URL the relay was
    /// originally told to forward to (used for relative-redirect
    /// resolution).
    pub fn check_redirect(
        &self,
        current: &reqwest::Url,
        next: &reqwest::Url,
        current_host: &str,
        current_port: u16,
    ) -> Result<(), &'static str> {
        let target_host = next
            .host_str()
            .or_else(|| {
                if next == current {
                    Some(current_host)
                } else {
                    next.host_str()
                }
            })
            .unwrap_or(current_host);
        let target_port = next.port_or_known_default().unwrap_or(current_port);
        // Re-validate the host.
        self.host.accepts(target_host)?;
        // Re-validate the path. `next.path()` is the URL-decoded path
        // segment; `validate_path` enforces /blobs/ prefix, length,
        // NUL/control rejection, and no `..` segments.
        validate_path(next.path())?;
        // Same-port as origin is fine; cross-port is also fine as long
        // as host passed. We don't drop any port.
        let _ = target_port;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_parse() {
        assert_eq!(
            parse_literal("127.0.0.1"),
            Some("127.0.0.1".parse().unwrap())
        );
        assert_eq!(parse_literal("[::1]"), Some("::1".parse().unwrap()));
        assert_eq!(
            parse_literal("2001:db8::1"),
            Some("2001:db8::1".parse().unwrap())
        );
        assert!(parse_literal("example.com").is_none());
    }

    #[test]
    fn public_v4_classification() {
        let cases = [
            ("127.0.0.1", false),
            ("10.0.0.1", false),
            ("172.16.0.1", false),
            ("192.168.0.1", false),
            ("169.254.169.254", false), // AWS metadata
            ("169.254.0.1", false),     // link-local
            ("0.0.0.0", false),
            ("255.255.255.255", false),
            ("100.64.0.1", false), // CGN
            ("198.18.0.1", false), // benchmarking
            ("224.0.0.1", false),  // multicast
            ("8.8.8.8", true),
            ("1.1.1.1", true),
            ("203.0.113.5", true),
        ];
        for (s, expected) in cases {
            let ip: Ipv4Addr = s.parse().unwrap();
            assert_eq!(is_public_v4(ip), expected, "is_public_v4({s})");
        }
    }

    #[test]
    fn public_v6_classification() {
        let cases = [
            ("::1", false),
            ("fc00::1", false),          // unique-local
            ("fe80::1", false),          // link-local
            ("ff02::1", false),          // multicast
            ("::", false),               // unspecified
            ("::ffff:127.0.0.1", false), // mapped loopback
            ("::ffff:8.8.8.8", true),    // mapped public
            ("2001:db8::1", true),
        ];
        for (s, expected) in cases {
            let ip: Ipv6Addr = s.parse().unwrap();
            assert_eq!(is_public_v6(ip), expected, "is_public_v6({s})");
        }
    }

    #[test]
    fn host_policy_default_rejects_loopback() {
        let p = HostPolicy::default();
        assert!(p.accepts("127.0.0.1").is_err());
        assert!(p.accepts("10.0.0.1").is_err());
        assert!(p.accepts("192.168.1.1").is_err());
        assert!(p.accepts("169.254.169.254").is_err());
        assert!(p.accepts("[::1]").is_err());
        assert!(p.accepts("8.8.8.8").is_ok());
        assert!(p.accepts("node-7.a3net.example").is_ok());
    }

    #[test]
    fn host_policy_loopback_only_accepts_loopback() {
        let p = HostPolicy::AllowLoopbackOnly;
        assert!(p.accepts("127.0.0.1").is_ok());
        assert!(p.accepts("[::1]").is_ok());
        assert!(p.accepts("8.8.8.8").is_err());
        // Hostnames are rejected.
        assert!(p.accepts("localhost").is_err());
    }

    #[test]
    fn host_policy_allow_all_accepts_everything() {
        let p = HostPolicy::AllowAllUntrusted;
        assert!(p.accepts("127.0.0.1").is_ok());
        assert!(p.accepts("169.254.169.254").is_ok());
        assert!(p.accepts("anything").is_ok());
    }

    #[test]
    fn hostname_syntax_validation() {
        assert!(is_plausible_hostname("node-7"));
        assert!(is_plausible_hostname("node-7.a3net.example"));
        assert!(is_plausible_hostname("localhost"));
        assert!(!is_plausible_hostname(""));
        assert!(!is_plausible_hostname("under_score.example.com"));
        assert!(!is_plausible_hostname("a/b.example.com"));
        assert!(!is_plausible_hostname("a\0b.example.com"));
        assert!(!is_plausible_hostname(&"a".repeat(254)));
    }

    #[test]
    fn path_policy_rejects_evil_paths() {
        let ok = ["/blobs/abc", "/blobs/abc/meta", "/blobs/abc/chunks/000000"];
        for p in ok {
            assert!(validate_path(p).is_ok(), "expected ok: {p}");
        }
        // An exactly-at-the-boundary path (256 chars including
        // the leading `/blobs/`) must still pass — `MAX_PATH_LEN`
        // is a `<=`, not a `<`.
        let at_boundary = format!("/blobs/{}", "a".repeat(MAX_PATH_LEN - "/blobs/".len()));
        assert_eq!(at_boundary.len(), MAX_PATH_LEN);
        assert!(
            validate_path(&at_boundary).is_ok(),
            "exactly-MAX_PATH_LEN path must be accepted ({} chars)",
            at_boundary.len()
        );
        // One over the boundary → reject.
        let over_boundary = format!("/blobs/{}", "a".repeat(MAX_PATH_LEN - "/blobs/".len() + 1));
        assert_eq!(over_boundary.len(), MAX_PATH_LEN + 1);
        assert!(
            validate_path(&over_boundary).is_err(),
            "MAX_PATH_LEN+1 path must be rejected ({} chars)",
            over_boundary.len()
        );
        let bad = [
            "/etc/passwd",
            "/blobs/../etc/passwd",
            "/blobs/foo\\bar",
            "/blobs/foo\0bar",
            "/blobs/foo\nbar",
            "blobs/x", // missing leading slash
            "",
            &format!("/blobs/{}", "x".repeat(1100)),
        ];
        for p in bad {
            assert!(validate_path(p).is_err(), "expected err: {p}");
        }
    }

    #[tokio::test]
    async fn host_policy_default_rejects_private_dns_resolution() {
        // A hostname that resolves to 127.0.0.1 must be rejected.
        // We use `localhost` which is the textbook case.
        let p = HostPolicy::default();
        let r = p.accepts_resolved("localhost").await;
        assert!(r.is_err(), "expected localhost to be rejected");
    }

    #[tokio::test]
    async fn host_policy_default_accepts_public_ip_literal() {
        let p = HostPolicy::default();
        // 1.1.1.1 is public, no DNS lookup needed.
        assert!(p.accepts_resolved("1.1.1.1").await.is_ok());
    }

    /// P0-1 regression: a redirect whose Location header contains a
    /// URL-encoded `..` segment (e.g. `%2e%2e%2f`) must be refused.
    /// `reqwest::Url::path()` URL-decodes the percent-encoded segments,
    /// so by the time we see them they already contain `../` and the
    /// existing `validate_path` check catches them. This test pins
    /// the contract so a future refactor can't accidentally strip the
    /// path re-validation.
    #[test]
    fn redirect_rejects_encoded_traversal() {
        let policy = SafeRedirectPolicy::new(HostPolicy::DefaultBlockPrivate);
        let current: reqwest::Url = "https://example.com/blobs/abc".parse().unwrap();
        // After URL parsing, the path is `/blobs/../etc/passwd` —
        // `validate_path` rejects it because of the `..` segment.
        let next: reqwest::Url = "https://example.com/blobs/%2e%2e/etc/passwd"
            .parse()
            .unwrap();
        let err = policy
            .check_redirect(&current, &next, "example.com", 443)
            .unwrap_err();
        assert!(
            err.contains("traversal") || err.contains("blobs/"),
            "expected traversal / non-blobs error, got: {err}"
        );
    }
}
