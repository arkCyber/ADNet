//! DNSLink — resolve `/ipns/<domain>` names via the special
//! `_dnslink.<domain>` TXT record.
//!
//! DNSLink is the IPFS convention for mapping human-friendly DNS
//! names to immutable CIDs without going through the IPNS
//! publisher/record flow. A domain owner publishes a TXT record of
//! the form:
//!
//! ```text
//! _dnslink.example.com.  60  IN  TXT  "dnslink=/ipfs/bafy..."
//! ```
//!
//! Resolvers fetch the TXT record, validate the `dnslink=` prefix,
//! and surface the path string. The path can be `/ipfs/<cid>` or
//! `/ipns/<other-name>`; consumers follow it as usual.
//!
//! This module provides an in-memory [`DnsLinkResolver`] that
//! surfaces the same content path the IPFS spec mandates. It does
//! **not** make real DNS queries — embedding the standard library's
//! sync `ToSendHesiod` resolver would block the runtime and pull
//! platform-specific dependencies. Callers that need a real network
//! lookup should plug their preferred resolver (e.g. `hickory-resolver`)
//! in via [`DnsLinkResolver::with_lookup`].

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Trait for plugging in an actual DNS resolver.
pub trait DnsLookup: Send + Sync + std::fmt::Debug {
    /// Return all TXT records for `_dnslink.<domain>`. An empty
    /// vec means the record does not exist.
    fn lookup_txt(&self, fqdn: &str) -> Vec<String>;

    /// Upcast hook used by [`DnsLinkResolver::in_memory`] to recover
    /// an `Arc<InMemoryLookup>` when the underlying trait object is
    /// in fact one. Implementations that are not in-memory return
    /// `None` so the resolver doesn't lose track of the original
    /// `Arc<dyn DnsLookup>`.
    fn as_in_memory(&self) -> Option<InMemoryLookup> {
        None
    }
}

/// Lookup that just consults an in-memory map. Used by tests and
/// as a default for callers that don't need network DNS.
#[derive(Debug, Default, Clone)]
pub struct InMemoryLookup {
    inner: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl InMemoryLookup {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a TXT record under a fully-qualified domain name.
    pub fn insert(&self, fqdn: impl Into<String>, records: Vec<String>) {
        self.inner.write().unwrap().insert(fqdn.into(), records);
    }

    /// Insert a single DNSLink record for a domain. Equivalent to
    /// `insert("_dnslink.<domain>", vec![format!("dnslink={path}")])`.
    pub fn insert_dnslink(&self, domain: &str, path: &str) {
        let fqdn = dnslink_fqdn(domain);
        self.insert(fqdn, vec![format!("dnslink={path}")]);
    }
}

impl DnsLookup for InMemoryLookup {
    fn lookup_txt(&self, fqdn: &str) -> Vec<String> {
        self.inner
            .read()
            .unwrap()
            .get(fqdn)
            .cloned()
            .unwrap_or_default()
    }

    fn as_in_memory(&self) -> Option<InMemoryLookup> {
        Some(self.clone())
    }
}

/// DNSLink resolver — resolves `/ipns/<domain>` paths by
/// consulting `_dnslink.<domain>` TXT records.
#[derive(Debug, Clone)]
pub struct DnsLinkResolver {
    lookup: Arc<dyn DnsLookup>,
}

impl DnsLinkResolver {
    pub fn new() -> Self {
        Self {
            lookup: Arc::new(InMemoryLookup::new()),
        }
    }

    pub fn with_lookup(lookup: Arc<dyn DnsLookup>) -> Self {
        Self { lookup }
    }

    pub fn in_memory(&self) -> Option<InMemoryLookup> {
        self.lookup.as_in_memory()
    }

    /// Resolve `<domain>` by reading the TXT record at
    /// `_dnslink.<domain>`. Returns the `dnslink=…` value if present
    /// and well-formed.
    pub fn resolve(&self, domain: &str) -> Result<String, DnsLinkError> {
        let domain = domain.trim_end_matches('.').to_ascii_lowercase();
        if domain.is_empty() {
            return Err(DnsLinkError::InvalidDomain);
        }
        let fqdn = dnslink_fqdn(&domain);
        let records = self.lookup.lookup_txt(&fqdn);
        if records.is_empty() {
            return Err(DnsLinkError::NotFound(fqdn));
        }
        parse_dnslink_records(&records).ok_or_else(|| DnsLinkError::NoLink(fqdn))
    }

    /// Convenience that returns the `/ipfs/<cid>` or `/ipns/<name>`
    /// path string, without further interpretation.
    pub fn resolve_path(&self, domain: &str) -> Result<DnsLinkPath, DnsLinkError> {
        let raw = self.resolve(domain)?;
        DnsLinkPath::parse(&raw).ok_or(DnsLinkError::InvalidPath(raw))
    }
}

impl Default for DnsLinkResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse `_dnslink.<domain>` (the convention used by IPFS gateways).
fn dnslink_fqdn(domain: &str) -> String {
    format!("_dnslink.{domain}")
}

/// Walk a TXT-record vec and pick the first `dnslink=…` payload.
/// The IPFS spec accepts both upper- and lowercase `dnslink=`
/// prefixes, so we downcase before matching.
fn parse_dnslink_records(records: &[String]) -> Option<String> {
    for rec in records {
        let trimmed = rec.trim();
        let lower = trimmed.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("dnslink=") {
            // Keep the original case for the path; only the prefix
            // comparison is case-insensitive.
            let value_start = trimmed.len() - rest.len();
            return Some(trimmed[value_start..].trim().to_string());
        }
    }
    None
}

/// Parsed DNSLink path — `/ipfs/<cid>`, `/ipns/<name>`, or
/// relative for sharded directory traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsLinkPath {
    Ipfs(String),
    Ipns(String),
    Relative(String),
}

impl DnsLinkPath {
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if let Some(rest) = raw.strip_prefix("/ipfs/") {
            Some(DnsLinkPath::Ipfs(rest.trim_start_matches('/').to_string()))
        } else if let Some(rest) = raw.strip_prefix("/ipns/") {
            Some(DnsLinkPath::Ipns(rest.trim_start_matches('/').to_string()))
        } else {
            Some(DnsLinkPath::Relative(raw.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            DnsLinkPath::Ipfs(c) => c,
            DnsLinkPath::Ipns(n) => n,
            DnsLinkPath::Relative(p) => p,
        }
    }
}

/// Errors raised by DNSLink resolution.
#[derive(Debug, thiserror::Error)]
pub enum DnsLinkError {
    #[error("domain name is empty")]
    InvalidDomain,
    #[error("no DNSLink TXT records found for {0}")]
    NotFound(String),
    #[error("TXT records found for {0} but none contained a dnslink= entry")]
    NoLink(String),
    #[error("DNSLink path {0} is malformed")]
    InvalidPath(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dnslink_record() {
        let out = parse_dnslink_records(&["dnslink=/ipfs/bafy...".into()]).unwrap();
        assert_eq!(out, "/ipfs/bafy...");
    }

    #[test]
    fn ignores_unrelated_txt() {
        let records = vec![
            "v=spf1 -all".to_string(),
            "dnslink=/ipfs/QmHash".to_string(),
        ];
        assert_eq!(
            parse_dnslink_records(&records).unwrap(),
            "/ipfs/QmHash"
        );
    }

    #[test]
    fn case_insensitive_dnslink_prefix() {
        let records = vec!["DNSLINK=/ipns/example.com".to_string()];
        assert_eq!(
            parse_dnslink_records(&records).unwrap(),
            "/ipns/example.com"
        );
    }

    #[test]
    fn resolve_returns_first_link() {
        let resolver = DnsLinkResolver::new();
        resolver
            .in_memory()
            .expect("default lookup is in-memory")
            .insert_dnslink("example.com", "/ipfs/bafy1");
        let path = resolver.resolve("example.com").unwrap();
        assert_eq!(path, "/ipfs/bafy1");
        // Case-insensitive domain matching.
        let path = resolver.resolve("EXAMPLE.com").unwrap();
        assert_eq!(path, "/ipfs/bafy1");
    }

    #[test]
    fn resolve_not_found() {
        let resolver = DnsLinkResolver::new();
        let err = resolver.resolve("absent.example").unwrap_err();
        match err {
            DnsLinkError::NotFound(fqdn) => assert_eq!(fqdn, "_dnslink.absent.example"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn resolve_path_parses_ipfs() {
        let resolver = DnsLinkResolver::new();
        resolver
            .in_memory()
            .unwrap()
            .insert_dnslink("example.com", "/ipfs/cid-only");
        let path = resolver.resolve_path("example.com").unwrap();
        assert_eq!(path, DnsLinkPath::Ipfs("cid-only".into()));
    }

    #[test]
    fn resolve_path_parses_ipns_chain() {
        let resolver = DnsLinkResolver::new();
        resolver
            .in_memory()
            .unwrap()
            .insert_dnslink("example.com", "/ipns/other-name");
        let path = resolver.resolve_path("example.com").unwrap();
        assert_eq!(path, DnsLinkPath::Ipns("other-name".into()));
    }

    #[test]
    fn resolve_path_parses_relative() {
        let resolver = DnsLinkResolver::new();
        resolver
            .in_memory()
            .unwrap()
            .insert_dnslink("example.com", "/some/relative/path");
        let path = resolver.resolve_path("example.com").unwrap();
        assert_eq!(path, DnsLinkPath::Relative("/some/relative/path".into()));
    }

    #[test]
    fn rejects_empty_domain() {
        let resolver = DnsLinkResolver::new();
        let err = resolver.resolve("").unwrap_err();
        assert!(matches!(err, DnsLinkError::InvalidDomain));
    }

    #[test]
    fn records_without_dnslink_entry_fail() {
        let resolver = DnsLinkResolver::new();
        // Insert a non-DNSLink TXT under `_dnslink.example.com` so the
        // lookup succeeds but the record vector has no `dnslink=`
        // entry. The expected error is `NoLink`, not `NotFound`.
        resolver
            .in_memory()
            .unwrap()
            .insert("_dnslink.example.com", vec!["v=spf1 -all".to_string()]);
        let err = resolver.resolve("example.com").unwrap_err();
        assert!(matches!(err, DnsLinkError::NoLink(_)));
    }
}