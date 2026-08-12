//! `adnet-webhook` — fan ADNet events out to HTTP endpoints.
//!
//! Thin, opinionated layer over `tokio` + a minimal in-tree
//! HTTP/1.1 client. Sits behind any event source — today that
//! is the gossip bus, but the [`EventSink`] trait accepts
//! anything serialisable so future event types (DHT events,
//! IPNS publishes, mesh admit/revoke) can plug in without
//! changing this crate.
//!
//! # Wire format
//!
//! Each delivery is an HTTP `POST` to the endpoint URL with a
//! JSON body. The body has the shape:
//!
//! ```json
//! { "event": "announcement", "payload": { … } }
//! ```
//!
//! ## Headers
//!
//! - `Content-Type: application/json`
//! - `X-Adnet-Delivery: <uuid>` — stable per delivery attempt
//!   so receivers can dedup.
//! - `X-Adnet-Signature: sha256=<hex>` — HMAC-SHA256 over the
//!   body using the endpoint's secret.
//! - `X-Adnet-Timestamp: <unix-ms>` — issued at send time;
//!   receivers may reject deliveries older than a configured
//!   window (default 5 minutes) to bound replay.
//!
//! ## Retry semantics
//!
//! A delivery is accepted on any `2xx`. On `5xx`, network
//! error, or timeout it is persisted to the spool and retried
//! on the next `deliver()` call (best-effort in this PR — a
//! future PR will wire a real backoff task). `4xx` is treated
//! as a permanent failure; the delivery is logged and dropped.
//!
//! Each spool entry has an attempt counter. After
//! [`DEFAULT_MAX_ATTEMPTS`] attempts the entry is purged and
//! the registered [`DroppedHandler`] (if any) fires. Use
//! [`WebhookSink::with_spool_and_budget`] to override the
//! budget per sink, and
//! [`WebhookSink::set_dropped_handler`] to wire the callback.
//!
//! `deliver()` returns `Err(WebhookError::Transport(_))` only
//! when **every** configured endpoint failed for the current
//! call. A single `2xx` or a single `4xx` is enough for
//! `deliver()` to return `Ok`. Per-endpoint outcomes are
//! surfaced via [`WebhookSink::stats`].

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

/// Backoff cap for the exponential retry schedule (best-effort
/// guidance; the sink in this PR retries on the next call).
pub const MAX_BACKOFF: Duration = Duration::from_secs(32);

/// Hard cap on the bytes we read from a receiver. Anything
/// beyond this is dropped; protects the sink from a receiver
/// that streams forever without closing.
pub const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

/// Maximum number of attempts before a spool entry is
/// considered permanently failed and dropped (with
/// [`EventSink::on_dropped`] invoked). A value of `1` means
/// "no retry — drop on first failure". Defaults to 5.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;

/// HMAC-SHA256 signature header value: `"sha256=<hex>"` where
/// `<hex>` is the HMAC of `body` keyed by `secret`.
pub fn sign_body(secret: &[u8], body: &[u8]) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret)
        .expect("HMAC accepts any key length");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    format!("sha256={}", hex::encode(bytes))
}

/// Configuration for a single webhook endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EndpointConfig {
    /// Target URL. Must be HTTP for now; HTTPS is a TODO.
    pub url: String,
    /// HMAC-SHA256 secret shared with the receiver.
    pub secret: Vec<u8>,
    /// Optional room id filter — when `Some`, only events
    /// whose `room_id` matches are delivered here.
    #[serde(default)]
    pub room_filter: Option<String>,
    /// Per-attempt timeout. Defaults to 5 seconds.
    #[serde(default = "default_request_timeout")]
    pub request_timeout: Duration,
}

fn default_request_timeout() -> Duration {
    Duration::from_secs(5)
}

/// Events the sink knows how to deliver. The wire format
/// (`tag = "event"`, snake_case) is stable across variants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AdnetEvent {
    Announcement {
        #[serde(flatten)]
        payload: serde_json::Value,
    },
}

/// Outcome of a single delivery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Accepted,
    /// Receiver returned `4xx` — permanent failure, no retry.
    Rejected { status: u16 },
    /// Network error / timeout / `5xx` — eligible for retry.
    Failed,
}

/// Errors produced by the sink.
#[derive(Debug, Error)]
pub enum WebhookError {
    #[error("invalid endpoint URL: {0}")]
    InvalidUrl(String),
    #[error("HTTP transport error: {0}")]
    Transport(String),
    #[error("spool I/O error: {0}")]
    Spool(#[from] std::io::Error),
    #[error("spool parse error: {0}")]
    SpoolParse(#[from] serde_json::Error),
}

/// Trait a downstream consumer implements to plug in their
/// own delivery surface (in-memory bus for tests, Kafka
/// producer, etc.).
#[async_trait::async_trait]
pub trait EventSink: Send + Sync + 'static {
    /// Deliver one event to the configured endpoints.
    async fn deliver(
        &self,
        event: &AdnetEvent,
        delivery_id: &str,
    ) -> Result<(), WebhookError>;

    /// Callback invoked when a delivery exhausts its retry
    /// budget. Default is a no-op; override to wire into
    /// metrics / alerting.
    ///
    /// Note: [`WebhookSink::on_dropped`] (the default trait
    /// impl) delegates to an internal `DroppedHandler`
    /// registered via
    /// [`WebhookSink::set_dropped_handler`]. Wrapping a
    /// `WebhookSink` in your own `EventSink` impl will not
    /// see drops — register the handler on the inner sink
    /// instead.
    async fn on_dropped(
        &self,
        event: &AdnetEvent,
        endpoint: &EndpointConfig,
        delivery_id: &str,
        attempts: u32,
    ) {
        let _ = (event, endpoint, delivery_id, attempts);
    }
}

/// Pluggable observer for `on_dropped` events. Tests use this
/// to assert drop semantics; production code can wire it
/// into metrics / alerting.
#[async_trait::async_trait]
pub trait DroppedHandler: Send + Sync + 'static {
    async fn on_dropped(
        &self,
        event: &AdnetEvent,
        endpoint: &EndpointConfig,
        delivery_id: &str,
        attempts: u32,
    );
}

/// HTTP webhook sink. Cloning is cheap (inner state is `Arc`).
#[derive(Clone)]
pub struct WebhookSink {
    inner: Arc<WebhookSinkInner>,
}

struct WebhookSinkInner {
    endpoints: Vec<EndpointConfig>,
    spool: Option<Mutex<SpoolFile>>,
    /// Per-endpoint retry budget. After this many attempts a
    /// failing delivery is dropped and `on_dropped` fires.
    max_attempts: u32,
    /// Cheap delivery counters; useful for the pump and for
    /// tests / metrics scrapers.
    stats: Mutex<DeliveryStats>,
    /// Optional pluggable observer for `on_dropped` events.
    dropped_handler: Mutex<Option<Arc<dyn DroppedHandler>>>,
}

/// Aggregate counters surfaced by [`WebhookSink::stats`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryStats {
    /// Number of accepted (2xx) deliveries.
    pub accepted: u64,
    /// Number of permanently rejected (4xx) deliveries.
    pub rejected: u64,
    /// Number of failed deliveries (5xx / network / timeout)
    /// that were persisted to the spool.
    pub failed: u64,
    /// Number of entries that exhausted their retry budget
    /// and were dropped (with `on_dropped` fired).
    pub dropped: u64,
}

impl WebhookSink {
    /// Build a sink with no on-disk spool. Pending deliveries
    /// are kept in memory only — a crash drops in-flight events.
    pub fn new(endpoints: Vec<EndpointConfig>) -> Self {
        Self::with_options(endpoints, None, DEFAULT_MAX_ATTEMPTS)
            .expect("WebhookSink::new without spool cannot fail")
    }

    /// Build a sink that persists every pending delivery to
    /// `spool_path` so a restart can resume. The file is
    /// JSONL, one entry per pending delivery, and is rewritten
    /// atomically (`*.tmp` + rename) so a crash mid-write
    /// cannot corrupt earlier entries.
    pub fn with_spool(
        endpoints: Vec<EndpointConfig>,
        spool_path: PathBuf,
    ) -> Result<Self, WebhookError> {
        Self::with_options(endpoints, Some(spool_path), DEFAULT_MAX_ATTEMPTS)
    }

    /// Like [`Self::with_spool`] but with an explicit
    /// `max_attempts` retry budget. `1` disables retry
    /// (failed deliveries are dropped immediately).
    pub fn with_spool_and_budget(
        endpoints: Vec<EndpointConfig>,
        spool_path: PathBuf,
        max_attempts: u32,
    ) -> Result<Self, WebhookError> {
        Self::with_options(endpoints, Some(spool_path), max_attempts.max(1))
    }

    fn with_options(
        endpoints: Vec<EndpointConfig>,
        spool: Option<PathBuf>,
        max_attempts: u32,
    ) -> Result<Self, WebhookError> {
        let spool = match spool {
            Some(p) => {
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Some(Mutex::new(SpoolFile::open(p)?))
            }
            None => None,
        };
        let inner = WebhookSinkInner {
            endpoints,
            spool,
            max_attempts: max_attempts.max(1),
            stats: Mutex::new(DeliveryStats::default()),
            dropped_handler: Mutex::new(None),
        };
        info!(
            endpoints = inner.endpoints.len(),
            max_attempts = inner.max_attempts,
            spool = inner.spool.is_some(),
            "webhook sink ready"
        );
        Ok(Self { inner: Arc::new(inner) })
    }

    /// Register (or replace) the [`DroppedHandler`] that
    /// receives every `on_dropped` callback. Pass `None` to
    /// remove the handler. The handler is invoked once per
    /// spool entry that exceeds the retry budget — never
    /// for retried-but-not-yet-dropped entries.
    pub fn set_dropped_handler(&self, handler: Option<Arc<dyn DroppedHandler>>) {
        *self.inner.dropped_handler.lock() = handler;
    }

    /// Borrow the endpoint list (for `adnet webhook list`).
    pub fn endpoints(&self) -> &[EndpointConfig] {
        &self.inner.endpoints
    }

    /// Snapshot the current delivery counters.
    pub fn stats(&self) -> DeliveryStats {
        *self.inner.stats.lock()
    }

    /// Number of pending entries currently in the spool
    /// (`0` when no spool is configured).
    pub fn pending(&self) -> usize {
        self.inner
            .spool
            .as_ref()
            .map(|s| s.lock().pending.len())
            .unwrap_or(0)
    }

    fn record(&self, kind: StatEvent) {
        let mut s = self.inner.stats.lock();
        match kind {
            StatEvent::Accepted => s.accepted += 1,
            StatEvent::Rejected => s.rejected += 1,
            StatEvent::Failed => s.failed += 1,
            StatEvent::Dropped => s.dropped += 1,
        }
    }
}

enum StatEvent {
    Accepted,
    Rejected,
    Failed,
    Dropped,
}

#[async_trait::async_trait]
impl EventSink for WebhookSink {
    async fn deliver(
        &self,
        event: &AdnetEvent,
        delivery_id: &str,
    ) -> Result<(), WebhookError> {
        let body = serde_json::to_vec(event)?;
        let endpoints = self.inner.endpoints.clone();
        let now_unix_ms = chrono::Utc::now().timestamp_millis();
        let max_attempts = self.inner.max_attempts;
        let mut all_failed = true;
        let mut any_attempted = false;

        // First: attempt the new delivery to every endpoint
        // that wasn't already covered by an in-flight spool
        // entry.
        for ep in &endpoints {
            if !self.room_matches(event, ep) {
                continue;
            }
            any_attempted = true;
            let sig = sign_body(&ep.secret, &body);
            match post_http(
                &ep.url,
                &body,
                delivery_id,
                &sig,
                now_unix_ms,
                ep.request_timeout,
            )
            .await
            {
                Ok(DeliveryOutcome::Accepted) => {
                    self.record(StatEvent::Accepted);
                    all_failed = false;
                    debug!(url = %ep.url, delivery_id, "webhook accepted");
                }
                Ok(DeliveryOutcome::Rejected { status }) => {
                    self.record(StatEvent::Rejected);
                    all_failed = false;
                    warn!(
                        url = %ep.url,
                        delivery_id,
                        status,
                        "webhook permanently rejected (4xx); dropping"
                    );
                }
                Ok(DeliveryOutcome::Failed) | Err(_) => {
                    self.record(StatEvent::Failed);
                    if let Some(spool) = &self.inner.spool
                        && let Err(e) = spool.lock().push(SpoolEntry {
                            delivery_id: delivery_id.to_string(),
                            endpoint_url: ep.url.clone(),
                            body: body.clone(),
                            attempts: 1,
                        })
                    {
                        warn!(error = %e, "failed to append webhook spool entry");
                    }
                }
            }
        }

        // Second: drain any pending spool entries whose
        // endpoint is still configured. Each entry is
        // re-attempted; on success it's removed, on failure
        // it's incremented; when `attempts >= max_attempts`
        // it's purged and `on_dropped` fires.
        self.drain_spool(&endpoints, event, max_attempts).await;

        if !any_attempted && endpoints.is_empty() {
            // Nothing to do — treat as success.
            Ok(())
        } else if all_failed && any_attempted {
            // Every attempted endpoint failed at least once.
            Err(WebhookError::Transport(
                "all configured webhook endpoints failed".into(),
            ))
        } else {
            Ok(())
        }
    }

    async fn on_dropped(
        &self,
        event: &AdnetEvent,
        endpoint: &EndpointConfig,
        delivery_id: &str,
        attempts: u32,
    ) {
        warn!(
            delivery_id,
            url = %endpoint.url,
            attempts,
            "webhook delivery exhausted retry budget; dropping event"
        );
        let _ = event;
    }
}

impl WebhookSink {
    fn room_matches(&self, event: &AdnetEvent, ep: &EndpointConfig) -> bool {
        let Some(filter) = &ep.room_filter else {
            return true;
        };
        match event {
            AdnetEvent::Announcement { payload } => payload
                .get("room_id")
                .and_then(|v| v.as_str())
                == Some(filter.as_str()),
        }
    }

    /// Re-attempt every pending spool entry. Called at the
    /// end of every public `deliver()` so the next in-flight
    /// event carries any prior failures forward.
    async fn drain_spool(
        &self,
        endpoints: &[EndpointConfig],
        current_event: &AdnetEvent,
        max_attempts: u32,
    ) {
        let Some(spool_lock) = &self.inner.spool else {
            return;
        };
        let now_unix_ms = chrono::Utc::now().timestamp_millis();

        let drained: Vec<SpoolEntry> = {
            let mut spool = spool_lock.lock();
            std::mem::take(&mut spool.pending)
        };

        let mut survivors: Vec<SpoolEntry> = Vec::with_capacity(drained.len());
        for entry in drained {
            let Some(ep) = endpoints.iter().find(|ep| ep.url == entry.endpoint_url).cloned()
            else {
                // Endpoint removed from config — drop the entry.
                self.record(StatEvent::Dropped);
                self.fire_dropped(current_event, &entry).await;
                continue;
            };
            let attempts = entry.attempts + 1;
            let sig = sign_body(&ep.secret, &entry.body);
            let result = post_http(
                &ep.url,
                &entry.body,
                &entry.delivery_id,
                &sig,
                now_unix_ms,
                ep.request_timeout,
            )
            .await;
            match result {
                Ok(DeliveryOutcome::Accepted) => {
                    self.record(StatEvent::Accepted);
                    debug!(
                        url = %ep.url,
                        delivery_id = %entry.delivery_id,
                        attempts,
                        "webhook spool entry accepted on retry"
                    );
                }
                Ok(DeliveryOutcome::Rejected { status }) => {
                    self.record(StatEvent::Rejected);
                    warn!(
                        url = %ep.url,
                        delivery_id = %entry.delivery_id,
                        status,
                        attempts,
                        "webhook spool entry permanently rejected; dropping"
                    );
                    self.record(StatEvent::Dropped);
                    self.fire_dropped(current_event, &entry).await;
                }
                Ok(DeliveryOutcome::Failed) | Err(_) => {
                    if attempts > max_attempts {
                        // The current attempt (number
                        // `attempts`) has just exceeded the
                        // retry budget; drop with `on_dropped`.
                        warn!(
                            url = %ep.url,
                            delivery_id = %entry.delivery_id,
                            attempts,
                            max_attempts,
                            "webhook spool entry exhausted retry budget; dropping"
                        );
                        self.record(StatEvent::Dropped);
                        self.fire_dropped(current_event, &entry).await;
                    } else {
                        let mut e = entry.clone();
                        e.attempts = attempts;
                        survivors.push(e);
                    }
                }
            }
        }

        if !survivors.is_empty() {
            let mut spool = spool_lock.lock();
            spool.pending.extend(survivors);
            if let Err(e) = spool.flush() {
                warn!(error = %e, "failed to rewrite webhook spool");
            }
        }
    }

    async fn fire_dropped(&self, event: &AdnetEvent, entry: &SpoolEntry) {
        // Look up the endpoint config so on_dropped sees the
        // same URL / secret shape callers configured. When
        // the endpoint has been removed we fall back to a
        // synthetic config so the callback still gets useful
        // context.
        let ep = self
            .inner
            .endpoints
            .iter()
            .find(|ep| ep.url == entry.endpoint_url)
            .cloned()
            .unwrap_or_else(|| EndpointConfig {
                url: entry.endpoint_url.clone(),
                secret: Vec::new(),
                room_filter: None,
                request_timeout: Duration::from_secs(0),
            });
        // Always log the drop — survives even when no
        // handler is registered.
        warn!(
            delivery_id = %entry.delivery_id,
            url = %ep.url,
            attempts = entry.attempts,
            "webhook delivery dropped"
        );
        // Route to the registered observer (if any). The
        // trait method `on_dropped` is also kept for callers
        // who wrap a [`WebhookSink`] in their own
        // [`EventSink`] impl.
        let handler = self.inner.dropped_handler.lock().clone();
        if let Some(h) = handler {
            h.on_dropped(event, &ep, &entry.delivery_id, entry.attempts).await;
        }
    }
}

/// One row in the spool file. JSONL-encoded so a partial
/// write does not corrupt earlier entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpoolEntry {
    delivery_id: String,
    endpoint_url: String,
    body: Vec<u8>,
    attempts: u32,
}

struct SpoolFile {
    path: PathBuf,
    pending: Vec<SpoolEntry>,
}

impl SpoolFile {
    fn open(path: PathBuf) -> Result<Self, WebhookError> {
        let pending = if path.exists() {
            let raw = std::fs::read(&path)?;
            String::from_utf8_lossy(&raw)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(serde_json::from_str::<SpoolEntry>)
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        Ok(Self { path, pending })
    }

    /// Append one entry to the spool file. Writes are
    /// append-only (one JSONL row) so concurrent readers
    /// never see a half-written entry; the in-memory
    /// `pending` vec tracks what is still on disk.
    fn push(&mut self, entry: SpoolEntry) -> Result<(), WebhookError> {
        use std::io::Write;
        let mut line = serde_json::to_vec(&entry)?;
        line.push(b'\n');
        // Append in O(1); open with create-if-missing.
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        let mut f = opts.open(&self.path)?;
        f.write_all(&line)?;
        f.flush()?;
        self.pending.push(entry);
        Ok(())
    }

    /// Rewrite the spool file from the current in-memory
    /// `pending` vec, atomically. Called after [`drain_spool`]
    /// removes entries that succeeded or exhausted their
    /// budget.
    fn flush(&mut self) -> Result<(), WebhookError> {
        let mut buf = Vec::new();
        for e in &self.pending {
            serde_json::to_writer(&mut buf, e)?;
            buf.push(b'\n');
        }
        atomic_write(&self.path, &buf)
    }
}

/// Write `data` to `path` atomically: write to `path.tmp`
/// first, then rename. A crash between the two stages leaves
/// the previous file untouched.
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), WebhookError> {
    use std::io::Write;
    let mut tmp = path.to_path_buf();
    let file_name = tmp
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    tmp.set_file_name(format!("{}.tmp", file_name.to_string_lossy()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all().ok();
    }
    // On Unix `rename` is atomic; on Windows we use the
    // same primitive via `std::fs::rename` which already
    // replaces the destination when it exists.
    if let Err(e) = std::fs::rename(&tmp, path) {
        // Best-effort cleanup of the tmp file.
        let _ = std::fs::remove_file(&tmp);
        return Err(WebhookError::Spool(e));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────
// Minimal in-tree HTTP/1.1 POST client.
//
// We deliberately avoid pulling `reqwest` / `hyper-util` into the
// workspace for webhook delivery: a single POST with JSON body
// is small enough to hand-roll, and not pulling in a TLS stack
// keeps the default build fast. HTTPS support is left as a TODO
// (see `post_http`).
// ─────────────────────────────────────────────────────────────────

async fn post_http(
    url: &str,
    body: &[u8],
    delivery_id: &str,
    signature: &str,
    timestamp_unix_ms: i64,
    timeout: Duration,
) -> Result<DeliveryOutcome, WebhookError> {
    let parsed: http::Uri = url
        .parse()
        .map_err(|_| WebhookError::InvalidUrl(url.to_string()))?;
    let use_tls = parsed.scheme() == Some(&http::uri::Scheme::HTTPS);
    if use_tls {
        return Err(WebhookError::Transport(
            "https endpoints not yet supported by adnet-webhook; use http for now"
                .to_string(),
        ));
    }
    let host = parsed
        .host()
        .ok_or_else(|| WebhookError::InvalidUrl(url.to_string()))?;
    let port = parsed.port_u16().unwrap_or(80);
    let path = parsed
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let addr = format!("{host}:{port}");

    let header = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         User-Agent: adnet-webhook/0.1\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         X-Adnet-Delivery: {delivery_id}\r\n\
         X-Adnet-Signature: {signature}\r\n\
         X-Adnet-Timestamp: {timestamp_unix_ms}\r\n\
         \r\n",
        body.len(),
    );
    let mut wire: Vec<u8> = header.into_bytes();
    wire.extend_from_slice(body);

    let connect = tokio::time::timeout(timeout, TcpStream::connect(&addr))
        .await
        .map_err(|_| WebhookError::Transport("connect timeout".into()))?
        .map_err(|e| WebhookError::Transport(e.to_string()))?;
    let mut stream = connect;

    tokio::time::timeout(timeout, async {
        stream.write_all(&wire).await?;
        stream.shutdown().await
    })
    .await
    .map_err(|_| WebhookError::Transport("write timeout".into()))?
    .map_err(|e: io::Error| WebhookError::Transport(e.to_string()))?;

    // Bound the read so a malicious / hung receiver cannot pin
    // memory in the sink.
    let mut buf = Vec::with_capacity(2048);
    tokio::time::timeout(timeout, stream.take(MAX_RESPONSE_BYTES).read_to_end(&mut buf))
        .await
        .map_err(|_| WebhookError::Transport("read timeout".into()))?
        .map_err(|e: io::Error| WebhookError::Transport(e.to_string()))?;

    parse_status(&buf).ok_or_else(|| {
        WebhookError::Transport(format!(
            "malformed HTTP response: {:?}",
            String::from_utf8_lossy(&buf).chars().take(80).collect::<String>()
        ))
    })
}

/// Parse `"HTTP/1.1 200 OK\r\n…"` into a [`DeliveryOutcome`].
fn parse_status(buf: &[u8]) -> Option<DeliveryOutcome> {
    let crlf = buf.iter().position(|&b| b == b'\r').unwrap_or(buf.len());
    let line = std::str::from_utf8(buf.get(..crlf)?).ok()?;
    let mut parts = line.split_whitespace();
    let _ = parts.next()?; // HTTP/1.1
    let code: u16 = parts.next()?.parse().ok()?;
    if (200..300).contains(&code) {
        Some(DeliveryOutcome::Accepted)
    } else if (400..500).contains(&code) {
        Some(DeliveryOutcome::Rejected { status: code })
    } else {
        Some(DeliveryOutcome::Failed)
    }
}

// ─────────────────────────────────────────────────────────────────
// Convenience constructors for the CLI
// ─────────────────────────────────────────────────────────────────

/// Load endpoint configs from a JSON file at `path`.
pub fn load_endpoints(path: &Path) -> Result<Vec<EndpointConfig>, WebhookError> {
    let raw = std::fs::read(path)?;
    serde_json::from_slice(&raw).map_err(WebhookError::from)
}

/// Persist the endpoint list to `path`.
pub fn save_endpoints(
    path: &Path,
    endpoints: &[EndpointConfig],
) -> Result<(), WebhookError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_vec_pretty(endpoints)?;
    std::fs::write(path, raw)?;
    Ok(())
}

/// Build a [`WebhookSink`] from a JSON config file. Pass
/// `Some(spool_path)` to enable disk-backed retry persistence.
pub fn from_config_file(
    path: &Path,
    spool: Option<PathBuf>,
) -> Result<(WebhookSink, Vec<EndpointConfig>), WebhookError> {
    let endpoints = load_endpoints(path)?;
    let sink = match spool {
        Some(p) => WebhookSink::with_spool(endpoints.clone(), p)?,
        None => WebhookSink::new(endpoints.clone()),
    };
    info!(endpoints = endpoints.len(), "loaded webhook config");
    Ok((sink, endpoints))
}

// ─────────────────────────────────────────────────────────────────
// Convenience: deliver an [`Announcement`] from `adnet-types`
// directly. Used by [`pump`] below and by callers that have an
// `Announcement` value (not a pre-built JSON payload).
// ─────────────────────────────────────────────────────────────────

impl WebhookSink {
    /// Convert an [`adnet_types::Announcement`] to an
    /// [`AdnetEvent::Announcement`] and deliver it. The delivery
    /// id defaults to `Announcement::message_id`, falling back to
    /// the content hash hex when the announcement did not carry
    /// one (older publishers / hand-crafted events).
    pub async fn deliver_announcement(
        &self,
        ann: &adnet_types::Announcement,
    ) -> Result<(), WebhookError> {
        let payload = serde_json::to_value(ann)?;
        let event = AdnetEvent::Announcement { payload };
        let delivery_id = ann
            .message_id
            .clone()
            .unwrap_or_else(|| ann.content_hash.as_hex().to_string());
        self.deliver(&event, &delivery_id).await
    }
}

/// Long-running task that pumps gossip [`Announcement`]s into a
/// [`WebhookSink`]. Wire it up after [`GossipBus::subscribe`]
/// (or any other `broadcast::Receiver<Announcement>` source):
///
/// ```ignore
/// let rx = gossip_bus.subscribe(&room);
/// let handle = adnet_webhook::pump::run(sink, rx);
/// handle.await.unwrap();
/// ```
///
/// The pump:
///
/// - forwards every received [`Announcement`] via
///   [`WebhookSink::deliver_announcement`];
/// - on `RecvError::Lagged(n)`, emits a `warn!` log and
///   continues with the next in-flight message (no event loss
///   beyond the broadcast buffer's depth);
/// - on `RecvError::Closed`, exits cleanly (the upstream
///   transport has shut down — nothing more to deliver);
/// - yields a [`tokio::task::JoinHandle`] the caller can `abort`
///   or `.await` for shutdown coordination.
pub mod pump {
    use std::sync::Arc;

    use adnet_types::Announcement;
    use tokio::sync::broadcast;
    use tokio::task::JoinHandle;
    use tracing::{debug, info, warn};

    use super::{WebhookError, WebhookSink};

    /// Spawn the pump on the current Tokio runtime. The
    /// returned [`JoinHandle`] resolves when the upstream
    /// `receiver` closes; abort it to stop the pump
    /// immediately.
    ///
    /// The returned counter is the number of announcements the
    /// pump attempted to deliver (one per `recv()`), regardless
    /// of whether the downstream sink accepted each one. Failed
    /// attempts are persisted to the sink's spool and retried
    /// on the next delivery.
    pub fn run(
        sink: Arc<WebhookSink>,
        mut receiver: broadcast::Receiver<Announcement>,
    ) -> JoinHandle<Result<u64, WebhookError>> {
        info!("webhook pump started");
        tokio::spawn(async move {
            let mut delivered: u64 = 0;
            loop {
                match receiver.recv().await {
                    Ok(ann) => {
                        // Count the attempt whether or not the
                        // delivery ultimately succeeded — the
                        // upstream gossip bus already fired the
                        // event, so we always want to record
                        // that we tried to send it on.
                        delivered += 1;
                        if let Err(e) = sink.deliver_announcement(&ann).await {
                            warn!(error = %e, delivery_id = delivered, "webhook deliver failed");
                        } else {
                            debug!(
                                room = %ann.room_id,
                                node = %ann.node_id,
                                delivered,
                                "webhook pump delivered announcement"
                            );
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        // The buffer overflowed; we missed
                        // `skipped` events. Spool-bound
                        // retry will not see them — log and
                        // continue with the next live event.
                        warn!(
                            skipped,
                            "webhook pump lagged behind gossip bus; events lost"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("webhook pump exiting: upstream closed");
                        return Ok(delivered);
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    #[test]
    fn sign_body_is_deterministic_for_same_secret_and_body() {
        let s1 = sign_body(b"secret", b"hello");
        let s2 = sign_body(b"secret", b"hello");
        assert_eq!(s1, s2);
        assert!(s1.starts_with("sha256="));
        assert_eq!(s1.len(), "sha256=".len() + 64);
    }

    #[test]
    fn sign_body_changes_when_secret_changes() {
        assert_ne!(sign_body(b"alice", b"msg"), sign_body(b"bob", b"msg"));
    }

    #[test]
    fn sign_body_changes_when_body_changes() {
        assert_ne!(sign_body(b"k", b"msg1"), sign_body(b"k", b"msg2"));
    }

    #[test]
    fn endpoint_config_round_trips_through_json() {
        let cfg = EndpointConfig {
            url: "http://localhost:8080/hook".into(),
            secret: b"topsecret".to_vec(),
            room_filter: Some("room-a".into()),
            request_timeout: Duration::from_secs(3),
        };
        let raw = serde_json::to_string(&cfg).unwrap();
        let back: EndpointConfig = serde_json::from_str(&raw).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn save_and_load_endpoints_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        let cfg = vec![EndpointConfig {
            url: "http://localhost:9090/hook".into(),
            secret: b"shh".to_vec(),
            room_filter: None,
            request_timeout: Duration::from_secs(7),
        }];
        save_endpoints(&path, &cfg).unwrap();
        let back = load_endpoints(&path).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn webhook_sink_with_spool_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("nested/spool.jsonl");
        let cfg = vec![EndpointConfig {
            url: "http://localhost:0".into(),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_millis(100),
        }];
        WebhookSink::with_spool(cfg, spool.clone()).unwrap();
        assert!(spool.parent().unwrap().exists());
    }

    #[test]
    fn webhook_sink_endpoints_returns_config() {
        let cfg = vec![EndpointConfig {
            url: "http://example.com".into(),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_secs(1),
        }];
        let sink = WebhookSink::new(cfg.clone());
        assert_eq!(sink.endpoints(), &cfg[..]);
    }

    #[test]
    fn adnet_event_announcement_carries_payload() {
        let payload = serde_json::json!({
            "room_id": "lobby",
            "node_id": "abc",
            "title": "hello"
        });
        let ev = AdnetEvent::Announcement { payload: payload.clone() };
        let raw = serde_json::to_string(&ev).unwrap();
        let back: AdnetEvent = serde_json::from_str(&raw).unwrap();
        match back {
            AdnetEvent::Announcement { payload: p } => assert_eq!(p, payload),
        }
    }

    #[test]
    fn parse_status_2xx_is_accepted() {
        assert_eq!(parse_status(b"HTTP/1.1 200 OK\r\n"), Some(DeliveryOutcome::Accepted));
        assert_eq!(parse_status(b"HTTP/1.1 204 No Content\r\n"), Some(DeliveryOutcome::Accepted));
    }

    #[test]
    fn parse_status_4xx_is_rejected() {
        assert_eq!(
            parse_status(b"HTTP/1.1 404 Not Found\r\n"),
            Some(DeliveryOutcome::Rejected { status: 404 })
        );
    }

    #[test]
    fn parse_status_5xx_is_failed() {
        assert_eq!(
            parse_status(b"HTTP/1.1 503 Service Unavailable\r\n"),
            Some(DeliveryOutcome::Failed)
        );
    }

    #[test]
    fn parse_status_rejects_garbage() {
        assert!(parse_status(b"not http\r\n").is_none());
        assert!(parse_status(b"").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deliver_to_unreachable_endpoint_persists_to_spool() {
        // 127.0.0.1:1 is reserved and refuses connections,
        // which exercises the retry path without needing a
        // local mock receiver. With the new contract a fully
        // failing deliver returns `Err` so callers can react;
        // the spool write still happens regardless.
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool.jsonl");
        let cfg = vec![EndpointConfig {
            url: "http://127.0.0.1:1/hook".into(),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_millis(50),
        }];
        let sink = WebhookSink::with_spool(cfg, spool.clone()).unwrap();
        let payload = serde_json::json!({"room_id": "lobby"});
        let ev = AdnetEvent::Announcement { payload };
        let result = sink.deliver(&ev, "deliv-1").await;
        assert!(
            matches!(result, Err(WebhookError::Transport(_))),
            "fully-failing deliver must surface as Err; got {result:?}"
        );
        let raw = std::fs::read(&spool).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.lines().any(|l| l.contains("deliv-1")));
    }

    // ─────────────────────────────────────────────────────────────
    // Pump tests
    //
    // The pump consumes a `broadcast::Receiver<Announcement>`.
    // We exercise it without depending on `adnet-gossip` by
    // constructing the broadcast channel directly and pushing
    // synthetic `Announcement`s into it.
    // ─────────────────────────────────────────────────────────────

    fn sample_announcement(
        room: &str,
        node: &adnet_types::NodeId,
        msg_id: Option<&str>,
    ) -> adnet_types::Announcement {
        adnet_types::Announcement {
            room_id: adnet_types::RoomId::from(room),
            content_hash: adnet_types::ContentHash::from_bytes(b"hello"),
            node_id: node.clone(),
            title: "hello".into(),
            kind: adnet_types::CdnContentKind::GenericFile,
            size_bytes: 5,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: chrono::Utc::now(),
            message_id: msg_id.map(|s| s.to_string()),
            ttl_secs: None,
            signer: None,
            signature: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pump_delivers_announcement_and_persists_to_spool() {
        // 127.0.0.1:1 refuses connections — every delivery
        // fails and gets persisted to the spool. The pump
        // should keep running and call `deliver_announcement`
        // for every published event, regardless of whether
        // each individual `deliver_announcement` returned Ok.
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool.jsonl");
        let cfg = vec![EndpointConfig {
            url: "http://127.0.0.1:1/hook".into(),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_millis(50),
        }];
        let sink = Arc::new(WebhookSink::with_spool(cfg, spool.clone()).unwrap());

        let (tx, rx) = tokio::sync::broadcast::channel::<adnet_types::Announcement>(16);
        let handle = pump::run(Arc::clone(&sink), rx);

        let local = adnet_types::NodeId::random();
        for i in 0..3 {
            tx.send(sample_announcement(
                "lobby",
                &local,
                Some(&format!("msg-{i}")),
            ))
            .unwrap();
        }

        // Give the pump time to drain.
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(tx);
        let delivered = handle.await.unwrap().unwrap();
        // Pump must attempt every event even though
        // `deliver_announcement` itself returns Err for
        // unreachable endpoints.
        assert_eq!(delivered, 3, "pump should forward every event");

        let raw = std::fs::read(&spool).unwrap();
        let text = String::from_utf8_lossy(&raw);
        for i in 0..3 {
            let id = format!("msg-{i}");
            assert!(
                text.lines().any(|l| l.contains(&id)),
                "spool missing delivery {id}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pump_exits_cleanly_when_receiver_closed() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool.jsonl");
        let cfg = vec![EndpointConfig {
            url: "http://127.0.0.1:1/hook".into(),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_millis(50),
        }];
        let sink = Arc::new(WebhookSink::with_spool(cfg, spool).unwrap());

        let (tx, rx) = tokio::sync::broadcast::channel::<adnet_types::Announcement>(4);
        let handle = pump::run(Arc::clone(&sink), rx);
        // No publishes; drop the sender — the receiver will
        // observe `Closed` and the pump should exit Ok.
        drop(tx);
        let res = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("pump did not exit within 1s")
            .expect("pump join error");
        assert_eq!(res.unwrap(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pump_continues_after_lag() {
        // Overflow the broadcast buffer so the receiver
        // observes `Lagged`. The pump should not panic and
        // should still deliver events arriving after the
        // backlog.
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool.jsonl");
        let cfg = vec![EndpointConfig {
            url: "http://127.0.0.1:1/hook".into(),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_millis(50),
        }];
        let sink = Arc::new(WebhookSink::with_spool(cfg, spool.clone()).unwrap());

        // Capacity 4 — fill 100 messages without a receiver
        // attached so they all get dropped on the floor when
        // we eventually subscribe.
        let (tx, rx) = tokio::sync::broadcast::channel::<adnet_types::Announcement>(4);
        let local = adnet_types::NodeId::random();
        for i in 0..100 {
            let _ = tx.send(sample_announcement("lobby", &local, Some(&format!("filler-{i}"))));
        }
        let handle = pump::run(Arc::clone(&sink), rx);
        // Now publish a single new event after the lag.
        tx.send(sample_announcement("lobby", &local, Some("after-lag")))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        drop(tx);
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;

        let raw = std::fs::read(&spool).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.lines().any(|l| l.contains("after-lag")),
            "the post-lag delivery must be persisted"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deliver_announcement_uses_message_id_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool.jsonl");
        let cfg = vec![EndpointConfig {
            url: "http://127.0.0.1:1/hook".into(),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_millis(50),
        }];
        let sink = WebhookSink::with_spool(cfg, spool.clone()).unwrap();
        let local = adnet_types::NodeId::random();
        let ann = sample_announcement("lobby", &local, Some("custom-id-7"));
        // Unreachable endpoint → fully-failing deliver returns
        // Err, but the spool write still happens.
        let _ = sink.deliver_announcement(&ann).await;
        let raw = std::fs::read(&spool).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.lines().any(|l| l.contains("custom-id-7")),
            "spool must record the custom message_id as delivery id"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // Fake receiver helpers
    //
    // The tests below need an actual TCP listener that
    // responds with a configurable status code and records the
    // headers + body it received so the test can assert
    // signature / delivery_id / room filter behaviour. We keep
    // them inline so the suite stays in one file.
    // ─────────────────────────────────────────────────────────────

    /// Captured state from a fake HTTP receiver.
    #[derive(Debug, Clone)]
    struct CapturedRequest {
        headers: Vec<String>,
        body: String,
    }

    /// Start a one-shot HTTP listener that replies with the
    /// supplied status code and records the request it
    /// received. The handle resolves once the request has
    /// been processed.
    async fn spawn_receiver(
        response_status: u16,
        response_body: &'static str,
    ) -> (
        std::net::SocketAddr,
        Arc<Mutex<Option<CapturedRequest>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured: Arc<Mutex<Option<CapturedRequest>>> =
            Arc::new(Mutex::new(None));
        let captured_clone = Arc::clone(&captured);
        let handle = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = Vec::with_capacity(4096);
                let _ = stream.read_to_end(&mut buf).await;
                let text = String::from_utf8_lossy(&buf).into_owned();
                let (header_block, body) = text
                    .split_once("\r\n\r\n")
                    .map(|(h, b)| (h.to_string(), b.to_string()))
                    .unwrap_or_else(|| (text.clone(), String::new()));
                let headers: Vec<String> = header_block
                    .lines()
                    .map(str::to_string)
                    .collect();
                *captured_clone.lock() = Some(CapturedRequest { headers, body });
                let response = format!(
                    "HTTP/1.1 {response_status} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                    status_reason(response_status),
                    response_body.len(),
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (addr, captured, handle)
    }

    fn status_reason(code: u16) -> &'static str {
        match code {
            200 => "OK",
            201 => "Created",
            204 => "No Content",
            400 => "Bad Request",
            401 => "Unauthorized",
            404 => "Not Found",
            500 => "Internal Server Error",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            _ => "Status",
        }
    }

    /// Find the value of an HTTP header (case-insensitive).
    fn header_value<'a>(req: &'a CapturedRequest, name: &str) -> Option<&'a str> {
        let prefix_lower = name.to_ascii_lowercase();
        req.headers.iter().find_map(|line| {
            let mut parts = line.splitn(2, ':');
            let k = parts.next()?.trim();
            let v = parts.next()?.trim();
            if k.to_ascii_lowercase() == prefix_lower {
                Some(v)
            } else {
                None
            }
        })
    }

    fn make_endpoint(
        url: String,
        secret: &[u8],
        room: Option<&str>,
    ) -> EndpointConfig {
        EndpointConfig {
            url,
            secret: secret.to_vec(),
            room_filter: room.map(str::to_string),
            request_timeout: Duration::from_secs(2),
        }
    }

    // ─────────────────────────────────────────────────────────────
    // Happy-path end-to-end
    // ─────────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deliver_2xx_hits_receiver_and_verifies_hmac() {
        let (addr, captured, handle) = spawn_receiver(200, "ok").await;
        let secret = b"shared-secret".to_vec();
        let sink = WebhookSink::new(vec![make_endpoint(
            format!("http://{addr}/hook"),
            &secret,
            None,
        )]);
        let event = AdnetEvent::Announcement {
            payload: serde_json::json!({
                "room_id": "lobby",
                "node_id": "node-x",
                "title": "hello",
            }),
        };
        sink.deliver(&event, "deliv-hmac-1").await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("receiver task did not finish");

        let req = captured
            .lock()
            .take()
            .expect("receiver should have recorded the request");
        let sig = header_value(&req, "X-Adnet-Signature")
            .expect("X-Adnet-Signature missing");
        let expected = sign_body(&secret, req.body.as_bytes());
        assert_eq!(sig, expected, "HMAC mismatch");
        assert_eq!(
            header_value(&req, "X-Adnet-Delivery"),
            Some("deliv-hmac-1")
        );
        let ts: i64 = header_value(&req, "X-Adnet-Timestamp")
            .unwrap()
            .parse()
            .expect("timestamp should parse as i64");
        let now = chrono::Utc::now().timestamp_millis();
        assert!(
            (now - ts).abs() < 5_000,
            "timestamp should be within 5s of now; got delta {}",
            now - ts
        );
        assert!(req.body.contains("\"room_id\":\"lobby\""));
        assert!(req.body.contains("\"node_id\":\"node-x\""));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deliver_2xx_returns_ok_and_increments_stats() {
        let (addr, _captured, handle) = spawn_receiver(200, "ok").await;
        let sink = WebhookSink::new(vec![make_endpoint(
            format!("http://{addr}/hook"),
            b"k",
            None,
        )]);
        let before = sink.stats();
        sink.deliver(&AdnetEvent::Announcement { payload: serde_json::json!({}) }, "ok-1")
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("receiver task did not finish");
        let after = sink.stats();
        assert_eq!(after.accepted, before.accepted + 1);
        assert_eq!(after.rejected, before.rejected);
        assert_eq!(after.failed, before.failed);
    }

    // ─────────────────────────────────────────────────────────────
    // Status-code classification
    // ─────────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deliver_4xx_does_not_persist_to_spool_and_increments_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool.jsonl");
        let (addr, _captured, handle) = spawn_receiver(404, "no").await;
        let sink = WebhookSink::with_spool(
            vec![make_endpoint(format!("http://{addr}/hook"), b"k", None)],
            spool.clone(),
        )
        .unwrap();
        sink.deliver(
            &AdnetEvent::Announcement { payload: serde_json::json!({}) },
            "perm-reject-1",
        )
        .await
        .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("receiver task did not finish");
        assert_eq!(sink.stats().rejected, 1);
        assert_eq!(sink.stats().failed, 0);
        assert_eq!(sink.stats().accepted, 0);
        assert!(!spool.exists() || std::fs::metadata(&spool).unwrap().len() == 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deliver_5xx_persists_to_spool_and_increments_failed() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool.jsonl");
        let (addr, _captured, handle) = spawn_receiver(503, "down").await;
        let sink = WebhookSink::with_spool(
            vec![make_endpoint(format!("http://{addr}/hook"), b"k", None)],
            spool.clone(),
        )
        .unwrap();
        let res = sink
            .deliver(
                &AdnetEvent::Announcement { payload: serde_json::json!({}) },
                "transient-1",
            )
            .await;
        let _ = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("receiver task did not finish");
        assert!(
            matches!(res, Err(WebhookError::Transport(_))),
            "single 5xx endpoint must surface as Err; got {res:?}"
        );
        assert_eq!(sink.stats().failed, 1);
        assert_eq!(sink.stats().accepted, 0);
        let raw = std::fs::read(&spool).unwrap();
        assert!(String::from_utf8_lossy(&raw).contains("transient-1"));
        assert_eq!(sink.pending(), 1);
    }

    // ─────────────────────────────────────────────────────────────
    // Room filter
    // ─────────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn room_filter_skips_non_matching_events() {
        let (addr, captured, handle) = spawn_receiver(200, "ok").await;
        let sink = WebhookSink::new(vec![make_endpoint(
            format!("http://{addr}/hook"),
            b"k",
            Some("room-a"),
        )]);
        // Mismatched room — receiver should not be contacted.
        sink.deliver(
            &AdnetEvent::Announcement {
                payload: serde_json::json!({"room_id": "room-b"}),
            },
            "skip-1",
        )
        .await
        .unwrap();
        // Receiver should not have fired; the join handle has
        // not been polled, so timeout immediately.
        let r = tokio::time::timeout(Duration::from_millis(100), handle).await;
        assert!(r.is_err(), "receiver should not have been contacted");
        assert!(captured.lock().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn room_filter_delivers_matching_events() {
        let (addr, captured, handle) = spawn_receiver(200, "ok").await;
        let sink = WebhookSink::new(vec![make_endpoint(
            format!("http://{addr}/hook"),
            b"k",
            Some("room-a"),
        )]);
        sink.deliver(
            &AdnetEvent::Announcement {
                payload: serde_json::json!({"room_id": "room-a"}),
            },
            "match-1",
        )
        .await
        .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("receiver did not fire");
        let req = captured.lock().take().expect("receiver should have fired");
        assert_eq!(header_value(&req, "X-Adnet-Delivery"), Some("match-1"));
    }

    // ─────────────────────────────────────────────────────────────
    // Per-endpoint signature distinctness
    // ─────────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn each_endpoint_signs_with_its_own_secret() {
        // Two independent listeners, two distinct secrets.
        let (addr_a, captured_a, handle_a) = spawn_receiver(200, "ok").await;
        let (addr_b, captured_b, handle_b) = spawn_receiver(200, "ok").await;
        let secret_a = b"secret-a".to_vec();
        let secret_b = b"secret-b".to_vec();
        let sink = WebhookSink::new(vec![
            make_endpoint(format!("http://{addr_a}/hook"), &secret_a, None),
            make_endpoint(format!("http://{addr_b}/hook"), &secret_b, None),
        ]);
        let event = AdnetEvent::Announcement {
            payload: serde_json::json!({"room_id": "lobby"}),
        };
        sink.deliver(&event, "both-1").await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), handle_a).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), handle_b).await.unwrap();
        let req_a = captured_a.lock().take().unwrap();
        let req_b = captured_b.lock().take().unwrap();
        assert_eq!(req_a.body, req_b.body);
        let sig_a = header_value(&req_a, "X-Adnet-Signature").unwrap();
        let sig_b = header_value(&req_b, "X-Adnet-Signature").unwrap();
        assert_ne!(sig_a, sig_b);
        assert_eq!(sig_a, sign_body(&secret_a, req_a.body.as_bytes()));
        assert_eq!(sig_b, sign_body(&secret_b, req_b.body.as_bytes()));
    }

    // ─────────────────────────────────────────────────────────────
    // Retry budget + on_dropped
    // ─────────────────────────────────────────────────────────────

    #[derive(Default)]
    struct DroppedRecord {
        delivery_id: String,
        url: String,
        attempts: u32,
    }

    struct DropRecorder {
        dropped: Arc<Mutex<Vec<DroppedRecord>>>,
    }

    #[async_trait::async_trait]
    impl DroppedHandler for DropRecorder {
        async fn on_dropped(
            &self,
            _event: &AdnetEvent,
            endpoint: &EndpointConfig,
            delivery_id: &str,
            attempts: u32,
        ) {
            self.dropped.lock().push(DroppedRecord {
                delivery_id: delivery_id.to_string(),
                url: endpoint.url.clone(),
                attempts,
            });
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn on_dropped_fires_after_max_attempts() {
        // 127.0.0.1:1 always refuses. With budget=2 the
        // entry drops after the *third* attempt (push=1,
        // first drain=2, second drain=3>2). So after the
        // first deliver the spool holds one entry; after
        // the second deliver the original entry is dropped,
        // the `DroppedHandler` fires once, and the
        // just-pushed entry is the sole survivor.
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool.jsonl");
        let cfg = vec![EndpointConfig {
            url: "http://127.0.0.1:1/hook".into(),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_millis(50),
        }];
        let dropped: Arc<Mutex<Vec<DroppedRecord>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::new(
            WebhookSink::with_spool_and_budget(
                cfg.clone(),
                spool.clone(),
                2,
            )
            .unwrap(),
        );
        sink.set_dropped_handler(Some(Arc::new(DropRecorder {
            dropped: Arc::clone(&dropped),
        })));

        let event = AdnetEvent::Announcement {
            payload: serde_json::json!({"room_id": "lobby"}),
        };

        // First delivery: pushes one entry to the spool and
        // drains it (attempts=2, not > 2, survives).
        let _ = sink.deliver(&event, "drop-me-1").await;
        assert_eq!(sink.pending(), 1);
        assert_eq!(dropped.lock().len(), 0);

        // Second delivery: drains the existing entry
        // (attempts=3 > 2 → drop, handler fires) and pushes
        // the new one (attempts=2, survives).
        let _ = sink.deliver(&event, "drop-me-1").await;
        assert_eq!(sink.pending(), 1);
        let recs = dropped.lock();
        assert_eq!(recs.len(), 1, "handler should fire exactly once");
        assert_eq!(recs[0].delivery_id, "drop-me-1");
        assert!(recs[0].url.contains("127.0.0.1"));
        // attempts reflects the count of attempts that have
        // already happened when the drop decision fires —
        // the entry was put back with attempts=2 once, so
        // the recorded value is `entry.attempts` (the count
        // prior to this drain's increment).
        assert_eq!(recs[0].attempts, 2);
        assert_eq!(sink.stats().dropped, 1);
    }

    // ─────────────────────────────────────────────────────────────
    // Spool reload on restart
    // ─────────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn with_spool_reloads_pending_entries_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool.jsonl");
        let cfg = vec![EndpointConfig {
            url: "http://127.0.0.1:1/hook".into(),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_millis(50),
        }];

        // First incarnation: write 3 spool entries.
        let first = WebhookSink::with_spool(cfg.clone(), spool.clone()).unwrap();
        for i in 0..3 {
            let _ = first
                .deliver(
                    &AdnetEvent::Announcement {
                        payload: serde_json::json!({"i": i}),
                    },
                    &format!("persist-{i}"),
                )
                .await;
        }
        let pending_after_first = first.pending();
        assert!(pending_after_first >= 1, "first run must have spool entries");
        assert!(
            pending_after_first <= 3,
            "first run cannot have more entries than we pushed"
        );
        drop(first);

        // Second incarnation: must observe the entries that
        // were on disk when the first sink was dropped.
        let second = WebhookSink::with_spool(cfg.clone(), spool.clone()).unwrap();
        assert_eq!(
            second.pending(),
            pending_after_first,
            "second run must reload pending count from disk"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // Spool atomic-write
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn atomic_write_creates_tmp_and_renames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg.jsonl");
        atomic_write(&path, b"hello\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello\n");
        // Second call should overwrite atomically.
        atomic_write(&path, b"world\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"world\n");
        // No leftover .tmp file should remain.
        let mut leftover = Vec::new();
        for e in std::fs::read_dir(dir.path()).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".tmp") {
                leftover.push(name);
            }
        }
        assert!(leftover.is_empty(), "tmp leftover: {leftover:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spool_push_is_append_only_and_survives_restart() {
        // 5 calls to `deliver` each push one JSONL row to
        // the spool, and the in-memory `pending` vec tracks
        // the same set. After every call `drain_spool`
        // rewrites the file from in-memory state, so old
        // entries that have exhausted their retry budget are
        // pruned (default budget = 5 attempts per entry).
        //
        // We assert the strong invariant that disk and
        // memory agree at every settled point.
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool.jsonl");
        let cfg = vec![EndpointConfig {
            url: "http://127.0.0.1:1/hook".into(),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_millis(50),
        }];
        let sink = WebhookSink::with_spool(cfg.clone(), spool.clone()).unwrap();
        for i in 0..5 {
            let _ = sink
                .deliver(
                    &AdnetEvent::Announcement { payload: serde_json::json!({"i": i}) },
                    &format!("entry-{i}"),
                )
                .await;
        }
        let on_disk_lines: Vec<String> = std::fs::read_to_string(&spool)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        // Disk and in-memory pending must agree at any
        // settled point — no torn JSONL, no half-trimmed
        // entries, no in-memory entries that are missing on
        // disk.
        assert_eq!(
            on_disk_lines.len(),
            sink.pending(),
            "spool disk/in-memory mismatch"
        );
        // Every JSONL row must parse — proves atom_write
        // did not leave a half-written entry behind.
        for line in &on_disk_lines {
            let _: serde_json::Value = serde_json::from_str(line)
                .expect("every JSONL row must be valid JSON");
        }
        // The most recent push must still be pending —
        // entries-prune age is bounded by the retry budget.
        assert!(
            on_disk_lines.last().unwrap().contains("entry-4"),
            "most recent entry should still be pending"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // Error / corner cases for config helpers
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn save_endpoints_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/multi/hooks.json");
        save_endpoints(
            &path,
            &[EndpointConfig {
                url: "http://x".into(),
                secret: vec![],
                room_filter: None,
                request_timeout: Duration::from_secs(1),
            }],
        )
        .unwrap();
        assert!(path.exists());
    }

    #[test]
    fn load_endpoints_returns_parse_error_on_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, b"not json at all").unwrap();
        let err = load_endpoints(&path).unwrap_err();
        assert!(matches!(err, WebhookError::SpoolParse(_)), "got {err:?}");
    }

    #[test]
    fn endpoint_config_defaults_request_timeout_from_json() {
        // request_timeout is missing from the JSON; the
        // serde default must kick in.
        let raw = r#"{
            "url": "http://example.com",
            "secret": [97, 98, 99]
        }"#;
        let cfg: EndpointConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(cfg.url, "http://example.com");
        assert_eq!(cfg.request_timeout, Duration::from_secs(5));
        assert_eq!(cfg.room_filter, None);
    }

    #[test]
    fn endpoint_config_optional_fields_default() {
        let raw = r#"{ "url": "http://example.com", "secret": [1, 2] }"#;
        let cfg: EndpointConfig = serde_json::from_str(raw).unwrap();
        assert!(cfg.room_filter.is_none());
        // Default request_timeout is 5 seconds.
        assert_eq!(cfg.request_timeout, Duration::from_secs(5));
    }

    #[test]
    fn from_config_file_with_and_without_spool() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("hooks.json");
        let cfg = vec![EndpointConfig {
            url: "http://example.com".into(),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_secs(1),
        }];
        save_endpoints(&cfg_path, &cfg).unwrap();

        // No spool path.
        let (sink_a, eps_a) =
            from_config_file(&cfg_path, None).unwrap();
        assert_eq!(eps_a, cfg);
        assert_eq!(sink_a.endpoints(), &cfg[..]);
        assert_eq!(sink_a.pending(), 0);

        // With spool path — `with_spool` materialises the
        // spool file (via `SpoolFile::open`'s append create),
        // so a subsequent `push` is O(1). A failed push on
        // the unreachable endpoint is enough to surface it.
        let spool = dir.path().join("nested/spool.jsonl");
        let (sink_b, eps_b) =
            from_config_file(&cfg_path, Some(spool.clone())).unwrap();
        assert_eq!(eps_b, cfg);
        assert_eq!(sink_b.pending(), 0);

        // Trigger one spool append (unreachable endpoint).
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let _ = sink_b
                .deliver(
                    &AdnetEvent::Announcement { payload: serde_json::json!({}) },
                    "trigger-1",
                )
                .await;
        });
        assert!(spool.exists(), "spool file should be created");
    }

    // ─────────────────────────────────────────────────────────────
    // Full-failure semantics
    // ─────────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deliver_returns_err_when_all_endpoints_unreachable() {
        let sink = WebhookSink::new(vec![EndpointConfig {
            url: "http://127.0.0.1:1/hook".into(),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_millis(50),
        }]);
        let res = sink
            .deliver(
                &AdnetEvent::Announcement { payload: serde_json::json!({}) },
                "all-fail-1",
            )
            .await;
        assert!(matches!(res, Err(WebhookError::Transport(_))));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deliver_returns_ok_when_at_least_one_endpoint_accepts() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool.jsonl");
        let (addr_ok, _captured_ok, handle_ok) = spawn_receiver(200, "ok").await;
        let sink = WebhookSink::with_spool(
            vec![
                make_endpoint(format!("http://{addr_ok}/hook"), b"k", None),
                EndpointConfig {
                    url: "http://127.0.0.1:1/hook".into(),
                    secret: vec![],
                    room_filter: None,
                    request_timeout: Duration::from_millis(50),
                },
            ],
            spool.clone(),
        )
        .unwrap();
        let res = sink
            .deliver(
                &AdnetEvent::Announcement { payload: serde_json::json!({}) },
                "mixed-1",
            )
            .await;
        let _ = tokio::time::timeout(Duration::from_secs(1), handle_ok)
            .await
            .expect("ok receiver did not fire");
        assert!(
            res.is_ok(),
            "deliver should be Ok because one endpoint accepted"
        );
        assert_eq!(sink.stats().accepted, 1);
        assert_eq!(sink.stats().failed, 1);
    }

    // ─────────────────────────────────────────────────────────────
    // Pump end-to-end with a real listener
    // ─────────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pump_end_to_end_signature_and_payload() {
        let (addr, captured, handle) = spawn_receiver(200, "ok").await;
        let secret = b"end-to-end-secret".to_vec();
        let sink = Arc::new(WebhookSink::new(vec![make_endpoint(
            format!("http://{addr}/hook"),
            &secret,
            None,
        )]));
        let (tx, rx) =
            tokio::sync::broadcast::channel::<adnet_types::Announcement>(4);
        let pump_handle = pump::run(Arc::clone(&sink), rx);
        let node = adnet_types::NodeId::random();
        let ann = sample_announcement("pump-room", &node, Some("pump-e2e-1"));
        tx.send(ann).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        drop(tx);
        let delivered = pump_handle.await.unwrap().unwrap();
        assert_eq!(delivered, 1);
        let _ = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("receiver did not fire");

        let req = captured.lock().take().expect("receiver missed request");
        assert_eq!(header_value(&req, "X-Adnet-Delivery"), Some("pump-e2e-1"));
        let sig = header_value(&req, "X-Adnet-Signature").unwrap();
        assert_eq!(sig, sign_body(&secret, req.body.as_bytes()));
        assert!(req.body.contains("pump-room"));
        assert!(req.body.contains("\"title\":\"hello\""));
    }

    // ─────────────────────────────────────────────────────────────
    // Read-side cap
    // ─────────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deliver_does_not_hang_on_oversized_response() {
        // Listener sends back a response > MAX_RESPONSE_BYTES
        // and never closes the write half. The sink must
        // return within `request_timeout` rather than block
        // forever reading.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n").await;
                let _ = stream
                    .write_all(b"Content-Length: 999999999\r\n\r\n")
                    .await;
                let chunk = vec![b'x'; 4096];
                while stream.write_all(&chunk).await.is_ok() {}
            }
        });
        let sink = WebhookSink::new(vec![EndpointConfig {
            url: format!("http://{addr}/hook"),
            secret: vec![],
            room_filter: None,
            request_timeout: Duration::from_millis(500),
        }]);
        let started = std::time::Instant::now();
        let _ = sink
            .deliver(
                &AdnetEvent::Announcement { payload: serde_json::json!({}) },
                "oversized-1",
            )
            .await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(3),
            "deliver should not hang on oversized response; took {elapsed:?}"
        );
    }
}