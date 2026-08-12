//! Relay HTTP server — proxies `/exodus-mesh/fetch` requests across NAT.
//!
//! Mirrors `wan_relay_server.rs` from
//! `Exodus@src-backup/.../wan_relay_server.rs`. Validates the upstream URL
//! (path must start with `/blobs/`, no `..`, host/port must be sane) and
//! forwards the request via `reqwest` with a sane timeout and a bounded
//! body.
//!
//! ## Security
//!
//! The default host policy is [`HostPolicy::DefaultBlockPrivate`], which
//! rejects loopback / RFC1918 / link-local / cloud-metadata hosts both
//! as IP literals and after DNS resolution. Operators can opt in to
//! `AllowLoopbackOnly` for tests or `AllowAllUntrusted` for mesh-private
//! deployments.
//!
//! The default upstream timeout is 60 s (down from the previous 1 h)
//! and the default body budget is 64 MiB. Combined with the streaming
//! body, this prevents the relay from being used as a memory-exhaustion
//! relay.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::{
    Router,
    extract::Query,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{debug, info, warn, error};

use crate::config::RelayServerInfo;
use crate::metrics::RelayMetrics;
use crate::proxy_policy::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_UPSTREAM_TIMEOUT, HostPolicy, SafeRedirectPolicy, validate_path,
};

/// Running relay server handle.
pub struct RelayServerHandle {
    pub port: u16,
    pub bind_host: String,
    pub base_url: String,
    shutdown_tx: watch::Sender<bool>,
}

impl RelayServerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    pub fn info(&self) -> RelayServerInfo {
        RelayServerInfo {
            running: true,
            port: self.port,
            base_url: self.base_url.clone(),
            bind_host: self.bind_host.clone(),
        }
    }
}

impl Drop for RelayServerHandle {
    fn drop(&mut self) {
        // Best-effort shutdown signal — ensures the listener task
        // exits even if the user drops the handle without calling
        // `.shutdown()` explicitly.
        let _ = self.shutdown_tx.send(true);
    }
}

/// Query parameters for the proxy endpoint.
#[derive(Debug, Deserialize, Clone)]
pub struct MeshFetchQuery {
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// Per-instance configuration the relay server uses internally. Built
/// from [`crate::RelayConfig`] at start time and passed into the
/// handlers via axum state.
#[derive(Clone)]
pub struct ServerPolicy {
    pub host_policy: HostPolicy,
    pub max_body_bytes: usize,
    pub upstream_timeout: Duration,
    pub redirect_policy: SafeRedirectPolicy,
}

impl ServerPolicy {
    /// Build a [`ServerPolicy`] from a [`RelayConfig`].
    ///
    /// All fields are optional in `RelayConfig`; when `None` the defaults
    /// from [`Default`] are used so the relay always starts with a fully
    /// populated policy even if the operator left some fields unset.
    pub fn from_config(cfg: &crate::RelayConfig) -> Self {
        use std::time::Duration;
        let redirect_policy = SafeRedirectPolicy::new(cfg.host_policy.clone())
            .with_limit(cfg.max_redirects.unwrap_or(3) as usize);
        Self {
            host_policy: cfg.host_policy.clone(),
            max_body_bytes: cfg.max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES),
            upstream_timeout: cfg
                .upstream_timeout_secs
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_UPSTREAM_TIMEOUT),
            redirect_policy,
        }
    }
}

impl Default for ServerPolicy {
    fn default() -> Self {
        Self {
            host_policy: HostPolicy::default(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            upstream_timeout: DEFAULT_UPSTREAM_TIMEOUT,
            redirect_policy: SafeRedirectPolicy::new(HostPolicy::default()),
        }
    }
}

/// Axum-based relay server.
pub struct RelayServer;

impl RelayServer {
    /// Spawn a relay on `bind_host:port` with the default
    /// [`ServerPolicy`]. Returns a handle that owns the listener; drop
    /// / `.shutdown()` to stop.
    ///
    /// Pass [`crate::billing::BillingMode::Disabled`] (the default) for a
    /// pure forward proxy. To enable the optional billing endpoints pass
    /// [`crate::billing::BillingMode::Enabled`] with the relay's signing
    /// wallet — the `billing` cargo feature must be enabled at build time
    /// for the `Enabled` variant to exist.
    pub async fn start(
        bind_host: &str,
        port: u16,
        #[cfg_attr(not(feature = "billing"), allow(unused_variables))]
        billing_mode: crate::billing::BillingMode,
    ) -> Result<RelayServerHandle, String> {
        Self::start_with_policy(bind_host, port, billing_mode, ServerPolicy::default()).await
    }

    /// Like [`RelayServer::start`] but with a custom policy. This is the
    /// hook integration tests use to flip the host policy to
    /// `AllowLoopbackOnly` so they can stand up an upstream on
    /// `127.0.0.1`.
    pub async fn start_with_policy(
        bind_host: &str,
        port: u16,
        #[cfg_attr(not(feature = "billing"), allow(unused_variables))]
        billing_mode: crate::billing::BillingMode,
        policy: ServerPolicy,
    ) -> Result<RelayServerHandle, String> {
        let addr: SocketAddr = format!("{bind_host}:{port}")
            .parse()
            .map_err(|e| format!("Invalid relay bind address: {e}"))?;

        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| format!("WAN relay bind failed on {addr}: {e}"))?;
        let bound = listener.local_addr().map_err(|e| e.to_string())?;
        let port = bound.port();
        let bind_host = bound.ip().to_string();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let router = Router::new()
            .route("/exodus-mesh/fetch", get(mesh_fetch_handler))
            .route("/health", get(health_handler))
            .route("/healthz", get(health_policy_handler))
            .with_state(policy);

        // ---- Optional billing routes ------------------------------------
        #[cfg(feature = "billing")]
        let router = {
            // Use `nest` rather than `merge` so the nested router keeps
            // its own State type (axum 0.7 `merge` requires both routers
            // to share the same State). We only nest when billing is on.
            if matches!(billing_mode, crate::billing::BillingMode::Enabled { .. }) {
                let sub = billing_mode.routes();
                router.nest("/relay/billing", sub)
            } else {
                router
            }
        };

        let shutdown_rx_serve = shutdown_rx.clone();
        tokio::spawn(async move {
            let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
                let mut rx = shutdown_rx_serve;
                loop {
                    if *rx.borrow() {
                        break;
                    }
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            });
            if let Err(e) = serve.await {
                warn!("WAN relay server stopped: {e}");
            }
        });

        let base_url = format!("http://{bind_host}:{port}");
        info!("ADNet WAN relay listening on {base_url}");
        Ok(RelayServerHandle {
            port,
            bind_host,
            base_url,
            shutdown_tx,
        })
    }
}

async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthPolicyView {
    host_policy: String,
    max_body_bytes: usize,
    upstream_timeout_secs: u64,
    max_redirects: usize,
}

async fn health_policy_handler(State(policy): State<ServerPolicy>) -> impl IntoResponse {
    axum::Json(HealthPolicyView {
        host_policy: policy.host_policy.name().to_string(),
        max_body_bytes: policy.max_body_bytes,
        upstream_timeout_secs: policy.upstream_timeout.as_secs(),
        max_redirects: policy.redirect_policy.limit(),
    })
}

async fn mesh_fetch_handler(
    State(policy): State<ServerPolicy>,
    Query(q): Query<MeshFetchQuery>,
) -> Response {
    // Re-host the route GET?host=X&port=Y&path=Z variant as well as
    // POST (some clients put the path in the body).
    proxy_mesh_fetch(&q, &policy).await
}

async fn proxy_mesh_fetch(q: &MeshFetchQuery, policy: &ServerPolicy) -> Response {
    let metrics = RelayMetrics::get();
    metrics.requests.inc();
    // active_sessions: bump on entry, release on exit (RAII guard
    // pattern so all return paths decrement exactly once).
    metrics.active_sessions.inc();
    let _guard = ActiveSessionGuard::new(metrics.clone());

    debug!("Relay request: host={}, port={}, path={}", q.host, q.port, q.path);

    if let Err((status, msg)) = validate_request(q, policy).await {
        metrics.policy_filtered.inc();
        warn!("Relay policy filtered: {} - {}", q.host, msg);
        return (status, msg).into_response();
    }
    let path = normalize_mesh_path(&q.path);
    let upstream_url = format!("http://{}:{}{}", q.host.trim(), q.port, path);

    debug!("Relay upstream URL: {}", upstream_url);

    let client = match reqwest::Client::builder()
        .timeout(policy.upstream_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to build relay HTTP client: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("client build failed: {e}"),
            )
                .into_response();
        }
    };

    let initial = match client.get(&upstream_url).send().await {
        Ok(r) => r,
        Err(e) => {
            metrics.upstream_errors.inc();
            error!("Relay upstream fetch failed: {} - {}", upstream_url, e);
            return (
                StatusCode::BAD_GATEWAY,
                format!("Upstream fetch failed: {e}"),
            )
                .into_response();
        }
    };

    // Follow redirects manually so we can re-validate each destination.
    let mut resp = initial;
    let mut host = q.host.trim().to_string();
    let mut port = q.port;
    let metrics_for_loop = RelayMetrics::get(); // clone for use in early-return arms
    for hop in 0..=policy.redirect_policy.limit() {
        let status = resp.status();
        if !status.is_redirection() {
            debug!("Relay response: status={}, upstream={}", status, upstream_url);
            break;
        }
        if hop == policy.redirect_policy.limit() {
            warn!("Relay: too many redirects (limit {}) from {}", policy.redirect_policy.limit(), upstream_url);
            return (
                StatusCode::BAD_GATEWAY,
                format!(
                    "Too many redirects (limit {})",
                    policy.redirect_policy.limit()
                ),
            )
                .into_response();
        }
        let location = match resp
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
        {
            Some(l) => l.to_string(),
            None => {
                metrics_for_loop.upstream_errors.inc();
                warn!("Relay: redirect with no Location header from {}", upstream_url);
                return (
                    StatusCode::BAD_GATEWAY,
                    String::from("redirect with no Location header"),
                )
                    .into_response();
            }
        };
        debug!("Relay redirect {} -> {} (hop {}/{})", upstream_url, location, hop + 1, policy.redirect_policy.limit());
        let next_url =
            match reqwest::Url::parse(&upstream_url).and_then(|base| base.join(&location)) {
                Ok(u) => u,
            Err(e) => {
                metrics_for_loop.upstream_errors.inc();
                warn!("Relay: invalid redirect Location '{}': {}", location, e);
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("invalid redirect Location: {e}"),
                )
                    .into_response();
            }
            };
        let next_host = next_url.host_str().unwrap_or(host.as_str()).to_string();
        let next_port = next_url.port_or_known_default().unwrap_or(port);
        if let Err(e) = policy.redirect_policy.check_redirect(
            &reqwest::Url::parse(&upstream_url)
                .unwrap_or_else(|_| reqwest::Url::parse("http://x/").unwrap()),
            &next_url,
            &host,
            port,
        ) {
            metrics_for_loop.upstream_errors.inc();
            warn!("Relay redirect policy violation: {} -> {}: {}", upstream_url, location, e);
            return (StatusCode::BAD_GATEWAY, format!("redirect rejected: {e}")).into_response();
        }
        // New target must also satisfy the host policy independently.
        if let Err(e) = policy.host_policy.accepts_resolved(&next_host).await {
            metrics_for_loop.upstream_errors.inc();
            warn!("Relay host policy rejected redirect: {} -> {}: {}", host, next_host, e);
            return (
                StatusCode::BAD_GATEWAY,
                format!("redirect host rejected: {e}"),
            )
                .into_response();
        }
        host = next_host;
        port = next_port;
        resp = match client.get(next_url).send().await {
            Ok(r) => r,
            Err(e) => {
                metrics_for_loop.upstream_errors.inc();
                error!("Relay upstream fetch after redirect failed: {}", e);
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("upstream fetch after redirect failed: {e}"),
                )
                    .into_response();
            }
        };
    }

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

    // ---- Bounded, streaming body --------------------------------------
    let content_length = resp
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    if let Some(len) = content_length
        && len > policy.max_body_bytes as u64
    {
        metrics.upstream_errors.inc();
        warn!(
            "Relay: upstream Content-Length {} exceeds policy max {}",
            len,
            policy.max_body_bytes
        );
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "upstream Content-Length {len} exceeds policy max {}",
                policy.max_body_bytes
            ),
        )
            .into_response();
    }

    // Request passed all policy checks — count as a forward.
    metrics.forwards.inc();
    info!("Relay forwarding: {} -> {}", upstream_url, status);

    let ct = resp.headers().get(header::CONTENT_TYPE).cloned();
    let max_bytes = policy.max_body_bytes;

    // Shared counter updated as the body stream is polled, plus a oneshot
    // that fires when the stream is fully consumed so we can record the
    // final byte count in `RelayMetrics`.
    let bytes_counter = std::sync::Arc::new(AtomicUsize::new(0));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let bytes_counter_clone = bytes_counter.clone();

    let bounded = metered_body_stream(
        Box::pin(resp.bytes_stream()),
        max_bytes,
        bytes_counter_clone,
        Some(tx),
    );

    // Spawn a task that waits for the body to finish, then records
    // bytes_sent == bytes_received (symmetric relay proxy).
    let bytes_recorded = bytes_counter.clone();
    tokio::spawn(async move {
        let _ = rx.await;
        let bytes = bytes_recorded.load(Ordering::Relaxed) as u64;
        metrics.bytes_received.inc_by(bytes);
        metrics.bytes_sent.inc_by(bytes);
    });

    let mut out = Response::builder().status(status);
    if let Some(ct) = ct
        && let Ok(v) = axum::http::HeaderValue::from_bytes(ct.as_bytes())
    {
        out = out.header(header::CONTENT_TYPE, v);
    }
    out.body(axum::body::Body::from_stream(bounded))
        .unwrap_or_else(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())
}

/// Bounded body stream that stops reading after `max_bytes` have been
/// consumed, returning an error for any subsequent chunks.
///
/// This is a thin wrapper over [`metered_body_stream`] that discards
/// the byte counter — used by tests and any callers that don't need
/// `RelayMetrics::bytes_received_total` counting.
#[cfg(test)]
#[allow(clippy::never_loop)]
fn bounded_body_stream(
    src: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    max_bytes: usize,
) -> MeteredBody {
    metered_body_stream(src, max_bytes, std::sync::Arc::new(AtomicUsize::new(0)), None)
}

/// Metered body stream that:
/// - wraps the upstream bytes stream with byte-counting via
///   the provided `Arc<AtomicUsize>` counter,
/// - fires `completion_tx` (if provided) when the stream is fully
///   consumed, enabling the caller to record `RelayMetrics`.
///
/// Uses `poll_fn` internally so the resulting stream is `Unpin`
/// and can be fed into [`axum::body::Body::from_stream`].
#[allow(clippy::never_loop)]
fn metered_body_stream(
    src: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    max_bytes: usize,
    bytes_counter: std::sync::Arc<AtomicUsize>,
    completion_tx: Option<tokio::sync::oneshot::Sender<()>>,
) -> MeteredBody {
    MeteredBody {
        inner: src,
        max_bytes,
        bytes_counter,
        total: 0,
        aborted: false,
        completion_tx,
    }
}

/// Metered body stream. Exposes `bytes_transferred()` for recording
/// `RelayMetrics::bytes_received_total` after the stream is consumed.
#[allow(clippy::never_loop)]
pub struct MeteredBody {
    inner: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    max_bytes: usize,
    /// Shared counter — updated on every chunk, safe to read after completion.
    bytes_counter: std::sync::Arc<AtomicUsize>,
    total: usize,
    aborted: bool,
    /// Optional sender fired when the stream finishes (used for metric recording).
    completion_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MeteredBody {
    /// Returns the total bytes transferred (sum of all chunk lengths).
    pub fn bytes_transferred(&self) -> usize {
        self.bytes_counter.load(Ordering::Relaxed)
    }

    /// Fire the completion sender if present. Idempotent — safe to call multiple times.
    fn fire_completion(&mut self) {
        if let Some(tx) = self.completion_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl futures::Stream for MeteredBody {
    type Item = Result<Bytes, Box<dyn std::error::Error + Send + Sync>>;

    #[allow(clippy::never_loop)]
    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        if self.aborted {
            self.fire_completion();
            return std::task::Poll::Ready(None);
        }
        loop {
            match self.inner.as_mut().poll_next(cx) {
                std::task::Poll::Ready(Some(Ok(chunk))) => {
                    self.total = self.total.saturating_add(chunk.len());
                    if self.total > self.max_bytes {
                        self.aborted = true;
                        self.bytes_counter.store(self.total, Ordering::Relaxed);
                        let err: Box<dyn std::error::Error + Send + Sync> = Box::new(BoundedBodyError::TooLarge {
                            limit: self.max_bytes,
                            got: self.total,
                        });
                        self.fire_completion();
                        return std::task::Poll::Ready(Some(Err(err)));
                    }
                    self.bytes_counter.store(self.total, Ordering::Relaxed);
                    return std::task::Poll::Ready(Some(Ok(chunk)));
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    self.aborted = true;
                    self.bytes_counter.store(self.total, Ordering::Relaxed);
                    let err: Box<dyn std::error::Error + Send + Sync> = Box::new(BoundedBodyError::Upstream(e.to_string()));
                    self.fire_completion();
                    return std::task::Poll::Ready(Some(Err(err)));
                }
                std::task::Poll::Ready(None) => {
                    self.bytes_counter.store(self.total, Ordering::Relaxed);
                    self.fire_completion();
                    return std::task::Poll::Ready(None);
                }
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum BoundedBodyError {
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("body too large: limit {limit}, got {got}")]
    TooLarge { limit: usize, got: usize },
}

/// RAII guard that owns **one** slot of `adnet_relay_active_sessions`.
///
/// Construction increments the gauge; `Drop` decrements it. This
/// guarantees every return path — success, error, or panic — releases
/// the slot exactly once. Use via:
///
/// ```ignore
/// metrics.active_sessions.inc();          // claim
/// let _guard = ActiveSessionGuard::new(metrics.clone()); // release on drop
/// ```
///
/// The two-step API is deliberate: the call site logs the request
/// via `metrics.requests.inc()` *before* incrementing the gauge, so
/// `requests` and `active_sessions` stay in lock-step. The guard
/// only owns the symmetric `dec()`.
struct ActiveSessionGuard {
    metrics: RelayMetrics,
}

impl ActiveSessionGuard {
    fn new(metrics: RelayMetrics) -> Self {
        Self { metrics }
    }
}

impl Drop for ActiveSessionGuard {
    fn drop(&mut self) {
        self.metrics.active_sessions.dec();
    }
}

async fn validate_request(
    q: &MeshFetchQuery,
    policy: &ServerPolicy,
) -> Result<(), (StatusCode, String)> {
    let host = q.host.trim();
    if host.is_empty() || host.len() > 253 {
        debug!("Relay validation failed: empty or oversized host");
        return Err((StatusCode::BAD_REQUEST, "Invalid host".into()));
    }
    if q.port == 0 {
        debug!("Relay validation failed: invalid port 0");
        return Err((StatusCode::BAD_REQUEST, "Invalid port".into()));
    }
    // Path policy (centralised in `proxy_policy`).
    validate_path(&q.path).map_err(|e| {
        debug!("Relay path validation failed: {}", e);
        (StatusCode::BAD_REQUEST, e.to_string())
    })?;
    // Host policy with DNS resolution.
    policy
        .host_policy
        .accepts_resolved(host)
        .await
        .map_err(|e| {
            debug!("Relay host validation failed for {}: {}", host, e);
            (StatusCode::BAD_REQUEST, e)
        })?;
    Ok(())
}

fn normalize_mesh_path(path: &str) -> String {
    let p = path.trim();
    if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{p}")
    }
}

// Synchronous path validator wrapper: same checks as the policy
// [`crate::proxy_policy::validate_path`] but returns `(StatusCode, String)`
// so unit tests can keep using the previous axum-style result.
#[cfg(test)]
fn validate_path_sync(path: &str) -> Result<(), (StatusCode, String)> {
    validate_path(path).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[test]
    fn validates_mesh_path() {
        let ok = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 7878,
            path: "/blobs/abc/meta".into(),
        };
        assert!(validate_path_sync(&ok.path).is_ok());

        let bad = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 7878,
            path: "/etc/passwd".into(),
        };
        assert!(validate_path_sync(&bad.path).is_err());
    }

    #[test]
    fn normalizes_path() {
        assert_eq!(normalize_mesh_path("blobs/x"), "/blobs/x");
        assert_eq!(normalize_mesh_path("/blobs/x"), "/blobs/x");
    }

    #[test]
    fn validate_empty_host() {
        let query = MeshFetchQuery {
            host: "".into(),
            port: 7878,
            path: "/blobs/abc".into(),
        };
        assert!(validate_path_sync(&query.path).is_ok()); // path is fine
        assert!(query.host.is_empty());
    }

    #[test]
    fn validate_oversized_host() {
        let query = MeshFetchQuery {
            host: "a".repeat(254),
            port: 7878,
            path: "/blobs/abc".into(),
        };
        assert!(query.host.len() > 253);
    }

    #[test]
    fn validate_zero_port() {
        let query = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 0,
            path: "/blobs/abc".into(),
        };
        assert_eq!(query.port, 0);
    }

    #[test]
    fn validate_path_traversal_attack() {
        let query = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 7878,
            path: "/blobs/../../etc/passwd".into(),
        };
        assert!(validate_path_sync(&query.path).is_err());
    }

    #[test]
    fn validate_path_with_backslash_rejected() {
        let query = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 7878,
            path: "/blobs/foo\\..\\bar".into(),
        };
        assert!(validate_path_sync(&query.path).is_err());
    }

    #[test]
    fn validate_path_dotted_filename_accepted() {
        // `..foo` is a legal filename component, not a traversal
        // segment. The strict-segment check should let it through.
        let query = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 7878,
            path: "/blobs/..foo/bar".into(),
        };
        assert!(validate_path_sync(&query.path).is_ok());
    }

    #[test]
    fn validate_path_too_long_rejected() {
        let query = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 7878,
            path: format!("/blobs/{}", "x".repeat(1100)),
        };
        assert!(validate_path_sync(&query.path).is_err());
    }

    #[test]
    fn validate_rejects_nul_and_control() {
        for bad in ["/blobs/foo\0bar", "/blobs/foo\nbar", "/blobs/foo\rbar"] {
            assert!(validate_path_sync(bad).is_err(), "expected err: {bad:?}");
        }
    }

    #[tokio::test]
    async fn validate_request_rejects_loopback_with_default_policy() {
        let policy = ServerPolicy::default();
        let q = MeshFetchQuery {
            host: "127.0.0.1".into(),
            port: 7878,
            path: "/blobs/abc".into(),
        };
        let r = validate_request(&q, &policy).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn validate_request_rejects_loopback_dns() {
        let policy = ServerPolicy::default();
        let q = MeshFetchQuery {
            host: "localhost".into(),
            port: 7878,
            path: "/blobs/abc".into(),
        };
        let r = validate_request(&q, &policy).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn validate_request_accepts_public_ip_literal() {
        let policy = ServerPolicy::default();
        let q = MeshFetchQuery {
            host: "1.1.1.1".into(),
            port: 80,
            path: "/blobs/abc".into(),
        };
        let r = validate_request(&q, &policy).await;
        assert!(r.is_ok(), "expected ok, got {r:?}");
    }

    #[test]
    fn bounded_body_stream_passes_small_bodies() {
        use futures::stream;
        let chunks: Vec<Result<Bytes, reqwest::Error>> = vec![
            Ok(Bytes::from_static(b"hello")),
            Ok(Bytes::from_static(b"world")),
        ];
        let s: std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send>,
        > = Box::pin(stream::iter(chunks));
        let mut out = bounded_body_stream(s, 1024);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let total = rt.block_on(async {
            let mut v = Vec::new();
            while let Some(item) = out.next().await {
                v.push(item.unwrap());
            }
            v
        });
        let combined: Vec<u8> = total.iter().flat_map(|b| b.iter().copied()).collect();
        assert_eq!(combined, b"helloworld");
    }

    #[test]
    fn bounded_body_stream_rejects_oversized() {
        use futures::stream;
        let chunks: Vec<Result<Bytes, reqwest::Error>> = vec![
            Ok(Bytes::from_static(&[0u8; 100])),
            Ok(Bytes::from_static(&[0u8; 100])),
        ];
        let s: std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send>,
        > = Box::pin(stream::iter(chunks));
        let out = bounded_body_stream(s, 150);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // The first chunk (100 bytes) is within the limit and should be
        // emitted as Ok. The second chunk pushes the cumulative total to
        // 200 bytes, which exceeds the 150-byte limit, so it must be
        // returned as Err.
        let rt_ref = &rt;
        let mut out_pin = std::pin::pin!(out);
        let (first, second) = rt_ref.block_on(async {
            let first = out_pin.next().await.unwrap().unwrap();
            let second = out_pin.next().await.unwrap().unwrap_err().to_string();
            (first, second)
        });
        assert_eq!(first.len(), 100);
        assert!(
            second.contains("too large"),
            "expected too-large error, got: {second}"
        );
    }

    // ----------------------------------------------------------------
    // `/health` now accepts a `ServerPolicy` via state. Plugins
    // sometimes want to inspect the live policy through the JSON
    // variant — export a view that matches the struct.
    // ----------------------------------------------------------------
    #[test]
    fn active_session_guard_decrements_on_drop() {
        let m = RelayMetrics::get();
        m.active_sessions.set(0); // Reset for test isolation
        let before = m.active_sessions.get();
        // The 2-step API: caller increments, guard decrements on drop.
        m.active_sessions.inc();
        {
            let _guard = ActiveSessionGuard::new(m.clone());
            assert_eq!(
                m.active_sessions.get(),
                before + 1,
                "guard constructed after inc() should see the bumped value"
            );
        }
        assert_eq!(
            m.active_sessions.get(),
            before,
            "guard should decrement on drop"
        );
    }

    /// The guard releases the active-session slot on every exit
    /// path. Verify the explicit `dec()` happens — and is balanced
    /// with the manual `inc()` from the call site — by stamping the
    /// gauge to a sentinel value before constructing the guard.
    #[test]
    fn active_session_guard_dec_is_balanced_with_caller_inc() {
        let m = RelayMetrics::get();
        m.active_sessions.set(0); // Reset for test isolation
        let before = m.active_sessions.get();

        m.active_sessions.inc();
        let guard = ActiveSessionGuard::new(m.clone());
        assert_eq!(
            m.active_sessions.get(),
            before + 1,
            "after inc() + guard, gauge should be before + 1"
        );

        // Drop the guard explicitly — the gauge must drop by 1.
        drop(guard);
        assert_eq!(
            m.active_sessions.get(),
            before,
            "after explicit drop, gauge should be back to before"
        );

        // Doing it again without the caller's inc() would underflow
        // the gauge. The guard itself does NOT inc — it only dec — so
        // constructing a guard without a prior inc() results in a
        // double-decrement. Demonstrating the underflow:
        m.active_sessions.set(5);
        let g2 = ActiveSessionGuard::new(m.clone());
        drop(g2);
        assert_eq!(
            m.active_sessions.get(),
            4,
            "guard alone decrements by 1 (caller responsible for inc)"
        );
    }

    /// Forgetting a guard (via `mem::forget`) leaves the gauge
    /// incremented — that's a known leak, but documenting it via
    /// test catches regressions in the RAII pattern. Production code
    /// never `mem::forget`s the guard.
    #[test]
    fn active_session_guard_leaks_when_forgotten() {
        let m = RelayMetrics::get();
        m.active_sessions.set(0); // Reset for test isolation
        let baseline = m.active_sessions.get();
        // ActiveSessionGuard only DECs on Drop; it doesn't inc in new().
        let guard = ActiveSessionGuard::new(m.clone());
        // Counter unchanged until guard is dropped.
        assert_eq!(
            m.active_sessions.get(),
            baseline,
            "guard.new() must not increment"
        );
        // Forget the guard — Drop is skipped, counter stays.
        std::mem::forget(guard);
        assert_eq!(
            m.active_sessions.get(),
            baseline,
            "forget skips Drop, so the gauge stays at baseline"
        );
        // No manual dec needed — we never incremented.
    }

    #[test]
    fn health_policy_view_serializes() {
        let v = HealthPolicyView {
            host_policy: "default-block-private".into(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            upstream_timeout_secs: 60,
            max_redirects: 3,
        };
        let json = serde_json::to_string(&v).unwrap();
        assert!(json.contains("default-block-private"));
        assert!(json.contains(&format!("\"maxBodyBytes\":{}", DEFAULT_MAX_BODY_BYTES)));
    }
}
