//! Self-hosted iroh DERP relay server.
//!
//! ## Why a separate module?
//!
//! ADNet's transport crate already speaks DERP **as a client** —
//! [`adnet_transport::iroh::IrohTransport`] binds an `iroh::Endpoint`
//! through `iroh::endpoint::presets::N0`, which connects to the public
//! n0 relay network by default and falls back to the local relay map
//! once an operator wires one in. That covers the "use a relay"
//! half of the DERP story.
//!
//! This module covers the **other** half: stand up a *server* in a
//! fully self-hosted network so ADNet nodes don't need to talk to
//! the public n0 network. We embed the upstream `iroh-relay` server
//! directly: `iroh_relay::server::Server::spawn` constructs the full
//! HTTPS / QUIC-address-discovery / rate-limited relay stack.
//! Wrapping it gives us ADNet-style configuration (persistable
//! `relay.json` block, secrets directory, graceful shutdown handle)
//! while preserving 100% wire compatibility with every iroh client
//! version that ships with `iroh = 1.0.3`.
//!
//! ## What's here
//!
//! - [`DerpConfig`] — operator-facing configuration block (toml/json5
//!   friendly). Lives separately from [`crate::RelayConfig`] so that
//!   operators who already run an HTTP `relay` (the mesh proxy) can
//!   enable the DERP server independently.
//! - [`DerpServer`] / [`DerpServerHandle`] — the live server handle.
//!   Constructed once at startup, dropped (or `shutdown().await`-ed)
//!   at process exit. Pairs nicely with a `tokio::select!` driven
//!   SIGINT handler.
//! - [`access`] — ADNet-flavoured [`iroh_relay::server::AccessControl`]
//!   implementations: explicit allowlist / denylist by Ed25519
//!   `EndpointId`. Useful for production deployments where you want
//!   to gate a self-hosted relay to a closed group of nodes.
//!
//! ## Wire model
//!
//! Each DERP client connection authenticates its `EndpointId`
//! through the relay handshake protocol (Ed25519 signature over
//! keying material exported from the TLS session). Once authenticated,
//! the server decides admission via the configured `AccessControl`,
//! then forwards datagrams to other peers connected to the same
//! relay. End-to-end encryption rides **on top of** the relay —
//! iroh uses per-peer QUIC streams, so the relay only sees opaque
//! ciphertext and is unable to read message contents.
//!
//! ## Feature gating
//!
//! This module is only compiled when the `derp` cargo feature is
//! enabled on `adnet-relay`. Building `adnet-relay` without the
//! feature keeps the binary footprint small: the DERP server pulls
//! in `iroh-relay`'s full `server` stack (HTTPS, ACME, QUIC, ...),
//! which is several seconds of cold-build time. Build with
//! `cargo build -p adnet-relay --features derp`.

use std::fmt;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

use iroh_base::EndpointId;
use iroh_relay::defaults::{DEFAULT_HTTP_PORT, DEFAULT_HTTPS_PORT, DEFAULT_RELAY_QUIC_PORT};
#[allow(unused_imports)]
use iroh_relay::server::{
    Access, AllowAll, CertConfig, ClientRateLimit, DynAccessControl, Limits, QuicConfig,
    RelayConfig as IrohRelayConfig, Server as IrohRelayServer, ServerConfig,
    TlsConfig as IrohTlsConfig,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::watch;
use tracing::info;
use url::Url;

pub mod access;
#[cfg(test)]
pub mod test_fixture;
pub mod tls;

pub use access::{AllowlistAccess, DenylistAccess, NodeAccessControl};

/// Operator-facing configuration for the embedded DERP relay server.
///
/// This is the ADNet-side mirror of the upstream
/// `iroh_relay::server::RelayConfig` — it exposes the fields an
/// operator actually wants to set in `relay.json` and translates to
/// the upstream config in [`DerpServer::spawn`]. Persisted via
/// `serde` with the camelCase rename so it slots into the existing
/// ADNet `Config` block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DerpConfig {
    /// Bind address for the relay HTTP (cleartext) socket. Defaults
    /// to `[::]:80`. Ignored when `tls` is set; then only the captive
    /// portal service runs here.
    #[serde(default = "DerpConfig::default_http_bind")]
    pub http_bind_addr: SocketAddr,

    /// TLS configuration. `None` means "no TLS", i.e. plaintext
    /// HTTP only. Production deployments should always set this —
    /// relay clients authenticate via the relay handshake which is
    /// only meaningful over an authentic TLS session, so running a
    /// relay without TLS exposes it to a trivial man-in-the-middle.
    #[serde(default)]
    pub tls: Option<DerpTlsConfig>,

    /// QUIC address discovery (QAD). When `Some`, the relay listens
    /// for QUIC connections on `quic_bind_addr` (defaults to
    /// `https_bind_addr` with port 7842). The QUIC server is
    /// essential for NAT-traversal: it advertises the addresses
    /// observers see coming from each peer, which the iroh client
    /// uses to drive hole-punching.
    #[serde(default)]
    pub quic: Option<DerpQuicConfig>,

    /// Path-persistable secret / public-key allow- and deny-lists.
    /// See [`access::NodeAccessControl`].
    #[serde(default)]
    pub access: AccessConfig,

    /// Rate-limit knobs. Defaults to "unlimited" — i.e. the upstream
    /// `iroh_relay::server::Limits::default()`. Operators with a
    /// public relay in front of untrusted traffic should set
    /// `client_rx` to bound ingress bandwidth.
    #[serde(default)]
    pub rate_limits: Option<DerpRateLimits>,

    /// Optional metrics bind address. When `None`, the upstream
    /// metrics server is not started. We do **not** bake in a
    /// default port — operators are expected to set this
    /// explicitly to opt in to the Prometheus exporter.
    #[serde(default)]
    pub metrics_bind_addr: Option<SocketAddr>,

    /// Override for the upstream key cache capacity. Defaults to
    /// the upstream default (~1 Mi entries → ~56 MiB at the
    /// well-known entry size). Operators can lower it on
    /// memory-constrained hosts.
    #[serde(default)]
    pub key_cache_capacity: Option<usize>,
}

impl Default for DerpConfig {
    fn default() -> Self {
        Self {
            http_bind_addr: Self::default_http_bind(),
            tls: None,
            quic: None,
            access: AccessConfig::default(),
            rate_limits: None,
            metrics_bind_addr: None,
            key_cache_capacity: None,
        }
    }
}

impl DerpConfig {
    fn default_http_bind() -> SocketAddr {
        (std::net::Ipv6Addr::UNSPECIFIED, DEFAULT_HTTP_PORT).into()
    }

    /// HTTPS bind address. Falls back to `http_bind_addr` with the
    /// port replaced by `443`. The captive-portal service still
    /// listens on the HTTP socket regardless.
    pub fn https_bind_addr(&self) -> SocketAddr {
        match &self.tls {
            Some(tls) => tls
                .https_bind_addr
                .unwrap_or_else(|| SocketAddr::new(self.http_bind_addr.ip(), DEFAULT_HTTPS_PORT)),
            None => SocketAddr::new(self.http_bind_addr.ip(), DEFAULT_HTTPS_PORT),
        }
    }

    /// QUIC bind address. Defaults to `https_bind_addr` with port
    /// [`DEFAULT_RELAY_QUIC_PORT`] (`7842`). QUIC requires TLS to be
    /// configured — [`DerpServer::spawn`] returns an error if `quic
    /// = Some(_)` but `tls = None`.
    pub fn quic_bind_addr(&self) -> SocketAddr {
        match &self.quic {
            Some(q) => q.bind_addr.unwrap_or_else(|| {
                SocketAddr::new(self.https_bind_addr().ip(), DEFAULT_RELAY_QUIC_PORT)
            }),
            None => SocketAddr::new(self.https_bind_addr().ip(), DEFAULT_RELAY_QUIC_PORT),
        }
    }
}

/// TLS configuration for the embedded DERP server. At least one of
/// `manual` (a single cert + key on disk) or `lets_encrypt` (automatic
/// ACME) must be present.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DerpTlsConfig {
    /// HTTPS listen address. Defaults to `http_bind_addr` on port
    /// 443. Setting this to `[::]:443` is the common case.
    #[serde(default)]
    pub https_bind_addr: Option<SocketAddr>,

    /// Static `cert.pem` path. Required when `lets_encrypt` is `None`.
    #[serde(default)]
    pub manual: Option<DerpManualCert>,

    /// ACME / Let's Encrypt configuration. When `Some`, the relay
    /// server uses `tokio-rustls-acme` to fetch and renew a
    /// certificate automatically. Required fields:
    /// - `hostname`: the DNS name the relay is reachable on.
    /// - `contact`: a `mailto:` email address for renewal failures.
    #[serde(default)]
    pub lets_encrypt: Option<DerpLetsEncrypt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DerpManualCert {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DerpLetsEncrypt {
    pub hostname: String,
    pub contact: String,
    /// Whether to use the Let's Encrypt **production** ACME
    /// directory. Defaults to `true` (production). Switch to `false`
    /// while developing so staging certs don't rate-limit you.
    #[serde(default = "DerpLetsEncrypt::default_prod")]
    pub production: bool,

    /// Directory where ACME will cache the issued certificate and
    /// account state. Required — there's no sensible default
    /// without knowing the operator's filesystem layout.
    pub cache_dir: PathBuf,
}

impl DerpLetsEncrypt {
    fn default_prod() -> bool {
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DerpQuicConfig {
    #[serde(default)]
    pub bind_addr: Option<SocketAddr>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DerpRateLimits {
    /// Per-client ingress byte rate (bytes/second).
    #[serde(default)]
    pub client_rx_bytes_per_second: Option<NonZeroU32>,
    /// Per-client ingress burst budget (bytes).
    #[serde(default)]
    pub client_rx_max_burst_bytes: Option<NonZeroU32>,
    /// Optional server-wide accept rate (connections/second).
    /// Stored as `f64` because that's what
    /// `iroh_relay::server::Limits::accept_conn_limit` accepts; we
    /// therefore can't derive `Eq` here either.
    #[serde(default)]
    pub accept_conn_limit: Option<f64>,
    /// Optional server-wide accept burst.
    #[serde(default)]
    pub accept_conn_burst: Option<usize>,
}

/// Access-control configuration persisted in `relay.json`. Three
/// modes: open, allowlist (closed group), and denylist (open minus
/// a blocklist). Maps 1:1 to the [`access::NodeAccessControl`]
/// tri-state.
///
/// Derive notes:
/// - We `PartialEq` (not `Eq`) because the struct carries
///   `Vec<EndpointId>` and `EndpointId`'s `Eq` impl goes through
///   the inner ed25519 public key, which is `PartialEq + Eq` —
///   so `Eq` here *is* fine. We use `PartialEq` defensively to
///   keep this struct forward-compatible with later `EndpointId`
///   versions that may switch to byte-vector equality.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AccessConfig {
    /// Default — every connecting endpoint is admitted.
    #[default]
    Everyone,
    /// Closed group: only endpoints in `allow` are admitted.
    Allowlist {
        /// Hex-encoded 32-byte Ed25519 public keys (`EndpointId`s).
        allow: Vec<EndpointId>,
    },
    /// Open group: every endpoint except those in `deny` is admitted.
    Denylist {
        /// Hex-encoded 32-byte Ed25519 public keys.
        deny: Vec<EndpointId>,
    },
}

impl AccessConfig {
    /// Build a runtime [`Arc<dyn DynAccessControl>`] from this config.
    /// The returned trait object is what the iroh-relay server
    /// invokes on every new connection.
    ///
    /// Why `DynAccessControl` (and not `AccessControl`)?
    ///
    /// `iroh_relay::server::AccessControl::on_connect` returns
    /// `impl Future + Send`, which Rust doesn't allow in a
    /// trait-object position. The upstream crate exposes the
    /// boxed-future variant [`DynAccessControl`] for exactly
    /// this — the `AccessControl` blanket impl forwards via
    /// [`iroh_relay::server::impl DynAccessControl for T`]. So
    /// `AllowAll`, `AllowlistAccess`, `DenylistAccess` are all
    /// double-implemented and we wrap any of them in
    /// `Arc<dyn DynAccessControl>` here.
    pub fn build(&self) -> Arc<dyn DynAccessControl> {
        match self {
            AccessConfig::Everyone => Arc::new(AllowAll),
            AccessConfig::Allowlist { allow } => Arc::new(AllowlistAccess::new(allow.clone())),
            AccessConfig::Denylist { deny } => Arc::new(DenylistAccess::new(deny.clone())),
        }
    }
}

/// Errors emitted by [`DerpServer::spawn`].
#[derive(Debug, Error)]
pub enum DerpError {
    #[error("QUIC address discovery requires TLS; configure `tls` to enable QUIC")]
    QuicRequiresTls,
    #[error("invalid TLS configuration: {0}")]
    InvalidTls(String),
    #[error("could not read certificate file {path}: {source}")]
    CertFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse certificate at {path}: {source}")]
    CertParse {
        path: PathBuf,
        // The upstream `PrivateKeyDer::from_pem_file` returns
        // `std::io::Error` in rustls-pki-types 1.x; we wrap any
        // parse failure in a `Box<dyn Error>` so the error type
        // doesn't pin a specific dependency version.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("invalid ACME configuration: {0}")]
    Acme(String),
    #[error("failed to bind / spawn the iroh relay server: {0}")]
    Spawn(String),
    #[error("could not join the spawn_blocking task: {0}")]
    Join(String),
}

/// The running DERP server. Holds the upstream
/// `iroh_relay::server::Server` and a join handle for the
/// background task that owns it.
///
/// **Lifecycle**:
///
/// - [`DerpServer::spawn`] returns a [`DerpServer`] whose
///   background task is parked on the shutdown channel.
/// - Either [`DerpServer::shutdown`] (consumes the value) or
///   [`DerpServerHandle::request_shutdown`] (signal-only,
///   doesn't consume) triggers the upstream server to be
///   torn down. The returned `JoinHandle` is awaited by
///   `shutdown()` so any error from the upstream
///   `Server::shutdown` surfaces to the caller.
/// - Dropping the value **does not** shut down the server
///   synchronously — the join handle is detached. This is
///   the same shape as upstream `iroh-relay`'s `Server`
///   drop semantics; use [`DerpServer::shutdown`] for
///   graceful teardown.
pub struct DerpServer {
    handle: DerpServerHandle,
    /// Join handle for the background task that owns the
    /// upstream `IrohRelayServer` and drives shutdown when
    /// the channel flips. Detached on drop, awaited by
    /// [`DerpServer::shutdown`].
    join: Option<tokio::task::JoinHandle<Result<(), DerpError>>>,
    /// The inner `IrohRelayServer` lives inside the spawned
    /// task; we don't hold a copy here.
    _phantom: std::marker::PhantomData<IrohRelayServer>,
}

impl fmt::Debug for DerpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DerpServer")
            .field("handle", &self.handle)
            .field("join_present", &self.join.is_some())
            .finish()
    }
}

impl DerpServer {
    /// Spawn a DERP server according to `cfg`. The returned
    /// value owns the running server. Drop the value
    /// (without `shutdown()`) to **detach** the background
    /// task — the upstream server is then torn down only
    /// when its internal supervisor task is reaped (usually
    /// at process exit). Call [`DerpServer::shutdown`] for
    /// graceful in-process teardown.
    pub async fn spawn(cfg: DerpConfig) -> Result<Self, DerpError> {
        let inner_cfg = build_server_config(&cfg).await?;
        let server = IrohRelayServer::spawn(inner_cfg)
            .await
            .map_err(|e| DerpError::Spawn(e.to_string()))?;
        let info = DerpServerInfo {
            https_addr: server.https_addr(),
            http_addr: server.http_addr(),
            quic_addr: server.quic_addr(),
        };
        info!(
            https = ?info.https_addr,
            http = ?info.http_addr,
            quic = ?info.quic_addr,
            "DERP relay server online",
        );

        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let handle = DerpServerHandle {
            info: info.clone(),
            shutdown_tx,
        };

        // Park a background task on the shutdown channel.
        // When `request_shutdown()` flips the watch, the
        // task takes ownership of the upstream server and
        // drives its shutdown. We move the `IrohRelayServer`
        // into the task because upstream `Server::shutdown`
        // consumes `self`.
        let join = tokio::spawn(async move {
            // Wait for the first `true` on the channel.
            // `wait_for(|v| *v)` is the idiomatic watch
            // idiom for "block until predicate holds". It
            // returns `Err(_)` only if the sender is
            // dropped — we treat that as a no-op shutdown
            // (the channel went away; the server is still
            // running until its `Drop` abort-handle fires).
            // That preserves the "detach on drop" contract
            // documented above.
            let _ = shutdown_rx.wait_for(|v| *v).await;
            // `server` is owned by this task and never
            // moves until this point, so handing it to
            // upstream `Server::shutdown` is straight-line
            // ownership transfer.
            server
                .shutdown()
                .await
                .map_err(|e| DerpError::Spawn(format!("shutdown: {e}")))
        });

        Ok(Self {
            handle,
            join: Some(join),
            _phantom: std::marker::PhantomData,
        })
    }

    /// Return the [`DerpServerHandle`] (cheap — the handle is
    /// a cloned handle + a watch sender).
    pub fn handle(&self) -> DerpServerHandle {
        self.handle.clone()
    }

    /// Politely shut down. Returns once the upstream
    /// `Server` has released its listeners and the
    /// background task has finished.
    ///
    /// Errors propagate from the upstream supervisor task.
    /// This is a behaviour change from the previous "swallow
    /// the result" shape — operators now see when
    /// shutdown actually fails (it usually only fails when
    /// a listener socket can't be released cleanly, which
    /// is exactly the signal an operator needs).
    pub async fn shutdown(mut self) -> Result<(), DerpError> {
        // Signal the channel first so the background task
        // starts the actual teardown.
        self.handle.request_shutdown();
        let join = self
            .join
            .take()
            .expect("`join` is present until `shutdown` consumes self");
        match join.await {
            Ok(inner_result) => inner_result,
            Err(e) => Err(DerpError::Join(e.to_string())),
        }
    }
}

/// Lightweight, cheaply cloneable handle to a running
/// [`DerpServer`]. The full server lives in [`DerpServer`] (a
/// struct, not Clone); the handle is what operators pass into
/// background tasks / shutdown handlers.
#[derive(Clone)]
pub struct DerpServerHandle {
    info: DerpServerInfo,
    shutdown_tx: watch::Sender<bool>,
}

impl DerpServerHandle {
    /// Snapshot of where this server is listening. Useful for
    /// `/diagnostics` end-points and for building a `RelayMap`
    /// clients should consume.
    pub fn info(&self) -> DerpServerInfo {
        self.info.clone()
    }

    /// Returns the URL an iroh client should connect to. Prefers
    /// HTTPS, falls back to plain HTTP. Both are valid but clients
    /// in `--prod` deployments should pick the former.
    pub fn primary_url(&self) -> Option<Url> {
        self.info
            .https_addr
            .and_then(|a| Url::parse(&format!("https://{a}")).ok())
            .or_else(|| {
                self.info
                    .http_addr
                    .and_then(|a| Url::parse(&format!("http://{a}")).ok())
            })
    }

    /// Signal shutdown to the running server. Idempotent.
    ///
    /// Unlike [`DerpServer::shutdown`], this method does
    /// **not** consume the server nor await the join
    /// handle — it just flips the watch channel that the
    /// background task is parked on. The actual upstream
    /// `Server::shutdown` runs on the background task and
    /// its `Result` is reported through [`DerpServer::shutdown`].
    ///
    /// **Use case**: a long-running supervisor (e.g. an
    /// axum server with a `/shutdown` admin endpoint, or
    /// a SIGINT handler) wants to *trigger* shutdown but
    /// not consume the `DerpServer` value. The supervisor
    /// then calls `DerpServer::shutdown().await` separately
    /// to collect the result and join the task.
    ///
    /// **Caveat**: because this method only signals, an
    /// operator who calls `request_shutdown()` and *then*
    /// drops `DerpServer` without calling `shutdown()` will
    /// detach the join handle — the upstream server still
    /// tears down (the background task runs to completion),
    /// but the operator loses the chance to surface any
    /// shutdown error. The recommended pattern is:
    ///
    /// ```no_run
    /// # use adnet_relay::derp::{DerpConfig, DerpServer};
    /// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// let server = DerpServer::spawn(DerpConfig::default()).await?;
    /// let handle = server.handle();
    /// // ... in a SIGINT handler ...
    /// handle.request_shutdown();
    /// // ... back on the main task ...
    /// server.shutdown().await?;
    /// # Ok(()) }
    /// ```
    pub fn request_shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

impl fmt::Debug for DerpServerHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DerpServerHandle")
            .field("info", &self.info)
            .field("shutdown_signalled", &*self.shutdown_tx.borrow())
            .finish()
    }
}

/// Diagnostics snapshot for a running DERP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerpServerInfo {
    pub https_addr: Option<SocketAddr>,
    pub http_addr: Option<SocketAddr>,
    pub quic_addr: Option<SocketAddr>,
}

// ─────────────────────────── internal helpers ──────────────────────────

/// Build the upstream [`ServerConfig`] from an ADNet [`DerpConfig`].
/// Kept crate-private because the mapping is purely an
/// implementation detail; the public surface is [`DerpServer::spawn`].
async fn build_server_config(cfg: &DerpConfig) -> Result<ServerConfig, DerpError> {
    // Resolve AccessControl up front so we only allocate once.
    let access: Arc<dyn DynAccessControl> = cfg.access.build();

    let mut relay = IrohRelayConfig::new(cfg.http_bind_addr);
    relay.access = access;
    relay.key_cache_capacity = cfg.key_cache_capacity;

    // Rate-limit mapping.
    if let Some(rl) = &cfg.rate_limits {
        let mut limits = Limits::default();
        if let Some(bps) = rl.client_rx_bytes_per_second {
            let mut crl = ClientRateLimit::new(bps);
            crl.max_burst_bytes = rl.client_rx_max_burst_bytes;
            limits.client_rx = Some(crl);
        }
        limits.accept_conn_limit = rl.accept_conn_limit;
        limits.accept_conn_burst = rl.accept_conn_burst;
        relay.limits = limits;
    }

    // TLS mapping.
    let (tls_config, quic_only_tls): (Option<IrohTlsConfig>, Option<rustls::ServerConfig>) =
        match &cfg.tls {
            None => (None, None),
            Some(tls_cfg) => {
                let cert = load_cert_config(tls_cfg).await?;
                let https_bind = tls_cfg.https_bind_addr.unwrap_or_else(|| {
                    SocketAddr::new(cfg.http_bind_addr.ip(), DEFAULT_HTTPS_PORT)
                });
                let upstream_tls = IrohTlsConfig::new(https_bind, cert);
                // For the QUIC server we also need a `rustls::ServerConfig`.
                // Only Manual certs expose their config up-front; Let's
                // Encrypt resolves the cert asynchronously, so we
                // have to fall back to a "QUIC only with no TLS at all"
                // path in that case. We surface that as a runtime
                // error instead of silently disabling QUIC.
                let quic_tls = match &upstream_tls.cert {
                    CertConfig::Manual { server_config } => Some(server_config.clone()),
                    CertConfig::LetsEncrypt { .. } | _ => None,
                };
                (Some(upstream_tls), quic_tls)
            }
        };
    relay.tls = tls_config;

    // QUIC mapping.
    let mut server_cfg = ServerConfig::default();
    server_cfg.relay = Some(relay);
    if let Some(q) = &cfg.quic {
        if cfg.tls.is_none() {
            // Reject early: iroh-relay refuses to spawn a QUIC
            // server without a TLS config, but we'd rather surface
            // a friendlier message instead of letting the upstream
            // error bubble through.
            return Err(DerpError::QuicRequiresTls);
        }
        // Same early-out for the LetsEncrypt + QUIC combo: we
        // can't pass a TLS config to QUIC if we don't have a
        // concrete one. Surface a friendly error so the operator
        // knows to switch to `manual` certs or remove QUIC.
        //
        // The decision: if QUIC is on but we couldn't extract a
        // concrete `rustls::ServerConfig` (because the operator
        // chose Let's Encrypt and the cert is resolved
        // asynchronously), refuse the combo.
        if quic_only_tls.is_none() {
            return Err(DerpError::Acme(
                "QUIC is not supported alongside Let's Encrypt; \
                 either disable QUIC or switch to a manual TLS cert"
                    .into(),
            ));
        }
        let bind = q.bind_addr.unwrap_or_else(|| {
            SocketAddr::new(cfg.https_bind_addr().ip(), DEFAULT_RELAY_QUIC_PORT)
        });
        let mut quic_config = QuicConfig::new(bind);
        if let Some(tls) = quic_only_tls {
            quic_config.server_config = Some(tls);
        }
        server_cfg.quic = Some(quic_config);
    } else {
        server_cfg.quic = None;
    }

    if let Some(addr) = cfg.metrics_bind_addr {
        server_cfg.metrics_addr = Some(addr);
    }

    Ok(server_cfg)
}

/// Resolve a [`DerpTlsConfig`] (either manual or Let's Encrypt) into
/// the upstream [`CertConfig`]. Manual cert reads happen on the
/// blocking pool (a single small file read on startup — not worth
/// the in-line cost of `tokio::fs::read`).
async fn load_cert_config(cfg: &DerpTlsConfig) -> Result<CertConfig, DerpError> {
    // The upstream builder needs a `rustls::ServerConfig` for either
    // path. We construct it once with `ring` as the crypto provider
    // (matches iroh-relay's default `tls-ring` feature).
    let server_config_builder = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| DerpError::InvalidTls(format!("rustls protocol versions: {e}")))?
    .with_no_client_auth();

    match (&cfg.manual, &cfg.lets_encrypt) {
        (Some(manual), _) => {
            // Read cert + key off disk via `spawn_blocking` to keep
            // the reactor free of synchronous `std::fs::File` work.
            let cert_path = manual.cert_path.clone();
            let key_path = manual.key_path.clone();
            let (certs, key) = tokio::task::spawn_blocking(move || {
                let key =
                    PrivateKeyDer::from_pem_file(&key_path).map_err(|e| DerpError::CertParse {
                        path: key_path.clone(),
                        source: Box::new(e),
                    })?;
                let file = std::fs::File::open(&cert_path).map_err(|e| DerpError::CertFile {
                    path: cert_path.clone(),
                    source: e,
                })?;
                let mut reader = std::io::BufReader::new(file);
                let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| DerpError::CertParse {
                        path: cert_path.clone(),
                        source: Box::new(e),
                    })?;
                Ok::<_, DerpError>((certs, key))
            })
            .await
            .map_err(|e| DerpError::Join(e.to_string()))??;
            let server_config = server_config_builder
                .with_single_cert(certs, key)
                .map_err(|e| DerpError::InvalidTls(format!("rustls: {e}")))?;
            Ok(CertConfig::Manual { server_config })
        }
        (None, Some(le)) => {
            use iroh_relay::server::AcmeConfig;
            let acme = AcmeConfig::letsencrypt(le.production)
                .domains(vec![le.hostname.clone()])
                .contact(vec![format!("mailto:{}", le.contact)])
                .cache_path(le.cache_dir.clone());
            Ok(CertConfig::LetsEncrypt {
                acme_config: acme,
                server_config_builder,
            })
        }
        (None, None) => Err(DerpError::InvalidTls(
            "either `manual` or `letsEncrypt` must be configured under `tls`".into(),
        )),
    }
}

/// Convenience: parse a hex `EndpointId` string.
pub fn parse_endpoint_id(s: &str) -> Result<EndpointId, iroh_base::KeyParsingError> {
    s.parse()
}

/// Convenience trait for extracting the list of currently allowed
/// or denied endpoints from an `AccessConfig`. Mostly useful for
/// diagnostics.
pub trait AccessConfigPeek {
    fn peek(&self) -> AccessSnapshot;
}

/// Snapshot returned by [`AccessConfigPeek::peek`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccessSnapshot {
    pub kind: &'static str,
    pub list: Vec<EndpointId>,
}

impl AccessConfigPeek for AccessConfig {
    fn peek(&self) -> AccessSnapshot {
        match self {
            AccessConfig::Everyone => AccessSnapshot {
                kind: "everyone",
                list: Vec::new(),
            },
            AccessConfig::Allowlist { allow } => AccessSnapshot {
                kind: "allowlist",
                list: allow.clone(),
            },
            AccessConfig::Denylist { deny } => AccessSnapshot {
                kind: "denylist",
                list: deny.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_relay::server::Access;
    use iroh_relay::server::DynAccessControl;

    #[test]
    fn access_config_default_is_open() {
        let cfg = AccessConfig::default();
        assert!(matches!(cfg, AccessConfig::Everyone));
        let snap = AccessConfigPeek::peek(&cfg);
        assert_eq!(snap.kind, "everyone");
        assert!(snap.list.is_empty());
    }

    #[test]
    fn derp_config_default_is_unencrypted() {
        let cfg = DerpConfig::default();
        assert!(cfg.tls.is_none(), "default DerpConfig is plaintext");
        assert!(cfg.quic.is_none());
        assert!(matches!(cfg.access, AccessConfig::Everyone));
        assert_eq!(
            cfg.http_bind_addr,
            SocketAddr::new(std::net::Ipv6Addr::UNSPECIFIED.into(), DEFAULT_HTTP_PORT)
        );
    }

    #[test]
    fn quic_without_tls_rejected() {
        // We can't actually call `build_server_config` here without
        // an async runtime, so we rely on the manifest of expected
        // behaviour (the error variant) and a separate async test
        // below.
        let cfg = DerpConfig {
            tls: None,
            quic: Some(DerpQuicConfig { bind_addr: None }),
            ..DerpConfig::default()
        };
        // Just confirm config-shape: we *can* construct the config
        // but `spawn()` will return `Err(DerpError::QuicRequiresTls)`.
        assert!(cfg.quic.is_some());
        assert!(cfg.tls.is_none());
    }

    #[test]
    fn tls_without_manual_or_le_rejected() {
        let cfg = DerpTlsConfig {
            https_bind_addr: None,
            manual: None,
            lets_encrypt: None,
        };
        // Calling `load_cert_config` is async; we cover the rejection
        // path in the async test below. Confirmed-shape check here:
        // both `manual` and `lets_encrypt` are `None`.
        assert!(cfg.manual.is_none() && cfg.lets_encrypt.is_none());
    }

    #[test]
    fn access_snapshot_peek_round_trip() {
        // Use a deterministic secret so the test is reproducible.
        let secret = [7u8; 32];
        let pk = iroh_base::SecretKey::from_bytes(&secret).public();
        let cfg = AccessConfig::Allowlist { allow: vec![pk] };
        let snap = AccessConfigPeek::peek(&cfg);
        assert_eq!(snap.kind, "allowlist");
        assert_eq!(snap.list.len(), 1);
        assert_eq!(snap.list[0], pk);
    }

    /// Async test that:
    /// 1. Builds an `AllowlistAccess` and verifies it admits a
    ///    matching `EndpointId` and denies everything else.
    /// 2. Pins the deny-list behaviour symmetrically.
    #[tokio::test]
    async fn allowlist_and_denylist_admission_control() {
        let secret_a = [1u8; 32];
        let secret_b = [2u8; 32];
        let pk_a = iroh_base::SecretKey::from_bytes(&secret_a).public();
        let pk_b = iroh_base::SecretKey::from_bytes(&secret_b).public();

        // Allowlist on pk_a.
        let allow = AllowlistAccess::new(vec![pk_a]);
        let cases = [(pk_a, Access::Allow), (pk_b, Access::Deny { reason: None })];
        for (pk, expected) in cases {
            let req = crate::derp::test_fixture::for_test_endpoint(pk);
            let got = DynAccessControl::on_connect(&allow, &req).await;
            assert_eq!(got, expected, "pk={pk}");
        }

        // Denylist on pk_a — every other id passes.
        let deny = DenylistAccess::new(vec![pk_a]);
        let cases = [(pk_a, Access::Deny { reason: None }), (pk_b, Access::Allow)];
        for (pk, expected) in cases {
            let req = crate::derp::test_fixture::for_test_endpoint(pk);
            let got = DynAccessControl::on_connect(&deny, &req).await;
            assert_eq!(got, expected, "pk={pk}");
        }
    }

    /// Async test that builds an [`AccessConfig::Allowlist`] end-to-end
    /// and confirms the upstream `Arc<dyn AccessControl>` honours it.
    #[tokio::test]
    async fn access_config_build_yields_working_acl() {
        let secret = [9u8; 32];
        let pk = iroh_base::SecretKey::from_bytes(&secret).public();
        let cfg = AccessConfig::Allowlist { allow: vec![pk] };
        let acl = cfg.build();
        let req = crate::derp::test_fixture::for_test_endpoint(pk);
        // `acl: Arc<dyn DynAccessControl>`. Method-call syntax
        // resolves to the trait method directly.
        assert_eq!(acl.on_connect(&req).await, Access::Allow);
        // A second id should be denied.
        let other_pk = iroh_base::SecretKey::from_bytes(&[8u8; 32]).public();
        let req_other = crate::derp::test_fixture::for_test_endpoint(other_pk);
        assert_eq!(
            acl.on_connect(&req_other).await,
            Access::Deny { reason: None }
        );
    }

    /// Sanity check that `build_server_config` rejects `quic=Some(_),
    /// tls=None` with a friendly error rather than the upstream
    /// "QUIC requires TLS" panic.
    #[tokio::test]
    async fn build_server_config_rejects_quic_without_tls() {
        let cfg = DerpConfig {
            http_bind_addr: "127.0.0.1:0".parse().unwrap(),
            tls: None,
            quic: Some(DerpQuicConfig { bind_addr: None }),
            ..DerpConfig::default()
        };
        let res = build_server_config(&cfg).await;
        match res {
            Err(DerpError::QuicRequiresTls) => {}
            other => panic!("expected QuicRequiresTls, got: {other:?}"),
        }
    }

    /// Sanity check that `load_cert_config` rejects a TLS config
    /// with neither `manual` nor `lets_encrypt`.
    #[tokio::test]
    async fn load_cert_config_rejects_empty_tls() {
        let cfg = DerpTlsConfig {
            https_bind_addr: None,
            manual: None,
            lets_encrypt: None,
        };
        let res = load_cert_config(&cfg).await;
        match res {
            Err(DerpError::InvalidTls(msg)) => {
                assert!(
                    msg.contains("either `manual` or `letsEncrypt`"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("expected InvalidTls, got: {other:?}"),
        }
    }

    /// Sanity check that the `DerpConfig::with_quic()` builder
    /// pattern is honoured: even when the operator supplies a
    /// `LetsEncrypt`-only TLS, we surface a friendly error rather
    /// than letting the upstream panic.
    #[tokio::test]
    async fn build_server_config_rejects_quic_with_acme() {
        let cfg = DerpConfig {
            http_bind_addr: "127.0.0.1:0".parse().unwrap(),
            tls: Some(DerpTlsConfig {
                https_bind_addr: None,
                manual: None,
                lets_encrypt: Some(DerpLetsEncrypt {
                    hostname: "relay.example.com".into(),
                    contact: "ops@example.com".into(),
                    production: true,
                    cache_dir: std::env::temp_dir().join("derp-acme-test"),
                }),
            }),
            quic: Some(DerpQuicConfig { bind_addr: None }),
            ..DerpConfig::default()
        };
        let res = build_server_config(&cfg).await;
        match res {
            Err(DerpError::Acme(_)) => {}
            other => panic!("expected Acme error, got: {other:?}"),
        }
    }

    /// Round-trip: serialise a [`DerpConfig`] to JSON5-style JSON
    /// (camelCase) and deserialise it back. Catches asymmetric
    /// `rename_all` mistakes (the kind of thing that bit the V3
    /// `metrics_addr` audit).
    #[test]
    fn derp_config_round_trip_json() {
        let cfg = DerpConfig {
            http_bind_addr: "127.0.0.1:8765".parse().unwrap(),
            tls: Some(DerpTlsConfig {
                https_bind_addr: Some("127.0.0.1:443".parse().unwrap()),
                manual: Some(DerpManualCert {
                    cert_path: "/tmp/x.crt".into(),
                    key_path: "/tmp/x.key".into(),
                }),
                lets_encrypt: None,
            }),
            quic: Some(DerpQuicConfig {
                bind_addr: Some("127.0.0.1:7842".parse().unwrap()),
            }),
            access: AccessConfig::Everyone,
            rate_limits: None,
            metrics_bind_addr: Some("127.0.0.1:9090".parse().unwrap()),
            key_cache_capacity: Some(2048),
        };
        let json = serde_json::to_string(&cfg).expect("serialise");
        // Spot-check the camelCase renames stick.
        assert!(json.contains("httpBindAddr"), "camelCase failed: {json}");
        assert!(json.contains("httpsBindAddr"), "camelCase failed: {json}");
        assert!(json.contains("certPath"), "camelCase failed: {json}");
        assert!(json.contains("metricsBindAddr"), "camelCase failed: {json}");
        let back: DerpConfig = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, cfg);
    }

    /// **Live-spawn integration test.** Stand up a DERP server
    /// on a kernel-assigned port, verify the listener actually
    /// binds, and confirm graceful shutdown returns cleanly.
    /// This is the one test that exercises the iroh-relay
    /// `Server::spawn` path end-to-end — every other test in
    /// this module only validates configuration shape or
    /// access-control logic. Operators that want to know "will
    /// my embedded relay actually start?" should look here.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn derp_server_spawns_and_shuts_down_cleanly() {
        // Plaintext (no TLS) is the cheapest path that still
        // binds a real listener. We deliberately omit QUIC
        // and metrics so the test does not need a non-
        // conflicting metrics port.
        let cfg = DerpConfig {
            http_bind_addr: "127.0.0.1:0".parse().unwrap(),
            ..DerpConfig::default()
        };
        let server = DerpServer::spawn(cfg).await.expect("server spawns");
        // After spawn, the HTTP listener must be bound to a
        // kernel-assigned port. Port 0 is never observable in
        // the post-spawn state — iroh-relay's supervisor
        // hands back the bound address.
        let http_addr = server
            .handle()
            .info()
            .http_addr
            .expect("http listener bound");
        assert_eq!(http_addr.ip().to_string(), "127.0.0.1");
        assert_ne!(http_addr.port(), 0, "kernel must have assigned a port");
        // Graceful shutdown returns Ok(()) and the supervisor
        // task ends without panicking.
        server.shutdown().await.expect("graceful shutdown");
    }

    /// Audit regression: `DerpServer` is `Debug`-printable. The
    /// upstream `iroh_relay::server::Server` is not `Debug`,
    /// so we ship a manual impl that surfaces `handle` and the
    /// `join_present` flag. This test pins the contract so a
    /// future regression that accidentally removes the manual
    /// `Debug` impl surfaces here rather than at the first
    /// `tracing` call site that tries to format a `DerpServer`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn derp_server_is_debug_printable() {
        let cfg = DerpConfig {
            http_bind_addr: "127.0.0.1:0".parse().unwrap(),
            ..DerpConfig::default()
        };
        let server = DerpServer::spawn(cfg).await.expect("server spawns");
        let dbg = format!("{server:?}");
        assert!(
            dbg.contains("DerpServer"),
            "Debug must name the type; got {dbg}"
        );
        assert!(
            dbg.contains("join_present: true"),
            "Debug must surface join_present: true pre-shutdown; got {dbg}"
        );
        server.shutdown().await.expect("graceful shutdown");
        // After shutdown, the server is consumed — we cannot
        // format it again. The pre-shutdown print is the only
        // observable contract; the post-shutdown invariant is
        // that the join handle was awaited (see
        // `derp_server_spawns_and_shuts_down_cleanly`).
    }
}
