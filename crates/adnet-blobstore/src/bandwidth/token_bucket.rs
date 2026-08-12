//! Token Bucket Implementation for Bandwidth Rate Limiting
//!
//! Token bucket is an efficient algorithm for rate limiting that allows
//! for bursty traffic while maintaining an average rate limit.
//!
//! ## How It Works
//!
//! 1. Tokens are added to the bucket at a constant rate (the bandwidth limit)
//! 2. Each byte transferred consumes tokens
//! 3. If the bucket is empty, transfers must wait
//! 4. The bucket has a maximum capacity for burst handling
//!
//! ## Advantages
//!
//! - Smooth rate limiting without jerky behavior
//! - Allows controlled bursts up to bucket capacity
//! - Memory efficient (only stores current tokens and last update time)
//! - Easy to implement and reason about

use std::time::{Duration, Instant};

/// Token bucket for rate limiting.
///
/// Tokens are added at a constant rate (refill_rate), and consuming
/// tokens blocks until enough are available.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    /// Maximum tokens (capacity).
    capacity: f64,
    /// Current available tokens.
    tokens: f64,
    /// Token refill rate per second.
    refill_rate: f64,
    /// Last update timestamp.
    last_update: Instant,
}

impl TokenBucket {
    /// Create a new token bucket with the specified capacity and refill rate.
    ///
    /// - `capacity`: Maximum tokens (typically set to max burst size in bytes)
    /// - `refill_rate`: Tokens added per second (typically set to bandwidth limit in bytes/sec)
    pub fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            capacity: capacity.max(1.0),
            tokens: capacity.max(1.0), // Start full
            refill_rate: refill_rate.max(0.0),
            last_update: Instant::now(),
        }
    }

    /// Create a new unlimited token bucket.
    pub fn unlimited() -> Self {
        Self {
            capacity: f64::MAX,
            tokens: f64::MAX,
            refill_rate: f64::MAX,
            last_update: Instant::now(),
        }
    }

    /// Check if this bucket is unlimited.
    pub fn is_unlimited(&self) -> bool {
        self.capacity == f64::MAX
    }

    /// Refill tokens based on elapsed time since last update.
    fn refill(&mut self) {
        if self.is_unlimited() {
            return;
        }

        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.last_update = now;

        // Add tokens based on elapsed time and refill rate
        let tokens_to_add = elapsed * self.refill_rate;
        self.tokens = (self.tokens + tokens_to_add).min(self.capacity);
    }

    /// Try to consume tokens without blocking.
    ///
    /// Returns the number of tokens actually consumed (may be less than requested
    /// if bucket doesn't have enough tokens).
    pub fn try_consume(&mut self, tokens: f64) -> f64 {
        self.refill();

        if self.is_unlimited() {
            return tokens;
        }

        let consumed = tokens.min(self.tokens);
        self.tokens -= consumed;
        consumed
    }

    /// Check if enough tokens are available without consuming.
    pub fn available(&self) -> f64 {
        let mut bucket = self.clone();
        bucket.refill();
        bucket.tokens
    }

    /// Check if the specified number of tokens are available.
    pub fn can_consume(&self, tokens: f64) -> bool {
        self.available() >= tokens
    }

    /// Current token level as a fraction of capacity (0.0 to 1.0).
    pub fn fill_level(&self) -> f64 {
        if self.is_unlimited() {
            return 1.0;
        }
        (self.tokens / self.capacity).clamp(0.0, 1.0)
    }

    /// Time until the specified number of tokens will be available.
    pub fn time_until(&self, tokens: f64) -> Duration {
        if self.is_unlimited() {
            return Duration::ZERO;
        }

        // Compute without modifying self
        let elapsed = Instant::now()
            .duration_since(self.last_update)
            .as_secs_f64();
        let tokens_to_add = elapsed * self.refill_rate;
        let available = (self.tokens + tokens_to_add).min(self.capacity);

        if available >= tokens {
            return Duration::ZERO;
        }

        let tokens_needed = tokens - available;
        let seconds = tokens_needed / self.refill_rate;
        Duration::from_secs_f64(seconds)
    }

    /// Update the refill rate dynamically.
    pub fn set_rate(&mut self, refill_rate: f64) {
        self.refill();
        self.refill_rate = refill_rate.max(0.0);
    }

    /// Update capacity (e.g., for burst multiplier changes).
    pub fn set_capacity(&mut self, capacity: f64) {
        self.capacity = capacity.max(1.0);
        self.tokens = self.tokens.min(self.capacity);
    }

    /// Drain all tokens (for emergency bandwidth cutoff).
    pub fn drain(&mut self) {
        self.tokens = 0.0;
    }

    /// Reset to full capacity.
    pub fn reset(&mut self) {
        self.tokens = self.capacity;
        self.last_update = Instant::now();
    }
}

impl Default for TokenBucket {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// Async-aware token bucket that can wait for tokens.
#[derive(Debug, Clone)]
pub struct AsyncTokenBucket {
    inner: TokenBucket,
}

impl AsyncTokenBucket {
    /// Create a new async token bucket.
    pub fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            inner: TokenBucket::new(capacity, refill_rate),
        }
    }

    /// Create a new unlimited async token bucket.
    pub fn unlimited() -> Self {
        Self {
            inner: TokenBucket::unlimited(),
        }
    }

    /// Try to consume tokens without blocking.
    pub fn try_consume(&mut self, tokens: f64) -> bool {
        self.inner.try_consume(tokens) >= tokens
    }

    /// Consume tokens, waiting if necessary.
    ///
    /// This is the async version that will yield to the runtime
    /// while waiting for tokens.
    pub async fn consume(&mut self, tokens: f64) {
        loop {
            if self.try_consume(tokens) {
                return;
            }

            // Wait for tokens to become available
            let wait_time = self.inner.time_until(tokens);
            if wait_time > Duration::ZERO {
                tokio::time::sleep(wait_time).await;
            }
        }
    }

    /// Consume tokens with a maximum wait time.
    ///
    /// Returns `true` if tokens were consumed, `false` if timeout occurred.
    pub async fn consume_timeout(&mut self, tokens: f64, max_wait: Duration) -> bool {
        let deadline = Instant::now() + max_wait;

        loop {
            if self.try_consume(tokens) {
                return true;
            }

            let now = Instant::now();
            if now >= deadline {
                return false;
            }

            let wait_time = self.inner.time_until(tokens);
            let remaining = deadline - now;

            if wait_time > remaining {
                tokio::time::sleep(remaining).await;
                return self.try_consume(tokens);
            }

            tokio::time::sleep(wait_time).await;
        }
    }

    /// Check available tokens.
    pub fn available(&self) -> f64 {
        self.inner.available()
    }

    /// Check if tokens are available.
    pub fn can_consume(&self, tokens: f64) -> bool {
        self.inner.can_consume(tokens)
    }

    /// Time until tokens are available.
    pub fn time_until(&self, tokens: f64) -> Duration {
        self.inner.time_until(tokens)
    }

    /// Update the refill rate.
    pub fn set_rate(&mut self, refill_rate: f64) {
        self.inner.set_rate(refill_rate);
    }

    /// Update capacity.
    pub fn set_capacity(&mut self, capacity: f64) {
        self.inner.set_capacity(capacity);
    }

    /// Check if unlimited.
    pub fn is_unlimited(&self) -> bool {
        self.inner.is_unlimited()
    }

    /// Get current fill level.
    pub fn fill_level(&self) -> f64 {
        self.inner.fill_level()
    }
}

impl Default for AsyncTokenBucket {
    fn default() -> Self {
        Self::unlimited()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_unlimited() {
        let bucket = TokenBucket::unlimited();
        assert!(bucket.is_unlimited());
        assert!(bucket.can_consume(1_000_000_000.0));
    }

    #[test]
    fn test_bucket_consume() {
        let mut bucket = TokenBucket::new(1024.0, 1024.0); // 1KB capacity, 1KB/s refill

        // Should be able to consume up to capacity
        assert!(bucket.try_consume(512.0) >= 512.0);
        assert!(bucket.try_consume(512.0) >= 512.0);

        // Bucket should be nearly empty now
        let available = bucket.available();
        assert!(available < 100.0);
    }

    #[test]
    fn test_bucket_refill() {
        let mut bucket = TokenBucket::new(1000.0, 1000.0); // 1KB capacity, 1KB/s

        // Consume some tokens
        bucket.try_consume(500.0);

        // Wait 500ms - should get ~500 tokens back
        std::thread::sleep(Duration::from_millis(600));

        let available = bucket.available();
        // Should have recovered to around 500-1000
        assert!(available >= 400.0, "Expected ~500-1000, got {}", available);
    }

    #[tokio::test]
    async fn test_async_bucket() {
        let mut bucket = AsyncTokenBucket::new(1024.0, 1024.0);

        // First consume should succeed immediately
        assert!(bucket.try_consume(512.0));

        // Second should also succeed
        assert!(bucket.try_consume(512.0));

        // Third should fail without waiting
        assert!(!bucket.try_consume(1.0));

        // Wait and try again
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(bucket.try_consume(100.0));
    }

    #[tokio::test]
    async fn test_consume_timeout_success() {
        let mut bucket = AsyncTokenBucket::new(1000.0, 1000.0);
        bucket.try_consume(900.0);

        // Should succeed within 1 second
        let result = bucket.consume_timeout(500.0, Duration::from_secs(1)).await;
        assert!(result);
    }

    #[tokio::test]
    async fn test_consume_timeout_failure() {
        let mut bucket = AsyncTokenBucket::new(100.0, 100.0);
        bucket.try_consume(100.0);

        // Should timeout
        let result = bucket
            .consume_timeout(200.0, Duration::from_millis(100))
            .await;
        assert!(!result);
    }

    #[test]
    fn test_fill_level() {
        let bucket = TokenBucket::new(1000.0, 1000.0);
        assert!((bucket.fill_level() - 1.0).abs() < 0.01);
    }
}
