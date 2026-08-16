//! Resilience patterns for video streaming: retry, circuit breaker, and fault tolerance.
//!
//! Provides production-grade reliability patterns for handling transient failures in video pipelines.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tokio::sync::mpsc;

/// Maximum number of retries for a single operation.
pub const MAX_RETRIES: u32 = 3;

/// Initial backoff delay in milliseconds.
pub const INITIAL_BACKOFF_MS: u64 = 100;

/// Maximum backoff delay in milliseconds.
pub const MAX_BACKOFF_MS: u64 = 5000;

/// Circuit breaker failure threshold.
pub const CIRCUIT_BREAKER_THRESHOLD: u64 = 5;

/// Circuit breaker recovery timeout in seconds.
pub const CIRCUIT_RECOVERY_TIMEOUT_SECS: u64 = 30;

// ============================================================================
// Retry Strategy
// ============================================================================

/// Retry strategy for handling transient failures.
#[derive(Debug, Clone, Copy)]
pub enum RetryStrategy {
    /// No retries.
    None,
    /// Fixed delay between retries.
    Fixed { retries: u32, delay_ms: u64 },
    /// Exponential backoff with jitter.
    ExponentialBackoff {
        retries: u32,
        initial_delay_ms: u64,
        max_delay_ms: u64,
        jitter_factor: f64,
    },
}

impl PartialEq for RetryStrategy {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::None, Self::None) => true,
            (Self::Fixed { retries: l, delay_ms: dl }, Self::Fixed { retries: r, delay_ms: dr }) => {
                l == r && dl == dr
            }
            (
                Self::ExponentialBackoff {
                    retries: l,
                    initial_delay_ms: il,
                    max_delay_ms: ml,
                    jitter_factor: jl,
                },
                Self::ExponentialBackoff {
                    retries: r,
                    initial_delay_ms: ir,
                    max_delay_ms: mr,
                    jitter_factor: jr,
                },
            ) => l == r && il == ir && ml == mr && (jl - jr).abs() < f64::EPSILON,
            _ => false,
        }
    }
}

impl Eq for RetryStrategy {}

impl Default for RetryStrategy {
    fn default() -> Self {
        RetryStrategy::ExponentialBackoff {
            retries: MAX_RETRIES,
            initial_delay_ms: INITIAL_BACKOFF_MS,
            max_delay_ms: MAX_BACKOFF_MS,
            jitter_factor: 0.1,
        }
    }
}

impl RetryStrategy {
    /// Returns the delay for a given attempt number.
    pub fn delay(&self, attempt: u32) -> Duration {
        match self {
            RetryStrategy::None => Duration::ZERO,
            RetryStrategy::Fixed { delay_ms, .. } => Duration::from_millis(*delay_ms),
            RetryStrategy::ExponentialBackoff {
                initial_delay_ms,
                max_delay_ms,
                jitter_factor,
                ..
            } => {
                let base_delay = (*initial_delay_ms as f64) * (2.0_f64.powi(attempt as i32));
                let capped_delay = base_delay.min(*max_delay_ms as f64);
                let jitter = capped_delay * jitter_factor * rand_factor();
                Duration::from_millis((capped_delay + jitter) as u64)
            }
        }
    }

    /// Returns true if more retries are allowed.
    pub fn should_retry(&self, attempt: u32) -> bool {
        match self {
            RetryStrategy::None => false,
            RetryStrategy::Fixed { retries, .. } => attempt < *retries,
            RetryStrategy::ExponentialBackoff { retries, .. } => attempt < *retries,
        }
    }
}

/// Simple random factor for jitter (0.5 to 1.5).
/// Uses a const-compatible pseudo-random seed generation.
fn rand_factor() -> f64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::Instant;
    
    // Create a pseudo-random seed from multiple sources
    let now = Instant::now();
    let nanos = now.elapsed().as_nanos() as u64;
    let mut hasher = DefaultHasher::new();
    nanos.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    let seed = hasher.finish();
    
    0.5 + (seed % 1000) as f64 / 1000.0
}

// ============================================================================
// Circuit Breaker
// ============================================================================

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Circuit is closed, requests pass through normally.
    Closed,
    /// Circuit is open, requests fail immediately.
    Open,
    /// Circuit is half-open, allowing a test request.
    HalfOpen,
}

impl Default for CircuitState {
    fn default() -> Self {
        CircuitState::Closed
    }
}

/// Circuit breaker for preventing cascade failures.
pub struct CircuitBreaker {
    /// Current state.
    state: RwLock<CircuitState>,
    /// Consecutive failure count.
    failures: AtomicU64,
    /// Success count in half-open state.
    half_open_successes: AtomicU64,
    /// Last failure timestamp.
    last_failure: RwLock<Instant>,
    /// Threshold for opening the circuit.
    threshold: u64,
    /// Recovery timeout.
    recovery_timeout: Duration,
}

impl CircuitBreaker {
    /// Creates a new circuit breaker with default settings.
    pub fn new() -> Self {
        Self::with_config(CIRCUIT_BREAKER_THRESHOLD, CIRCUIT_RECOVERY_TIMEOUT_SECS)
    }

    /// Creates a circuit breaker with custom configuration.
    pub fn with_config(threshold: u64, recovery_timeout_secs: u64) -> Self {
        Self {
            state: RwLock::new(CircuitState::Closed),
            failures: AtomicU64::new(0),
            half_open_successes: AtomicU64::new(0),
            last_failure: RwLock::new(Instant::now()),
            threshold,
            recovery_timeout: Duration::from_secs(recovery_timeout_secs),
        }
    }

    /// Returns the current circuit state.
    pub fn state(&self) -> CircuitState {
        *self.state.read()
    }

    /// Returns true if the circuit allows requests.
    pub fn allows_request(&self) -> bool {
        let state = self.state();
        match state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check if recovery timeout has elapsed
                let elapsed = self.last_failure.read().elapsed();
                if elapsed >= self.recovery_timeout {
                    // Transition to half-open
                    *self.state.write() = CircuitState::HalfOpen;
                    self.half_open_successes.store(0, Ordering::SeqCst);
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => true,
        }
    }

    /// Records a successful operation.
    pub fn record_success(&self) {
        let state = self.state();

        match state {
            CircuitState::Closed => {
                // Reset failure count on success
                self.failures.store(0, Ordering::SeqCst);
            }
            CircuitState::HalfOpen => {
                let successes = self.half_open_successes.fetch_add(1, Ordering::SeqCst) + 1;
                if successes >= 3 {
                    // Close the circuit after 3 consecutive successes
                    *self.state.write() = CircuitState::Closed;
                    self.failures.store(0, Ordering::SeqCst);
                }
            }
            CircuitState::Open => {}
        }
    }

    /// Records a failed operation.
    pub fn record_failure(&self) {
        let state = self.state();

        match state {
            CircuitState::Closed => {
                let failures = self.failures.fetch_add(1, Ordering::SeqCst) + 1;
                *self.last_failure.write() = Instant::now();

                if failures >= self.threshold {
                    // Open the circuit
                    *self.state.write() = CircuitState::Open;
                }
            }
            CircuitState::HalfOpen => {
                // Any failure in half-open state opens the circuit
                *self.state.write() = CircuitState::Open;
                *self.last_failure.write() = Instant::now();
            }
            CircuitState::Open => {
                *self.last_failure.write() = Instant::now();
            }
        }
    }

    /// Returns the current failure count.
    pub fn failure_count(&self) -> u64 {
        self.failures.load(Ordering::SeqCst)
    }

    /// Resets the circuit breaker to closed state.
    pub fn reset(&self) {
        *self.state.write() = CircuitState::Closed;
        self.failures.store(0, Ordering::SeqCst);
        self.half_open_successes.store(0, Ordering::SeqCst);
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Resilient Operation
// ============================================================================

/// A resilient operation that wraps a fallible operation with retry and circuit breaker.
pub struct ResilientOperation<T> {
    operation: Box<dyn Fn() -> Result<T, crate::error::VideoError> + Send + Sync>,
    strategy: RetryStrategy,
    circuit: Arc<CircuitBreaker>,
}

impl<T: 'static + Send> ResilientOperation<T> {
    /// Creates a new resilient operation.
    pub fn new<F>(operation: F) -> Self
    where
        F: Fn() -> Result<T, crate::error::VideoError> + Send + Sync + 'static,
    {
        Self {
            operation: Box::new(operation),
            strategy: RetryStrategy::default(),
            circuit: Arc::new(CircuitBreaker::new()),
        }
    }

    /// Sets the retry strategy.
    pub fn with_retry_strategy(mut self, strategy: RetryStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Sets a custom circuit breaker.
    pub fn with_circuit_breaker(mut self, circuit: Arc<CircuitBreaker>) -> Self {
        self.circuit = circuit;
        self
    }

    /// Executes the operation with retry and circuit breaker.
    pub fn execute(&self) -> Result<T, crate::error::VideoError> {
        if !self.circuit.allows_request() {
            return Err(crate::error::VideoError::PipelineComponentFailed {
                component: "circuit_breaker",
                cause: Box::new(crate::error::VideoError::TrackNotConnected),
            });
        }

        let mut attempt = 0;
        loop {
            match (self.operation)() {
                Ok(result) => {
                    self.circuit.record_success();
                    return Ok(result);
                }
                Err(e) => {
                    if self.strategy.should_retry(attempt) {
                        attempt += 1;
                        self.circuit.record_failure();

                        // Wait before retrying
                        std::thread::sleep(self.strategy.delay(attempt));

                        // Check circuit again after delay
                        if !self.circuit.allows_request() {
                            return Err(crate::error::VideoError::PipelineComponentFailed {
                                component: "circuit_breaker",
                                cause: Box::new(e),
                            });
                        }
                    } else {
                        self.circuit.record_failure();
                        return Err(e);
                    }
                }
            }
        }
    }

    /// Returns a reference to the circuit breaker.
    pub fn circuit_breaker(&self) -> &Arc<CircuitBreaker> {
        &self.circuit
    }
}

// ============================================================================
// Fallback Strategy
// ============================================================================

/// Fallback strategy when primary fails.
pub enum FallbackStrategy<T> {
    /// Return a default value.
    Default(T),
    /// Use an alternative operation.
    Alternative(Box<dyn Fn() -> Result<T, crate::error::VideoError> + Send + Sync>),
    /// Return an error.
    Error(crate::error::VideoError),
}

impl<T> FallbackStrategy<T> {
    /// Returns true if this is a default strategy.
    pub fn is_default(&self) -> bool {
        matches!(self, FallbackStrategy::Default(_))
    }
}

impl<T> std::fmt::Debug for FallbackStrategy<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FallbackStrategy::Default(_) => f.debug_tuple("Default").finish(),
            FallbackStrategy::Alternative(_) => f.debug_tuple("Alternative").finish(),
            FallbackStrategy::Error(e) => f.debug_tuple("Error").field(e).finish(),
        }
    }
}

// ============================================================================
// Health Monitor
// ============================================================================

/// Health status of a video component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// Component is healthy.
    Healthy,
    /// Component is degraded but functional.
    Degraded,
    /// Component is unhealthy.
    Unhealthy,
}

impl Default for HealthStatus {
    fn default() -> Self {
        HealthStatus::Healthy
    }
}

/// Health metrics for a video component.
#[derive(Debug, Clone, Default)]
pub struct HealthMetrics {
    /// Total operations attempted.
    pub operations_attempted: u64,
    /// Operations that succeeded.
    pub operations_succeeded: u64,
    /// Operations that failed.
    pub operations_failed: u64,
    /// Operations that timed out.
    pub operations_timed_out: u64,
    /// Average latency in milliseconds.
    pub avg_latency_ms: f64,
    /// Current health status.
    pub status: HealthStatus,
}

impl HealthMetrics {
    /// Creates a new health metrics tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a successful operation.
    pub fn record_success(&mut self, latency_ms: f64) {
        self.operations_attempted += 1;
        self.operations_succeeded += 1;
        self.update_avg_latency(latency_ms);
        self.update_status();
    }

    /// Records a failed operation.
    pub fn record_failure(&mut self) {
        self.operations_attempted += 1;
        self.operations_failed += 1;
        self.update_status();
    }

    /// Records a timeout.
    pub fn record_timeout(&mut self) {
        self.operations_attempted += 1;
        self.operations_timed_out += 1;
        self.update_status();
    }

    /// Returns the success rate (0.0 to 1.0).
    pub fn success_rate(&self) -> f64 {
        if self.operations_attempted == 0 {
            1.0
        } else {
            self.operations_succeeded as f64 / self.operations_attempted as f64
        }
    }

    /// Returns the error rate (0.0 to 1.0).
    pub fn error_rate(&self) -> f64 {
        1.0 - self.success_rate()
    }

    fn update_avg_latency(&mut self, latency_ms: f64) {
        let n = self.operations_succeeded as f64;
        self.avg_latency_ms = (self.avg_latency_ms * (n - 1.0) + latency_ms) / n;
    }

    fn update_status(&mut self) {
        let error_rate = self.error_rate();
        self.status = if error_rate < 0.01 {
            HealthStatus::Healthy
        } else if error_rate < 0.1 {
            HealthStatus::Degraded
        } else {
            HealthStatus::Unhealthy
        };
    }

    /// Resets all metrics.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

/// Thread-safe health monitor.
#[derive(Default)]
pub struct HealthMonitor {
    metrics: RwLock<HealthMetrics>,
}

impl HealthMonitor {
    /// Creates a new health monitor.
    pub fn new() -> Self {
        Self {
            metrics: RwLock::new(HealthMetrics::new()),
        }
    }

    /// Records a successful operation.
    pub fn record_success(&self, latency_ms: f64) {
        self.metrics.write().record_success(latency_ms);
    }

    /// Records a failed operation.
    pub fn record_failure(&self) {
        self.metrics.write().record_failure();
    }

    /// Records a timeout.
    pub fn record_timeout(&self) {
        self.metrics.write().record_timeout();
    }

    /// Returns a snapshot of current metrics.
    pub fn metrics(&self) -> HealthMetrics {
        self.metrics.read().clone()
    }

    /// Returns the current health status.
    pub fn status(&self) -> HealthStatus {
        self.metrics.read().status
    }
}

// ============================================================================
// Rate Limiter
// ============================================================================

/// Token bucket rate limiter.
pub struct RateLimiter {
    tokens: RwLock<f64>,
    capacity: f64,
    refill_rate: f64, // tokens per second
    last_refill: RwLock<Instant>,
}

impl RateLimiter {
    /// Creates a new rate limiter.
    pub fn new(capacity: u64, refill_per_second: u64) -> Self {
        let capacity = capacity as f64;
        Self {
            tokens: RwLock::new(capacity),
            capacity,
            refill_rate: refill_per_second as f64,
            last_refill: RwLock::new(Instant::now()),
        }
    }

    /// Attempts to acquire a token, returns true if successful.
    pub fn try_acquire(&self) -> bool {
        self.refill();

        let mut tokens = self.tokens.write();
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Acquires a token, blocking until available.
    pub async fn acquire(&self) {
        while !self.try_acquire() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Returns the number of available tokens.
    pub fn available(&self) -> f64 {
        self.refill();
        *self.tokens.read()
    }

    fn refill(&self) {
        let mut last_refill = self.last_refill.write();
        let elapsed = last_refill.elapsed().as_secs_f64();
        let refill = elapsed * self.refill_rate;

        let mut tokens = self.tokens.write();
        *tokens = (*tokens + refill).min(self.capacity);
        *last_refill = Instant::now();
    }
}

// ============================================================================
// Error Recovery
// ============================================================================

/// Configuration for automatic error recovery.
#[derive(Debug, Clone, Default)]
pub struct RecoveryConfig {
    /// Maximum number of recovery attempts.
    pub max_attempts: u32,
    /// Base delay between recovery attempts.
    pub base_delay_ms: u64,
    /// Maximum delay between recovery attempts.
    pub max_delay_ms: u64,
    /// Whether to reset failure counters after successful recovery.
    pub reset_on_success: bool,
    /// Whether to escalate after consecutive failures.
    pub escalate_on_consecutive_failures: bool,
}

/// Error recovery state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryState {
    /// No recovery in progress.
    Idle,
    /// Recovery in progress.
    Recovering,
    /// Recovery succeeded.
    Recovered,
    /// Recovery failed after all attempts.
    Failed,
}

/// Result of a recovery attempt.
#[derive(Debug, Clone)]
pub struct RecoveryResult {
    /// Whether the recovery succeeded.
    pub success: bool,
    /// Number of attempts made.
    pub attempts: u32,
    /// Total time spent recovering.
    pub total_time_ms: u64,
    /// Error message if recovery failed.
    pub error: Option<String>,
}

/// Automatic error recovery handler.
pub struct ErrorRecovery {
    config: RecoveryConfig,
    state: RwLock<RecoveryState>,
    attempts: AtomicU64,
    consecutive_failures: AtomicU64,
    last_recovery: RwLock<Option<Instant>>,
}

impl ErrorRecovery {
    /// Creates a new error recovery handler with default configuration.
    pub fn new() -> Self {
        Self::with_config(RecoveryConfig::default())
    }

    /// Creates a new error recovery handler with custom configuration.
    pub fn with_config(config: RecoveryConfig) -> Self {
        Self {
            config,
            state: RwLock::new(RecoveryState::Idle),
            attempts: AtomicU64::new(0),
            consecutive_failures: AtomicU64::new(0),
            last_recovery: RwLock::new(None),
        }
    }

    /// Returns the current recovery state.
    pub fn state(&self) -> RecoveryState {
        *self.state.read()
    }

    /// Returns the number of recovery attempts.
    pub fn attempts(&self) -> u32 {
        self.attempts.load(Ordering::SeqCst) as u32
    }

    /// Returns whether recovery is in progress.
    pub fn is_recovering(&self) -> bool {
        self.state() == RecoveryState::Recovering
    }

    /// Returns the delay until the next recovery attempt.
    pub fn next_retry_delay(&self) -> Duration {
        let attempts = self.attempts() as u64;
        let delay = self.config.base_delay_ms * 2u64.pow(attempts as u32);
        Duration::from_millis(delay.min(self.config.max_delay_ms))
    }

    /// Records a recovery attempt.
    pub fn record_attempt(&self) {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        *self.state.write() = RecoveryState::Recovering;
    }

    /// Records a successful recovery.
    pub fn record_success(&self) {
        if self.config.reset_on_success {
            self.consecutive_failures.store(0, Ordering::SeqCst);
        }
        self.attempts.store(0, Ordering::SeqCst);
        *self.state.write() = RecoveryState::Recovered;
        *self.last_recovery.write() = Some(Instant::now());
    }

    /// Records a failed recovery attempt.
    pub fn record_failure(&self) {
        self.consecutive_failures.fetch_add(1, Ordering::SeqCst);
        self.attempts.fetch_add(1, Ordering::SeqCst);

        if self.attempts() >= self.config.max_attempts {
            *self.state.write() = RecoveryState::Failed;
        }
    }

    /// Resets the recovery state.
    pub fn reset(&self) {
        self.attempts.store(0, Ordering::SeqCst);
        self.consecutive_failures.store(0, Ordering::SeqCst);
        *self.state.write() = RecoveryState::Idle;
        *self.last_recovery.write() = None;
    }

    /// Checks if escalation should occur based on consecutive failures.
    pub fn should_escalate(&self) -> bool {
        self.config.escalate_on_consecutive_failures
            && self.consecutive_failures.load(Ordering::SeqCst) >= 3
    }

    /// Returns time since last recovery attempt.
    pub fn time_since_last_recovery(&self) -> Option<Duration> {
        self.last_recovery.read().map(|t| t.elapsed())
    }
}

impl Default for ErrorRecovery {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_strategy_exponential() {
        let strategy = RetryStrategy::ExponentialBackoff {
            retries: 3,
            initial_delay_ms: 100,
            max_delay_ms: 1000,
            jitter_factor: 0.0,
        };

        assert!(strategy.should_retry(0));
        assert!(strategy.should_retry(1));
        assert!(strategy.should_retry(2));
        assert!(!strategy.should_retry(3));

        // Exponential backoff: delay = initial * 2^attempt
        // delay(1) = 100 * 2^1 = 200ms
        // delay(2) = 100 * 2^2 = 400ms
        // delay(3) = 100 * 2^3 = 800ms
        assert_eq!(strategy.delay(1), Duration::from_millis(200));
        assert_eq!(strategy.delay(2), Duration::from_millis(400));
        assert_eq!(strategy.delay(3), Duration::from_millis(800));
    }

    #[test]
    fn test_circuit_breaker_closed() {
        let cb = CircuitBreaker::new();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allows_request());

        // Record some failures
        cb.record_failure();
        cb.record_failure();
        cb.record_failure();

        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allows_request());
    }

    #[test]
    fn test_circuit_breaker_opens() {
        let cb = CircuitBreaker::with_config(3, 60); // 3 failures to open, 60s recovery
        assert!(cb.allows_request());
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert!(cb.allows_request());
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert!(cb.allows_request());
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        // After 3 failures, circuit should open
        assert!(!cb.allows_request());
        assert_eq!(cb.state(), CircuitState::Open);

        // Successes don't close the circuit when it's open
        cb.record_success();
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_breaker_half_open() {
        let cb = CircuitBreaker::with_config(3, 0); // 0 second timeout for testing
        assert!(cb.allows_request());

        // Open the circuit
        cb.record_failure();
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for recovery (timeout is 0, so any allows_request triggers half-open)
        assert!(cb.allows_request());
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // 3 successes close the circuit
        cb.record_success();
        cb.record_success();
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_health_metrics() {
        let mut metrics = HealthMetrics::new();

        metrics.record_success(10.0);
        metrics.record_success(20.0);
        metrics.record_failure();

        assert_eq!(metrics.operations_attempted, 3);
        assert_eq!(metrics.operations_succeeded, 2);
        assert_eq!(metrics.operations_failed, 1);
        assert!((metrics.success_rate() - 2.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn test_rate_limiter() {
        let limiter = RateLimiter::new(2, 10); // 2 tokens, refill 10/sec

        assert!(limiter.try_acquire()); // 1 left
        assert!(limiter.try_acquire()); // 0 left
        assert!(!limiter.try_acquire()); // exhausted

        // Wait for refill
        std::thread::sleep(Duration::from_millis(300));
        assert!(limiter.try_acquire());
    }

    #[test]
    fn test_error_recovery_default() {
        let recovery = ErrorRecovery::new();
        assert_eq!(recovery.state(), RecoveryState::Idle);
        assert_eq!(recovery.attempts(), 0);
        assert!(!recovery.is_recovering());
    }

    #[test]
    fn test_error_recovery_record_attempt() {
        let recovery = ErrorRecovery::new();
        recovery.record_attempt();
        assert_eq!(recovery.attempts(), 1);
        assert!(recovery.is_recovering());
    }

    #[test]
    fn test_error_recovery_success() {
        let recovery = ErrorRecovery::new();
        recovery.record_attempt();
        recovery.record_attempt();
        recovery.record_success();
        assert_eq!(recovery.state(), RecoveryState::Recovered);
        assert_eq!(recovery.attempts(), 0);
    }

    #[test]
    fn test_error_recovery_failure() {
        // max_attempts = 5: after 2 failures, attempts = 4 < 5 -> Recovering
        // We need a separate test to verify the Failed state
        let recovery = ErrorRecovery::with_config(RecoveryConfig {
            max_attempts: 5,
            base_delay_ms: 100,
            max_delay_ms: 5000,
            reset_on_success: false,
            escalate_on_consecutive_failures: false,
        });
        
        // Start recovering
        recovery.record_attempt();
        recovery.record_failure();
        assert_eq!(recovery.state(), RecoveryState::Recovering);

        // Second failure: attempts = 4 < 5 -> Still Recovering
        recovery.record_attempt();
        recovery.record_failure();
        assert_eq!(recovery.state(), RecoveryState::Recovering);

        // Third failure: attempts = 6 >= 5 -> Failed
        recovery.record_attempt();
        recovery.record_failure();
        assert_eq!(recovery.state(), RecoveryState::Failed);
    }

    #[test]
    fn test_error_recovery_reset() {
        let recovery = ErrorRecovery::new();
        recovery.record_attempt();
        recovery.record_failure();
        recovery.reset();
        assert_eq!(recovery.state(), RecoveryState::Idle);
        assert_eq!(recovery.attempts(), 0);
    }

    #[test]
    fn test_error_recovery_consecutive_failures() {
        let recovery = ErrorRecovery::with_config(RecoveryConfig {
            max_attempts: 5,
            base_delay_ms: 100,
            max_delay_ms: 5000,
            reset_on_success: false,
            escalate_on_consecutive_failures: true,
        });
        for _ in 0..3 {
            recovery.record_failure();
        }
        assert!(recovery.should_escalate());
    }

    #[test]
    fn test_error_recovery_next_retry_delay() {
        let recovery = ErrorRecovery::with_config(RecoveryConfig {
            max_attempts: 5,
            base_delay_ms: 100,
            max_delay_ms: 5000,
            reset_on_success: false,
            escalate_on_consecutive_failures: false,
        });
        // First attempt: base_delay * 2^0 = 100ms
        assert_eq!(recovery.next_retry_delay(), Duration::from_millis(100));

        recovery.record_attempt();
        // Second attempt: base_delay * 2^1 = 200ms
        assert_eq!(recovery.next_retry_delay(), Duration::from_millis(200));
    }

    #[test]
    fn test_recovery_config_default() {
        let config = RecoveryConfig::default();
        assert_eq!(config.max_attempts, 0);
        assert_eq!(config.base_delay_ms, 0);
        assert_eq!(config.max_delay_ms, 0);
        assert!(!config.reset_on_success);
        assert!(!config.escalate_on_consecutive_failures);
    }

    #[test]
    fn test_recovery_config_with_values() {
        let config = RecoveryConfig {
            max_attempts: 5,
            base_delay_ms: 200,
            max_delay_ms: 10000,
            reset_on_success: true,
            escalate_on_consecutive_failures: true,
        };
        assert_eq!(config.max_attempts, 5);
        assert_eq!(config.base_delay_ms, 200);
        assert_eq!(config.max_delay_ms, 10000);
    }
}
