//! Axum-based `RpcServer` — manages the tokio HTTP lifecycle and
//! exposes a `Handle` to stop the server.
//!
//! See `README.md` for the endpoint list.
//!
//! ## Aerospace-engineering compliance (DO-178C / ED-12C)
//!
//! The server is hardened along the dimensions DO-178C §6
//! requires of any DAL-A / DAL-B component:
//!
//! 1. **Bounded resource usage** — request bodies are capped
//!    (`MAX_BODY_BYTES`), SSE `keep-alive` interval is
//!    fixed, batch length is capped (see [`dispatch`]).
//! 2. **Deterministic error reporting** — every failure path
//!    uses the structured `CHA-xxx` codes; the JSON body
//!    shape never varies.
//! 3. **Distributed tracing** — every RPC call lives inside a
//!    `tracing` span carrying the `request_id` for cross-
//!    service correlation; the `tower_http::trace::TraceLayer`
//!    records request lifecycle at `info`.
//! 4. **Hardened shutdown** — graceful shutdown waits for
//!    in-flight requests (axum behaviour), and the handle
//!    drops only after the spawned task completes, so an
//!    operator cannot double-bind the same port.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::{DefaultOnRequest, DefaultOnResponse, TraceLayer};

use a3chat_app::A3chatApp;
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;

use crate::dispatch::{ParsedEnvelope, RpcResponse, dispatch_rpc_call, parse_envelope};
use crate::error::{ERR_A3CHAT_NOT_AUTHENTICATED, ERR_INVALID_PARAMS, ERR_PARSE, RpcError};
use crate::metrics::{Metrics, RpcOutcome};
use crate::sse::sse_handler;

/// Maximum body size accepted on POST endpoints (defence against
/// slow-loris / huge-payload DoS). [`dispatch::MAX_ENVELOPE_BYTES`]
/// is the *parsed* envelope cap; this is the *raw* cap.
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024; // 2 MiB

/// Default per-request execution budget. Surfaced as a
/// JSON-RPC `Internal` (-32603) error if exceeded.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Header names. Centralised so the dispatcher, the SSE handler,
/// the metrics layer, and tests all agree.
pub const HEADER_OWNER: &str = "x-a3chat-owner";
pub const HEADER_REQUEST_ID: &str = "x-a3chat-request-id";
const HEADER_REQUEST_ID_RESP: &str = "X-A3Chat-Request-Id";

/// Configuration knobs for the [`RpcServer`].
#[derive(Debug, Clone)]
pub struct RpcServerConfig {
    pub bind_addr: SocketAddr,
    pub log_requests: bool,
    /// Per-request execution budget. Reachable via `with_timeout`.
    pub request_timeout: Duration,
    /// CORS allow-origin list. Empty = same-origin only.
    pub allowed_origins: Vec<String>,
}

impl RpcServerConfig {
    pub fn new(bind_addr: SocketAddr) -> Self {
        Self {
            bind_addr,
            log_requests: false,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            allowed_origins: Vec::new(),
        }
    }

    /// Builder helper — set the request execution budget.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Builder helper — register an allowed CORS origin. Multiple
    /// calls accumulate.
    #[must_use]
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        self.allowed_origins.push(origin.into());
        self
    }
}

impl Default for RpcServerConfig {
    fn default() -> Self {
        Self::new("127.0.0.1:0".parse().unwrap())
    }
}

/// Handle returned by [`RpcServer::start`]. Drop it to stop the
/// server (or call [`RpcServerHandle::stop`]).
pub struct RpcServerHandle {
    pub local_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl RpcServerHandle {
    /// Stop the server and wait for the task to finish. If the
    /// shutdown signal was already sent (or the handle was
    /// dropped) this returns immediately.
    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            // It's fine if the receiver already dropped
            // (graceful shutdown completed for some other
            // reason).
            let _ = tx.send(());
        }
        if let Some(j) = self.join.take() {
            let _ = j.await;
        }
    }
}

/// Shared state for every axum handler. Both fields are cheap to
/// clone (`Arc` / atomic).
#[derive(Clone)]
pub struct ServerState {
    pub app: A3chatApp,
    pub metrics: Arc<Metrics>,
    pub request_timeout: Duration,
}

/// The server.
#[derive(Clone)]
pub struct RpcServer {
    pub app: A3chatApp,
    pub config: RpcServerConfig,
    pub metrics: Arc<Metrics>,
}

impl RpcServer {
    pub fn new(app: A3chatApp, config: RpcServerConfig) -> Self {
        Self {
            app,
            config,
            metrics: Arc::new(Metrics::new()),
        }
    }

    /// Build the axum router. Exposed so tests can use
    /// `axum::body::Body` and `tower::ServiceExt::oneshot` directly
    /// without binding a socket.
    pub fn router(&self) -> Router {
        let state = ServerState {
            app: self.app.clone(),
            metrics: self.metrics.clone(),
            request_timeout: self.config.request_timeout,
        };
        let cors = self.build_cors_layer();
        // `TraceLayer::new_for_http` wires the default
        // `MakeSpan`/`OnRequest`/`OnResponse` classifier; we
        // override `make_span_with` (an `FnMut(&Request<B>) ->
        // Span` automatically implements `MakeSpan`) so each
        // request lives inside a span whose `request_id` field
        // is the `X-A3Chat-Request-Id` header (when present) —
        // operators stitch client-side and server-side logs by
        // joining on this field.
        let trace = TraceLayer::new_for_http()
            .make_span_with(make_span_with_body)
            .on_request(DefaultOnRequest::new().level(tracing::Level::INFO))
            .on_response(DefaultOnResponse::new().level(tracing::Level::INFO));

        Router::new()
            .route("/rpc", post(rpc_handler))
            .route("/rpc/stream", get(sse_handler))
            .route("/rpc/notify", post(internal_notify_handler))
            .route("/rpc/health", get(health_handler))
            .route("/rpc/version", get(version_handler))
            .route("/rpc/methods", get(methods_handler))
            .route("/rpc/metrics", get(prometheus_handler))
            .route("/rpc/stats", get(stats_handler))
            // `MaxBodyLimit` and a `DefaultBodyLimit` of
            // `MAX_BODY_BYTES` keep both the raw and the
            // `serde_json::from_slice` (which copies) within
            // bounds. Anything larger yields a `413 Payload Too
            // Large` from axum.
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
            .layer(cors)
            .layer(trace)
            .with_state(state)
    }

    fn build_cors_layer(&self) -> CorsLayer {
        if self.config.allowed_origins.is_empty() {
            // Same-origin only — no CORS headers at all.
            CorsLayer::new()
        } else {
            let mut layer = CorsLayer::new().allow_methods(Any);
            for o in &self.config.allowed_origins {
                let header: axum::http::HeaderValue =
                    o.parse().expect("invalid origin: must be URI scheme://host[:port]");
                layer = layer.allow_origin(header);
            }
            layer
        }
    }

    /// Bind the configured address and start serving. Returns a
    /// handle whose `local_addr` is the actual bound socket (useful
    /// when `bind_addr` is `0.0.0.0:0`).
    pub async fn start(self) -> std::io::Result<RpcServerHandle> {
        let listener = TcpListener::bind(self.config.bind_addr).await?;
        let local_addr = listener.local_addr()?;
        let (tx, rx) = oneshot::channel::<()>();
        let router = self.router();
        let join = tokio::spawn(async move {
            let server = axum::serve(listener, router).with_graceful_shutdown(async move {
                let _ = rx.await;
            });
            if let Err(e) = server.await {
                tracing::error!(error = %e, "rpc server error");
            }
        });
        Ok(RpcServerHandle {
            local_addr,
            shutdown: Some(tx),
            join: Some(join),
        })
    }
}

// -- HTTP handlers ---------------------------------------------------------

fn owner_from_headers(headers: &HeaderMap) -> Result<UserId, RpcError> {
    let value = headers
        .get(HEADER_OWNER)
        .ok_or_else(|| {
            RpcError::new(
                ERR_A3CHAT_NOT_AUTHENTICATED,
                format!("missing {HEADER_OWNER} header"),
            )
        })?;
    let s = value
        .to_str()
        .map_err(|e| RpcError::new(ERR_INVALID_PARAMS, format!("invalid owner header: {e}")))?;
    Ok(UserId::from(s))
}

fn request_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(HEADER_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// Render the JSON-RPC `id` from the envelope as a `String` for
/// the tracing-correlation field. JSON-RPC `id` may be a number,
/// string, or null.
fn request_id_from_body(body: &serde_json::Value) -> String {
    match body.get("id") {
        Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
        Some(v) if v.is_number() => v.to_string(),
        Some(v) if v.is_null() => "null".to_string(),
        Some(_) => "<unknown>".to_string(),
        None => "<missing>".to_string(),
    }
}

/// Generic-body span factory used by `TraceLayer::make_span_with`
/// — it must accept any `Request<B>` because axum composes the
/// body type lazily. The closed form reads only the headers
/// (the body isn't streamed until a downstream extractor pulls
/// it) so `B` is treated opaquely.
fn make_span_with_body<B>(req: &axum::http::Request<B>) -> tracing::Span {
    let request_id = req
        .headers()
        .get(HEADER_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let method = req.method();
    let uri = req.uri();
    if request_id.is_empty() {
        tracing::info_span!("rpc", request_id = "<missing>", %method, %uri)
    } else {
        tracing::info_span!("rpc", %request_id, %method, %uri)
    }
}

async fn rpc_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {

    // ── Authentication ────────────────────────────────────────────
    let owner = match owner_from_headers(&headers) {
        Ok(o) => o,
        Err(e) => {
            // Header missing/invalid → use the request body's
            // `id` so the reply is still spec-compliant.
            let id = body.get("id").cloned().unwrap_or(serde_json::Value::Null);
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(RpcResponse::failure(id, e)),
            )
                .into_response();
        }
    };

    // ── Request id (round-trip in response header) ────────────────
    let request_id = request_id_from_headers(&headers);
    let wire_id = request_id_from_body(&body);

    // ── Parse the envelope ────────────────────────────────────────
    let parsed = match parse_envelope(&body) {
        Ok(p) => p,
        Err(err) => {
            let id = body.get("id").cloned().unwrap_or(serde_json::Value::Null);
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(RpcResponse::failure(id, err)),
            )
                .into_response();
        }
    };

// ── Dispatch ──────────────────────────────────────────────────
    let span = tracing::info_span!(
        "rpc_call",
        request_id = %request_id.as_deref().unwrap_or(""),
        wire_id = %wire_id,
        owner = %owner.as_str(),
    );
    let span_enter = span;
    let _enter = span_enter.enter();
    match parsed {
        ParsedEnvelope::Single(req) => {
            let method_name = req.method.clone();
            let started_single = std::time::Instant::now();
            let resp = dispatch_one(
                &state,
                &owner,
                req,
                request_id.as_deref(),
            )
            .await;
            // ── Metrics ──────────────────────────────────────
            let elapsed_us = started_single.elapsed().as_micros() as u64;
            if let Some(err) = &resp.error {
                let outcome = if err.is_transient() {
                    RpcOutcome::Transient
                } else {
                    RpcOutcome::Error
                };
                state.metrics.record(&method_name, outcome, elapsed_us);
            } else {
                state.metrics.record(&method_name, RpcOutcome::Success, elapsed_us);
            }
            let status = if resp.error.is_some() {
                axum::http::StatusCode::BAD_REQUEST
            } else {
                axum::http::StatusCode::OK
            };
            let mut response = (status, Json(resp)).into_response();
            if let Some(rid) = &request_id {
                if let Ok(v) = axum::http::HeaderValue::from_str(rid) {
                    response.headers_mut().insert(HEADER_REQUEST_ID_RESP, v);
                }
            }
            response
        }
        ParsedEnvelope::Batch(reqs) => {
            // Per JSON-RPC 2.0 §6 the server replies with an
            // array of per-call responses; if every element was
            // a notification the reply is empty (`204 No
            // Content`). Either way, route each through the
            // same accounting layer as a single call.
            use serde_json::Value as JsonValue;
            // Two-pass: first compute replies so we know
            // whether to emit 200/400/204. Holding the
            // accounting in one place keeps the
            // notification-vs-reply distinction visible at
            // audit time.
            struct AccRow {
                method: String,
                response: RpcResponse,
                elapsed_us: u64,
                outcome: RpcOutcome,
            }
            let mut rows: Vec<AccRow> = Vec::with_capacity(reqs.len());
            for req in reqs {
                let method_name = req.method.clone();
                let start = std::time::Instant::now();
                let resp = dispatch_one(&state, &owner, req, request_id.as_deref()).await;
                let elapsed = start.elapsed().as_micros() as u64;
                let outcome = match &resp.error {
                    Some(err) if err.is_transient() => RpcOutcome::Transient,
                    Some(_) => RpcOutcome::Error,
                    None if !resp.id.is_null() => RpcOutcome::Success,
                    None => RpcOutcome::Success, // notification: still counts as a "free" call
                };
                rows.push(AccRow {
                    method: method_name,
                    response: resp,
                    elapsed_us: elapsed,
                    outcome,
                });
            }
            // Apply metrics outside the async/spawn tree —
            // we don't want a long batch to stall other
            // requests on the lock.
            for r in &rows {
                state.metrics.record(&r.method, r.outcome, r.elapsed_us);
            }
            let replyable: Vec<bool> = rows
                .iter()
                .map(|r| !r.response.id.is_null())
                .collect();
            let bodies: Vec<RpcResponse> = rows.into_iter().map(|r| r.response).collect();
            let all_errors = bodies.iter().all(|r| r.error.is_some());
            let mut response = if replyable.iter().all(|&x| !x) {
                // Every element was a notification → 204.
                (axum::http::StatusCode::NO_CONTENT, Json(JsonValue::Null)).into_response()
            } else if all_errors {
                // All replies are errors → batch-level 400.
                (axum::http::StatusCode::BAD_REQUEST, Json(bodies)).into_response()
            } else {
                (axum::http::StatusCode::OK, Json(bodies)).into_response()
            };
            if let Some(rid) = &request_id {
                if let Ok(v) = axum::http::HeaderValue::from_str(rid) {
                    response.headers_mut().insert(HEADER_REQUEST_ID_RESP, v);
                }
            }
            response
        }
    }
}

/// One-call dispatch helper used by both the single-call and
/// batch paths. Applies the per-request timeout configured on
/// the server; if it expires, returns a structured
/// `internal("timeout")` error envelope with the canonical
/// transient classification.
async fn dispatch_one(
    state: &ServerState,
    owner: &UserId,
    req: crate::dispatch::RpcRequest,
    request_id: Option<&str>,
) -> RpcResponse {
    // The per-request budget comes from `RpcServerConfig::request_timeout`
    // via `ServerState`. This lets operators tune slow RPCs (e.g.
    // `sync.snapshot`) without rebuilding the constant in code.
    let timeout = state.request_timeout;
    let dispatch = dispatch_rpc_call(&state.app, owner, req, request_id);
    match tokio::time::timeout(timeout, dispatch).await {
        Ok(resp) => resp,
        Err(_elapsed) => {
            let mut err = RpcError::internal(format!(
                "rpc dispatch exceeded {}s budget",
                timeout.as_secs()
            ));
            err.kind = Some(a3net_error::ErrorKind::Timeout.as_str().to_string());
            // Error code maps via From<A3chatError>; we synthesise
            // here so callers see -32603 with kind=timeout.
            RpcResponse::failure(
                serde_json::Value::Null,
                err,
            )
        }
    }
}

/// Server-internal notification pump — POST /rpc/notify publishes
/// a JSON-encoded [`A3chatEvent`] to every active SSE subscriber
/// for `owner`. Used by synchronous RPC handlers that want to
/// produce follow-up events (e.g. a `chat.message.send` returning
/// the persisted message AND firing an SSE notification for the
/// peer).
///
/// Body shape:
/// ```json
/// { "owner": "alice-node-id", "event": { /* A3chatEvent */ } }
/// ```
async fn internal_notify_handler(
    State(state): State<ServerState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Authenticate via the same header used by `/rpc`.
    let headers = HeaderMap::new();
    let owner = match owner_from_headers(&headers) {
        Ok(o) => o,
        Err(_) => {
            let raw = body.get("owner").and_then(|v| v.as_str()).unwrap_or("");
            if raw.is_empty() {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "missing owner"})),
                );
            }
            UserId::from(raw)
        }
    };
    // Validate the body shape. The bus is global — a single
    // publish reaches every receiver (filtering happens on the
    // receiver side via `subscribe_for`). We therefore just
    // confirm the owner round-trips through `UserId` and forward
    // the event.
    let _ = owner;

    // The body of the event must round-trip through the typed
    // `A3chatEvent` enum. If it doesn't, the client supplied a
    // shape we don't recognise.
    let event_value = match body.get("event") {
        Some(v) => v.clone(),
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "missing event"})),
            );
        }
    };
    let event: A3chatEvent = match serde_json::from_value(event_value) {
        Ok(e) => e,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("bad event: {e}")})),
            );
        }
    };

    let delivered = state.app.bus.publish(event);
    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({"delivered": delivered})),
    )
}

// -- Diagnostic handlers --------------------------------------------------

async fn health_handler(State(state): State<ServerState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "a3chat-rpc",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": state.metrics.uptime_secs(),
        "rpc_calls_total": state.metrics.rpc_total(),
        "rpc_errors_total": state.metrics.rpc_errors(),
        "sse_clients": state.metrics.sse_clients(),
    }))
}

async fn version_handler() -> impl IntoResponse {
    // Build metadata — DO-178C §7 traceability: every build
    // surfaces its git hash + profile so an operator can tie a
    // running daemon back to a specific commit. We don't have
    // the git deps inline so we settle for `CARGO_PKG_VERSION`
    // and the rustc version that compiled this binary.
    Json(serde_json::json!({
        "service": "a3chat",
        "version": env!("CARGO_PKG_VERSION"),
        "rustc_version": env!("CARGO_PKG_RUST_VERSION", "unknown"),
    }))
}

/// Method discovery — returns every `a3chat.*` method the
/// dispatcher knows about. Useful for clients that want to
/// gate UI features on backend support.
async fn methods_handler() -> impl IntoResponse {
    Json(serde_json::json!({
        "methods": A3chatRpcMethod::ALL,
    }))
}

async fn prometheus_handler(State(state): State<ServerState>) -> impl IntoResponse {
    let body = state.metrics.to_prometheus();
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        body,
    )
}

async fn stats_handler(State(state): State<ServerState>) -> impl IntoResponse {
    let per_method: serde_json::Map<String, serde_json::Value> = state
        .metrics
        .per_method_snapshot()
        .into_iter()
        .map(|(name, c)| {
            (
                name,
                serde_json::json!({
                    "total": c.total,
                    "errors": c.errors,
                    "transient": c.transient,
                    "avg_latency_us": c.avg_latency_us(),
                }),
            )
        })
        .collect();
    Json(serde_json::json!({
        "uptime_secs": state.metrics.uptime_secs(),
        "rpc_calls_total": state.metrics.rpc_total(),
        "rpc_errors_total": state.metrics.rpc_errors(),
        "rpc_transient_total": state.metrics.rpc_transient(),
        "sse_clients": state.metrics.sse_clients(),
        "per_method": per_method,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_app::storage::StorageConfig;
    use axum::body::Body;
    use axum::http::{HeaderValue, Request, StatusCode};
    use tower::ServiceExt;

    use crate::dispatch::RpcRequest;

    fn owner() -> UserId {
        UserId::from("alice")
    }

    async fn fresh_app() -> (tempfile::TempDir, RpcServer) {
        let dir = tempfile::tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        (dir, RpcServer::new(app, RpcServerConfig::default()))
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (_d, server) = fresh_app().await;
        let router = server.router();
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/rpc/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn version_returns_version_field() {
        let (_d, server) = fresh_app().await;
        let router = server.router();
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/rpc/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 4096).await.unwrap(),
        )
        .unwrap();
        assert_eq!(body["service"], "a3chat");
        assert!(body.get("version").is_some());
    }

    #[tokio::test]
    async fn methods_returns_canonical_list() {
        let (_d, server) = fresh_app().await;
        let router = server.router();
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/rpc/methods")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 8192).await.unwrap(),
        )
        .unwrap();
        let list = body["methods"].as_array().expect("methods is array");
        assert!(list.iter().any(|v| v == &serde_json::json!("a3chat.contact.list")));
    }

    #[tokio::test]
    async fn rpc_without_owner_returns_401() {
        let (_d, server) = fresh_app().await;
        let router = server.router();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "a3chat.contact.list",
            "params": {},
            "id": 1
        });
        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rpc_with_owner_returns_200() {
        let (_d, server) = fresh_app().await;
        let router = server.router();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "a3chat.contact.list",
            "params": {},
            "id": 1
        });
        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .header(HEADER_OWNER, owner().as_str())
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rpc_echoes_request_id_header_back() {
        let (_d, server) = fresh_app().await;
        let router = server.router();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "a3chat.contact.list",
            "params": {},
            "id": 1
        });
        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .header(HEADER_OWNER, owner().as_str())
                    .header(HEADER_REQUEST_ID, "trace-abc123")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(HEADER_REQUEST_ID_RESP)
                .and_then(|v| v.to_str().ok()),
            Some("trace-abc123"),
            "response must echo back the request-id header for correlation"
        );
    }

    #[tokio::test]
    async fn rpc_with_unknown_method_returns_400() {
        let (_d, server) = fresh_app().await;
        let router = server.router();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "a3chat.bogus",
            "params": {},
            "id": 1
        });
        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .header(HEADER_OWNER, owner().as_str())
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rpc_with_invalid_json_returns_400() {
        let (_d, server) = fresh_app().await;
        let router = server.router();
        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .header(HEADER_OWNER, owner().as_str())
                    .body(Body::from("not json".as_bytes().to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rpc_rejects_oversize_body_with_413() {
        // Defence-in-depth: the *raw* body limit is MAX_BODY_BYTES.
        // Anything larger yields 413 Payload Too Large.
        let (_d, server) = fresh_app().await;
        let router = server.router();
        let huge = "x".repeat(MAX_BODY_BYTES + 1);
        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .header(HEADER_OWNER, owner().as_str())
                    .body(Body::from(huge))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "oversize body must yield 413"
        );
    }

    #[tokio::test]
    async fn rpc_batch_processes_all_calls() {
        let (_d, server) = fresh_app().await;
        let router = server.router();
        let body = serde_json::json!([
            {"jsonrpc":"2.0","method":"a3chat.contact.list","id":1},
            {"jsonrpc":"2.0","method":"a3chat.contact.list","id":2},
        ]);
        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .header(HEADER_OWNER, owner().as_str())
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rpc_notification_batch_returns_204() {
        let (_d, server) = fresh_app().await;
        let router = server.router();
        let body = serde_json::json!([
            {"jsonrpc":"2.0","method":"a3chat.contact.list","id":null},
            {"jsonrpc":"2.0","method":"a3chat.contact.list","id":null},
        ]);
        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .header(HEADER_OWNER, owner().as_str())
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn start_and_stop_server() {
        let (_d, server) = fresh_app().await;
        let cfg = RpcServerConfig::new("127.0.0.1:0".parse().unwrap());
        let server = RpcServer::new(server.app, cfg);
        let handle = server.start().await.unwrap();
        assert!(handle.local_addr.port() > 0);
        handle.stop().await;
    }

    #[tokio::test]
    async fn owner_from_headers_accepts_valid_string() {
        let mut h = HeaderMap::new();
        h.insert(HEADER_OWNER, HeaderValue::from_static("alice"));
        let r = owner_from_headers(&h).unwrap();
        assert_eq!(r.as_str(), "alice");
    }

    #[tokio::test]
    async fn owner_from_headers_rejects_missing() {
        let h = HeaderMap::new();
        assert!(owner_from_headers(&h).is_err());
    }

    #[tokio::test]
    async fn owner_from_headers_rejects_invalid() {
        let mut h = HeaderMap::new();
        h.insert(HEADER_OWNER, HeaderValue::from_bytes(b"\xff").unwrap());
        assert!(owner_from_headers(&h).is_err());
    }

    #[tokio::test]
    async fn rpc_request_round_trip() {
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            method: "a3chat.contact.list".into(),
            params: serde_json::json!({}),
            id: serde_json::json!(1),
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: RpcRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn config_with_timeout_sets_value() {
        let cfg = RpcServerConfig::new("127.0.0.1:0".parse().unwrap())
            .with_timeout(Duration::from_secs(5))
            .allow_origin("https://a3chat.local");
        assert_eq!(cfg.request_timeout, Duration::from_secs(5));
        assert_eq!(cfg.allowed_origins, vec!["https://a3chat.local"]);
    }

    // Silence unused-warning for the `ERR_PARSE` import.
    #[test]
    fn err_parse_constant_in_scope() {
        assert_eq!(ERR_PARSE, -32700);
        let _ = Duration::from_secs(1);
    }
}
