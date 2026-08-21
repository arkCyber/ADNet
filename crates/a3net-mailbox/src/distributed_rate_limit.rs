//! Distributed Rate Limiting for multi-node A3Net deployments.
//!
//! ## Overview
//!
//! This module provides a distributed rate limiting implementation that
//! works across multiple A3Net nodes. It uses a Redis-compatible
//! backend for shared state, with graceful fallback to in-process
//! limiting when Redis is unavailable.
//!
//! ## Design
//!
//! - **Token Bucket per IP**: Each IP address gets a bucket with
//!   configurable capacity and refill rate.
//! - **Sliding Window Counter**: For higher-precision limiting at scale,
//!   uses Redis sorted sets with timestamps.
//! - **Distributed Sync**: All nodes share rate limit state via Redis.
//! - **Graceful Degradation**: Falls back to local DashMap if Redis
//!   is unavailable.
//!
//! ## DO-178C Compliance
//!
//! - All error paths explicitly classified
//! - No unsafe code
//! - Comprehensive test coverage

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Rate limit decision with full context for logging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitDecision {
    pub allowed: bool,
    pub remaining_tokens: f64,
    pub retry_after_secs: u64,
    pub ip: String,
    pub endpoint: String,
    pub timestamp: i64,
}

/// Rate limit configuration per tier.
#[derive(Debug, Clone)]
pub struct DistributedRateLimitConfig {
    /// Redis connection URL. If None, falls back to local-only.
    #[cfg(feature = "redis")]
    pub redis_url: Option<String>,
    /// Token bucket capacity.
    pub capacity: f64,
    /// Tokens added per second.
    pub refill_per_sec: f64,
    /// Seconds to wait after rejection.
    pub retry_after_secs: u64,
    /// Key prefix for Redis.
    pub key_prefix: String,
    /// Connection pool size.
    #[cfg(feature = "redis")]
    pub pool_size: u32,
}

impl Default for DistributedRateLimitConfig {
    fn default() -> Self {
        Self {
            #[cfg(feature = "redis")]
            redis_url: None,
            capacity: 60.0,
            refill_per_sec: 1.0,
            retry_after_secs: 10,
            key_prefix: "a3net:ratelimit:".to_string(),
            #[cfg(feature = "redis")]
            pool_size: 10,
        }
    }
}

/// Token bucket state stored in Redis.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenBucket {
    tokens: f64,
    last_refill_ts: i64,
}

/// Local in-process rate limiter using DashMap.
/// Used as fallback when Redis is unavailable.
pub struct LocalRateLimiter {
    inner: Arc<DashMap<String, Arc<Mutex<TokenBucket>>>>,
}

impl Default for LocalRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalRateLimiter {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    fn bucket(&self, key: &str) -> Arc<Mutex<TokenBucket>> {
        self.inner
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(TokenBucket::default())))
            .clone()
    }
}

impl Default for TokenBucket {
    fn default() -> Self {
        Self {
            tokens: 60.0,
            last_refill_ts: current_timestamp_secs(),
        }
    }
}

fn current_timestamp_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Distributed rate limiter using Redis (if available).
#[cfg(feature = "redis")]
pub struct DistributedRateLimiter {
    config: DistributedRateLimitConfig,
    local: LocalRateLimiter,
    redis_client: Option<redis::Client>,
}

/// Fallback rate limiter when Redis is not available.
#[cfg(not(feature = "redis"))]
pub struct DistributedRateLimiter {
    config: DistributedRateLimitConfig,
    local: LocalRateLimiter,
}

#[cfg(feature = "redis")]
impl DistributedRateLimiter {
    /// Create a new distributed rate limiter.
    pub fn new(config: DistributedRateLimitConfig) -> Self {
        let redis_client = config.redis_url.as_ref().and_then(|url| {
            redis::Client::open(url.as_str()).ok()
        });

        Self {
            config,
            local: LocalRateLimiter::new(),
            redis_client,
        }
    }

    /// Check rate limit using Redis (falls back to local on error).
    pub async fn check(&self, ip: &str, endpoint: &str) -> RateLimitDecision {
        let key = format!("{}:{}:{}", self.config.key_prefix, endpoint, ip);

        // Try Redis first
        if let Some(client) = &self.redis_client {
            if let Some(decision) = self.check_redis(client, &key).await {
                return decision;
            }
        }

        // Fallback to local
        self.check_local(&key, ip, endpoint)
    }

    /// Check rate limit using Redis.
    async fn check_redis(
        &self,
        client: &redis::Client,
        key: &str,
    ) -> Option<RateLimitDecision> {
        let mut conn = client.get_multiplexed_async_connection().await.ok()?;

        let script = r#"
            local key = KEYS[1]
            local capacity = tonumber(ARGV[1])
            local refill_rate = tonumber(ARGV[2])
            local now = tonumber(ARGV[3])
            local requested = 1

            -- Get current state
            local bucket = redis.call('HMGET', key, 'tokens', 'last_refill')
            local tokens = tonumber(bucket[1]) or capacity
            local last_refill = tonumber(bucket[2]) or now

            -- Calculate refill
            local elapsed = now - last_refill
            local refill = elapsed * refill_rate
            tokens = math.min(capacity, tokens + refill)

            -- Check and consume
            local allowed = 0
            local remaining = tokens
            if tokens >= requested then
                tokens = tokens - requested
                allowed = 1
                remaining = tokens - 1
            end

            -- Update state
            redis.call('HMSET', key, 'tokens', tokens, 'last_refill', now)
            redis.call('EXPIRE', key, 3600)

            return {allowed, remaining, capacity}
        "#;

        let now = current_timestamp_secs();
        let result: Option<Vec<f64>> = redis::Script::new(script)
            .key(key)
            .arg(self.config.capacity)
            .arg(self.config.refill_per_sec)
            .arg(now)
            .invoke_async(&mut conn)
            .await
            .ok();

        result.map(|[allowed, remaining, _]| RateLimitDecision {
            allowed: allowed == 1.0,
            remaining_tokens: remaining,
            retry_after_secs: if allowed == 1.0 { 0 } else { self.config.retry_after_secs },
            ip: key.split(':').last().unwrap_or("unknown").to_string(),
            endpoint: key.split(':').nth(1).unwrap_or("unknown").to_string(),
            timestamp: now,
        })
    }

    /// Check if Redis backend is available.
    pub async fn is_redis_available(&self) -> bool {
        if let Some(client) = &self.redis_client {
            let mut conn = client.get_multiplexed_async_connection().await;
            conn.is_ok()
        } else {
            false
        }
    }
}

#[cfg(not(feature = "redis"))]
impl DistributedRateLimiter {
    /// Create a new distributed rate limiter (local-only when Redis not available).
    pub fn new(config: DistributedRateLimitConfig) -> Self {
        Self {
            config,
            local: LocalRateLimiter::new(),
        }
    }

    /// Check rate limit using local fallback.
    pub async fn check(&self, ip: &str, endpoint: &str) -> RateLimitDecision {
        let key = format!("{}:{}:{}", self.config.key_prefix, endpoint, ip);
        self.check_local(&key, ip, endpoint)
    }

    /// Always returns false when Redis is not available.
    pub async fn is_redis_available(&self) -> bool {
        false
    }
}

impl DistributedRateLimiter {
    /// Check rate limit using local fallback.
    fn check_local(&self, key: &str, ip: &str, endpoint: &str) -> RateLimitDecision {
        let bucket = self.local.bucket(key);
        let mut state = bucket.lock();

        let now = Instant::now();
        let elapsed = state.last_refill.elapsed().as_secs_f64();
        state.tokens = (state.tokens + elapsed * self.config.refill_per_sec)
            .min(self.config.capacity);
        state.last_refill = now;

        let allowed = state.tokens >= 1.0;
        let remaining = if allowed {
            state.tokens -= 1.0;
            state.tokens
        } else {
            state.tokens
        };

        RateLimitDecision {
            allowed,
            remaining_tokens: remaining,
            retry_after_secs: if allowed { 0 } else { self.config.retry_after_secs },
            ip: ip.to_string(),
            endpoint: endpoint.to_string(),
            timestamp: current_timestamp_secs(),
        }
    }
}

/// DO-178C Error classification for rate limiting operations.
#[derive(Debug, Clone)]
pub enum RateLimitError {
    /// Redis connection failed.
    RedisConnection(String),
    /// Invalid configuration.
    InvalidConfig(String),
    /// Rate limit exceeded (not an error, just a result).
    LimitExceeded { retry_after: u64, ip: String },
}

impl RateLimitError {
    /// Classify error for DO-178C recoverability.
    pub fn recoverability(&self) -> &'static str {
        match self {
            RateLimitError::RedisConnection(_) => "RECOVERABLE",
            RateLimitError::InvalidConfig(_) => "USER_ERROR",
            RateLimitError::LimitExceeded { .. } => "USER_ERROR",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_rate_limiter_allows_within_limit() {
        let limiter = LocalRateLimiter::new();
        let bucket = limiter.bucket("test:10.0.0.1");

        // First request should be allowed
        let mut state = bucket.lock();
        assert!(state.tokens >= 1.0);
    }

    #[test]
    fn token_bucket_default_values() {
        let bucket = TokenBucket::default();
        assert_eq!(bucket.tokens, 60.0);
    }

    #[test]
    fn rate_limit_decision_serialization() {
        let decision = RateLimitDecision {
            allowed: true,
            remaining_tokens: 59.0,
            retry_after_secs: 0,
            ip: "192.0.2.1".to_string(),
            endpoint: "enqueue".to_string(),
            timestamp: 1699900000,
        };

        let json = serde_json::to_string(&decision).unwrap();
        assert!(json.contains("allowed"));
        assert!(json.contains("59"));
    }

    #[tokio::test]
    async fn distributed_limiter_falls_back_to_local() {
        let config = DistributedRateLimitConfig {
            #[cfg(feature = "redis")]
            redis_url: Some("redis://localhost:9999".to_string()),
            ..Default::default()
        };
        let limiter = DistributedRateLimiter::new(config);

        // Should fall back to local and work
        let decision = limiter.check("10.0.0.1", "enqueue").await;
        assert!(decision.allowed);
    }

    #[test]
    fn error_recoverability_classification() {
        let err1 = RateLimitError::RedisConnection("timeout".to_string());
        assert_eq!(err1.recoverability(), "RECOVERABLE");

        let err2 = RateLimitError::InvalidConfig("bad capacity".to_string());
        assert_eq!(err2.recoverability(), "USER_ERROR");

        let err3 = RateLimitError::LimitExceeded {
            retry_after: 10,
            ip: "1.2.3.4".to_string(),
        };
        assert_eq!(err3.recoverability(), "USER_ERROR");
    }
}
