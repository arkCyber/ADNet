//! IPC client — connect to a running `a3net daemon` over its JSON-RPC socket or HTTP endpoint.
//!
//! Supports two transports:
//! - **Unix Domain Socket** (`{data_dir}/ipc.sock`) — fastest, local-only.
//! - **HTTP/REST** (`http://host:11436/rpc`) — works over TCP, supports batch,
//!   retry, and SSE event streaming.
//!
//! # Reliability features
//!
//! - **Persistent HTTP client** — re-uses a single `reqwest::Client` so
//!   the underlying TLS/TCP connection pool is shared across calls.
//! - **Configurable retry with exponential backoff** — transient network
//!   errors (timeouts, connection refused, 5xx, 429) are retried
//!   automatically up to a configurable max-attempts count.
//! - **Env-var resolution** — `ADNET_HTTP_URL` / `ADNET_HOST` /
//!   `ADNET_HTTP_PORT` are honoured before explicit CLI flags, so shell
//!   aliases and CI scripts can route all `a3net` calls through one URL.
//! - **Batch RPC** — `call_batch` sends an array of requests in one HTTP
//!   round-trip and deserialises the array of responses.
//! - **SSE consumer** — `subscribe_events` connects to `/rpc/stream` and
//!   yields server-pushed events as they arrive.
//! - **Auto daemon discovery** — `discover_http_daemon` probes a few
//!   well-known ports on `127.0.0.1` if no URL is explicitly configured.
//!
//! # Examples
//!
//! ```no_run
//! # use a3net_cli::ipc_client::IpcClient;
//! # async fn run() -> anyhow::Result<()> {
//! let client = IpcClient::http("127.0.0.1");
//! let node = client.info().await?;
//! println!("connected to {}", node.node_id);
//! # Ok(()) }
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::RwLock;

const IPC_SOCKET_NAME: &str = "ipc.sock";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const RPC_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_HTTP_PORT: u16 = 11436;

/// Well-known ports to probe when auto-discovering a local daemon
/// (in priority order). The first one that accepts a TCP connection wins.
pub const DEFAULT_DISCOVERY_PORTS: &[u16] = &[11436, 11437, 11438, 11439];

/// HTTP status codes that are safe to retry.
fn is_retryable_status(code: u16) -> bool {
    matches!(code, 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

/// Retry / backoff configuration for [`IpcClient`] HTTP calls.
///
/// `max_attempts = 1` disables retry (single attempt, no backoff).
/// `max_attempts = 0` is treated as `1`.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub backoff_multiplier: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(150),
            max_backoff: Duration::from_secs(5),
            backoff_multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    /// Single attempt, no retry.
    pub fn no_retry() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// Compute the delay before the `n`-th retry (1-indexed).
    pub fn delay_for(&self, attempt: u32) -> Duration {
        let exp = (attempt as i32 - 1).max(0) as u32;
        let factor = self.backoff_multiplier.powi(exp as i32);
        let delay_ms = (self.initial_backoff.as_millis() as f64) * factor;
        let delay_ms = delay_ms.min(self.max_backoff.as_millis() as f64) as u64;
        Duration::from_millis(delay_ms)
    }
}

/// Transport mode for connecting to the daemon.
#[derive(Debug, Clone)]
pub enum Transport {
    /// Unix Domain Socket (default)
    UnixSocket(PathBuf),
    /// HTTP/REST endpoint
    Http(String),
}

impl Default for Transport {
    fn default() -> Self {
        Transport::UnixSocket(PathBuf::from("./.a3net-data").join(IPC_SOCKET_NAME))
    }
}

impl Transport {
    /// Create a Unix socket transport from data directory.
    pub fn unix_socket(data_dir: impl Into<PathBuf>) -> Self {
        Transport::UnixSocket(data_dir.into().join(IPC_SOCKET_NAME))
    }

    /// Create an HTTP transport with the given base URL.
    /// e.g., "http://127.0.0.1:11436"
    pub fn http(url: impl Into<String>) -> Self {
        Transport::Http(url.into())
    }

    /// Create an HTTP transport with the default port 11436.
    pub fn http_default(host: impl Into<String>) -> Self {
        Transport::Http(format!("http://{}:{}", host.into(), DEFAULT_HTTP_PORT))
    }
}

#[derive(Debug, Serialize)]
struct RpcRequest {
    jsonrpc: &'static str,
    method: String,
    params: serde_json::Value,
    id: i64,
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    #[serde(default)]
    jsonrpc: String,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<RpcError>,
    #[serde(default)]
    id: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

/// A single JSON-RPC request frame as exposed via `call_batch`.
///
/// Unlike the internal [`RpcRequest`], this lets callers pick the
/// `id` of each request so they can match responses to requests.
#[derive(Debug, Serialize)]
pub struct BatchRequest {
    pub id: serde_json::Value,
    pub method: String,
    pub params: serde_json::Value,
}

/// A single JSON-RPC response frame returned by `call_batch`.
#[derive(Debug, Deserialize, Clone)]
pub struct BatchResponse {
    pub id: serde_json::Value,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<BatchError>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BatchError {
    pub code: i64,
    pub message: String,
}

/// Unified IPC client supporting both Unix Socket and HTTP transports.
///
/// Cheap to clone (the inner `reqwest::Client` and `RetryPolicy` are
/// shared via `Arc`).
#[derive(Clone)]
pub struct IpcClient {
    transport: Transport,
    retry_policy: Arc<RetryPolicy>,
    http_client: Arc<reqwest::Client>,
    timeout: Duration,
}

impl std::fmt::Debug for IpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcClient")
            .field("transport", &self.transport)
            .field("retry_policy", &*self.retry_policy)
            .field("timeout_secs", &self.timeout.as_secs())
            .finish()
    }
}

impl IpcClient {
    /// Create a client using Unix socket (default path: {data_dir}/ipc.sock).
    pub fn connect(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            transport: Transport::unix_socket(data_dir),
            retry_policy: Arc::new(RetryPolicy::no_retry()),
            http_client: Arc::new(reqwest::Client::new()),
            timeout: RPC_TIMEOUT,
        }
    }

    /// Create a client with explicit transport.
    pub fn with_transport(transport: Transport) -> Self {
        Self {
            transport,
            retry_policy: Arc::new(RetryPolicy::no_retry()),
            http_client: Arc::new(reqwest::Client::new()),
            timeout: RPC_TIMEOUT,
        }
    }

    /// Create a client using HTTP transport with default port 11436.
    pub fn http(host: impl Into<String>) -> Self {
        Self {
            transport: Transport::http_default(host),
            retry_policy: Arc::new(RetryPolicy::default()),
            http_client: Arc::new(reqwest::Client::new()),
            timeout: RPC_TIMEOUT,
        }
    }

    /// Create a client using a specific HTTP URL.
    pub fn http_url(url: impl Into<String>) -> Self {
        Self {
            transport: Transport::Http(url.into()),
            retry_policy: Arc::new(RetryPolicy::default()),
            http_client: Arc::new(reqwest::Client::new()),
            timeout: RPC_TIMEOUT,
        }
    }

    /// Build a client honouring `ADNET_HTTP_URL` / `ADNET_HOST` /
    /// `ADNET_HTTP_PORT` env vars before falling back to the
    /// explicit args. Returns `None` if none of the env vars are set.
    pub fn from_env_or(
        data_dir: Option<&str>,
        explicit_http_host: Option<&str>,
        explicit_http_port: Option<u16>,
    ) -> Option<Self> {
        if let Ok(url) = std::env::var("ADNET_HTTP_URL") {
            if !url.trim().is_empty() {
                return Some(Self::http_url(url));
            }
        }
        if let Ok(host) = std::env::var("ADNET_HOST") {
            if !host.trim().is_empty() {
                let port = std::env::var("ADNET_HTTP_PORT")
                    .ok()
                    .and_then(|p| p.parse::<u16>().ok())
                    .or(explicit_http_port)
                    .unwrap_or(DEFAULT_HTTP_PORT);
                let url = if host.contains("://") {
                    host
                } else {
                    format!("http://{}:{}", host, port)
                };
                return Some(Self::http_url(url));
            }
        }
        if let Some(host) = explicit_http_host {
            let port = explicit_http_port.unwrap_or(DEFAULT_HTTP_PORT);
            let url = if host.contains("://") {
                host.to_string()
            } else {
                format!("http://{}:{}", host, port)
            };
            return Some(Self::http_url(url));
        }
        let dir = data_dir.unwrap_or("./.a3net-data");
        Some(Self::connect(dir))
    }

    /// Replace the retry policy. Useful for tests or operators who want
    /// to disable retry for benchmarking.
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = Arc::new(policy);
        self
    }

    /// Replace the per-request timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Get the current transport.
    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    /// Get the Unix socket path (if applicable).
    pub fn socket_path(&self) -> Option<PathBuf> {
        match &self.transport {
            Transport::UnixSocket(path) => Some(path.clone()),
            Transport::Http(_) => None,
        }
    }

    /// Get the HTTP URL (if applicable).
    pub fn as_http_url(&self) -> Option<String> {
        match &self.transport {
            Transport::UnixSocket(_) => None,
            Transport::Http(url) => Some(url.clone()),
        }
    }

    /// Check if daemon is running via Unix socket.
    pub fn is_daemon_running(&self) -> bool {
        match &self.transport {
            Transport::UnixSocket(path) => path.exists(),
            Transport::Http(_) => true, // Can't check HTTP without connecting
        }
    }

    /// Check daemon health via HTTP.
    pub async fn http_health_check(&self) -> Result<bool> {
        let url = match &self.transport {
            Transport::Http(base) => format!("{}/health", base),
            Transport::UnixSocket(_) => return Ok(false),
        };

        let resp = reqwest::get(&url).await?;
        Ok(resp.status().is_success())
    }

    /// Send a JSON-RPC request and decode the result.
    ///
    /// HTTP calls honour the configured [`RetryPolicy`] — transient
    /// connection errors and 5xx / 429 responses are retried with
    /// exponential backoff.
    pub async fn call<P: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R> {
        match &self.transport {
            Transport::UnixSocket(socket) => self.call_unix(socket, method, params).await,
            Transport::Http(base) => {
                let resp = self.call_http_with_retry(base, method, params).await?;
                serde_json::from_value(resp)
                    .with_context(|| format!("failed to parse result for {method}"))
            }
        }
    }

    /// Send a JSON-RPC request and return the raw JSON value (no
    /// second deserialisation pass). Use this when the response shape
    /// is dynamic or you want to forward it to a script.
    pub async fn call_raw<P: Serialize>(
        &self,
        method: &str,
        params: P,
    ) -> Result<serde_json::Value> {
        match &self.transport {
            Transport::UnixSocket(socket) => {
                let resp = self.call_unix_raw(socket, method, params).await?;
                Ok(resp)
            }
            Transport::Http(base) => self.call_http_with_retry(base, method, params).await,
        }
    }

    /// Send multiple JSON-RPC requests in a single HTTP round-trip.
    ///
    /// Only available over HTTP (Unix-socket mode falls back to one
    /// call per request).
    ///
    /// Returns the responses in the **same order** as the input
    /// requests. If a transport error occurs, retries are performed
    /// at the batch level (the entire batch is re-sent).
    pub async fn call_batch(
        &self,
        requests: Vec<BatchRequest>,
    ) -> Result<Vec<BatchResponse>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        match &self.transport {
            Transport::UnixSocket(_) => {
                // Fall back to sequential calls.
                let mut out = Vec::with_capacity(requests.len());
                for req in requests {
                    let resp = self.call_raw(req.method.as_str(), req.params).await;
                    match resp {
                        Ok(v) => out.push(BatchResponse {
                            id: req.id,
                            result: Some(v),
                            error: None,
                        }),
                        Err(e) => out.push(BatchResponse {
                            id: req.id,
                            result: None,
                            error: Some(BatchError {
                                code: -1,
                                message: e.to_string(),
                            }),
                        }),
                    }
                }
                Ok(out)
            }
            Transport::Http(base) => {
                let payload: Vec<serde_json::Value> = requests
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": r.method,
                            "params": r.params,
                            "id": r.id,
                        })
                    })
                    .collect();
                let url = format!("{}/rpc", base);
                let attempts = self.retry_policy.max_attempts.max(1);
                let mut last_err: Option<anyhow::Error> = None;
                for attempt in 1..=attempts {
                    let resp = self
                        .http_client
                        .post(&url)
                        .json(&payload)
                        .timeout(self.timeout)
                        .send()
                        .await;
                    match resp {
                        Ok(r) if r.status().is_success() => {
                            let body: Vec<BatchResponse> = r.json().await?;
                            return Ok(body);
                        }
                        Ok(r) if attempt < attempts && is_retryable_status(r.status().as_u16()) => {
                            last_err = Some(anyhow!("HTTP {} from daemon", r.status()));
                        }
                        Ok(r) => {
                            return Err(anyhow!(
                                "batch HTTP error: {} (after {} attempt(s))",
                                r.status(),
                                attempt
                            ));
                        }
                        Err(e) if attempt < attempts => {
                            last_err = Some(anyhow::Error::from(e));
                        }
                        Err(e) => return Err(anyhow::Error::from(e).context("batch HTTP failed")),
                    }
                    let delay = self.retry_policy.delay_for(attempt + 1);
                    tokio::time::sleep(delay).await;
                }
                Err(last_err.unwrap_or_else(|| anyhow!("batch failed after {attempts} attempts")))
            }
        }
    }

    /// Call via Unix socket.
    async fn call_unix<P: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        socket: &Path,
        method: &str,
        params: P,
    ) -> Result<R> {
        let raw = self.call_unix_raw(socket, method, params).await?;
        serde_json::from_value(raw)
            .with_context(|| format!("failed to parse result for {method}"))
    }

    /// Same as `call_unix` but returns the raw JSON value.
    async fn call_unix_raw<P: Serialize>(
        &self,
        socket: &Path,
        method: &str,
        params: P,
    ) -> Result<serde_json::Value> {
        if !socket.exists() {
            return Err(anyhow!(
                "daemon not running: {} does not exist. Start with `a3net daemon` or `a3net serve`.",
                socket.display()
            ));
        }

        let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(socket))
            .await
            .context("timeout connecting to daemon")?
            .context("failed to connect to daemon IPC socket")?;

        let req = RpcRequest {
            jsonrpc: "2.0",
            method: method.to_string(),
            params: serde_json::to_value(params)?,
            id: 1,
        };

        let req_bytes = serde_json::to_vec(&req)?;
        stream.write_all(&req_bytes).await?;
        stream.write_all(b"\n").await?;
        stream.flush().await?;

        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();

        tokio::time::timeout(RPC_TIMEOUT, reader.read_line(&mut line))
            .await
            .context("timeout waiting for daemon response")?
            .context("failed to read daemon response")?;

        let resp: RpcResponse =
            serde_json::from_str(line.trim()).context("invalid JSON-RPC response")?;

        if let Some(err) = resp.error {
            return Err(anyhow!("RPC error {}: {}", err.code, err.message));
        }

        resp.result
            .ok_or_else(|| anyhow!("missing result in RPC response"))
    }

    /// HTTP RPC call with retry / backoff. Returns the raw result value.
    async fn call_http_with_retry<P: Serialize>(
        &self,
        base: &str,
        method: &str,
        params: P,
    ) -> Result<serde_json::Value> {
        let url = format!("{}/rpc", base);

        let req = RpcRequest {
            jsonrpc: "2.0",
            method: method.to_string(),
            params: serde_json::to_value(params)?,
            id: 1,
        };

        let attempts = self.retry_policy.max_attempts.max(1);
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 1..=attempts {
            let resp = self
                .http_client
                .post(&url)
                .json(&req)
                .timeout(self.timeout)
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    let body: RpcResponse = r
                        .json()
                        .await
                        .context("invalid JSON-RPC response over HTTP")?;
                    if let Some(err) = body.error {
                        return Err(anyhow!("RPC error {}: {}", err.code, err.message));
                    }
                    return body
                        .result
                        .ok_or_else(|| anyhow!("missing result in RPC response"));
                }
                Ok(r) if attempt < attempts && is_retryable_status(r.status().as_u16()) => {
                    last_err = Some(anyhow!("HTTP {} from daemon", r.status()));
                }
                Ok(r) => {
                    return Err(anyhow!(
                        "HTTP error: {} (method={method}, attempt {attempt}/{attempts})",
                        r.status()
                    ));
                }
                Err(e) if attempt < attempts => {
                    last_err = Some(anyhow::Error::from(e));
                }
                Err(e) => {
                    return Err(anyhow::Error::from(e).context("HTTP request failed"));
                }
            }
            let delay = self.retry_policy.delay_for(attempt + 1);
            tokio::time::sleep(delay).await;
        }
        Err(last_err.unwrap_or_else(|| anyhow!("HTTP failed after {attempts} attempts")))
    }

    pub async fn call_no_params<R: for<'de> Deserialize<'de>>(&self, method: &str) -> Result<R> {
        self.call(method, serde_json::json!({})).await
    }

    pub async fn info(&self) -> Result<NodeInfo> {
        self.call_no_params("info").await
    }

    pub async fn ping(&self) -> Result<bool> {
        #[derive(Deserialize)]
        struct Response {
            ok: bool
        }
        let resp: Response = self.call("ping", serde_json::json!({})).await?;
        Ok(resp.ok)
    }

    pub async fn list_rooms(&self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Response(Vec<String>);
        let resp: Response = self.call_no_params("list_rooms").await?;
        Ok(resp.0)
    }

    pub async fn join(&self, room: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Params<'a> {
            room: &'a str
        }
        #[derive(Deserialize)]
        struct Response {}
        let _: Response = self.call("join", Params { room }).await?;
        Ok(())
    }

    pub async fn leave(&self, room: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Params<'a> {
            room: &'a str
        }
        #[derive(Deserialize)]
        struct Response {}
        let _: Response = self.call("leave", Params { room }).await?;
        Ok(())
    }

    pub async fn feed(&self, room: &str) -> Result<RoomFeed> {
        #[derive(Serialize)]
        struct Params<'a> {
            room: &'a str
        }
        self.call("feed", Params { room }).await
    }

    pub async fn peers_for(&self, hash: &str) -> Result<Vec<String>> {
        #[derive(Serialize)]
        struct Params {
            hash: String,
        }
        #[derive(Deserialize)]
        struct Response(Vec<String>);
        let resp: Response = self.call("peers_for", Params { hash: hash.to_string() }).await?;
        Ok(resp.0)
    }

    pub async fn make_ticket(&self, hash: &str) -> Result<String> {
        #[derive(Serialize)]
        struct Params {
            hash: String,
        }
        #[derive(Deserialize)]
        struct Response(String);
        let resp: Response = self.call("make_ticket", Params { hash: hash.to_string() }).await?;
        Ok(resp.0)
    }

    pub async fn announce(
        &self,
        room: &str,
        file: &str,
        title: Option<&str>,
        kind: Option<&str>,
    ) -> Result<AnnounceResponse> {
        #[derive(Serialize)]
        struct Params<'a> {
            room: &'a str,
            file: &'a str,
            title: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            kind: Option<&'a str>,
        }
        self.call(
            "announce",
            Params {
                room,
                file,
                title: title.unwrap_or("shared file"),
                kind,
            },
        )
        .await
    }

    // ── HTTP-only helpers (SSE / discovery) ─────────────────────────────────

    /// Subscribe to the daemon's Server-Sent-Events stream
    /// (`GET /rpc/stream`) and yield parsed events as they arrive.
    ///
    /// The returned stream completes when the HTTP connection drops
    /// or the daemon closes the response. The caller is responsible
    /// for breaking out of the stream when they no longer need events.
    ///
    /// Only meaningful for HTTP transports; for Unix-socket mode the
    /// caller should use the streaming notification channel exposed
    /// by the IPC adapter directly.
    pub fn subscribe_events(
        &self,
    ) -> Result<futures::stream::BoxStream<'static, SseEvent>> {
        let url = match &self.transport {
            Transport::Http(base) => format!("{}/rpc/stream", base),
            Transport::UnixSocket(_) => {
                return Err(anyhow!(
                    "subscribe_events is only supported on HTTP transport"
                ));
            }
        };

        let client = self.http_client.clone();
        let timeout = self.timeout;

        let stream = async_stream::stream! {
            let resp = match client.get(&url).timeout(timeout).send().await {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    yield SseEvent {
                        event: "error".to_string(),
                        data: serde_json::json!({"reason": format!("HTTP {}", r.status())}),
                    };
                    return;
                }
                Err(e) => {
                    yield SseEvent {
                        event: "error".to_string(),
                        data: serde_json::json!({"reason": e.to_string()}),
                    };
                    return;
                }
            };
            let mut byte_stream = resp.bytes_stream();
            let mut buffer: Vec<u8> = Vec::new();
            while let Some(chunk) = byte_stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        buffer.extend_from_slice(&bytes);
                        while let Some(pos) = find_sse_boundary(&buffer) {
                            let frame: Vec<u8> = buffer.drain(..pos + 2).collect();
                            if let Some(ev) = parse_sse_frame(&frame) {
                                yield ev;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        };
        Ok(stream.boxed())
    }

    /// Probe `127.0.0.1` on a list of well-known ports and return the
    /// first one that accepts a TCP connection and replies 200 on
    /// `/health`. Returns `None` if no daemon was found.
    pub async fn discover_http_daemon() -> Option<Self> {
        for &port in DEFAULT_DISCOVERY_PORTS {
            let url = format!("http://127.0.0.1:{}/health", port);
            if let Ok(resp) = reqwest::Client::new()
                .get(&url)
                .timeout(Duration::from_millis(500))
                .send()
                .await
            {
                if resp.status().is_success() {
                    return Some(Self::http_url(format!("http://127.0.0.1:{}", port)));
                }
            }
        }
        None
    }
}

fn find_sse_boundary(buf: &[u8]) -> Option<usize> {
    // Look for the standard SSE event terminator `\n\n`.
    buf.windows(2).position(|w| w == b"\n\n")
}

fn parse_sse_frame(frame: &[u8]) -> Option<SseEvent> {
    let text = std::str::from_utf8(frame).ok()?;
    let mut event: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim().to_string());
        }
    }
    if event.is_none() && data_lines.is_empty() {
        return None;
    }
    let data_str = data_lines.join("\n");
    let data: serde_json::Value = serde_json::from_str(&data_str)
        .unwrap_or_else(|_| serde_json::Value::String(data_str));
    Some(SseEvent {
        event: event.unwrap_or_else(|| "message".to_string()),
        data,
    })
}

/// One event parsed from the daemon's SSE stream.
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event: String,
    pub data: serde_json::Value,
}

/// Build an IpcClient from CLI arguments.
///
/// Resolution order (first hit wins):
/// 1. `--http` / `--http-port` flags.
/// 2. `ADNET_HTTP_URL` env var.
/// 3. `ADNET_HOST` + `ADNET_HTTP_PORT` env vars.
/// 4. `--data-dir` flag.
/// 5. Default `./.a3net-data`.
pub fn client_from_cli(
    data_dir: Option<&str>,
    http_host: Option<&str>,
    http_port: Option<u16>,
) -> Result<IpcClient> {
    Ok(IpcClient::from_env_or(data_dir, http_host, http_port)
        .unwrap_or_else(|| IpcClient::connect("./.a3net-data")))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnounceResponse {
    pub room: String,
    pub hash: String,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
    pub ticket: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeInfo {
    #[serde(rename = "nodeId")]
    pub node_id: String,
    #[serde(rename = "dataDir")]
    pub data_dir: Option<String>,
    #[serde(rename = "joinedRooms")]
    pub joined_rooms: Vec<String>,
    #[serde(rename = "startedAt")]
    pub started_at: Option<String>,
    pub mesh: Option<MeshInfo>,
    pub relay: Option<RelayInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MeshInfo {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RelayInfo {
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoomFeed {
    pub room: String,
    pub assets: Vec<RoomAsset>,
    #[serde(rename = "peerMap")]
    pub peer_map: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoomAsset {
    pub hash: String,
    pub title: String,
    pub kind: String,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    #[serde(rename = "sourceUrl")]
    pub source_url: Option<String>,
    #[serde(rename = "announcerNodeId")]
    pub announcer_node_id: String,
    #[serde(rename = "announcedAt")]
    pub announced_at: String,
}

pub async fn require_daemon(data_dir: &Path) -> Result<IpcClient> {
    let client = IpcClient::connect(data_dir);
    if !client.is_daemon_running() {
        return Err(anyhow!(
            "daemon not running in {}. Start with `a3net daemon` first.",
            data_dir.display()
        ));
    }
    client.info().await?;
    Ok(client)
}

// ─────────────────────────────────────────────────────────────────────────────
// Daemon discovery helper (stateful caching so the CLI doesn't probe every
// invocation). The cache is process-local and time-limited to 5 seconds.
// ─────────────────────────────────────────────────────────────────────────────

use std::sync::OnceLock;

static DISCOVERY_CACHE: OnceLock<RwLock<Option<(std::time::Instant, IpcClient)>>> = OnceLock::new();

/// Probe for a local daemon, cache the result for 5s, return the
/// [`IpcClient`] if one was found. Falls back to a Unix-socket client
/// pointing at `./.a3net-data` if no daemon is reachable.
pub async fn auto_discover_or_default() -> IpcClient {
    let cache = DISCOVERY_CACHE.get_or_init(|| RwLock::new(None));
    {
        let guard = cache.read().await;
        if let Some((when, client)) = guard.as_ref() {
            if when.elapsed() < Duration::from_secs(5) {
                return client.clone();
            }
        }
    }
    if let Some(client) = IpcClient::discover_http_daemon().await {
        let mut guard = cache.write().await;
        *guard = Some((std::time::Instant::now(), client.clone()));
        return client;
    }
    IpcClient::connect("./.a3net-data")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_construction() {
        let client = IpcClient::connect("/tmp/test-data");
        assert_eq!(
            client.socket_path(),
            Some(PathBuf::from("/tmp/test-data/ipc.sock"))
        );
    }

    #[test]
    fn http_url_construction() {
        let client = IpcClient::http("127.0.0.1");
        assert_eq!(client.as_http_url(), Some("http://127.0.0.1:11436".to_string()));
    }

    #[test]
    fn http_url_custom() {
        let client = IpcClient::http_url("http://localhost:8080");
        assert_eq!(client.as_http_url(), Some("http://localhost:8080".to_string()));
    }

    #[test]
    fn transport_unix() {
        let transport = Transport::unix_socket("/tmp/data");
        assert!(matches!(transport, Transport::UnixSocket(_)));
    }

    #[test]
    fn transport_http() {
        let transport = Transport::http("http://127.0.0.1:11436");
        assert!(matches!(transport, Transport::Http(_)));
    }

    #[test]
    fn clone_is_cheap() {
        let client = IpcClient::connect("/tmp/test");
        let _ = client.clone();
    }

    #[tokio::test]
    async fn not_running_when_missing() {
        let client = IpcClient::connect("/nonexistent/path/12345");
        assert!(!client.is_daemon_running());
    }

    #[test]
    fn default_transport_is_unix() {
        let transport = Transport::default();
        match transport {
            Transport::UnixSocket(path) => {
                assert!(path.ends_with("ipc.sock"));
            }
            Transport::Http(_) => panic!("expected UnixSocket"),
        }
    }

    #[test]
    fn retry_policy_no_retry() {
        let p = RetryPolicy::no_retry();
        assert_eq!(p.max_attempts, 1);
    }

    #[test]
    fn retry_policy_default_is_backoff() {
        let p = RetryPolicy::default();
        assert!(p.max_attempts >= 2);
        let d1 = p.delay_for(1);
        let d2 = p.delay_for(2);
        let d3 = p.delay_for(3);
        assert!(d1 <= d2);
        assert!(d2 <= d3);
        // Capped at max_backoff.
        let huge = p.delay_for(20);
        assert!(huge <= p.max_backoff);
    }

    #[test]
    fn retry_policy_is_status_retryable() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(503));
        assert!(!is_retryable_status(200));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(404));
        assert!(!is_retryable_status(401));
    }

    #[test]
    fn parse_sse_frame_simple() {
        let frame = b"event: ping\ndata: {\"a\":1}\n\n";
        let ev = parse_sse_frame(frame).expect("parsed");
        assert_eq!(ev.event, "ping");
        assert_eq!(ev.data["a"], 1);
    }

    #[test]
    fn parse_sse_frame_no_event() {
        let frame = b"data: hello\n\n";
        let ev = parse_sse_frame(frame).expect("parsed");
        assert_eq!(ev.event, "message");
        assert_eq!(ev.data, serde_json::json!("hello"));
    }

    #[test]
    fn parse_sse_frame_invalid() {
        let frame = b"\n\n";
        assert!(parse_sse_frame(frame).is_none());
    }

    #[test]
    fn sse_boundary_detected() {
        // `event: x\ndata: y\n\nrest`
        // positions:    0123456789012345678901
        // `event: x\n`  = 9 bytes (positions 0..8, '\n' at 8)
        // `data: y\n`   = 8 bytes (positions 9..16, '\n' at 16)
        // `\n`          = position 17 — together with 16 forms the `\n\n` boundary.
        // `windows(2)` returns the index of the first element of the
        // matching window, so position() returns 16.
        let buf = b"event: x\ndata: y\n\nrest";
        assert_eq!(find_sse_boundary(buf), Some(16));
    }

    #[test]
    fn sse_boundary_absent() {
        let buf = b"event: x\ndata: y\n";
        assert!(find_sse_boundary(buf).is_none());
    }

    #[test]
    fn with_retry_policy_returns_client() {
        let c = IpcClient::http("127.0.0.1").with_retry_policy(RetryPolicy::no_retry());
        assert_eq!(c.retry_policy.max_attempts, 1);
    }

    #[test]
    fn with_timeout_returns_client() {
        let c = IpcClient::http("127.0.0.1").with_timeout(Duration::from_secs(10));
        assert_eq!(c.timeout, Duration::from_secs(10));
    }

    #[test]
    fn from_env_or_returns_default_when_unset() {
        // We can't safely mutate process-wide env vars from a test (Rust
        // 2024 made set_var/remove_var `unsafe`), so this test only
        // asserts the unhappy path when the env vars happen to be
        // unset. If a CI runner has them set, this test is skipped.
        if std::env::var_os("ADNET_HTTP_URL").is_some() || std::env::var_os("ADNET_HOST").is_some() {
            return;
        }
        let c = IpcClient::from_env_or(Some("/tmp/data"), None, None).unwrap();
        assert!(matches!(c.transport, Transport::UnixSocket(_)));
    }

    #[test]
    fn from_env_or_honours_a3net_http_url() {
        if std::env::var_os("ADNET_HTTP_URL").is_none() {
            return; // Can't reliably mutate env in tests; skip if unset.
        }
        let c = IpcClient::from_env_or(Some("/tmp/data"), None, None).unwrap();
        assert!(c.as_http_url().is_some(), "should use HTTP transport when ADNET_HTTP_URL is set");
    }

    #[tokio::test]
    async fn call_batch_empty_returns_empty() {
        let c = IpcClient::http("127.0.0.1");
        let out = c.call_batch(Vec::new()).await.unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn default_port_constant_is_11436() {
        assert_eq!(DEFAULT_HTTP_PORT, 11436);
    }

    #[test]
    fn transport_debug_does_not_panic() {
        let c = IpcClient::http("127.0.0.1");
        let s = format!("{c:?}");
        assert!(s.contains("IpcClient"));
        assert!(s.contains("11436"));
    }
}
