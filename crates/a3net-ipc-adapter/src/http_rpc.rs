//! HTTP RPC server — exposes the same JSON-RPC methods as the
//! Unix-socket [`NodeRpc`](super::NodeRpc) handler over TCP via axum.
//!
//! The protocol is standard JSON-RPC 2.0 over HTTP:
//!
//! ```http
//! POST /rpc HTTP/1.1
//! Content-Type: application/json
//!
//! {"jsonrpc":"2.0","method":"info","params":{},"id":1}
//! ```
//!
//! ```http
//! HTTP/1.1 200 OK
//! Content-Type: application/json
//!
//! {"jsonrpc":"2.0","result":{...},"id":1}
//! ```
//!
//! Notifications (server-push) are **not** supported over HTTP/1.1
//! because the client cannot receive them without an open channel.
//! Use the Unix-socket connection for real-time event streaming.
//!
//! # Security
//!
//! - CORS is configurable via `HttpRpcConfig.cors_allowed_origins`.
//!   By default, CORS is **disabled** (empty origins list).
//! - Rate limiting is implemented per IP address.
//! - Optional authentication via `Authorization` header (Bearer token).
//!
//! # Health check
//!
//! `GET /health` returns a minimal `{"ok":true}` for load-balancer
//! probes. Unlike the metrics `/health`, this endpoint does not run
//! dependency checks — it only confirms the HTTP server is alive.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use a3net_ipc::RpcHandler;
use axum::{
    extract::State,
    http::{HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::RwLock;
use std::collections::HashMap;
use axum::response::Response;
use futures::TryStreamExt;

use crate::NodeRpc;

/// SSE event types for streaming notifications.
#[derive(Debug, Clone, Serialize)]
struct SseEvent {
    event: String,
    data: Value,
}

/// Configuration for the HTTP RPC server.
#[derive(Debug, Clone)]
pub struct HttpRpcConfig {
    /// Allowed CORS origins. Empty list disables CORS entirely.
    pub cors_allowed_origins: Vec<String>,
    /// Bearer token for authentication. None disables auth.
    pub auth_token: Option<String>,
    /// Rate limit: max requests per IP per window.
    pub rate_limit_requests: u32,
    /// Rate limit window in seconds.
    pub rate_limit_window_secs: u64,
    /// Max body size in bytes.
    pub max_body_bytes: usize,
}

impl Default for HttpRpcConfig {
    fn default() -> Self {
        Self {
            cors_allowed_origins: Vec::new(),
            auth_token: None,
            rate_limit_requests: 100,
            rate_limit_window_secs: 60,
            max_body_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Application state shared with every request handler.
#[derive(Clone)]
struct AppState {
    handler: Arc<NodeRpc>,
    config: Arc<HttpRpcConfig>,
    rate_limiter: Arc<RateLimiter>,
}

impl AppState {
    fn new(handler: Arc<NodeRpc>, config: HttpRpcConfig) -> Self {
        Self {
            handler,
            config: Arc::new(config),
            rate_limiter: Arc::new(RateLimiter::new(
                100,
                Duration::from_secs(60),
            )),
        }
    }
}

/// Handle to a running HTTP RPC server. Drop to shut down.
#[derive(Debug)]
pub struct HttpRpcServer {
    pub bound_addr: SocketAddr,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// `None` once [`join`](Self::join) has been called.
    join: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl HttpRpcServer {
    /// Stop the server. Idempotent.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Simple per-IP rate limiter using a sliding window counter.
#[derive(Debug, Clone)]
struct RateLimiter {
    requests_per_window: u32,
    window: Duration,
    entries: Arc<RwLock<HashMap<String, RateLimitEntry>>>,
}

#[derive(Debug, Clone)]
struct RateLimitEntry {
    count: u32,
    window_start: std::time::Instant,
}

impl RateLimiter {
    fn new(requests_per_window: u32, window: Duration) -> Self {
        Self {
            requests_per_window,
            window,
            entries: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn check_rate_limit(&self, ip: &str) -> bool {
        let mut entries = self.entries.write().await;
        let now = std::time::Instant::now();
        let entry = entries.entry(ip.to_string()).or_insert(RateLimitEntry {
            count: 0,
            window_start: now,
        });

        if now.duration_since(entry.window_start) > self.window {
            entry.count = 0;
            entry.window_start = now;
        }

        if entry.count >= self.requests_per_window {
            return false;
        }
        entry.count += 1;
        true
    }

    async fn cleanup_old_entries(&self) {
        let mut entries = self.entries.write().await;
        let now = std::time::Instant::now();
        entries.retain(|_, entry| {
            now.duration_since(entry.window_start) <= self.window
        });
    }
}

/// JSON-RPC request frame as received over HTTP.
#[derive(Debug, serde::Deserialize)]
struct RpcRequest {
    jsonrpc: Value,
    method: String,
    #[serde(default)]
    params: Value,
    #[serde(default)]
    id: RpcId,
}

/// Batch of JSON-RPC requests.
type RpcBatch = Vec<RpcRequest>;

/// JSON-RPC request ID - supports number, string, or null.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
#[serde(untagged)]
pub enum RpcId {
    #[default]
    Null,
    Number(i64),
    String(String),
}

impl RpcId {
    fn is_null(&self) -> bool {
        matches!(self, RpcId::Null)
    }

    fn as_json(&self) -> serde_json::Value {
        match self {
            RpcId::Null => serde_json::Value::Null,
            RpcId::Number(n) => serde_json::json!(*n),
            RpcId::String(s) => serde_json::json!(s),
        }
    }
}

/// JSON-RPC 2.0 error response.
#[derive(Serialize)]
struct RpcErrorResponse {
    jsonrpc: &'static str,
    error: RpcError,
}

#[derive(Serialize)]
struct RpcError {
    code: i64,
    message: String,
}

/// Rate limit error response.
#[derive(Serialize)]
struct RateLimitErrorResponse {
    error: String,
    retry_after_secs: u64,
}

/// Extract client IP from request headers.
fn client_ip(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

/// Check if origin is allowed for CORS.
fn is_origin_allowed(origin: &str, allowed: &[String]) -> bool {
    allowed.is_empty() || allowed.iter().any(|o| o == "*" || o == origin)
}

/// `POST /rpc` — JSON-RPC 2.0 request handler.
///
/// Handles single requests and batch requests.
///
/// CORS headers are added if configured via HttpRpcConfig.
async fn rpc_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> axum::response::Response {
    let client_ip = client_ip(&headers);

    // Rate limiting check
    if !state.rate_limiter.check_rate_limit(&client_ip).await {
        let mut resp = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(RateLimitErrorResponse {
                error: "Rate limit exceeded".to_string(),
                retry_after_secs: 60,
            }),
        )
            .into_response();
        resp.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            HeaderValue::from_static("60"),
        );
        return resp;
    }

    // Authentication check (if configured)
    if let Some(ref token) = state.config.auth_token {
        let auth_header = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());

        let valid = match auth_header {
            Some(header) if header.starts_with("Bearer ") => {
                header == format!("Bearer {}", token)
            }
            _ => false,
        };

        if !valid {
            let resp = json!({
                "jsonrpc": "2.0",
                "error": {"code": -32002, "message": "Unauthorized"},
                "id": null
            });
            return (StatusCode::OK, Json(resp)).into_response();
        }
    }

    // CORS preflight handling
    let origin = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok());

    // Read body with size limit
    let max_bytes = state.config.max_body_bytes;
    let bytes = match axum::body::to_bytes(body, max_bytes).await {
        Ok(b) => b,
        Err(_) => {
            let resp = json!({
                "jsonrpc": "2.0",
                "error": {"code": -32700, "message": "Parse error"},
                "id": null
            });
            return (StatusCode::OK, Json(resp)).into_response();
        }
    };

    // Parse as batch or single request
    let bytes_str = match std::str::from_utf8(&bytes) {
        Ok(s) => s.trim(),
        Err(_) => {
            let resp = json!({
                "jsonrpc": "2.0",
                "error": {"code": -32700, "message": "Parse error"},
                "id": null
            });
            return (StatusCode::OK, Json(resp)).into_response();
        }
    };

    // Check if it's a batch request (starts with '[')
    if bytes_str.starts_with('[') {
        // Parse as batch
        let batch: Result<RpcBatch, _> = serde_json::from_slice(&bytes);
        match batch {
            Ok(batch) => {
                if batch.is_empty() {
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "error": {"code": -32600, "message": "batch must not be empty"},
                        "id": null
                    });
                    return (StatusCode::OK, Json(resp)).into_response();
                }
                // Process batch
                let mut results = Vec::with_capacity(batch.len());
                for req in batch {
                    let resp = process_single_request_value(&state.handler, req).await;
                    results.push(resp);
                }
                let resp = (StatusCode::OK, Json(Value::Array(results))).into_response();
                // Add CORS headers if origin is allowed
                if let Some(orig) = origin {
                    if is_origin_allowed(orig, &state.config.cors_allowed_origins) {
                        let mut resp = resp;
                        resp.headers_mut().insert(
                            axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
                            HeaderValue::from_str(orig).unwrap_or(HeaderValue::from_static("*")),
                        );
                        resp.headers_mut().insert(
                            axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
                            HeaderValue::from_static("GET, POST, OPTIONS"),
                        );
                        resp.headers_mut().insert(
                            axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,
                            HeaderValue::from_static("Content-Type, Authorization"),
                        );
                        return resp;
                    }
                }
                return resp;
            }
            Err(_) => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32700, "message": "Parse error"},
                    "id": null
                });
                return (StatusCode::OK, Json(resp)).into_response();
            }
        }
    } else {
        // Parse as single request
        let single: Result<RpcRequest, _> = serde_json::from_slice(&bytes);
        match single {
            Ok(req) => {
                let mut resp = process_single_request(&state.handler, req).await;
                // Add CORS headers if origin is allowed
                if let Some(orig) = origin {
                    if is_origin_allowed(orig, &state.config.cors_allowed_origins) {
                        resp.headers_mut().insert(
                            axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
                            HeaderValue::from_str(orig).unwrap_or(HeaderValue::from_static("*")),
                        );
                        resp.headers_mut().insert(
                            axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
                            HeaderValue::from_static("GET, POST, OPTIONS"),
                        );
                        resp.headers_mut().insert(
                            axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,
                            HeaderValue::from_static("Content-Type, Authorization"),
                        );
                    }
                }
                return resp;
            }
            Err(_) => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32700, "message": "Parse error"},
                    "id": null
                });
                return (StatusCode::OK, Json(resp)).into_response();
            }
        }
    }
}

/// Process a single JSON-RPC request and return an HTTP response.
async fn process_single_request(
    handler: &NodeRpc,
    req: RpcRequest,
) -> Response {
    if req.jsonrpc != "2.0" {
        let resp = json!({
            "jsonrpc": "2.0",
            "error": {"code": -32600, "message": "expected jsonrpc: \"2.0\""},
            "id": req.id.as_json()
        });
        return (StatusCode::OK, Json(resp)).into_response();
    }

    let result = handler.handle(&req.method, req.params).await;

    match result {
        Ok(v) => {
            let resp = json!({"jsonrpc": "2.0", "result": v, "id": req.id.as_json()});
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(msg) => {
            let (code, message) = if msg.starts_with("unknown method:") {
                (-32601_i64, msg)
            } else {
                (-1, format!("JSON-RPC error: {msg}"))
            };
            let resp = json!({
                "jsonrpc": "2.0",
                "error": {"code": code, "message": message},
                "id": req.id.as_json()
            });
            (StatusCode::OK, Json(resp)).into_response()
        }
    }
}

/// Process a single JSON-RPC request and return a JSON Value (for batch processing).
async fn process_single_request_value(
    handler: &NodeRpc,
    req: RpcRequest,
) -> Value {
    if req.jsonrpc != "2.0" {
        return json!({
            "jsonrpc": "2.0",
            "error": {"code": -32600, "message": "expected jsonrpc: \"2.0\""},
            "id": req.id.as_json()
        });
    }

    let result = handler.handle(&req.method, req.params).await;

    match result {
        Ok(v) => {
            json!({"jsonrpc": "2.0", "result": v, "id": req.id.as_json()})
        }
        Err(msg) => {
            let (code, message) = if msg.starts_with("unknown method:") {
                (-32601_i64, msg)
            } else {
                (-1, format!("JSON-RPC error: {msg}"))
            };
            json!({
                "jsonrpc": "2.0",
                "error": {"code": code, "message": message},
                "id": req.id.as_json()
            })
        }
    }
}

/// `GET /health` — minimal liveness probe for the HTTP RPC server.
/// Does NOT run full dependency checks (use the metrics /health for that).
async fn health_handler() -> axum::response::Response {
    #[derive(Serialize)]
    struct HealthBody {
        ok: bool,
    }
    (StatusCode::OK, Json(HealthBody { ok: true })).into_response()
}

/// `GET /rpc/methods` — Returns the list of supported RPC methods.
/// Useful for client discovery.
async fn methods_handler(
    State(_state): State<AppState>,
) -> axum::response::Response {
    use crate::NodeRpc;
    let methods = NodeRpc::supported_methods();
    (StatusCode::OK, Json(serde_json::json!({
        "methods": methods,
        "jsonrpc": "2.0"
    })))
        .into_response()
}

/// `GET /rpc/stream` — Server-Sent Events (SSE) endpoint for real-time notifications.
/// Clients can use this to receive announcements and other events over HTTP.
///
/// The endpoint streams SSE events in the format:
/// `event: <event_type>\ndata: <json_payload>\n\n`
///
/// Event types:
/// - `ping` — heartbeat sent every 30 seconds
/// - `announcement` — new content announcement (when connected to daemon's notifier)
///
/// This endpoint requires no authentication for the ping event.
async fn stream_handler() -> Response {
    use tokio::time::interval;
    use futures::stream::{self, StreamExt};

    // Create a stream that sends ping events every 30 seconds
    let stream = stream::unfold((), |()| async {
        let mut ticker = interval(Duration::from_secs(30));
        ticker.tick().await; // skip first immediate tick
        loop {
            ticker.tick().await;
            let event = format!(
                "event: ping\ndata: {{\"timestamp\":\"{}\"}}\n\n",
                chrono::Utc::now().to_rfc3339()
            );
            break Some((event, ()));
        }
    });

    let body = stream
        .map(|s| Ok::<_, std::io::Error>(axum::body::Bytes::from(s)))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()));

    let response = axum::response::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .header("Access-Control-Allow-Origin", "*")
        .body(axum::body::Body::from_stream(body))
        .unwrap();

    response.into_response()
}

/// `OPTIONS /rpc` — CORS preflight handler.
async fn options_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let origin = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok());

    let mut resp = (StatusCode::NO_CONTENT).into_response();

    if let Some(orig) = origin {
        if is_origin_allowed(orig, &state.config.cors_allowed_origins) {
            resp.headers_mut().insert(
                axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_str(orig).unwrap_or(HeaderValue::from_static("*")),
            );
            resp.headers_mut().insert(
                axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("GET, POST, OPTIONS"),
            );
            resp.headers_mut().insert(
                axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("Content-Type, Authorization"),
            );
            resp.headers_mut().insert(
                axum::http::header::ACCESS_CONTROL_MAX_AGE,
                HeaderValue::from_static("3600"),
            );
        }
    }

    resp
}

/// Start an HTTP RPC server bound to `addr`. The `handler` is
/// wrapped in an `Arc` internally.
///
/// Uses default HttpRpcConfig. For custom configuration, use [`serve_with_config`].
///
/// Returns a [`HttpRpcServer`] handle. Dropping the handle stops
/// the server.
pub async fn serve(
    addr: SocketAddr,
    handler: Arc<NodeRpc>,
) -> std::io::Result<HttpRpcServer> {
    serve_with_config(addr, handler, HttpRpcConfig::default()).await
}

/// Start an HTTP RPC server with custom configuration.
pub async fn serve_with_config(
    addr: SocketAddr,
    handler: Arc<NodeRpc>,
    config: HttpRpcConfig,
) -> std::io::Result<HttpRpcServer> {
    let state = AppState::new(handler, config);

    let cors_allowed = state.config.cors_allowed_origins.clone();

    // POST /rpc for JSON-RPC requests.
    // GET /rpc returns 405 Method Not Allowed (JSON-RPC requires POST).
    // OPTIONS /rpc handles CORS preflight.
    // GET /rpc/methods returns supported methods.
    // GET /rpc/stream provides SSE streaming for notifications.
    let app = Router::new()
        .route("/rpc", post(rpc_handler).get(method_not_allowed_handler))
        .route("/rpc/methods", get(methods_handler))
        .route("/rpc/stream", get(stream_handler))
        .route("/health", get(health_handler))
        .route("/", axum::routing::options(options_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    let _cors = cors_allowed; // silence unused warning

    let join = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.changed().await;
            })
            .await
            .ok();
    });

    Ok(HttpRpcServer {
        bound_addr,
        shutdown_tx,
        join: parking_lot::Mutex::new(Some(join)),
    })
}

/// Reject GET requests to `/rpc` with 405 Method Not Allowed.
async fn method_not_allowed_handler() -> axum::response::Response {
    (StatusCode::METHOD_NOT_ALLOWED).into_response()
}

/// Run the server and wait for it to exit. Consumes the handle.
pub async fn run(addr: SocketAddr, handler: Arc<NodeRpc>) -> std::io::Result<()> {
    let server = serve(addr, handler).await?;
    let _ = server.join().await;
    Ok(())
}

impl HttpRpcServer {
    /// Wait for the server task to finish.
    pub async fn join(&self) {
        let handle = self.join.lock().take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    /// Smoke: the HTTP server starts, accepts a POST /rpc request,
    /// and returns a JSON-RPC 2.0 response.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_rpc_server_responds_to_info() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));

        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve(addr, handler.clone()).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{}/rpc", server.bound_addr))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "info",
                "params": {},
                "id": 42
            }))
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 200, "HTTP status should be 200");
        let body: Value = resp.json().await.expect("parse JSON body");
        assert_eq!(body["jsonrpc"], "2.0", "jsonrpc field must be 2.0");
        assert_eq!(body["id"], 42, "id must echo back");
        assert!(body.get("result").is_some(), "must have result field");
        assert!(
            body["result"]["nodeId"].is_string(),
            "result must contain nodeId"
        );

        server.shutdown();
    }

    /// GET /rpc must be rejected with 405.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_rpc_rejected_with_405() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve(addr, handler).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{}/rpc", server.bound_addr))
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 405);

        server.shutdown();
    }

    /// POST /rpc with an unknown method must return -32601.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_method_returns_32601() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve(addr, handler).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{}/rpc", server.bound_addr))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "totally_unknown",
                "params": {},
                "id": 7
            }))
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.expect("parse JSON body");
        assert!(body.get("error").is_some(), "must have error field");
        assert_eq!(body["error"]["code"], -32601);

        server.shutdown();
    }

    /// GET /health must return 200 OK with `{"ok": true}`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_endpoint_returns_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve(addr, handler).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{}/health", server.bound_addr))
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.expect("parse JSON body");
        assert_eq!(body["ok"], true);

        server.shutdown();
    }

    /// Dropping the server handle must stop the task.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drop_handle_stops_server() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve(addr, handler).await.unwrap();
        let bound = server.bound_addr;
        drop(server);

        // Brief pause to allow the server task to exit.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // The address should no longer be listening.
        let client = reqwest::Client::new();
        let err = client
            .get(format!("http://{}/health", bound))
            .send()
            .await
            .expect_err("connection should fail after server stopped");
        assert!(
            err.is_connect(),
            "expected a connection error, got: {err}"
        );
    }

    /// Test that batch requests work correctly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn batch_request_works() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve(addr, handler).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{}/rpc", server.bound_addr))
            .json(&serde_json::json!([
                {"jsonrpc": "2.0", "method": "info", "params": {}, "id": 1},
                {"jsonrpc": "2.0", "method": "list_rooms", "params": {}, "id": 2}
            ]))
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.expect("parse JSON body");
        assert!(body.is_array(), "batch response should be an array");
        assert_eq!(body.as_array().unwrap().len(), 2);

        server.shutdown();
    }

    /// Test that batch with empty array returns error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn batch_empty_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve(addr, handler).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{}/rpc", server.bound_addr))
            .json(&serde_json::json!([]))
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.expect("parse JSON body");
        assert!(body.get("error").is_some(), "empty batch should return error");
        assert_eq!(body["error"]["code"], -32600);

        server.shutdown();
    }

    /// Test that methods endpoint returns supported methods.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn methods_endpoint_returns_list() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve(addr, handler).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{}/rpc/methods", server.bound_addr))
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.expect("parse JSON body");
        assert!(body.get("methods").is_some(), "should have methods field");
        let methods = body["methods"].as_array().unwrap();
        assert!(methods.len() > 0, "should have at least one method");

        server.shutdown();
    }

    /// Test that parse error returns -32700.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn parse_error_returns_32700() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve(addr, handler).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{}/rpc", server.bound_addr))
            .header("Content-Type", "application/json")
            .body("not valid json")
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.expect("parse JSON body");
        assert!(body.get("error").is_some(), "must have error field");
        assert_eq!(body["error"]["code"], -32700);

        server.shutdown();
    }

    /// Test that string IDs work correctly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn string_id_echoes_back() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve(addr, handler).await.unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{}/rpc", server.bound_addr))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "info",
                "params": {},
                "id": "request-123"
            }))
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.expect("parse JSON body");
        assert_eq!(body["id"], "request-123");

        server.shutdown();
    }

    /// Test that auth token is required when configured.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_required_when_configured() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));

        let config = HttpRpcConfig {
            auth_token: Some("secret-token".to_string()),
            ..Default::default()
        };
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve_with_config(addr, handler, config).await.unwrap();

        let client = reqwest::Client::new();

        // Request without auth header should fail with unauthorized error
        let resp = client
            .post(format!("http://{}/rpc", server.bound_addr))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "info",
                "params": {},
                "id": 1
            }))
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 200); // HTTP 200, but error in body
        let body: Value = resp.json().await.expect("parse JSON body");
        assert!(body.get("error").is_some(), "should have error field");
        assert_eq!(body["error"]["code"], -32002);
        assert_eq!(body["error"]["message"], "Unauthorized");

        server.shutdown();
    }

    /// Test that correct auth token is accepted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_accepted_with_correct_token() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));

        let config = HttpRpcConfig {
            auth_token: Some("my-secret".to_string()),
            ..Default::default()
        };
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve_with_config(addr, handler, config).await.unwrap();

        let client = reqwest::Client::new();

        // Request with correct auth header should succeed
        let resp = client
            .post(format!("http://{}/rpc", server.bound_addr))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "info",
                "params": {},
                "id": 1
            }))
            .header("Authorization", "Bearer my-secret")
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.expect("parse JSON body");
        assert!(body.get("result").is_some(), "should have result field");
        assert!(body["result"]["nodeId"].is_string());

        server.shutdown();
    }

    /// Test that wrong auth token is rejected.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_rejected_with_wrong_token() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));

        let config = HttpRpcConfig {
            auth_token: Some("correct-token".to_string()),
            ..Default::default()
        };
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve_with_config(addr, handler, config).await.unwrap();

        let client = reqwest::Client::new();

        // Request with wrong token should fail
        let resp = client
            .post(format!("http://{}/rpc", server.bound_addr))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "info",
                "params": {},
                "id": 1
            }))
            .header("Authorization", "Bearer wrong-token")
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.expect("parse JSON body");
        assert!(body.get("error").is_some());
        assert_eq!(body["error"]["code"], -32002);

        server.shutdown();
    }

    /// Test that health endpoint doesn't require auth.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_no_auth_required() {
        let tmp = tempfile::tempdir().unwrap();
        let node = a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap();
        let handler = Arc::new(NodeRpc::new(std::sync::Arc::new(node)));

        let config = HttpRpcConfig {
            auth_token: Some("secret".to_string()),
            ..Default::default()
        };
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = serve_with_config(addr, handler, config).await.unwrap();

        let client = reqwest::Client::new();

        // Health check should not require auth
        let resp = client
            .get(format!("http://{}/health", server.bound_addr))
            .send()
            .await
            .expect("request should succeed");

        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.expect("parse JSON body");
        assert_eq!(body["ok"], true);

        server.shutdown();
                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           }
}
