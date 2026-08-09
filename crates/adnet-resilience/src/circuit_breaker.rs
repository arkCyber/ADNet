//! Circuit breaker — fail fast after `failure_threshold` errors inside
//! `window_duration`, recover after `timeout` elapses with `success_threshold`
//! successful probes.
//!
//! Ported from
//! `Exodus@src-backup/.../microservice/circuit_breaker.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

/// Closed = traffic flows normally. Open = fail fast. HalfOpen = probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of failures within `window_duration` that trip the breaker.
    pub failure_threshold: u32,
    /// Open-state dwell time before transitioning to HalfOpen.
    pub timeout: Duration,
    /// Successful probes needed in HalfOpen to close the breaker again.
    pub success_threshold: u32,
    /// Sliding window over which failures are counted.
    pub window_duration: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            timeout: Duration::from_secs(60),
            success_threshold: 2,
            window_duration: Duration::from_secs(30),
        }
    }
}

#[derive(Debug)]
struct Inner {
    state: CircuitState,
    failures: u32,
    successes: u32,
    window_start: Instant,
    opened_at: Option<Instant>,
}

impl Inner {
    fn new() -> Self {
        Self {
            state: CircuitState::Closed,
            failures: 0,
            successes: 0,
            window_start: Instant::now(),
            opened_at: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    cfg: CircuitBreakerConfig,
    inner: Arc<RwLock<Inner>>,
}

impl CircuitBreaker {
    pub fn new() -> Self {
        Self::with_config(CircuitBreakerConfig::default())
    }

    pub fn with_config(cfg: CircuitBreakerConfig) -> Self {
        Self {
            cfg,
            inner: Arc::new(RwLock::new(Inner::new())),
        }
    }

    pub async fn state(&self) -> CircuitState {
        self.inner.read().await.state
    }

    /// Should the caller attempt an operation? Returns `false` when the
    /// breaker is Open (or HalfOpen with an outstanding probe).
    pub async fn allow_request(&self) -> bool {
        let mut g = self.inner.write().await;
        match g.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                let elapsed = g.opened_at.map(|t| t.elapsed()).unwrap_or_default();
                if elapsed >= self.cfg.timeout {
                    g.state = CircuitState::HalfOpen;
                    g.successes = 0;
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                // Allow a single probe; subsequent callers wait until the
                // current probe finishes.
                g.successes == 0
            }
        }
    }

    pub async fn record_success(&self) {
        let mut g = self.inner.write().await;
        match g.state {
            CircuitState::HalfOpen => {
                g.successes += 1;
                if g.successes >= self.cfg.success_threshold {
                    g.state = CircuitState::Closed;
                    g.failures = 0;
                    g.opened_at = None;
                }
            }
            CircuitState::Closed => {
                g.failures = 0;
                g.window_start = Instant::now();
            }
            CircuitState::Open => {}
        }
    }

    pub async fn record_failure(&self) {
        let mut g = self.inner.write().await;
        match g.state {
            CircuitState::Closed => {
                if g.window_start.elapsed() > self.cfg.window_duration {
                    g.failures = 0;
                    g.window_start = Instant::now();
                }
                g.failures += 1;
                if g.failures >= self.cfg.failure_threshold {
                    g.state = CircuitState::Open;
                    g.opened_at = Some(Instant::now());
                }
            }
            CircuitState::HalfOpen => {
                g.state = CircuitState::Open;
                g.opened_at = Some(Instant::now());
            }
            CircuitState::Open => {}
        }
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

    #[tokio::test]
    async fn closed_allows_traffic() {
        let cb = CircuitBreaker::new();
        assert_eq!(cb.state().await, CircuitState::Closed);
        assert!(cb.allow_request().await);
    }

    #[tokio::test]
    async fn trips_after_threshold() {
        let cb = CircuitBreaker::with_config(CircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        });
        for _ in 0..3 {
            cb.record_failure().await;
        }
        assert_eq!(cb.state().await, CircuitState::Open);
        assert!(!cb.allow_request().await);
    }

    #[tokio::test]
    async fn recovers_via_half_open() {
        let cb = CircuitBreaker::with_config(CircuitBreakerConfig {
            failure_threshold: 1,
            timeout: Duration::from_millis(10),
            success_threshold: 1,
            window_duration: Duration::from_secs(60),
        });
        cb.record_failure().await;
        assert_eq!(cb.state().await, CircuitState::Open);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(cb.allow_request().await);
        assert_eq!(cb.state().await, CircuitState::HalfOpen);
        cb.record_success().await;
        assert_eq!(cb.state().await, CircuitState::Closed);
    }
}
