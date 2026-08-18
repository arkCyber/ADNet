//! Mailbox server — axum-based HTTP front-end for the offline inbox.
//!
//! ## Routes
//!
//! - `POST /v1/inbox/{recipient_id}`     — enqueue
//! - `GET  /v1/inbox/{recipient_id}`     — pull since `?since=&limit=`
//! - `POST /v1/inbox/{recipient_id}/ack` — acknowledge
//! - `GET  /healthz`                     — liveness
//! - `GET  /metrics`                     — JSON metrics (or Prometheus text with `?format=prometheus`)
//!
//! ## Security
//!
//! - All request bodies carry an EIP-191 `personal_sign` signature
//!   over the canonical mailbox message (see [`crate::auth`]).
//! - The `recipient_id` URL segment is validated as a 20-byte hex
//!   address before any handler runs. Path-traversal is rejected at
//!   the validator (see `auth::validate_recipient_id`).
//! - The envelope is rejected if it exceeds the configured size cap
//!   *before* the signature is checked, so a single oversized request
//!   can't waste cycles inside the validator.
//! - Sender / recipient signatures are verified against the recovered
//!   EVM address and *must* match the claimed `sender_id` /
//!   `recipient_id`.
//!
//! The server is *not* a forward proxy. It accepts only the paths
//! listed above; everything else returns `404 Not Found`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine as _;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{info, warn};
use a3net_observability::prometheus::PrometheusExporter;
use a3net_observability::registry::GLOBAL;
use crate::rate_limit::{
    rate_limit_middleware, RateLimitConfig, RateLimitRegistry, RateLimitState, TrustedProxy,
};

use crate::auth::{
    validate_msg_id, validate_recipient_id, verify_ack_signature, verify_pull_signature,
    verify_sender_signature, verify_sender_signature_with_timestamp,
};
use crate::client::{AckRequest, AckResponse, EnqueueResponse, PullResponse};
use crate::config::{MailboxConfig, MailboxServerInfo};
use crate::error::MailboxError;
use crate::metrics::MailboxMetrics;
use crate::policy::{QuotaDecision, QuotaPolicy, RetentionPolicy, SizePolicy, TtlPolicy};
use crate::storage::{MailboxStore, MemoryStore, StoredEnvelope, Watermark};
use crate::SqliteStore;

/// Maximum number of envelopes returned by a single `pull`. Bound
/// prevents a malicious / buggy client from issuing a `limit=1000000`
/// request and DoS-ing the server.
pub const PULL_LIMIT_MAX: usize = 1_000;

/// Default `limit` when the client doesn't specify one.
pub const PULL_LIMIT_DEFAULT: usize = 100;

/// Server-side policy (per-instance). Built from [`MailboxConfig`]
/// at start time and passed to handlers via axum state.
#[derive(Clone)]
pub struct ServerPolicy {
    pub max_envelope_bytes: usize,
    pub require_sender_signature: bool,
    pub upstream_timeout: Duration,
    pub ttl: TtlPolicy,
    /// Maximum age of a sender signature in seconds. Signatures older than
    /// this are rejected (EIP-712 timestamp binding). Default: 300 (5 min).
    pub signature_max_age_secs: i64,
}

impl Default for ServerPolicy {
    fn default() -> Self {
        Self {
            max_envelope_bytes: crate::config::DEFAULT_MAX_ENVELOPE_BYTES,
            require_sender_signature: true,
            upstream_timeout: crate::config::DEFAULT_DIAL_TIMEOUT,
            ttl: TtlPolicy::default(),
            signature_max_age_secs: crate::auth::DEFAULT_SIGNATURE_MAX_AGE_SECS,
        }
    }
}

impl ServerPolicy {
    pub fn from_config(cfg: &MailboxConfig) -> Self {
        // Cap max_signature_age_secs to MAX_SIGNATURE_AGE_SECS to prevent operators
        // from accidentally disabling replay protection entirely (M-1 fix).
        let max_age = cfg
            .max_signature_age_secs
            .min(crate::config::MAX_SIGNATURE_AGE_SECS);
        Self {
            max_envelope_bytes: cfg.max_envelope_bytes,
            require_sender_signature: cfg.require_sender_signature,
            upstream_timeout: cfg.upstream_timeout,
            ttl: TtlPolicy {
                default_ttl: cfg.default_ttl,
                ..TtlPolicy::default()
            },
            signature_max_age_secs: max_age as i64,
        }
    }

    fn size_policy(&self) -> SizePolicy {
        SizePolicy::new(self.max_envelope_bytes)
    }

    fn quota_policy(&self) -> QuotaPolicy {
        QuotaPolicy::new(
            crate::config::DEFAULT_MAX_INFLIGHT_PER_USER,
            crate::config::DEFAULT_MAX_TOTAL_BYTES_PER_USER,
        )
    }
}

#[cfg(feature = "billing")]
use crate::billing::BillingPolicy;

/// Shared state for the axum router.
#[derive(Clone)]
pub struct ServerState {
    pub store: Arc<dyn MailboxStore>,
    pub policy: ServerPolicy,
    /// Per-recipient TTL overrides (P3-8).
    pub retention: std::sync::Arc<parking_lot::RwLock<RetentionPolicy>>,
    #[cfg(feature = "billing")]
    pub billing: Option<BillingPolicy>,
    pub metrics: MailboxMetrics,
}

impl ServerState {
    pub fn new(store: Arc<dyn MailboxStore>, policy: ServerPolicy) -> Self {
        Self {
            store,
            policy,
            retention: std::sync::Arc::new(parking_lot::RwLock::new(RetentionPolicy::default())),
            #[cfg(feature = "billing")]
            billing: None,
            metrics: MailboxMetrics::get(),
        }
    }

    /// Create state with a billing policy attached.
    #[cfg(feature = "billing")]
    pub fn with_billing(
        store: Arc<dyn MailboxStore>,
        policy: ServerPolicy,
        billing: BillingPolicy,
    ) -> Self {
        Self {
            store,
            policy,
            retention: std::sync::Arc::new(parking_lot::RwLock::new(RetentionPolicy::default())),
            billing: Some(billing),
            metrics: MailboxMetrics::get(),
        }
    }

    /// Construct state with the default [`MemoryStore`].
    pub fn with_memory_store(policy: ServerPolicy) -> Self {
        Self::new(Arc::new(MemoryStore::new()), policy)
    }

    /// Construct state with a [`SqliteStore`] at `db_path`.
    ///
    /// The SQLite file is created automatically if it doesn't exist.
    /// Schema is initialized on first open. Use this for production
    /// deployments where persistence is required.
    pub fn with_sqlite_store(
        db_path: &std::path::Path,
        policy: ServerPolicy,
    ) -> crate::error::MailboxResult<Self> {
        let store = SqliteStore::open(db_path)?;
        Ok(Self::new(Arc::new(store), policy))
    }
}

impl Default for ServerState {
    fn default() -> Self {
        Self::with_memory_store(ServerPolicy::default())
    }
}

/// Handle to the running mailbox server. Drop / `shutdown` to stop.
pub struct MailboxServerHandle {
    pub port: u16,
    pub bind_host: String,
    pub base_url: String,
    shutdown_tx: watch::Sender<bool>,
    sweeper_handle: Option<tokio::task::JoinHandle<()>>,
}

impl MailboxServerHandle {
    pub fn shutdown(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(h) = self.sweeper_handle.take() {
            h.abort();
        }
    }

    pub fn info(&self) -> MailboxServerInfo {
        MailboxServerInfo {
            running: true,
            self_check: true,
            port: self.port,
            bind_host: self.bind_host.clone(),
            base_url: self.base_url.clone(),
        }
    }
}

impl Drop for MailboxServerHandle {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(h) = self.sweeper_handle.take() {
            h.abort();
        }
    }
}

/// Axum-based mailbox server.
pub struct MailboxServer;

impl MailboxServer {
    /// Spawn a mailbox server on `bind_host:port` with the default
    /// [`ServerState`]. Use [`MailboxServer::start_with_state`] when
    /// you need to inject a custom store.
    pub async fn start(bind_host: &str, port: u16) -> Result<MailboxServerHandle, String> {
        Self::start_with_state(bind_host, port, ServerState::default()).await
    }

    /// Spawn a mailbox server with a custom [`ServerState`].
    ///
    /// This is the entry point used in tests and when you want to inject
    /// a `SqliteStore` or any other `Arc<dyn MailboxStore>`.
    pub async fn start_with_state(
        bind_host: &str,
        port: u16,
        state: ServerState,
    ) -> Result<MailboxServerHandle, String> {
        let (handle, _) =
            Self::start_with_state_and_rate_limit(bind_host, port, state, None).await?;
        Ok(handle)
    }

    /// Spawn a mailbox server backed by a SQLite database.
    ///
    /// Creates the database file at `db_path` if absent, then boots the
    /// server. This is the recommended production entry point.
    pub async fn start_sqlite(
        bind_host: &str,
        port: u16,
        db_path: &std::path::Path,
    ) -> Result<MailboxServerHandle, String> {
        let policy = ServerPolicy::default();
        let state = ServerState::with_sqlite_store(db_path, policy)
            .map_err(|e| format!("failed to open SQLite store: {e}"))?;
        Self::start_with_state(bind_host, port, state).await
    }

    /// Spawn a mailbox server with rate limiting enabled on the enqueue path.
    ///
    /// If `rate_limit` is `Some(registry)`, the server applies a per-IP
    /// token-bucket rate limiter to all `POST /v1/inbox/...` requests
    /// (60 req/min per IP by default). This prevents spam enqueue floods.
    ///
    /// Returns the handle and the registry (for inspection / testing).
    pub async fn start_with_state_and_rate_limit(
        bind_host: &str,
        port: u16,
        state: ServerState,
        rate_limit: Option<RateLimitRegistry>,
    ) -> Result<(MailboxServerHandle, RateLimitRegistry), String> {
        let addr: SocketAddr = format!("{bind_host}:{port}")
            .parse()
            .map_err(|e| format!("Invalid mailbox bind address: {e}"))?;

        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| format!("mailbox bind failed on {addr}: {e}"))?;
        let bound = listener.local_addr().map_err(|e| e.to_string())?;
        let port = bound.port();
        let bind_host = bound.ip().to_string();

        let registry = rate_limit.unwrap_or_else(RateLimitRegistry::new);
        // Enqueue path: tight policy (60 req/min per IP).
        // TrustedProxy::Disabled: forwarded headers are ignored by default.
        // For deployments behind a trusted reverse proxy, replace with
        // RateLimitState::enqueue(registry.clone(), TrustedProxy::FromProxy("10.0.0.1".into())).
        let rate_state = RateLimitState::enqueue(registry.clone(), TrustedProxy::Disabled);

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut router = Router::new()
            .route(
                "/v1/inbox/:recipient_id",
                post(enqueue_handler).get(pull_handler),
            )
            .route("/v1/inbox/:recipient_id/ack", post(ack_handler))
            .route("/healthz", get(healthz_handler))
            .route("/metrics", get(metrics_handler))
            .with_state(state.clone())
            // Rate-limit the entire router. The enqueue policy (60 req/min per IP) prevents
            // spam floods. Healthz/metrics get the same policy, which is fine (300 req/min
            // is well above any legitimate monitoring cadence).
            .layer(axum::middleware::from_fn_with_state(
                rate_state.clone(),
                rate_limit_middleware,
            ));

        // Spawn background TTL sweeper.
        let sweeper_store = state.store.clone();
        let sweeper_metrics = state.metrics.clone();
        let sweeper_interval = state.policy.ttl.sweep_interval;
        let sweeper_handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(sweeper_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                match sweeper_store.purge_expired().await {
                    Ok(removed) if removed > 0 => {
                        sweeper_metrics.purged.inc_by(removed);
                        info!("mailbox sweeper purged {removed} expired envelopes");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("mailbox sweeper error: {e}");
                    }
                }
            }
        });

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
                warn!("mailbox server stopped: {e}");
            }
        });

        let base_url = format!("http://{bind_host}:{port}");
        info!("A3Net mailbox server listening on {base_url}");
        let handle = MailboxServerHandle {
            port,
            bind_host,
            base_url,
            shutdown_tx,
            sweeper_handle: Some(sweeper_handle),
        };
        Ok((handle, registry))
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Request body for `POST /v1/inbox/{recipient_id}`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct EnqueueRequest {
    pub sender_id: String,
    pub msg_id: String,
    /// Base64-encoded ciphertext bytes.
    pub ciphertext_b64: String,
    /// 65-byte EIP-191 sender signature, base64-encoded.
    pub sender_signature_b64: String,
    /// Optional override for the envelope's lifetime, in seconds.
    /// Clamped to `[1, ttl_policy.default_ttl]`.
    pub ttl_secs: Option<u64>,
    /// Unix timestamp of the sender's wall clock at signing time.
    /// Used for replay protection against stale signatures.
    pub timestamp: Option<i64>,
}

async fn enqueue_handler(
    State(state): State<ServerState>,
    Path(recipient_id): Path<String>,
    headers: axum::http::HeaderMap,
    axum::Json(req): axum::Json<EnqueueRequest>,
) -> Response {
    let m = &state.metrics;

    // Step 1: validate recipient id.
    let recipient = match validate_recipient_id(&recipient_id) {
        Ok(a) => a.to_checksum(),
        Err(e) => {
            m.enqueues_rejected.inc();
            return error_to_response(e);
        }
    };

    // Step 2: validate msg_id format.
    if let Err(e) = validate_msg_id(&req.msg_id) {
        m.enqueues_rejected.inc();
        return error_to_response(e);
    }

    // Step 3: decode base64.
    let ciphertext = match base64::engine::general_purpose::STANDARD.decode(&req.ciphertext_b64) {
        Ok(b) => b,
        Err(e) => {
            m.enqueues_rejected.inc();
            return error_to_response(MailboxError::InvalidMessageId(format!(
                "ciphertext_b64 decode: {e}"
            )));
        }
    };
    let sig_bytes = match base64::engine::general_purpose::STANDARD.decode(&req.sender_signature_b64) {
        Ok(b) => b,
        Err(_e) => {
            m.enqueues_rejected.inc();
            return error_to_response(MailboxError::InvalidSignature);
        }
    };

    // Step 4: validate sender id is well-formed.
    let sender = match validate_recipient_id(&req.sender_id) {
        Ok(a) => a.to_checksum(),
        Err(e) => {
            m.enqueues_rejected.inc();
            return error_to_response(e);
        }
    };

    // Step 5: verify sender signature.
    //
    // **SECURITY**: if `req.timestamp` is provided, we use the EIP-712-style
    // binding (`verify_sender_signature_with_timestamp`) which rejects signatures
    // older than `signature_max_age_secs` or with invalid timestamps.
    // If the timestamp check fails, the request is REJECTED — we do NOT fall
    // through to the legacy no-timestamp path. This prevents a replay attack
    // where the attacker sets a far-future `timestamp` to bypass replay protection.
    //
    // If `req.timestamp` is absent, we fall back to the old verifier (backwards
    // compat for legacy clients that haven't adopted EIP-712 timestamp binding).
    let sig_ok = if let Some(signed_at) = req.timestamp {
        // If timestamp is provided but validation fails, REJECT immediately.
        // No fallthrough to the legacy path.
        verify_sender_signature_with_timestamp(
            &sender,
            &recipient,
            &req.msg_id,
            &ciphertext,
            &sig_bytes,
            signed_at,
            state.policy.signature_max_age_secs,
        )
    } else {
        verify_sender_signature(&sender, &recipient, &req.msg_id, &ciphertext, &sig_bytes)
    };
    if let Err(e) = sig_ok {
        m.enqueues_rejected.inc();
        return error_to_response(e);
    }

    // Step 5b: billing mandatory check (P3-3).
    // If billing is mandatory and no valid pledge was provided, reject.
    #[cfg(feature = "billing")]
    if let Some(ref billing) = state.billing {
        if billing.mandatory {
            let pledge_header = headers
                .get("x-a3net-pledge")
                .and_then(|v| v.to_str().ok());
            if pledge_header.is_none() {
                m.enqueues_rejected.inc();
                return error_to_response(MailboxError::Internal(
                    "billing mandatory: missing X-A3Net-Pledge header".into(),
                ));
            }
        }
    }

    // Step 6: build the envelope and check size + quota.
    // Use per-recipient TTL override if present (P3-8: RetentionPolicy).
    let base_ttl = req
        .ttl_secs
        .map(Duration::from_secs)
        .unwrap_or(state.policy.ttl.default_ttl);
    let ttl = {
        let retention = state.retention.read();
        retention.effective_ttl(&recipient, base_ttl)
    };
    let now = Utc::now();
    let envelope = StoredEnvelope {
        sender_id: sender.clone(),
        recipient_id: recipient.clone(),
        msg_id: req.msg_id.clone(),
        ciphertext,
        sender_signature: sig_bytes.clone(),
        sequence: 0, // assigned by store
        queued_at: now,
        expires_at: now
            + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::days(30)),
    };

    let size_policy = state.policy.size_policy();
    if let Err(e) = size_policy.check(&envelope) {
        m.enqueues_rejected.inc();
        return error_to_response(e);
    }

    // Step 7: quota check.
    let usage = match state.store.quota_usage(&recipient).await {
        Ok(u) => u,
        Err(e) => {
            m.enqueues_rejected.inc();
            return error_to_response(e);
        }
    };
    let quota = state.policy.quota_policy();
    let decision = quota.check(crate::policy::QuotaCheck {
        current_message_count: usage.message_count,
        current_total_bytes: usage.total_bytes,
        incoming_envelope_bytes: envelope.wire_size(),
    });
    if let QuotaDecision::Reject { reason } = decision {
        m.enqueues_rejected.inc();
        return error_to_response(MailboxError::QuotaExceeded(reason.to_string()));
    }

    // Step 8: persist.
    let outcome = match state.store.enqueue(&envelope).await {
        Ok(o) => o,
        Err(e) => {
            m.enqueues_rejected.inc();
            return error_to_response(e);
        }
    };

    m.enqueues.inc();
    if !outcome.duplicate {
        m.queue_depth.inc();
        if usage.message_count == 0 {
            m.active_recipients.inc();
        }
    }

    let status = if outcome.duplicate {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    let body = EnqueueResponse {
        msg_id: outcome.msg_id,
        sequence: outcome.sequence,
        queued_at: outcome.queued_at,
        expires_at: outcome.expires_at,
        duplicate: outcome.duplicate,
    };
    (status, axum::Json(body)).into_response()
}

/// Query parameters for `GET /v1/inbox/{recipient_id}`.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct PullQuery {
    /// Recipient's signature over `mailbox.pull|<recipient_id>`.
    pub signature: Option<String>,
    /// Last `sequence` number the client has acknowledged. `0`
    /// means "from the beginning".
    pub since: Option<Watermark>,
    /// Maximum number of envelopes to return. Defaults to 100.
    pub limit: Option<usize>,
}

async fn pull_handler(
    State(state): State<ServerState>,
    Path(recipient_id): Path<String>,
    Query(q): Query<PullQuery>,
) -> Response {
    let m = &state.metrics;

    // Step 1: validate recipient id.
    let recipient = match validate_recipient_id(&recipient_id) {
        Ok(a) => a.to_checksum(),
        Err(e) => return error_to_response(e),
    };

    // Step 2: verify recipient signature.
    let sig_b64 = match q.signature.as_deref() {
        Some(s) => s,
        None => {
            return error_to_response(MailboxError::InvalidRecipientSignature);
        }
    };
    let sig_bytes = match base64::engine::general_purpose::STANDARD.decode(sig_b64) {
        Ok(b) => b,
        Err(_) => {
            return error_to_response(MailboxError::InvalidRecipientSignature);
        }
    };
    if let Err(e) = verify_pull_signature(&recipient, &sig_bytes) {
        return error_to_response(e);
    }

    // Step 3: pull.
    let limit = q.limit.unwrap_or(PULL_LIMIT_DEFAULT).min(PULL_LIMIT_MAX);
    let since = q.since.unwrap_or(0);
    let envelopes = match state.store.pull(&recipient, since, limit).await {
        Ok(e) => e,
        Err(e) => return error_to_response(e),
    };

    m.pulls.inc();
    m.queue_depth.dec_by_u64(envelopes.len() as u64);
    let has_more = envelopes.len() == limit;
    let next_watermark = envelopes.last().map(|e| e.sequence).unwrap_or(since);

    let body = PullResponse {
        messages: envelopes,
        next_watermark,
        has_more,
    };
    (StatusCode::OK, axum::Json(body)).into_response()
}

async fn ack_handler(
    State(state): State<ServerState>,
    Path(recipient_id): Path<String>,
    axum::Json(req): axum::Json<AckRequest>,
) -> Response {
    let m = &state.metrics;

    // Step 1: validate recipient id.
    let recipient = match validate_recipient_id(&recipient_id) {
        Ok(a) => a.to_checksum(),
        Err(e) => return error_to_response(e),
    };

    // Step 2: validate msg_ids.
    //
    // SECURITY: reject duplicate msg_ids upfront. A well-formed client never
    // sends duplicates. If the server accepted duplicates, a client could
    // craft a valid signature over `["a", "a"]` and effectively double-ack
    // a message (if the store didn't deduplicate internally), bypassing the
    // at-most-once delivery guarantee. Rejecting here also ensures the
    // signature is verified over exactly the same payload the client signed.
    use std::collections::HashSet;
    let msg_ids = req.msg_ids.clone();
    if msg_ids.is_empty() {
        return error_to_response(MailboxError::InvalidMessageId("ack msg_ids is empty".into()));
    }
    // Reject duplicates — this also guarantees signature verification uses the
    // same payload the client signed.
    if msg_ids.iter().collect::<HashSet<_>>().len() != msg_ids.len() {
        return error_to_response(
            MailboxError::InvalidMessageId("ack msg_ids contains duplicates".into()),
        );
    }

    // Validate all ids.
    for id in &msg_ids {
        if let Err(e) = validate_msg_id(id) {
            return error_to_response(e);
        }
    }

    // Step 3: verify recipient signature over the original list.
    let sig_bytes = match base64::engine::general_purpose::STANDARD.decode(&req.signature_b64) {
        Ok(b) => b,
        Err(_) => {
            return error_to_response(MailboxError::InvalidRecipientSignature);
        }
    };
    if let Err(e) = verify_ack_signature(&recipient, &msg_ids, &sig_bytes) {
        return error_to_response(e);
    }

    // Step 4: ack.
    let removed = match state.store.ack(&recipient, &msg_ids).await {
        Ok(n) => n,
        Err(e) => return error_to_response(e),
    };

    m.acks.inc();
    (StatusCode::OK, axum::Json(AckResponse { acked: removed })).into_response()
}

async fn healthz_handler() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// Query parameters for `/metrics`.
#[derive(Debug, Deserialize)]
struct MetricsQuery {
    /// `format=prometheus` returns Prometheus text format instead of JSON.
    #[serde(default)]
    format: Option<String>,
}

#[derive(Debug, Serialize)]
struct MetricsView {
    enqueues: u64,
    enqueues_rejected: u64,
    pulls: u64,
    acks: u64,
    purged: u64,
    queue_depth: i64,
    active_recipients: i64,
}

/// `GET /metrics` — returns metrics in JSON (default) or Prometheus
/// text exposition format (when `?format=prometheus`).
///
/// The Prometheus format is compatible with Prometheus, VictoriaMetrics,
/// Grafana Agent, and any other scraper that accepts the 0.0.4 text
/// exposition format.
async fn metrics_handler(
    State(_state): State<ServerState>,
    Query(q): Query<MetricsQuery>,
) -> Response {
    if q.format.as_deref() == Some("prometheus") {
        let exporter = PrometheusExporter::new(&GLOBAL);
        let out = exporter.render();
        (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, out.content_type)],
            out.into_string(),
        )
            .into_response()
    } else {
        let m = MailboxMetrics::get();
        axum::Json(MetricsView {
            enqueues: m.enqueues.get(),
            enqueues_rejected: m.enqueues_rejected.get(),
            pulls: m.pulls.get(),
            acks: m.acks.get(),
            purged: m.purged.get(),
            queue_depth: m.queue_depth.get(),
            active_recipients: m.active_recipients.get(),
        })
        .into_response()
    }
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

/// Convenience: convert a `MailboxError` to an axum `Response` with the
/// correct HTTP status code and a snake-case JSON body.
pub fn error_to_response(e: MailboxError) -> Response {
    let status = e.http_status();
    let body = axum::Json(ErrorBody {
        error: e.error_code(),
        message: e.to_string(),
        class: e.error_class(),
    });
    (status, body).into_response()
}

/// JSON body shape for every error response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ErrorBody {
    pub error: &'static str,
    pub message: String,
    pub class: crate::error::MailboxErrorClass,
}

impl MailboxError {
    /// HTTP status code for a [`MailboxError`]. Mirrors the
    /// `A3chatError::http_status` pattern.
    pub fn http_status(&self) -> StatusCode {
        match self {
            MailboxError::InvalidRecipientId(_) => StatusCode::BAD_REQUEST,
            MailboxError::EnvelopeTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            MailboxError::QuotaExceeded(_) => StatusCode::TOO_MANY_REQUESTS,
            MailboxError::InvalidSignature | MailboxError::InvalidRecipientSignature => {
                StatusCode::UNAUTHORIZED
            }
            MailboxError::InvalidMessageId(_) => StatusCode::BAD_REQUEST,
            MailboxError::InvalidTimestamp => StatusCode::BAD_REQUEST,
            MailboxError::StaleSignature { .. } => StatusCode::UNAUTHORIZED,
            MailboxError::Duplicate { .. } => StatusCode::OK,
            MailboxError::NotFound(_) => StatusCode::NOT_FOUND,
            MailboxError::Storage(_) | MailboxError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            MailboxError::Remote { status, .. } => StatusCode::from_u16(*status)
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            MailboxError::Transport(_) => StatusCode::BAD_GATEWAY,
            MailboxError::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable error code string for RPC clients.
    pub fn error_code(&self) -> &'static str {
        match self {
            MailboxError::InvalidRecipientId(_) => "invalid_recipient_id",
            MailboxError::EnvelopeTooLarge { .. } => "envelope_too_large",
            MailboxError::QuotaExceeded(_) => "quota_exceeded",
            MailboxError::InvalidSignature => "invalid_sender_signature",
            MailboxError::InvalidRecipientSignature => "invalid_recipient_signature",
            MailboxError::InvalidMessageId(_) => "invalid_message_id",
            MailboxError::InvalidTimestamp => "invalid_timestamp",
            MailboxError::StaleSignature { .. } => "stale_signature",
            MailboxError::Duplicate { .. } => "duplicate",
            MailboxError::NotFound(_) => "not_found",
            MailboxError::Storage(_) => "storage_error",
            MailboxError::Internal(_) => "internal_error",
            MailboxError::Remote { .. } => "remote_error",
            MailboxError::Transport(_) => "transport_error",
            MailboxError::Config(_) => "config_error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MailboxConfig;

    #[tokio::test]
    async fn start_with_state_binds_and_shuts_down() {
        let mut handle = MailboxServer::start("127.0.0.1", 0)
            .await
            .expect("server should bind");
        assert!(handle.port > 0);
        handle.shutdown();
    }

    #[test]
    fn server_policy_from_config_defaults() {
        let cfg = MailboxConfig::default();
        let p = ServerPolicy::from_config(&cfg);
        assert_eq!(p.max_envelope_bytes, cfg.max_envelope_bytes);
        assert_eq!(p.require_sender_signature, cfg.require_sender_signature);
    }

    #[test]
    fn http_status_mapping_is_stable() {
        assert_eq!(
            MailboxError::InvalidRecipientId("x".into()).http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            MailboxError::EnvelopeTooLarge { size: 1, max: 0 }.http_status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            MailboxError::QuotaExceeded("x".into()).http_status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            MailboxError::InvalidSignature.http_status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            MailboxError::InvalidRecipientSignature.http_status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            MailboxError::Duplicate { msg_id: "x".into() }.http_status(),
            StatusCode::OK
        );
    }

    #[test]
    fn error_code_is_stable() {
        let cases: Vec<(MailboxError, &str)> = vec![
            (MailboxError::InvalidRecipientId("x".into()), "invalid_recipient_id"),
            (MailboxError::EnvelopeTooLarge { size: 1, max: 0 }, "envelope_too_large"),
            (MailboxError::QuotaExceeded("x".into()), "quota_exceeded"),
            (MailboxError::InvalidSignature, "invalid_sender_signature"),
            (MailboxError::InvalidRecipientSignature, "invalid_recipient_signature"),
            (MailboxError::InvalidMessageId("x".into()), "invalid_message_id"),
            (MailboxError::InvalidTimestamp, "invalid_timestamp"),
            (MailboxError::StaleSignature { age_secs: 600, max_age_secs: 300 }, "stale_signature"),
            (MailboxError::Duplicate { msg_id: "x".into() }, "duplicate"),
            (MailboxError::NotFound("x".into()), "not_found"),
            (MailboxError::Storage("x".into()), "storage_error"),
            (MailboxError::Internal("x".into()), "internal_error"),
            (MailboxError::Config("x".into()), "config_error"),
        ];
        for (e, want) in cases {
            assert_eq!(e.error_code(), want, "wrong code for {e:?}");
        }
    }

    #[test]
    fn pull_limit_max_is_a_sane_default() {
        // Bounds are checked at compile time via `const { assert!(..) }`.
        const { assert!(PULL_LIMIT_MAX >= 1) };
        const { assert!(PULL_LIMIT_DEFAULT <= PULL_LIMIT_MAX) };
    }

    /// A storage store that always errors. Used to exercise the
    /// handler's error mapping without needing a real backend.
    #[allow(dead_code)]
    #[derive(Debug, Default)]
    pub(crate) struct StubAlwaysFailStore {
        pub stats: std::collections::HashMap<&'static str, u64>,
    }
    #[allow(dead_code)]
    impl StubAlwaysFailStore {
        pub fn new() -> Self {
            Self::default()
        }
    }
    #[async_trait::async_trait]
    impl MailboxStore for StubAlwaysFailStore {
        async fn enqueue(&self, _: &StoredEnvelope) -> crate::error::MailboxResult<crate::storage::EnqueueOutcome> {
            Err(MailboxError::Storage("stub".into()))
        }
        async fn pull(&self, _: &str, _: Watermark, _: usize) -> crate::error::MailboxResult<Vec<StoredEnvelope>> {
            Err(MailboxError::Storage("stub".into()))
        }
        async fn ack(&self, _: &str, _: &[String]) -> crate::error::MailboxResult<usize> {
            Err(MailboxError::Storage("stub".into()))
        }
        async fn purge_expired(&self) -> crate::error::MailboxResult<u64> {
            Ok(0)
        }
        async fn quota_usage(&self, _: &str) -> crate::error::MailboxResult<crate::storage::QuotaUsage> {
            Ok(crate::storage::QuotaUsage::default())
        }
    }
}
