//! Phase 5d+: Enhanced retry logic with improved error handling and logging.
//!
//! This module provides resilient retry strategies for handling transient
//! failures in P2P group synchronization. It implements exponential backoff
//! with jitter to avoid thundering herd problems.

use chrono::{DateTime, Utc};
use rand::Rng;
use std::time::Duration;

/// Retry policy configuration with validation.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts before giving up.
    pub max_attempts: u32,
    /// Initial backoff duration (e.g., 1 second).
    pub initial_backoff: Duration,
    /// Maximum backoff duration cap (e.g., 60 seconds).
    pub max_backoff: Duration,
    /// Backoff multiplier for exponential growth (e.g., 2.0 for doubling).
    pub backoff_multiplier: f64,
    /// Jitter factor to randomize backoff (0.0-1.0, e.g., 0.1 for ±10%).
    pub jitter_factor: f64,
}

/// Configuration validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryConfigError {
    InvalidMaxAttempts,
    InvalidBackoffRange,
    InvalidMultiplier,
    InvalidJitterFactor,
}

impl std::fmt::Display for RetryConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidMaxAttempts => write!(f, "max_attempts must be > 0"),
            Self::InvalidBackoffRange => write!(f, "initial_backoff must be <= max_backoff"),
            Self::InvalidMultiplier => write!(f, "backoff_multiplier must be >= 1.0"),
            Self::InvalidJitterFactor => write!(f, "jitter_factor must be in range [0.0, 1.0]"),
        }
    }
}

impl std::error::Error for RetryConfigError {}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            backoff_multiplier: 2.0,
            jitter_factor: 0.1,
        }
    }
}

impl RetryPolicy {
    /// Create a new retry policy with validated settings.
    pub fn new(
        max_attempts: u32,
        initial_backoff: Duration,
        max_backoff: Duration,
        backoff_multiplier: f64,
        jitter_factor: f64,
    ) -> Result<Self, RetryConfigError> {
        // Validation
        if max_attempts == 0 {
            return Err(RetryConfigError::InvalidMaxAttempts);
        }
        
        if initial_backoff > max_backoff {
            return Err(RetryConfigError::InvalidBackoffRange);
        }
        
        if backoff_multiplier < 1.0 || !backoff_multiplier.is_finite() {
            return Err(RetryConfigError::InvalidMultiplier);
        }
        
        if jitter_factor < 0.0 || jitter_factor > 1.0 || !jitter_factor.is_finite() {
            return Err(RetryConfigError::InvalidJitterFactor);
        }
        
        Ok(Self {
            max_attempts,
            initial_backoff,
            max_backoff,
            backoff_multiplier,
            jitter_factor,
        })
    }

    /// Calculate the backoff duration for a given attempt number with overflow protection.
    ///
    /// Formula: `min(initial * (multiplier ^ attempt), max_backoff) + jitter`
    ///
    /// The jitter is a random value in the range `[-jitter_factor * backoff, +jitter_factor * backoff]`.
    pub fn calculate_backoff(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::from_secs(0);
        }

        // Prevent overflow: cap at reasonable attempt count
        if attempt > 20 {
            tracing::warn!(
                attempt,
                max_backoff_secs = self.max_backoff.as_secs(),
                "Attempt count very high, using max_backoff"
            );
            return self.max_backoff;
        }

        // Exponential backoff: initial * (multiplier ^ attempt)
        let base_backoff_secs = self.initial_backoff.as_secs_f64()
            * self.backoff_multiplier.powi(attempt as i32);

        // Check for invalid values
        if !base_backoff_secs.is_finite() {
            tracing::error!("Backoff calculation resulted in non-finite value, using max_backoff");
            return self.max_backoff;
        }

        // Cap at max_backoff
        let capped_backoff_secs = base_backoff_secs.min(self.max_backoff.as_secs_f64());

        // Add jitter: random value in [-jitter_factor * backoff, +jitter_factor * backoff]
        let jitter_range = capped_backoff_secs * self.jitter_factor;
        let mut rng = rand::thread_rng();
        let jitter = rng.gen_range(-jitter_range..=jitter_range);

        let final_backoff_secs = (capped_backoff_secs + jitter).max(0.0);

        Duration::from_secs_f64(final_backoff_secs)
    }

    /// Check if the given attempt number has exceeded the maximum.
    pub fn should_give_up(&self, attempt: u32) -> bool {
        attempt >= self.max_attempts
    }
}

/// Retry state for a single sync operation.
#[derive(Debug, Clone)]
pub struct RetryState {
    /// Current retry attempt number (0 = first attempt, no retries yet).
    pub attempt: u32,
    /// Timestamp of the last failure.
    pub last_failure_at: Option<DateTime<Utc>>,
    /// Timestamp when the next retry should be attempted.
    pub next_retry_at: Option<DateTime<Utc>>,
    /// Consecutive failures without a successful sync.
    pub consecutive_failures: u32,
    /// Last error message (for debugging and auditing).
    pub last_error: Option<String>,
}

impl Default for RetryState {
    fn default() -> Self {
        Self {
            attempt: 0,
            last_failure_at: None,
            next_retry_at: None,
            consecutive_failures: 0,
            last_error: None,
        }
    }
}

impl RetryState {
    /// Create a new retry state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a failure and schedule the next retry with logging.
    pub fn record_failure(&mut self, policy: &RetryPolicy, error: String) {
        self.attempt += 1;
        self.consecutive_failures += 1;
        self.last_failure_at = Some(Utc::now());
        self.last_error = Some(error.clone());

        tracing::warn!(
            attempt = self.attempt,
            consecutive_failures = self.consecutive_failures,
            error = %error,
            "Retry: recording failure"
        );

        if policy.should_give_up(self.attempt) {
            // Max attempts reached, do not schedule another retry.
            tracing::error!(
                max_attempts = policy.max_attempts,
                total_failures = self.consecutive_failures,
                "Retry: giving up after max attempts"
            );
            self.next_retry_at = None;
        } else {
            // Calculate backoff and schedule next retry.
            let backoff = policy.calculate_backoff(self.attempt);
            
            // Safe conversion with fallback
            let retry_at = Utc::now() + chrono::Duration::from_std(backoff)
                .unwrap_or_else(|_| {
                    tracing::error!("Failed to convert backoff duration, using 60s");
                    chrono::Duration::seconds(60)
                });
            
            self.next_retry_at = Some(retry_at);
            
            tracing::info!(
                backoff_secs = backoff.as_secs(),
                next_attempt = self.attempt + 1,
                retry_at = %retry_at,
                "Retry: scheduling next attempt"
            );
        }
    }

    /// Record a successful sync, resetting the retry state with logging.
    pub fn record_success(&mut self) {
        let was_retrying = self.attempt > 0;
        let attempts = self.attempt;
        
        self.attempt = 0;
        self.consecutive_failures = 0;
        self.last_failure_at = None;
        self.next_retry_at = None;
        self.last_error = None;
        
        if was_retrying {
            tracing::info!(
                attempts,
                "Retry: operation succeeded after retries"
            );
        }
    }

    /// Check if we should attempt a retry now.
    ///
    /// Returns `true` if:
    /// - There is a scheduled retry time.
    /// - The current time is >= the scheduled retry time.
    pub fn should_retry_now(&self) -> bool {
        match self.next_retry_at {
            Some(retry_at) => Utc::now() >= retry_at,
            None => false,
        }
    }

    /// Check if this operation is in a backoff period (should not retry yet).
    pub fn is_backing_off(&self) -> bool {
        match self.next_retry_at {
            Some(retry_at) => Utc::now() < retry_at,
            None => false,
        }
    }

    /// Check if we have exhausted all retry attempts.
    pub fn has_given_up(&self, policy: &RetryPolicy) -> bool {
        policy.should_give_up(self.attempt) && self.next_retry_at.is_none()
    }

    /// Get the time remaining until the next retry.
    pub fn time_until_retry(&self) -> Option<Duration> {
        self.next_retry_at.map(|retry_at| {
            let now = Utc::now();
            if retry_at > now {
                (retry_at - now).to_std().unwrap_or(Duration::from_secs(0))
            } else {
                Duration::from_secs(0)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_validation() {
        // Valid config
        assert!(RetryPolicy::new(
            5,
            Duration::from_secs(1),
            Duration::from_secs(60),
            2.0,
            0.1
        ).is_ok());

        // Invalid: max_attempts = 0
        assert_eq!(
            RetryPolicy::new(0, Duration::from_secs(1), Duration::from_secs(60), 2.0, 0.1).unwrap_err(),
            RetryConfigError::InvalidMaxAttempts
        );

        // Invalid: initial > max
        assert_eq!(
            RetryPolicy::new(5, Duration::from_secs(100), Duration::from_secs(10), 2.0, 0.1).unwrap_err(),
            RetryConfigError::InvalidBackoffRange
        );

        // Invalid: multiplier < 1.0
        assert_eq!(
            RetryPolicy::new(5, Duration::from_secs(1), Duration::from_secs(60), 0.5, 0.1).unwrap_err(),
            RetryConfigError::InvalidMultiplier
        );

        // Invalid: jitter > 1.0
        assert_eq!(
            RetryPolicy::new(5, Duration::from_secs(1), Duration::from_secs(60), 2.0, 1.5).unwrap_err(),
            RetryConfigError::InvalidJitterFactor
        );
    }

    #[test]
    fn test_overflow_protection() {
        let policy = RetryPolicy::default();

        // Very high attempt should use max_backoff
        let backoff = policy.calculate_backoff(100);
        assert_eq!(backoff, policy.max_backoff);
    }

    #[test]
    fn test_jitter_stays_within_bounds() {
        // Repeating jitter should stay within ±jitter_factor of the
        // computed backoff. With jitter_factor = 0.1 the envelope is
        // small, but the test is robust against the RNG draw.
        let policy = RetryPolicy {
            jitter_factor: 0.5,
            ..RetryPolicy::default()
        };
        for _ in 0..200 {
            let base = policy.calculate_backoff(3);
            let expected = 1.0 * 2f64.powi(3); // 8s
            assert!(
                base.as_secs_f64() >= expected * 0.4,
                "backoff below jitter lower bound: {:?}",
                base
            );
            assert!(
                base.as_secs_f64() <= policy.max_backoff.as_secs_f64(),
                "backoff above max_backoff: {:?}",
                base
            );
        }
    }

    #[test]
    fn test_attempt_zero_returns_zero_backoff() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.calculate_backoff(0), Duration::from_secs(0));
    }

    #[test]
    fn test_should_give_up_at_attempt_max() {
        let policy = RetryPolicy {
            max_attempts: 3,
            ..RetryPolicy::default()
        };
        assert!(!policy.should_give_up(0));
        assert!(!policy.should_give_up(2));
        assert!(policy.should_give_up(3));
        assert!(policy.should_give_up(4));
    }

    #[test]
    fn test_retry_state_giving_up_clears_next_retry_at() {
        let policy = RetryPolicy {
            max_attempts: 2,
            ..RetryPolicy::default()
        };
        let mut state = RetryState::new();

        state.record_failure(&policy, "boot failure".to_string());
        assert!(state.next_retry_at.is_some(), "first failure should schedule retry");

        state.record_failure(&policy, "second failure".to_string());
        assert!(state.next_retry_at.is_none(), "max_attempts reached; no more retries");
        assert!(state.has_given_up(&policy));
    }

    #[test]
    fn test_retry_state_time_until_retry_zero_when_due() {
        let policy = RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(1),
            ..RetryPolicy::default()
        };
        let mut state = RetryState::new();
        state.record_failure(&policy, "tick".to_string());
        std::thread::sleep(Duration::from_millis(5));
        let remaining = state.time_until_retry().unwrap_or_default();
        assert_eq!(remaining, Duration::from_secs(0));
    }
}
