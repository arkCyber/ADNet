//! Rate limiter for keyword notifications using Token Bucket algorithm.
//!
//! Prevents notification storms by limiting the rate at which keyword
//! matches can trigger notifications.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use a3chat_core::id::UserId;

/// Rate limiter configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimiterConfig {
    /// Maximum number of notifications allowed per time window
    pub max_notifications: u32,
    /// Time window duration in seconds
    pub window_seconds: u64,
    /// Whether to use per-keyword limiting (true) or global per-user limiting (false)
    pub per_keyword: bool,
}

impl Default for RateLimiterConfig {
    fn default() -> Self {
        Self {
            max_notifications: 10, // 10 notifications per window
            window_seconds: 60,    // 1 minute window
            per_keyword: true,     // Limit per (user, keyword) pair
        }
    }
}

/// Token bucket for rate limiting
#[derive(Debug, Clone)]
struct TokenBucket {
    /// Maximum number of tokens (capacity)
    capacity: u32,
    /// Current number of tokens
    tokens: f64,
    /// Last refill timestamp
    last_refill: Instant,
    /// Refill rate (tokens per second)
    refill_rate: f64,
}

impl TokenBucket {
    fn new(capacity: u32, window_seconds: u64) -> Self {
        let refill_rate = capacity as f64 / window_seconds as f64;
        Self {
            capacity,
            tokens: capacity as f64,
            last_refill: Instant::now(),
            refill_rate,
        }
    }

    /// Try to consume a token. Returns true if successful, false if rate limited.
    fn try_consume(&mut self) -> bool {
        self.refill();
        
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Refill tokens based on elapsed time
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        
        if elapsed > 0.0 {
            let new_tokens = elapsed * self.refill_rate;
            self.tokens = (self.tokens + new_tokens).min(self.capacity as f64);
            self.last_refill = now;
        }
    }

    /// Get current number of available tokens
    fn available_tokens(&mut self) -> u32 {
        self.refill();
        self.tokens.floor() as u32
    }
}

/// Key for rate limiting (user + optional keyword)
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct RateLimitKey {
    user_id: UserId,
    keyword: Option<String>,
}

impl RateLimitKey {
    fn user_only(user_id: UserId) -> Self {
        Self {
            user_id,
            keyword: None,
        }
    }

    fn user_keyword(user_id: UserId, keyword: String) -> Self {
        Self {
            user_id,
            keyword: Some(keyword),
        }
    }
}

/// Rate limiter for keyword notifications
pub struct KeywordRateLimiter {
    config: RateLimiterConfig,
    buckets: Arc<RwLock<HashMap<RateLimitKey, TokenBucket>>>,
    /// Total number of rate-limited notifications
    total_limited: Arc<parking_lot::Mutex<u64>>,
}

impl KeywordRateLimiter {
    /// Create a new rate limiter with the given configuration
    pub fn new(config: RateLimiterConfig) -> Self {
        Self {
            config,
            buckets: Arc::new(RwLock::new(HashMap::new())),
            total_limited: Arc::new(parking_lot::Mutex::new(0)),
        }
    }

    /// Create a rate limiter with default configuration
    pub fn default() -> Self {
        Self::new(RateLimiterConfig::default())
    }

    /// Check if a notification is allowed for the given user and keyword
    ///
    /// Returns `true` if the notification should be sent, `false` if rate limited.
    pub fn check_allowed(&self, user_id: &UserId, keyword: &str) -> bool {
        let key = if self.config.per_keyword {
            RateLimitKey::user_keyword(user_id.clone(), keyword.to_string())
        } else {
            RateLimitKey::user_only(user_id.clone())
        };

        let mut buckets = self.buckets.write();
        let bucket = buckets.entry(key).or_insert_with(|| {
            TokenBucket::new(self.config.max_notifications, self.config.window_seconds)
        });

        let allowed = bucket.try_consume();
        
        if !allowed {
            // Increment rate-limited counter
            *self.total_limited.lock() += 1;
        }

        allowed
    }

    /// Get the number of available notifications for a user/keyword
    pub fn available_quota(&self, user_id: &UserId, keyword: Option<&str>) -> u32 {
        let key = match keyword {
            Some(kw) if self.config.per_keyword => {
                RateLimitKey::user_keyword(user_id.clone(), kw.to_string())
            }
            _ => RateLimitKey::user_only(user_id.clone()),
        };

        let mut buckets = self.buckets.write();
        buckets
            .get_mut(&key)
            .map(|b| b.available_tokens())
            .unwrap_or(self.config.max_notifications)
    }

    /// Get total number of rate-limited notifications
    pub fn total_limited(&self) -> u64 {
        *self.total_limited.lock()
    }

    /// Clean up old buckets that haven't been used recently
    pub fn cleanup_old_buckets(&self, max_age: Duration) {
        let mut buckets = self.buckets.write();
        let now = Instant::now();
        
        buckets.retain(|_, bucket| {
            now.duration_since(bucket.last_refill) < max_age
        });
    }

    /// Get rate limiter statistics
    pub fn stats(&self) -> RateLimiterStats {
        let buckets = self.buckets.read();
        RateLimiterStats {
            total_buckets: buckets.len(),
            total_limited: *self.total_limited.lock(),
            config: self.config.clone(),
        }
    }
}

/// Rate limiter statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimiterStats {
    /// Total number of active rate limit buckets
    pub total_buckets: usize,
    /// Total number of rate-limited notifications
    pub total_limited: u64,
    /// Current configuration
    pub config: RateLimiterConfig,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_token_bucket_basic() {
        let mut bucket = TokenBucket::new(5, 10); // 5 tokens, 10 second window
        
        // Should be able to consume 5 tokens immediately
        assert!(bucket.try_consume());
        assert!(bucket.try_consume());
        assert!(bucket.try_consume());
        assert!(bucket.try_consume());
        assert!(bucket.try_consume());
        
        // 6th should fail
        assert!(!bucket.try_consume());
    }

    #[test]
    fn test_token_bucket_refill() {
        let mut bucket = TokenBucket::new(2, 1); // 2 tokens per second
        
        // Consume both tokens
        assert!(bucket.try_consume());
        assert!(bucket.try_consume());
        assert!(!bucket.try_consume());
        
        // Wait for refill
        thread::sleep(Duration::from_millis(600));
        
        // Should have at least 1 token now
        assert!(bucket.try_consume());
    }

    #[test]
    fn test_rate_limiter_per_keyword() {
        let config = RateLimiterConfig {
            max_notifications: 3,
            window_seconds: 60,
            per_keyword: true,
        };
        let limiter = KeywordRateLimiter::new(config);
        
        let user_id = UserId::from("test_user");
        
        // Should allow 3 notifications for "urgent"
        assert!(limiter.check_allowed(&user_id, "urgent"));
        assert!(limiter.check_allowed(&user_id, "urgent"));
        assert!(limiter.check_allowed(&user_id, "urgent"));
        assert!(!limiter.check_allowed(&user_id, "urgent"));
        
        // Should still allow notifications for "important" (different keyword)
        assert!(limiter.check_allowed(&user_id, "important"));
        assert!(limiter.check_allowed(&user_id, "important"));
    }

    #[test]
    fn test_rate_limiter_global() {
        let config = RateLimiterConfig {
            max_notifications: 3,
            window_seconds: 60,
            per_keyword: false, // Global limiting per user
        };
        let limiter = KeywordRateLimiter::new(config);
        
        let user_id = UserId::from("test_user");
        
        // Should allow 3 notifications total across all keywords
        assert!(limiter.check_allowed(&user_id, "urgent"));
        assert!(limiter.check_allowed(&user_id, "important"));
        assert!(limiter.check_allowed(&user_id, "critical"));
        assert!(!limiter.check_allowed(&user_id, "warning")); // 4th should fail
    }

    #[test]
    fn test_available_quota() {
        let config = RateLimiterConfig {
            max_notifications: 5,
            window_seconds: 60,
            per_keyword: true,
        };
        let limiter = KeywordRateLimiter::new(config);
        
        let user_id = UserId::from("test_user");
        
        // Initial quota should be max
        assert_eq!(limiter.available_quota(&user_id, Some("test")), 5);
        
        // Consume 2
        limiter.check_allowed(&user_id, "test");
        limiter.check_allowed(&user_id, "test");
        
        // Should have 3 left
        assert_eq!(limiter.available_quota(&user_id, Some("test")), 3);
    }
}
