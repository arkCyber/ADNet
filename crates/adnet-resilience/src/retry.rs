//! Retry with exponential backoff + jitter.
//!
//! Mirrors `retry_with_backoff` / `RetryPolicy` / `RetryConfig` from
//! `Exodus@src-backup/.../microservice/resilience.rs`.

use std::future::Future;
use std::time::Duration;

use rand::Rng;
use tracing::warn;

/// Configuration for a retry-with-backoff loop.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of attempts (1 = no retry).
    pub max_attempts: u32,
    /// Initial delay before the first retry.
    pub initial_delay: Duration,
    /// Upper bound on per-attempt delay.
    pub max_delay: Duration,
    /// Backoff multiplier applied between attempts.
    pub backoff_multiplier: f64,
    /// Jitter factor in `[0.0, 1.0]` to prevent thundering-herd.
    pub jitter_factor: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(5),
            backoff_multiplier: 2.0,
            jitter_factor: 0.1,
        }
    }
}

/// Pre-built retry profiles for common scenarios. Mirrors the four variants
/// in `Exodus@src-backup/.../microservice/resilience.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryPolicy {
    /// No retry — single attempt.
    None,
    /// Quick retries for transient failures (3 attempts, fast).
    Transient,
    /// Aggressive retries for network issues (5 attempts, exponential).
    Aggressive,
    /// Conservative retries for critical operations (2 attempts, mild).
    Conservative,
}

impl RetryPolicy {
    pub fn to_config(self) -> RetryConfig {
        match self {
            RetryPolicy::None => RetryConfig {
                max_attempts: 1,
                ..Default::default()
            },
            RetryPolicy::Transient => RetryConfig {
                max_attempts: 3,
                initial_delay: Duration::from_millis(50),
                max_delay: Duration::from_secs(1),
                backoff_multiplier: 1.5,
                jitter_factor: 0.2,
            },
            RetryPolicy::Aggressive => RetryConfig {
                max_attempts: 5,
                initial_delay: Duration::from_millis(100),
                max_delay: Duration::from_secs(10),
                backoff_multiplier: 2.0,
                jitter_factor: 0.1,
            },
            RetryPolicy::Conservative => RetryConfig {
                max_attempts: 2,
                initial_delay: Duration::from_millis(200),
                max_delay: Duration::from_secs(2),
                backoff_multiplier: 1.2,
                jitter_factor: 0.3,
            },
        }
    }
}

/// Execute `operation` with retry + exponential backoff.
///
/// `operation` is called up to `config.max_attempts` times; between attempts
/// the function sleeps for `delay + jitter` then doubles `delay` (capped at
/// `config.max_delay`). The first successful return is propagated.
pub async fn retry_with_backoff<F, Fut, T, E>(operation: F, config: RetryConfig) -> Result<T, E>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut last_error: Option<E> = None;
    let mut delay = config.initial_delay;
    for attempt in 0..config.max_attempts {
        match operation().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt + 1 >= config.max_attempts {
                    last_error = Some(e);
                    break;
                }
                warn!(attempt, error = %e, "retry_with_backoff: attempt failed");
                let jitter = if config.jitter_factor > 0.0 {
                    let mut rng = rand::thread_rng();
                    let max_ms = (delay.as_millis() as f64 * config.jitter_factor) as u64;
                    Duration::from_millis(rng.gen_range(0..=max_ms))
                } else {
                    Duration::ZERO
                };
                tokio::time::sleep(delay + jitter).await;
                let next_ms = (delay.as_millis() as f64 * config.backoff_multiplier)
                    .min(config.max_delay.as_millis() as f64) as u64;
                delay = Duration::from_millis(next_ms);
                last_error = Some(e);
            }
        }
    }
    Err(last_error.expect("max_attempts >= 1"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn succeeds_on_first_try() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_c = Arc::clone(&calls);
        let r: Result<i32, &'static str> = retry_with_backoff(
            move || {
                let c = Arc::clone(&calls_c);
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok::<i32, &'static str>(7)
                }
            },
            RetryPolicy::Transient.to_config(),
        )
        .await;
        assert_eq!(r.unwrap(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_until_success() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_c = Arc::clone(&calls);
        let r: Result<i32, &'static str> = retry_with_backoff(
            move || {
                let c = Arc::clone(&calls_c);
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst);
                    if n < 2 {
                        Err("nope")
                    } else {
                        Ok::<i32, &'static str>(42)
                    }
                }
            },
            RetryConfig {
                max_attempts: 4,
                initial_delay: Duration::from_millis(1),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(r.unwrap(), 42);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn exhausts_max_attempts() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_c = Arc::clone(&calls);
        let r: Result<i32, String> = retry_with_backoff(
            move || {
                let c = Arc::clone(&calls_c);
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err::<i32, String>("always".into())
                }
            },
            RetryConfig {
                max_attempts: 3,
                initial_delay: Duration::from_millis(1),
                jitter_factor: 0.0,
                ..Default::default()
            },
        )
        .await;
        assert!(r.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
