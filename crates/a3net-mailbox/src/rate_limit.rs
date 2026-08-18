//! Per-IP rate limiting middleware for the mailbox server.
//!
//! ## Design
//!
//! - **Token-bucket per remote IP**: each IP address gets a bucket of
//!   `capacity` tokens refilled at `refill_per_sec` rate. When the bucket
//!   is empty, requests are rejected with `429 Too Many Requests` and a
//!   `Retry-After` header.
//! - **Axum middleware via `axum::middleware::from_fn_with_state`**:
//!   a simple async function that wraps handlers, extracts the client IP
//!   from `ConnectInfo<SocketAddr>` (set by `TcpListener` bind) or from
//!   `X-Forwarded-For` header (for proxied setups).
//! - **Separate limits per operation**: `enqueue` has tighter limits
//!   (spam prevention); `pull`/`ack` have looser limits (polling clients).
//! - **No shared state across processes**: in-process only. Multi-node
//!   deployments need a shared store (Redis / Memcached) — see P3-backlog.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderValue, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use dashmap::DashMap;
use parking_lot::Mutex;
use serde::Serialize;
use tokio::time::sleep;

/// Shared per-IP token-bucket registry.
#[derive(Debug, Clone)]
pub struct RateLimitRegistry {
    inner: Arc<DashMap<String, Arc<Mutex<BucketState>>>>,
}

impl Default for RateLimitRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self { inner: Arc::new(DashMap::new()) }
    }

    /// Get the bucket for `ip`, creating it if absent.
    fn bucket(&self, ip: &str) -> Arc<Mutex<BucketState>> {
        self.inner
            .entry(ip.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(BucketState::default())))
            .clone()
    }
}

/// Per-IP token bucket.
#[derive(Debug, Clone)]
struct BucketState {
    tokens: f64,
    last_refill: Instant,
}

impl Default for BucketState {
    fn default() -> Self {
        Self { tokens: POLICY_DEFAULT.capacity, last_refill: Instant::now() }
    }
}

/// Configuration for one rate-limit tier.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    /// Maximum burst size (tokens in the bucket).
    pub capacity: f64,
    /// Tokens added per second.
    pub refill_per_sec: f64,
    /// Seconds to wait after rejection (Retry-After header value).
    pub retry_after_secs: u64,
}

impl RateLimitConfig {
    /// Enqueue policy: tight limits to prevent spam. 60 req/min per IP.
    pub fn enqueue() -> Self {
        Self { capacity: 60.0, refill_per_sec: 1.0, retry_after_secs: 10 }
    }

    /// Read policy: looser limits for pull/ack polling. 300 req/min per IP.
    pub fn read() -> Self {
        Self { capacity: 300.0, refill_per_sec: 5.0, retry_after_secs: 5 }
    }
}

const POLICY_DEFAULT: RateLimitConfig = RateLimitConfig {
    capacity: 60.0,
    refill_per_sec: 1.0,
    retry_after_secs: 10,
};

impl Default for RateLimitConfig {
    fn default() -> Self {
        POLICY_DEFAULT
    }
}

/// Result of checking an IP. `Ok(remaining)` means allowed; `Err(retry_after)`
/// means the IP is rate-limited.
#[derive(Debug)]
pub enum RateLimitResult {
    Allowed(f64),
    Rejected { retry_after: u64 },
}

/// Try to consume one token for `ip` from the registry.
/// Returns `Ok(remaining_tokens)` if allowed, `Err(retry_after_secs)` if not.
pub fn check_and_consume(
    registry: &RateLimitRegistry,
    policy: &RateLimitConfig,
    ip: &str,
) -> RateLimitResult {
    let bucket = registry.bucket(ip);
    let mut state = bucket.lock();
    let now = Instant::now();
    let elapsed = now.duration_since(state.last_refill).as_secs_f64();

    // Refill tokens based on elapsed time.
    state.tokens = (state.tokens + elapsed * policy.refill_per_sec).min(policy.capacity);
    state.last_refill = now;

    if state.tokens < 1.0 {
        RateLimitResult::Rejected { retry_after: policy.retry_after_secs }
    } else {
        state.tokens -= 1.0;
        RateLimitResult::Allowed(state.tokens)
    }
}

/// Extract the client IP from `req`.
///
/// Tries in order:
/// 1. `X-Real-IP` header (set by some reverse proxies like nginx).
/// 2. `X-Forwarded-For` first value (for proxied setups).
/// 3. Fallback to `"0.0.0.0"` (will be globally rate-limited).
#[inline]
pub fn client_ip(req: &Request<Body>) -> String {
    // 1. Try X-Real-IP header (set by some reverse proxies).
    if let Some(val) = req.headers().get("x-real-ip") {
        if let Ok(ip) = val.to_str() {
            let ip = ip.trim();
            if !ip.is_empty() && ip.len() < 48 {
                return ip.to_string();
            }
        }
    }
    // 2. X-Forwarded-For.
    if let Some(val) = req.headers().get("x-forwarded-for") {
        if let Ok(s) = val.to_str() {
            if let Some(ip) = s.split(',').next() {
                let ip = ip.trim();
                if !ip.is_empty() {
                    return ip.to_string();
                }
            }
        }
    }
    // 3. Fallback.
    "0.0.0.0".to_string()
}

/// JSON body for a rate-limit rejection.
#[derive(Serialize)]
struct RateLimitBody<'a> {
    error: &'a str,
    message: &'a str,
    retry_after: u64,
}

/// Build a 429 response with JSON body and Retry-After header.
fn rate_limit_response(retry_after: u64) -> Response {
    let body = serde_json::json!({
        "error": "rate_limited",
        "message": "too many requests from this IP",
        "retry_after": retry_after,
    });
    let mut res = Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header(axum::http::header::RETRY_AFTER, retry_after.to_string())
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    res
}

// ---------------------------------------------------------------------------
// Axum middleware functions (to be used with `from_fn_with_state`)
// ---------------------------------------------------------------------------

use crate::error::MailboxError;

/// State carried through the rate-limit middleware.
#[derive(Clone)]
pub struct RateLimitState {
    pub registry: RateLimitRegistry,
    pub policy: RateLimitConfig,
}

impl RateLimitState {
    pub fn new(registry: RateLimitRegistry, policy: RateLimitConfig) -> Self {
        Self { registry, policy }
    }

    /// Enqueue middleware: use this when you want the tighter enqueue policy.
    pub fn enqueue(registry: RateLimitRegistry) -> Self {
        Self::new(registry, RateLimitConfig::enqueue())
    }

    /// Read middleware: use this for pull/ack (looser policy).
    pub fn read(registry: RateLimitRegistry) -> Self {
        Self::new(registry, RateLimitConfig::read())
    }
}

/// Middleware: check rate limit before passing to next handler.
/// Uses `state.policy` so callers choose the tier by constructing the right state.
pub async fn rate_limit_middleware(
    State(state): State<RateLimitState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let ip = client_ip(&req);
    match check_and_consume(&state.registry, &state.policy, &ip) {
        RateLimitResult::Allowed(_) => next.run(req).await,
        RateLimitResult::Rejected { retry_after } => {
            tracing::warn!(ip = %ip, retry_after, "rate limited");
            rate_limit_response(retry_after)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const IP1: &str = "198.51.100.42";
    const IP2: &str = "203.0.113.1";

    fn default_policy() -> RateLimitConfig {
        RateLimitConfig::enqueue()
    }

    #[test]
    fn policy_defaults() {
        let p = RateLimitConfig::default();
        assert_eq!(p.capacity, 60.0);
        assert_eq!(p.refill_per_sec, 1.0);
    }

    #[test]
    fn enqueue_is_tighter_than_read() {
        let enq = RateLimitConfig::enqueue();
        let read = RateLimitConfig::read();
        assert!(enq.capacity < read.capacity);
        assert!(enq.refill_per_sec <= read.refill_per_sec);
    }

    #[test]
    fn within_burst_allowed() {
        let registry = RateLimitRegistry::new();
        let policy = RateLimitConfig::enqueue();
        for _ in 0..5 {
            let r = check_and_consume(&registry, &policy, IP1);
            assert!(matches!(r, RateLimitResult::Allowed(_)));
        }
    }

    #[test]
    fn burst_exhausted_rejected() {
        let registry = RateLimitRegistry::new();
        let policy = RateLimitConfig::enqueue();
        for _ in 0..policy.capacity as usize {
            let _ = check_and_consume(&registry, &policy, IP1);
        }
        let r = check_and_consume(&registry, &policy, IP1);
        match r {
            RateLimitResult::Rejected { retry_after } => {
                assert_eq!(retry_after, 10);
            }
            _ => panic!("expected Rejected, got {r:?}"),
        }
    }

    #[test]
    fn tokens_refill_over_time() {
        let registry = RateLimitRegistry::new();
        let policy = RateLimitConfig::enqueue();
        for _ in 0..policy.capacity as usize {
            let _ = check_and_consume(&registry, &policy, IP1);
        }
        // Tokens should start refilling immediately after the next check.
        // Wait enough time for at least 1 token.
        std::thread::sleep(Duration::from_millis(1100));
        let r = check_and_consume(&registry, &policy, IP1);
        assert!(matches!(r, RateLimitResult::Allowed(_)));
    }

    #[test]
    fn different_ips_independent() {
        let registry = RateLimitRegistry::new();
        let policy = RateLimitConfig::enqueue();
        // Exhaust IP1.
        for _ in 0..policy.capacity as usize {
            let _ = check_and_consume(&registry, &policy, IP1);
        }
        // IP2 should still have a full bucket.
        let r = check_and_consume(&registry, &policy, IP2);
        assert!(matches!(r, RateLimitResult::Allowed(_)));
    }

    #[test]
    fn bucket_created_lazily() {
        let registry = RateLimitRegistry::new();
        let policy = RateLimitConfig::enqueue();
        // No bucket for IP1 yet.
        assert!(!registry.inner.contains_key(IP1));
        let _ = check_and_consume(&registry, &policy, IP1);
        assert!(registry.inner.contains_key(IP1));
    }

    #[test]
    fn multiple_ips_all_tracked() {
        let registry = RateLimitRegistry::new();
        let policy = RateLimitConfig::enqueue();
        for ip in ["10.0.0.1", "10.0.0.2", "10.0.0.3"] {
            let _ = check_and_consume(&registry, &policy, ip);
        }
        assert_eq!(registry.inner.len(), 3);
    }

    #[tokio::test]
    async fn client_ip_from_forwarded_for() {
        let mut req = Request::builder()
            .header("x-forwarded-for", "5.6.7.8, 10.0.0.1")
            .body(Body::empty())
            .unwrap();
        assert_eq!(client_ip(&req), "5.6.7.8");
    }

    #[tokio::test]
    async fn client_ip_fallback() {
        let req = Request::builder().body(Body::empty()).unwrap();
        assert_eq!(client_ip(&req), "0.0.0.0");
    }

    #[tokio::test]
    async fn client_ip_real_ip_header_takes_priority() {
        let mut req = Request::builder()
            .header("x-real-ip", "9.9.9.9")
            .header("x-forwarded-for", "5.6.7.8")
            .body(Body::empty())
            .unwrap();
        assert_eq!(client_ip(&req), "9.9.9.9");
    }
}
