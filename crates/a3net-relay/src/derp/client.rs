//! DERP client + multi-relay routing wrappers around `iroh-relay`.
//!
//! `a3net-relay`'s `server` module is the *service* (our own mesh HTTP
//! forward proxy plus, behind a feature flag, the embedded iroh DERP
//! server). This module is the matching *client* half: it lets an
//! A3Net node dial a DERP relay — ours or a remote one — and use it
//! for outbound connectivity.
//!
//! ## Why this lives in our crate
//!
//! `iroh-relay` already ships a generic [`iroh_relay::client::Client`]
//! and a multi-relay [`iroh_relay::RelayMap`] that iroh's `Endpoint`
//! uses. We don't reimplement the dial/handshake mechanics — we wrap
//! them behind A3Net-flavoured constructors so the rest of the
//! workspace doesn't have to reach into `iroh-relay`/`iroh-dns`
//! directly.
//!
//! The wrappers here are deliberately small:
//!
//! - [`DerpClientConfig`] — operator-friendly JSON-serialisable
//!   settings for "dial this relay over HTTPS, optional QUIC, optional
//!   auth token, with this secret key".
//! - [`DerpClient`] / [`DerpClientBuilder`] — thin handle around
//!   [`iroh_relay::client::Client`] plus its dial URL. Built via
//!   [`DerpClient::builder`].
    //! - [`DerpEndpoint`] — convenience split-into-(stream, sink)
//   matching the upstream API.
//! - [`DerpRelayMap`] — JSON-serialisable list of [`DerpRelayEntry`]
//!   that builds an upstream [`iroh_relay::RelayMap`] on demand.
//!
//! ## Feature gating
//!
//! Like [`super`], this module is compiled only when the `derp`
//! feature is enabled. Without `derp`, the `iroh-relay` and
//! `iroh-dns` crates aren't pulled in.

use std::sync::Arc;

use iroh_base::RelayUrl;
use iroh_base::SecretKey;
use iroh_dns::dns::DnsResolver;
use iroh_relay::client::Client as IrohClient;
use iroh_relay::client::ClientBuilder as IrohClientBuilder;
use iroh_relay::RelayConfig as IrohRelayConfig;
use iroh_relay::RelayMap as IrohRelayMap;
use iroh_relay::RelayQuicConfig as IrohRelayQuicConfig;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use a3net_error::{ErrorKind, IntoReport, Severity};

/// Errors surfaced by the DERP client wrappers.
#[derive(Debug, Error)]
pub enum DerpClientError {
    #[error("DERP client connect failed: {0}")]
    Connect(String),
    #[error("invalid relay URL {0}: {1}")]
    Url(String, String),
}

// P0-5: unified error reporting.  Codes `RPC-NNN` are the relay
// client side.  Keep them disjoint from `DRP-001..009` (server).
impl IntoReport for DerpClientError {
    fn code(&self) -> &'static str {
        match self {
            Self::Connect(_) => "RPC-001",
            Self::Url(_, _) => "RPC-002",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            // Bad URL — caller gave us garbage.
            Self::Url(_, _) => ErrorKind::BadRequest,
            // Connect failed — relay is down or network is broken.
            Self::Connect(_) => ErrorKind::Unavailable,
        }
    }

    fn severity(&self) -> Severity {
        match self {
            // Bad URL is a config error — warn.
            Self::Url(_, _) => Severity::Warn,
            // Connectivity failure — page.
            Self::Connect(_) => Severity::Error,
        }
    }
}

/// Operator-facing JSON-serialisable settings for a single DERP
/// relay.
///
/// Only the URL is strictly required at the type level. In practice
/// the caller must also supply a [`rustls::ClientConfig`] via
/// [`DerpClientBuilder::with_tls`] before [`DerpClientBuilder::connect`]
/// can succeed — iroh-relay's `ClientBuilder` requires it and there's
/// no secure default we can pick. The `auth_token` and `prefer_ipv6`
/// fields have safe defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DerpClientConfig {
    /// The relay URL, e.g. `https://relay.example.com`.
    pub url: String,
    /// Authorisation token (sent as `Authorization: Bearer <token>`).
    #[serde(default)]
    pub auth_token: Option<String>,
    /// Whether to prefer IPv6 when the dual-stack resolver returns
    /// both families. Default `false`.
    #[serde(default)]
    pub prefer_ipv6: bool,
    /// Capacity of the public-key cache. Defaults to 128 (matches
    /// upstream).
    #[serde(default)]
    pub key_cache_capacity: Option<usize>,
    /// Optional QUIC port. When set, the relay advertises a QUIC
    /// endpoint at `<host>:<port>` for QUIC address discovery.
    #[serde(default)]
    pub quic_port: Option<u16>,
}

impl DerpClientConfig {
    /// Build a config from just a URL string.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            auth_token: None,
            prefer_ipv6: false,
            key_cache_capacity: None,
            quic_port: None,
        }
    }

    /// Set the auth token.
    pub fn with_auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    /// Set `prefer_ipv6`.
    pub fn with_prefer_ipv6(mut self, prefer: bool) -> Self {
        self.prefer_ipv6 = prefer;
        self
    }

    /// Set the key cache capacity.
    pub fn with_key_cache_capacity(mut self, n: usize) -> Self {
        self.key_cache_capacity = Some(n);
        self
    }

    /// Set the QUIC port advertised for QUIC address discovery.
    pub fn with_quic_port(mut self, port: u16) -> Self {
        self.quic_port = Some(port);
        self
    }

    /// Parse the URL into an [`iroh_relay::RelayUrl`].
    pub fn parsed_url(&self) -> Result<RelayUrl, DerpClientError> {
        self.url
            .parse::<RelayUrl>()
            .map_err(|e| DerpClientError::Url(self.url.clone(), e.to_string()))
    }
}

/// Builder for an A3Net DERP client.
///
/// Constructed via [`DerpClient::builder`].
#[derive(Debug, Clone)]
pub struct DerpClientBuilder {
    /// Parsed URL cached so we can move it into the upstream builder
    /// without re-parsing and without keeping the original `cfg`'s
    /// `url` string alive twice.
    url: Option<RelayUrl>,
    /// Original URL string, kept for diagnostics.
    url_str: String,
    cfg: DerpClientConfig,
    secret_key: SecretKey,
    dns_resolver: DnsResolver,
    tls_config: Option<Arc<rustls::ClientConfig>>,
}

impl DerpClientBuilder {
    /// Set the TLS configuration the dial should use. Required
    /// before [`Self::connect`] when dialling a non-loopback server.
    pub fn with_tls(mut self, tls_config: rustls::ClientConfig) -> Self {
        self.tls_config = Some(Arc::new(tls_config));
        self
    }

    /// Use a custom DNS resolver. Mostly useful for tests that want
    /// to pin resolution behaviour.
    pub fn with_dns_resolver(mut self, resolver: DnsResolver) -> Self {
        self.dns_resolver = resolver;
        self
    }

    /// Borrow the underlying config (for inspection / logging).
    pub fn config(&self) -> &DerpClientConfig {
        &self.cfg
    }

    /// Consume the builder and open a connection to the configured
    /// relay.
    ///
    /// The connection is **fully established** before this returns —
    /// the underlying WebSocket upgrade + iroh handshake are
    /// completed. To check connectivity, call
    /// [`IrohClient::split`] and time a round-trip on the
    /// resulting stream/sink pair.
    pub async fn connect(self) -> Result<DerpClient, DerpClientError> {
        let url = self.url.ok_or_else(|| {
            // `DerpClient::builder` does the parse eagerly; this
            // branch only fires if someone bypasses it.
            DerpClientError::Url(self.url_str.clone(), "no url".into())
        })?;
        // Upstream signature: `ClientBuilder::new(url, secret_key, dns_resolver)`.
        let mut upstream: IrohClientBuilder =
            IrohClientBuilder::new(url.clone(), self.secret_key, self.dns_resolver);
        if let Some(tls) = self.tls_config {
            upstream = upstream.tls_client_config((*tls).clone());
        }
        if let Some(tok) = &self.cfg.auth_token {
            upstream = upstream.auth_token(tok.clone());
        }
        if self.cfg.prefer_ipv6 {
            upstream = upstream.address_family_selector(|| true);
        }
        if let Some(cap) = self.cfg.key_cache_capacity {
            upstream = upstream.key_cache_capacity(cap);
        }
        let inner = upstream
            .connect()
            .await
            .map_err(|e| DerpClientError::Connect(e.to_string()))?;
        Ok(DerpClient {
            url,
            url_str: self.url_str,
            inner,
        })
    }
}

/// Live connection to a single DERP relay. NOT `Clone` because the
/// upstream `iroh_relay::client::Client` wraps a unique tokio
/// stream/sink pair — splitting with [`DerpClient::split`] hands
/// ownership of one half to each downstream task.
#[derive(Debug)]
pub struct DerpClient {
    /// The underlying URL.
    url: RelayUrl,
    /// Original string form, for diagnostics.
    url_str: String,
    /// Live upstream client.
    inner: IrohClient,
}

impl DerpClient {
    /// Begin building a [`DerpClient`] for `cfg` using the local
    /// node's `secret_key`.
    pub fn builder(cfg: DerpClientConfig, secret_key: SecretKey) -> DerpClientBuilder {
        let parsed_url = cfg.parsed_url().ok();
        DerpClientBuilder {
            url: parsed_url,
            url_str: cfg.url.clone(),
            cfg,
            secret_key,
            dns_resolver: DnsResolver::default(),
            tls_config: None,
        }
    }

    /// The relay URL we connected to.
    pub fn url(&self) -> &RelayUrl {
        &self.url
    }

    /// Original URL string (preserves the form the caller supplied).
    pub fn url_str(&self) -> &str {
        &self.url_str
    }

    /// Underlying upstream client. Advanced callers may need this
    /// for streaming or splitting the connection into
    /// `(ClientStream, ClientSink)`.
    pub fn inner(&self) -> &IrohClient {
        &self.inner
    }

    /// Split the client into a stream and sink half. The caller
    /// takes ownership of the halves — pair each with a dedicated
    /// tokio task for bidirectional traffic.
    pub fn split(self) -> DerpEndpoint {
        let (stream, sink) = self.inner.split();
        DerpEndpoint { stream, sink }
    }
}

/// What we hand back to operators after splitting a [`DerpClient`]
/// into its halves. Mirrors the upstream split API.
#[derive(Debug)]
pub struct DerpEndpoint {
    pub stream: iroh_relay::client::ClientStream,
    pub sink: iroh_relay::client::ClientSink,
}

// ────────────────────────────── RelayMap ──────────────────────────────

/// One entry in [`DerpRelayMap`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DerpRelayEntry {
    pub url: String,
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub quic_port: Option<u16>,
}

impl DerpRelayEntry {
    /// Parse this entry into an [`IrohRelayConfig`].
    pub fn to_iroh_config(&self) -> Result<IrohRelayConfig, DerpClientError> {
        let url: RelayUrl = self
            .url
            .parse::<RelayUrl>()
            .map_err(|e: iroh_base::RelayUrlParseError| DerpClientError::Url(
                self.url.clone(),
                e.to_string(),
            ))?;
        let quic = self.quic_port.map(IrohRelayQuicConfig::new);
        let mut cfg = IrohRelayConfig::new(url, quic);
        if let Some(tok) = &self.auth_token {
            cfg = cfg.with_auth_token(tok.clone());
        }
        Ok(cfg)
    }
}

/// Operator-facing multi-relay configuration. Persistent, JSON-friendly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DerpRelayMap {
    #[serde(default)]
    pub entries: Vec<DerpRelayEntry>,
}

impl DerpRelayMap {
    /// Empty map.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Add an entry.
    pub fn push(mut self, entry: DerpRelayEntry) -> Self {
        self.entries.push(entry);
        self
    }

    /// Extend from an iterator.
    pub fn extend<I: IntoIterator<Item = DerpRelayEntry>>(mut self, it: I) -> Self {
        self.entries.extend(it);
        self
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no entries are configured.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Build the upstream [`IrohRelayMap`]. Returns
    /// `DerpClientError::Url` on the first malformed URL.
    pub fn to_iroh_map(&self) -> Result<IrohRelayMap, DerpClientError> {
        let map: IrohRelayMap = IrohRelayMap::empty();
        for e in &self.entries {
            let cfg = Arc::new(e.to_iroh_config()?);
            map.insert(cfg.url.clone(), cfg);
        }
        Ok(map)
    }

    /// All configured relay URLs as strings (in declaration order).
    pub fn urls(&self) -> impl Iterator<Item = &str> + '_ {
        self.entries.iter().map(|e| e.url.as_str())
    }
}

impl FromIterator<DerpRelayEntry> for DerpRelayMap {
    fn from_iter<I: IntoIterator<Item = DerpRelayEntry>>(it: I) -> Self {
        Self {
            entries: it.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_serialises_camel_case() {
        let cfg = DerpClientConfig::new("https://relay.example.com")
            .with_auth_token("secret")
            .with_prefer_ipv6(true)
            .with_quic_port(7842);
        let j = serde_json::to_string(&cfg).unwrap();
        assert!(j.contains("\"url\""));
        assert!(j.contains("\"authToken\""));
        assert!(j.contains("\"preferIpv6\""));
        assert!(j.contains("\"quicPort\":7842"));
        let back: DerpClientConfig = serde_json::from_str(&j).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn parsed_url_rejects_garbage() {
        let cfg = DerpClientConfig::new("not a url");
        assert!(cfg.parsed_url().is_err());
    }

    #[test]
    fn parsed_url_accepts_https() {
        let cfg = DerpClientConfig::new("https://relay.example.com");
        let url = cfg.parsed_url().unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("relay.example.com"));
    }

    #[test]
    fn relay_map_round_trip_json() {
        let map = DerpRelayMap::empty().push(DerpRelayEntry {
            url: "https://a.example.com".into(),
            auth_token: Some("tok-a".into()),
            quic_port: None,
        });
        let j = serde_json::to_string(&map).unwrap();
        let back: DerpRelayMap = serde_json::from_str(&j).unwrap();
        assert_eq!(back, map);
        assert_eq!(back.len(), 1);
        assert!(!back.is_empty());
    }

    #[test]
    fn relay_map_to_iroh_map_rejects_bad_urls() {
        let map = DerpRelayMap::empty().push(DerpRelayEntry {
            url: "not-a-url".into(),
            auth_token: None,
            quic_port: None,
        });
        assert!(map.to_iroh_map().is_err());
    }

    #[test]
    fn relay_map_to_iroh_map_succeeds_for_well_formed() {
        let map = DerpRelayMap::empty()
            .push(DerpRelayEntry {
                url: "https://a.example.com".into(),
                auth_token: Some("tok".into()),
                quic_port: Some(7842),
            })
            .push(DerpRelayEntry {
                url: "https://b.example.com".into(),
                auth_token: None,
                quic_port: None,
            });
        let upstream = map.to_iroh_map().unwrap();
        assert_eq!(upstream.len(), 2);
    }

    #[test]
    fn entry_holds_optional_quic_and_token() {
        let entry = DerpRelayEntry {
            url: "https://r.example.com".into(),
            auth_token: None,
            quic_port: None,
        };
        let cfg = entry.to_iroh_config().unwrap();
        assert_eq!(cfg.url.to_string(), "https://r.example.com/");
    }

    // P0-5: pin DerpClientError codes.
    #[test]
    fn derp_client_error_codes_are_stable() {
        let pairs: Vec<(DerpClientError, &str, ErrorKind, Severity)> = vec![
            (
                DerpClientError::Connect("x".into()),
                "RPC-001",
                ErrorKind::Unavailable,
                Severity::Error,
            ),
            (
                DerpClientError::Url("u".into(), "err".into()),
                "RPC-002",
                ErrorKind::BadRequest,
                Severity::Warn,
            ),
        ];
        for (err, code, kind, sev) in pairs {
            assert_eq!(err.code(), code, "code for {err:?}");
            assert_eq!(err.kind(), kind, "kind for {err:?}");
            assert_eq!(err.severity(), sev, "severity for {err:?}");
        }
    }

    #[test]
    fn derp_client_error_into_report_carries_cause() {
        let e = DerpClientError::Url("h://".into(), "bad scheme".into());
        let report = e.into_report("a3net-relay");
        assert_eq!(report.code, "RPC-002");
        assert!(report.cause.as_deref().unwrap_or("").contains("bad scheme"));
    }
}
