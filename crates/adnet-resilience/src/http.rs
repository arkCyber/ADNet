//! `ResilientHttpClient` — wraps [`reqwest::Client`] with a circuit breaker
//! + retry-with-backoff policy.
//!
//! Ported from `Exodus@src-backup/.../microservice/resilience.rs::ResilientHttpClient`.

use std::sync::Arc;
use std::time::Duration;

use tracing::warn;

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
use crate::retry::{RetryConfig, retry_with_backoff};

/// Combined HTTP client config.
#[derive(Debug, Clone)]
pub struct ResilientHttpConfig {
    pub retry: RetryConfig,
    pub breaker: CircuitBreakerConfig,
    pub request_timeout: Duration,
}

impl Default for ResilientHttpConfig {
    fn default() -> Self {
        Self {
            retry: RetryConfig::default(),
            breaker: CircuitBreakerConfig::default(),
            request_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Clone)]
pub struct ResilientHttpClient {
    client: reqwest::Client,
    breaker: Arc<CircuitBreaker>,
    retry: RetryConfig,
}

impl ResilientHttpClient {
    pub fn new() -> Self {
        Self::with_config(ResilientHttpConfig::default())
    }

    pub fn with_config(cfg: ResilientHttpConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(cfg.request_timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            breaker: Arc::new(CircuitBreaker::with_config(cfg.breaker)),
            retry: cfg.retry,
        }
    }

    pub fn breaker(&self) -> Arc<CircuitBreaker> {
        Arc::clone(&self.breaker)
    }

    /// HTTP GET with circuit breaker + retry.
    pub async fn get(&self, url: &str) -> Result<reqwest::Response, String> {
        if !self.breaker.allow_request().await {
            return Err("circuit breaker is open".into());
        }
        let client = self.client.clone();
        let url = url.to_string();
        let breaker = Arc::clone(&self.breaker);
        let result = retry_with_backoff(
            move || {
                let client = client.clone();
                let url = url.clone();
                async move {
                    let resp = client
                        .get(&url)
                        .send()
                        .await
                        .map_err(|e| format!("send: {e}"))?;
                    if !resp.status().is_success() {
                        return Err(format!("HTTP {}", resp.status()));
                    }
                    Ok(resp)
                }
            },
            self.retry.clone(),
        )
        .await;
        match result {
            Ok(resp) => {
                breaker.record_success().await;
                Ok(resp)
            }
            Err(e) => {
                breaker.record_failure().await;
                warn!(error = %e, "resilient http get failed");
                Err(e)
            }
        }
    }

    /// HTTP GET, returning the body bytes on success.
    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        let resp = self.get(url).await?;
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("read body: {e}"))
    }
}

impl Default for ResilientHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_when_breaker_open() {
        let cfg = ResilientHttpConfig {
            breaker: CircuitBreakerConfig {
                failure_threshold: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let client = ResilientHttpClient::with_config(cfg);
        client.breaker.record_failure().await;
        // Breaker should now be Open — get() must fail fast.
        let err = client.get("http://127.0.0.1:1/never").await.unwrap_err();
        assert!(err.contains("circuit breaker"));
    }
}
