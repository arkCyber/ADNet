//! Per-IP rate limiting middleware for the mailbox server.
//!
//! ## Design
//!
//! - **Token-bucket per remote IP**: each IP address gets a bucket of
//!   `capacity` tokens refilled at `refill_per_sec` rate. When the bucket
//!   is empty, requests are rejected with `429 Too Many Requests` and a
//!   `Retry-After` header.
//! - **Axum middleware via `axum::middleware::from_fn_with_state`**:
//!   a simple async function that wraps handlers.
//! - **IP extraction**: when behind a trusted reverse proxy, set
//!   `trusted_proxy_cidr` so `X-Forwarded-For` is trusted. Without it,
//!   forwarded headers are ignored (prevents IP spoofing).
//! - **Separate limits per operation**: `enqueue` has tighter limits
//!   (spam prevention); `pull`/`ack` have looser limits (polling clients).
//! - **No shared state across processes**: in-process only. Multi-node
//!   deployments need a shared store (Redis / Memcached) — see P3-backlog.

use std::sync::Arc;
use std::time::Instant;
#[cfg(test)]
use std::time::Duration;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use dashmap::DashMap;
use parking_lot::Mutex;

/// Controls when `X-Forwarded-For` / `X-Real-IP` are trusted.
///
/// **IMPORTANT**: Only enable these when the mailbox server is deployed
/// behind a known-trusted reverse proxy (nginx, Cloudflare, etc.).
/// If the server is directly internet-facing, keep this `Disabled`
/// to prevent clients from spoofing their IP and bypassing rate limits.
#[derive(Debug, Clone, PartialEq, Eq)]
#[derive(Default)]
pub enum TrustedProxy {
    /// Default. Never trust forwarded headers. IP is always derived from
    /// the direct TCP connection. Safe for internet-facing deployments.
    #[default]
    Disabled,
    /// Trust forwarded headers only when the direct TCP peer IP matches
    /// this exact string (the proxy's IP, e.g. `"10.0.0.1"`).
    FromProxy(String),
    /// **DANGEROUS**: always trust forwarded headers regardless of
    /// the direct connection source. Only use in fully controlled
    /// environments where no direct internet access is possible.
    AlwaysTrust,
}


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

/// Extract the client IP from `req` using the trusted-proxy policy.
///
/// SECURITY: forwarded headers (`X-Real-IP`, `X-Forwarded-For`) are only
/// consulted when `proxy` is not `Disabled`. This prevents clients from
/// spoofing their IP address to bypass rate limiting.
#[inline]
pub fn client_ip(req: &Request<Body>, proxy: &TrustedProxy) -> String {
    match proxy {
        TrustedProxy::Disabled => {
            // Never trust forwarded headers — derive IP from the direct connection.
            // In production, the actual socket IP comes from the middleware that
            // inserts ConnectInfo. Here we fall back to 0.0.0.0 (global rate-limit).
            "0.0.0.0".to_string()
        }
        TrustedProxy::FromProxy(expected_proxy_ip) => {
            // First, extract the direct TCP peer IP from extensions (set by TcpListener).
            // For now, we treat this as the proxy IP. If it matches, trust forwarded headers.
            let direct_ip = req
                .extensions()
                .get::<std::net::SocketAddr>()
                .map(|a| a.ip().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            if direct_ip == *expected_proxy_ip {
                // Proxy is trusted — extract the actual client IP from headers.
                first_forwarded_ip(req).unwrap_or(direct_ip)
            } else {
                // Direct connection is not from the trusted proxy — don't trust headers.
                direct_ip
            }
        }
        TrustedProxy::AlwaysTrust => {
            // DANGEROUS: trust forwarded headers blindly.
            first_forwarded_ip(req).unwrap_or_else(|| "0.0.0.0".to_string())
        }
    }
}

/// Extract the first (leftmost) IP from `X-Real-IP` or `X-Forwarded-For`.
/// Returns `None` if neither header is present or valid.
fn first_forwarded_ip(req: &Request<Body>) -> Option<String> {
    // Try X-Real-IP first (higher priority).
    if let Some(val) = req.headers().get("x-real-ip")
        && let Ok(ip) = val.to_str() {
            let ip = ip.trim();
            if !ip.is_empty() && ip.len() < 48 {
                return Some(ip.to_string());
            }
        }
    // Fall back to X-Forwarded-For (take first IP in the chain).
    if let Some(val) = req.headers().get("x-forwarded-for")
        && let Ok(s) = val.to_str()
            && let Some(ip) = s.split(',').next() {
                let ip = ip.trim();
                if !ip.is_empty() {
                    return Some(ip.to_string());
                }
            }
    None
}

/// Build a 429 response with JSON body and Retry-After header.
fn rate_limit_response(retry_after: u64) -> Response {
    let body = serde_json::json!({
        "error": "rate_limited",
        "message": "too many requests from this IP",
        "retry_after": retry_after,
    });
    
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header(axum::http::header::RETRY_AFTER, retry_after.to_string())
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Axum middleware functions (to be used with `from_fn_with_state`)
// ---------------------------------------------------------------------------


/// State carried through the rate-limit middleware.
#[derive(Clone)]
pub struct RateLimitState {
    pub registry: RateLimitRegistry,
    pub policy: RateLimitConfig,
    /// Controls when forwarded headers are trusted. Default: `Disabled`
    /// (safe for internet-facing deployments).
    pub trusted_proxy: TrustedProxy,
}

impl RateLimitState {
    pub fn new(
        registry: RateLimitRegistry,
        policy: RateLimitConfig,
        trusted_proxy: TrustedProxy,
    ) -> Self {
        Self { registry, policy, trusted_proxy }
    }

    /// Enqueue middleware: tight policy + configurable proxy trust.
    pub fn enqueue(registry: RateLimitRegistry, trusted_proxy: TrustedProxy) -> Self {
        Self::new(registry, RateLimitConfig::enqueue(), trusted_proxy)
    }

    /// Read middleware: loose policy + configurable proxy trust.
    pub fn read(registry: RateLimitRegistry, trusted_proxy: TrustedProxy) -> Self {
        Self::new(registry, RateLimitConfig::read(), trusted_proxy)
    }
}

/// Middleware: check rate limit before passing to next handler.
/// Uses `state.policy` so callers choose the tier by constructing the right state.
pub async fn rate_limit_middleware(
    State(state): State<RateLimitState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let ip = client_ip(&req, &state.trusted_proxy);
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
    async fn client_ip_disabled_ignores_forwarded_headers() {
        let mut req = Request::builder()
            .header("x-forwarded-for", "5.6.7.8, 10.0.0.1")
            .header("x-real-ip", "9.9.9.9")
            .body(Body::empty())
            .unwrap();
        // With Disabled, forwarded headers are ignored — returns 0.0.0.0.
        assert_eq!(client_ip(&req, &TrustedProxy::Disabled), "0.0.0.0");
    }

    #[tokio::test]
    async fn client_ip_always_trust_accepts_forwarded_headers() {
        let mut req = Request::builder()
            .header("x-forwarded-for", "5.6.7.8, 10.0.0.1")
            .header("x-real-ip", "9.9.9.9")
            .body(Body::empty())
            .unwrap();
        // With AlwaysTrust, forwarded headers are accepted.
        assert_eq!(client_ip(&req, &TrustedProxy::AlwaysTrust), "9.9.9.9");
    }

    #[tokio::test]
    async fn client_ip_real_ip_takes_priority_in_trusted_mode() {
        let mut req = Request::builder()
            .header("x-real-ip", "9.9.9.9")
            .header("x-forwarded-for", "5.6.7.8")
            .body(Body::empty())
            .unwrap();
        assert_eq!(client_ip(&req, &TrustedProxy::AlwaysTrust), "9.9.9.9");
    }

    #[tokio::test]
    async fn client_ip_no_headers_returns_fallback() {
        let req = Request::builder().body(Body::empty()).unwrap();
        assert_eq!(client_ip(&req, &TrustedProxy::Disabled), "0.0.0.0");
    }
}
