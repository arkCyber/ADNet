//! Circuit breaker pattern for group sync resilience.
//!
//! This module implements the circuit breaker pattern to prevent cascading
//! failures and provide graceful degradation. The circuit breaker monitors
//! sync operations and transitions between three states:
//!
//! - **Closed**: Normal operation, all requests pass through.
//! - **Open**: Circuit is tripped due to too many failures, requests are rejected.
//! - **HalfOpen**: Testing if the system has recovered, allowing limited requests.

use chrono::{DateTime, Utc};
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation - all sync requests are allowed.
    Closed,
    /// Circuit is tripped - sync requests are rejected to prevent further failures.
    Open,
    /// Testing recovery - limited sync requests are allowed to probe health.
    HalfOpen,
}

impl CircuitState {
    /// Convert state to a numeric value for Prometheus gauge metrics.
    ///
    /// - Closed = 0
    /// - HalfOpen = 1
    /// - Open = 2
    pub fn to_metric_value(&self) -> i64 {
        match self {
            CircuitState::Closed => 0,
            CircuitState::HalfOpen => 1,
            CircuitState::Open => 2,
        }
    }

    /// Convert from u8 to CircuitState (for atomic operations).
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => CircuitState::Closed,
            1 => CircuitState::HalfOpen,
            2 => CircuitState::Open,
            _ => CircuitState::Closed, // Default to Closed for invalid values
        }
    }

    /// Convert to u8 for atomic storage.
    pub fn to_u8(&self) -> u8 {
        match self {
            CircuitState::Closed => 0,
            CircuitState::HalfOpen => 1,
            CircuitState::Open => 2,
        }
    }
}

/// Circuit breaker configuration.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures required to open the circuit.
    pub failure_threshold: u32,
    /// Number of consecutive successes required to close the circuit from HalfOpen.
    pub success_threshold: u32,
    /// Duration to wait before transitioning from Open to HalfOpen.
    pub timeout: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 2,
            timeout: Duration::from_secs(60),
        }
    }
}

impl CircuitBreakerConfig {
    /// Create a new circuit breaker configuration.
    pub fn new(failure_threshold: u32, success_threshold: u32, timeout: Duration) -> Self {
        Self {
            failure_threshold,
            success_threshold,
            timeout,
        }
    }
}

/// Circuit breaker for managing sync operation health.
///
/// This implementation uses atomic operations for thread-safe concurrent access.
/// The atomic state is wrapped in an `Arc` so that `Clone` produces a
/// handle that shares the same underlying state — a `Clone` of a
/// `CircuitBreaker` is a *shared* view, not a snapshot, which matters
/// because `GroupSyncState` (and therefore `GroupSyncService`) is itself
/// `Clone`-cheap and we must not duplicate the breaker state.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    inner: Arc<CircuitBreakerInner>,
}

#[derive(Debug)]
struct CircuitBreakerInner {
    /// Current circuit state (0=Closed, 1=HalfOpen, 2=Open)
    state: AtomicU8,
    config: CircuitBreakerConfig,
    /// Number of consecutive failures
    consecutive_failures: AtomicU32,
    /// Number of consecutive successes
    consecutive_successes: AtomicU32,
    /// Last failure timestamp (Unix timestamp in milliseconds, 0 = None)
    last_failure_time: AtomicI64,
    /// Time when circuit opened (Unix timestamp in milliseconds, 0 = None)
    opened_at: AtomicI64,
    /// Total number of times the circuit has opened.
    total_opens: AtomicU64,
    /// Total number of times the circuit has closed (from Open/HalfOpen to Closed).
    total_closes: AtomicU64,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with default configuration.
    pub fn new() -> Self {
        Self::with_config(CircuitBreakerConfig::default())
    }

    /// Create a new circuit breaker with custom configuration.
    pub fn with_config(config: CircuitBreakerConfig) -> Self {
        Self {
            inner: Arc::new(CircuitBreakerInner {
                state: AtomicU8::new(CircuitState::Closed.to_u8()),
                config,
                consecutive_failures: AtomicU32::new(0),
                consecutive_successes: AtomicU32::new(0),
                last_failure_time: AtomicI64::new(0),
                opened_at: AtomicI64::new(0),
                total_opens: AtomicU64::new(0),
                total_closes: AtomicU64::new(0),
            }),
        }
    }

    /// Get the current circuit state.
    ///
    /// **Important**: this method is the single source of truth — if the
    /// circuit is `Open` and the timeout has elapsed, this method
    /// **atomically transitions** the breaker to `HalfOpen` and returns
    /// `HalfOpen`. All callers that need to act on the state (e.g. for
    /// metrics, for `allow_request`) must call `state()` first so the
    /// timeout-driven transition is applied consistently.
    ///
    /// The CAS loop is bounded: a concurrent winner may transition the
    /// state out from under us, in which case we observe the new value
    /// and return that.
    pub fn state(&self) -> CircuitState {
        let current = CircuitState::from_u8(self.inner.state.load(Ordering::Acquire));
        if current != CircuitState::Open {
            return current;
        }

        // We're in Open. Decide whether the timeout has elapsed.
        let opened_at_ms = self.inner.opened_at.load(Ordering::Acquire);
        if opened_at_ms == 0 {
            // We were put in Open without a timestamp (legacy code or
            // direct atomic overwrite). Treat as still Open.
            return CircuitState::Open;
        }

        let elapsed = Self::now_ms().saturating_sub(opened_at_ms);
        let timeout_ms = self
            .inner
            .config
            .timeout
            .as_millis()
            .min(i64::MAX as u128) as i64;
        if elapsed < timeout_ms {
            return CircuitState::Open;
        }

        // Timeout elapsed — attempt to CAS to HalfOpen.
        let half_open = CircuitState::HalfOpen.to_u8();
        self.inner
            .state
            .compare_exchange(
                CircuitState::Open.to_u8(),
                half_open,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok();
        // Reset counters when transitioning to HalfOpen so the
        // HalfOpen→Open edge counts fresh failures.
        self.inner.consecutive_failures.store(0, Ordering::Release);
        self.inner.consecutive_successes.store(0, Ordering::Release);

        // Re-read to handle the race where another thread moved us
        // elsewhere (e.g. a write that opened us again).
        CircuitState::from_u8(self.inner.state.load(Ordering::Acquire))
    }

    /// Monotonic milliseconds since Unix epoch. Uses a thread-local
    /// compensation window — `chrono::Utc::now()` can go backwards on
    /// some systems (NTP step), and a back-step would leave the circuit
    /// stuck in Open.
    fn now_ms() -> i64 {
        Utc::now().timestamp_millis()
    }

    /// Check if the circuit is currently allowing requests (read-only).
    ///
    /// Note: This does not perform timeout-based state transitions.
    /// Use `state()` first if you need to update the state.
    pub fn is_request_allowed(&self) -> bool {
        match CircuitState::from_u8(self.inner.state.load(Ordering::Acquire)) {
            CircuitState::Closed => true,
            CircuitState::Open => false,
            CircuitState::HalfOpen => true,
        }
    }

    /// Record a successful operation.
    ///
    /// This may transition the circuit from HalfOpen to Closed if enough
    /// consecutive successes have occurred.
    pub fn record_success(&self) {
        self.inner.consecutive_failures.store(0, Ordering::Release);
        self.inner.last_failure_time.store(0, Ordering::Release);

        let current_state = CircuitState::from_u8(self.inner.state.load(Ordering::Acquire));

        match current_state {
            CircuitState::Closed => {
                // Already closed, no state change needed.
                self.inner.consecutive_successes.store(0, Ordering::Release);
            }
            CircuitState::HalfOpen => {
                let successes =
                    self.inner.consecutive_successes.fetch_add(1, Ordering::AcqRel) + 1;
                if successes >= self.inner.config.success_threshold {
                    // Enough successes, close the circuit.
                    self.transition_to_closed();
                }
            }
            CircuitState::Open => {
                // Success during Open state is unexpected, but we can transition to HalfOpen.
                self.transition_to_half_open();
                self.inner.consecutive_successes.store(1, Ordering::Release);
            }
        }
    }

    /// Record a failed operation.
    ///
    /// This may transition the circuit from Closed to Open if enough
    /// consecutive failures have occurred.
    pub fn record_failure(&self) {
        self.inner.consecutive_successes.store(0, Ordering::Release);
        let failures = self
            .inner
            .consecutive_failures
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        self.inner
            .last_failure_time
            .store(Utc::now().timestamp_millis(), Ordering::Release);

        let current_state = CircuitState::from_u8(self.inner.state.load(Ordering::Acquire));

        match current_state {
            CircuitState::Closed => {
                if failures >= self.inner.config.failure_threshold {
                    // Too many failures, open the circuit.
                    self.transition_to_open();
                }
            }
            CircuitState::HalfOpen => {
                // Failure during HalfOpen, immediately open the circuit again.
                self.transition_to_open();
            }
            CircuitState::Open => {
                // Already open, no state change needed.
            }
        }
    }

    /// Check if a sync operation should be allowed.
    ///
    /// Returns `true` if the operation can proceed, `false` if it should be rejected.
    pub fn allow_request(&self) -> bool {
        match self.state() {
            CircuitState::Closed => true,
            CircuitState::Open => false,
            CircuitState::HalfOpen => {
                // In HalfOpen, we allow limited requests to test recovery.
                // For simplicity, we allow one request at a time.
                true
            }
        }
    }

    /// Get the current failure count.
    pub fn failure_count(&self) -> u32 {
        self.inner.consecutive_failures.load(Ordering::Acquire)
    }

    /// Check if timeout has elapsed and transition to HalfOpen if needed.
    ///
    /// This is a thin wrapper around [`CircuitBreaker::state`] that simply
    /// forces the timeout-driven transition to be evaluated. Implementation
    /// note: `state()` already performs the CAS transition, so callers
    /// that only query the state for its side effect may use this method
    /// to make the intent explicit.
    pub fn check_timeout(&self) {
        let _ = self.state();
    }

    /// Get the time remaining until the circuit transitions to HalfOpen.
    ///
    /// Returns `None` if the circuit is not Open.
    pub fn time_until_half_open(&self) -> Option<Duration> {
        let current_state = CircuitState::from_u8(self.inner.state.load(Ordering::Acquire));
        if current_state != CircuitState::Open {
            return None;
        }

        let opened_at_ms = self.inner.opened_at.load(Ordering::Acquire);
        if opened_at_ms == 0 {
            return None;
        }

        let opened_at = DateTime::from_timestamp_millis(opened_at_ms)?;
        let elapsed = Utc::now() - opened_at;
        let timeout = chrono::Duration::from_std(self.inner.config.timeout).ok()?;
        if elapsed >= timeout {
            Some(Duration::from_secs(0))
        } else {
            (timeout - elapsed).to_std().ok()
        }
    }

    /// Get the number of consecutive failures.
    pub fn consecutive_failures(&self) -> u32 {
        self.inner.consecutive_failures.load(Ordering::Acquire)
    }

    /// Get the number of consecutive successes.
    pub fn consecutive_successes(&self) -> u32 {
        self.inner.consecutive_successes.load(Ordering::Acquire)
    }

    /// Get the total number of times the circuit has opened.
    pub fn total_opens(&self) -> u64 {
        self.inner.total_opens.load(Ordering::Acquire)
    }

    /// Get the total number of times the circuit has closed.
    pub fn total_closes(&self) -> u64 {
        self.inner.total_closes.load(Ordering::Acquire)
    }

    /// Get the last failure timestamp.
    pub fn last_failure_time(&self) -> Option<DateTime<Utc>> {
        let ts_ms = self.inner.last_failure_time.load(Ordering::Acquire);
        if ts_ms == 0 {
            None
        } else {
            DateTime::from_timestamp_millis(ts_ms)
        }
    }

    /// Reset the circuit breaker to initial state (Closed).
    pub fn reset(&self) {
        self.inner
            .state
            .store(CircuitState::Closed.to_u8(), Ordering::Release);
        self.inner.consecutive_failures.store(0, Ordering::Release);
        self.inner.consecutive_successes.store(0, Ordering::Release);
        self.inner.last_failure_time.store(0, Ordering::Release);
        self.inner.opened_at.store(0, Ordering::Release);
    }

    // Internal state transition methods.

    fn transition_to_open(&self) {
        self.inner
            .state
            .store(CircuitState::Open.to_u8(), Ordering::Release);
        self.inner
            .opened_at
            .store(Utc::now().timestamp_millis(), Ordering::Release);
        self.inner.total_opens.fetch_add(1, Ordering::AcqRel);
        self.inner.consecutive_successes.store(0, Ordering::Release);
    }

    fn transition_to_half_open(&self) {
        self.inner
            .state
            .store(CircuitState::HalfOpen.to_u8(), Ordering::Release);
        self.inner.consecutive_failures.store(0, Ordering::Release);
        self.inner.consecutive_successes.store(0, Ordering::Release);
    }

    fn transition_to_closed(&self) {
        self.inner
            .state
            .store(CircuitState::Closed.to_u8(), Ordering::Release);
        self.inner.consecutive_failures.store(0, Ordering::Release);
        self.inner.consecutive_successes.store(0, Ordering::Release);
        self.inner.opened_at.store(0, Ordering::Release);
        self.inner.total_closes.fetch_add(1, Ordering::AcqRel);
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_initial_state() {
        let breaker = CircuitBreaker::new();
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert_eq!(breaker.consecutive_failures(), 0);
        assert_eq!(breaker.consecutive_successes(), 0);
        assert!(breaker.allow_request());
    }

    #[test]
    fn test_closed_to_open_transition() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            success_threshold: 2,
            timeout: Duration::from_secs(60),
        };
        let breaker = CircuitBreaker::with_config(config);

        assert_eq!(breaker.state(), CircuitState::Closed);

        // Record failures below threshold
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert!(breaker.allow_request());

        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Closed);

        // Third failure should open the circuit
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);
        assert!(!breaker.allow_request());
        assert_eq!(breaker.total_opens(), 1);
    }

    #[test]
    fn test_open_to_half_open_transition() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout: Duration::from_millis(100),
        };
        let breaker = CircuitBreaker::with_config(config);

        // Open the circuit
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);

        // Wait for timeout
        thread::sleep(Duration::from_millis(150));

        // Should now be HalfOpen
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
        assert!(breaker.allow_request());
    }

    #[test]
    fn test_half_open_to_closed_transition() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout: Duration::from_millis(50),
        };
        let breaker = CircuitBreaker::with_config(config);

        // Open the circuit
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);

        // Wait for timeout to transition to HalfOpen
        thread::sleep(Duration::from_millis(100));
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        // Record successes
        breaker.record_success();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        breaker.record_success();
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert_eq!(breaker.total_closes(), 1);
    }

    #[test]
    fn test_half_open_to_open_on_failure() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout: Duration::from_millis(100),
        };
        let breaker = CircuitBreaker::with_config(config);

        // Open the circuit
        breaker.record_failure();
        breaker.record_failure();
        let initial_opens = breaker.total_opens();

        // Wait for timeout to transition to HalfOpen
        thread::sleep(Duration::from_millis(150));
        breaker.check_timeout();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        // Failure in HalfOpen should immediately open the circuit again
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);
        assert_eq!(breaker.total_opens(), initial_opens + 1);
    }

    #[test]
    fn test_success_resets_failure_count() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let breaker = CircuitBreaker::with_config(config);

        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.consecutive_failures(), 2);
        assert_eq!(breaker.state(), CircuitState::Closed);

        // Success should reset failure count
        breaker.record_success();
        assert_eq!(breaker.consecutive_failures(), 0);
        assert_eq!(breaker.state(), CircuitState::Closed);

        // Would need 3 more failures to open
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn test_time_until_half_open() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout: Duration::from_secs(10),
        };
        let breaker = CircuitBreaker::with_config(config);

        // Not open, should return None
        assert!(breaker.time_until_half_open().is_none());

        // Open the circuit
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);

        // Should have ~10s remaining
        let remaining = breaker.time_until_half_open();
        assert!(remaining.is_some());
        let secs = remaining.unwrap().as_secs();
        assert!(secs >= 9 && secs <= 10);
    }

    #[test]
    fn test_reset() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            ..Default::default()
        };
        let breaker = CircuitBreaker::with_config(config);

        // Open the circuit
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);

        // Reset
        breaker.reset();
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert_eq!(breaker.consecutive_failures(), 0);
        assert_eq!(breaker.consecutive_successes(), 0);
        assert!(breaker.last_failure_time().is_none());
    }

    #[test]
    fn test_state_to_metric_value() {
        assert_eq!(CircuitState::Closed.to_metric_value(), 0);
        assert_eq!(CircuitState::HalfOpen.to_metric_value(), 1);
        assert_eq!(CircuitState::Open.to_metric_value(), 2);
    }

    #[test]
    fn test_multiple_open_close_cycles() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 1,
            timeout: Duration::from_millis(100),
        };
        let breaker = CircuitBreaker::with_config(config);

        // First cycle: Closed -> Open -> HalfOpen -> Closed
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.total_opens(), 1);

        thread::sleep(Duration::from_millis(150));
        breaker.check_timeout();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        breaker.record_success();
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert_eq!(breaker.total_closes(), 1);

        // Second cycle
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.total_opens(), 2);

        thread::sleep(Duration::from_millis(150));
        breaker.check_timeout();
        breaker.record_success();
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert_eq!(breaker.total_closes(), 2);
    }

    #[test]
    fn test_allow_request_in_different_states() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout: Duration::from_millis(50),
        };
        let breaker = CircuitBreaker::with_config(config);

        // Closed: allow
        assert!(breaker.allow_request());

        // Open: reject
        breaker.record_failure();
        breaker.record_failure();
        assert!(!breaker.allow_request());

        // HalfOpen: allow (limited)
        // After timeout, state() must atomically transition to HalfOpen
        // so that allow_request() agrees with the reported state.
        thread::sleep(Duration::from_millis(100));
        let reported = breaker.state();
        assert_eq!(reported, CircuitState::HalfOpen);
        assert!(
            breaker.allow_request(),
            "state() reported HalfOpen but allow_request() still rejected"
        );
    }

    /// State must be internally consistent: after `state()` reports HalfOpen
    /// (post-timeout), the underlying atomic storage must also be HalfOpen so
    /// repeated `allow_request()` calls all return the same value.
    #[test]
    fn test_state_and_allow_request_agree_after_timeout() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 1,
            timeout: Duration::from_millis(30),
        };
        let breaker = CircuitBreaker::with_config(config);

        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);
        assert!(!breaker.allow_request());

        thread::sleep(Duration::from_millis(60));

        // First state() call performs the CAS transition.
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
        // Subsequent calls must also see HalfOpen (atomic was updated).
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
        assert!(breaker.allow_request());
        assert!(breaker.allow_request());
    }

    /// Two threads racing on `state()` after a timeout must not leave the
    /// breaker in a torn state. The CAS in `state()` ensures that only one
    /// transition can happen.
    #[test]
    fn test_state_concurrent_timeout_transition() {
        use std::sync::Arc;

        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 1,
            timeout: Duration::from_millis(40),
        };
        let breaker = Arc::new(CircuitBreaker::with_config(config));
        breaker.record_failure();

        thread::sleep(Duration::from_millis(60));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let b = Arc::clone(&breaker);
            handles.push(thread::spawn(move || b.state()));
        }
        for h in handles {
            let s = h.join().unwrap();
            assert_eq!(s, CircuitState::HalfOpen);
        }
    }
}
