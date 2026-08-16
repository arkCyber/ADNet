//! Health check surface for the metrics HTTP server.
//!
//! ## What this module provides
//!
//! PR4 enhances the `/health` endpoint from a simple
//! always-200 liveness probe to a **readiness probe** that
//! runs registered dependency checks and returns a
//! structured response.
//!
//! The design is intentionally simple:
//! - A [`HealthCheck`] trait with a single async `check()`
//!   method returning `Result<(), HealthCheckError>`.
//! - A [`HealthRegistry`] that holds the registered checks
//!   and runs them all on each `/health` request.
//! - A [`HealthStatus`] response type returned by
//!   [`run_checks`].
//!
//! ## Response format
//!
//! ```json
//! {
//!   "status": "ok",
//!   "checks": [
//!     { "status": "ok", "name": "disk_space", "message": null },
//!     { "status": "err", "name": "relay_upstream", "message": "connection refused" }
//!   ]
//! }
//! ```
//!
//! HTTP status codes:
//! - `200 OK` — all checks passed.
//! - `503 Service Unavailable` — at least one check failed.
//!
//! ## Registering checks
//!
//! Call [`register_health_check`] before starting the
//! metrics server. Checks are held in a process-global
//! `RwLock<Vec<Arc<dyn HealthCheck>>>` and run on every
//! `/health` request. The check vector is **append-only**.
//!
//! ## Implementing a health check
//!
//! ```ignore
//! // The `check()` method uses `Pin<Box<dyn Future>>` so the
//! // trait is `dyn`-compatible and can be stored globally.
//!
//! struct RelayUpstreamCheck { relay_url: url::Url }
//!
//! impl HealthCheck for RelayUpstreamCheck {
//!     fn name(&self) -> &'static str { "relay_upstream" }
//!
//!     fn check(self: Arc<Self>) -> Pin<Box<dyn Future<Output = Result<(), HealthCheckError>> + Send>> {
//!         let url = self.relay_url.clone();
//!         Box::pin(async move {
//!             let client = reqwest::Client::new();
//!             client.get(url).send().await
//!                 .map_err(|e| HealthCheckError::new(format!("relay unreachable: {e}")))?;
//!             Ok(())
//!         })
//!     }
//! }
//!
//! // At startup:
//! register_health_check(RelayUpstreamCheck { relay_url });
//! ```

use std::pin::Pin;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Errors from a health check. Wraps a plain `String` so
/// implementors don't have to define their own error types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckError {
    message: String,
}

impl HealthCheckError {
    /// Construct from any `Display`-able value.
    pub fn new(msg: impl std::fmt::Display) -> Self {
        Self {
            message: msg.to_string(),
        }
    }

    /// Construct from a plain `String`.
    pub fn from_string(msg: String) -> Self {
        Self { message: msg }
    }
}

impl std::fmt::Display for HealthCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for HealthCheckError {}

/// Trait for a single health check. Implement this for each
/// dependency you want to verify before advertising the
/// node as "ready".
///
/// **Dyn-safe**: the `check()` method returns `Pin<Box<dyn Future>>`
/// so this trait can be stored as `Arc<dyn HealthCheck>` in
/// the global check registry. Implementors use
/// `self: Arc<Self>` so the method takes ownership of the
/// `Arc` — this is the idiomatic Rust pattern for dyn-safe
/// async traits.
///
/// The check should be **fast** (network timeouts should be
/// bounded to a few seconds). Slow checks block the
/// `/health` response.
pub trait HealthCheck: Send + Sync + 'static {
    /// Stable name for this check. Used as the key in the
    /// `/health` JSON response.
    fn name(&self) -> &'static str;

    /// Run the check. Return `Ok(())` if the dependency is
    /// healthy; return `Err(HealthCheckError)` if unhealthy.
    ///
    /// `self` is `Arc<Self>` — the idiomatic Rust pattern
    /// for dyn-safe async traits.
    fn check(
        self: Arc<Self>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), HealthCheckError>> + Send>>;
}

/// Result of a single check, suitable for JSON serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum CheckResult {
    /// Check passed.
    Ok,
    /// Check failed — the dependency is unhealthy.
    Err { name: String, message: String },
}

/// Overall health response for `GET /health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    /// `"ok"` when all checks pass; `"err"` when at least
    /// one check fails.
    pub status: &'static str,
    /// Per-check results, in registration order.
    pub checks: Vec<CheckResult>,
}

// ──────────────────── Registry ───────────────────────────────────────────────

type CheckBox = Arc<dyn HealthCheck>;

/// The registry — a plain `static` so `clear()` and `run_checks()`
/// always access the **same** `RwLock` regardless of call order.
/// This is the simplest possible global state with interior mutability.
static REGISTRY: RwLock<Vec<CheckBox>> = RwLock::new(Vec::new());

/// Accessor for the global registry.
fn registry() -> &'static RwLock<Vec<CheckBox>> {
    &REGISTRY
}

/// Register a health check into the process-global registry.
/// Checks are append-only (no unregister API). The check
/// is wrapped in `Arc` and stored in `Vec<Arc<dyn HealthCheck>>`.
pub fn register_health_check<C: HealthCheck + 'static>(check: C) {
    registry().write().push(Arc::new(check));
}

/// Clear all registered health checks. Primarily useful in tests.
#[cfg(test)]
pub fn clear_checks() {
    registry().write().clear();
}

/// Run every registered health check and return an aggregate
/// [`HealthStatus`]. Does **not** set the HTTP status code —
/// callers decide 200 vs 503 based on whether any check
/// failed.
pub async fn run_checks() -> HealthStatus {
    let checks: Vec<CheckBox> = {
        let guard = registry().read();
        let checks = guard.clone();
        drop(guard); // Ensure guard is dropped before any await
        checks
    };

    let mut results: Vec<CheckResult> = Vec::with_capacity(checks.len());

    for check in checks {
        let name = check.name();
        let outcome = check.check().await;
        match outcome {
            Ok(()) => results.push(CheckResult::Ok),
            Err(e) => {
                tracing::debug!(check = name, error = %e, "health check failed");
                results.push(CheckResult::Err {
                    name: name.to_string(),
                    message: e.message,
                });
            }
        }
    }

    let status = if results.iter().all(|r| matches!(r, CheckResult::Ok)) {
        "ok"
    } else {
        "err"
    };

    HealthStatus {
        status,
        checks: results,
    }
}

/// Clear all registered health checks. Intended for use in
/// tests to prevent check accumulation across test cases that
/// share the process-global registry.
pub fn clear_health_checks() {
    registry().write().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysOk;
    impl HealthCheck for AlwaysOk {
        fn name(&self) -> &'static str { "always_ok" }
        fn check(self: Arc<Self>) -> Pin<Box<dyn std::future::Future<Output = Result<(), HealthCheckError>> + Send>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct AlwaysFail(String);
    impl HealthCheck for AlwaysFail {
        fn name(&self) -> &'static str { "always_fail" }
        fn check(self: Arc<Self>) -> Pin<Box<dyn std::future::Future<Output = Result<(), HealthCheckError>> + Send>> {
            let msg = self.0.clone();
            Box::pin(async move { Err(HealthCheckError::new(msg)) })
        }
    }

    
    
    #[tokio::test]

    async fn all_ok_returns_ok_status() {
        clear_checks(); // Ensure clean state
        register_health_check(AlwaysOk);
        let status = run_checks().await;
        assert_eq!(status.status, "ok");
        assert_eq!(status.checks.len(), 1);
        assert!(matches!(status.checks[0], CheckResult::Ok));
    }

    
    
    #[tokio::test]

    async fn one_fail_returns_err_status() {
        clear_checks(); // Ensure clean state
        register_health_check(AlwaysFail("boom".to_string()));
        let status = run_checks().await;
        assert_eq!(status.status, "err");
        assert_eq!(status.checks.len(), 1);
        assert!(matches!(
            status.checks[0],
            CheckResult::Err { ref name, ref message }
            if name == "always_fail" && message == "boom"
        ));
    }

    
    
    #[tokio::test]

    async fn mixed_checks_partial_failure() {
        clear_checks(); // Ensure clean state
        register_health_check(AlwaysOk);
        register_health_check(AlwaysFail("downstream error".to_string()));
        register_health_check(AlwaysOk);
        let status = run_checks().await;
        assert_eq!(status.status, "err");
        assert_eq!(status.checks.len(), 3);
        assert!(matches!(status.checks[0], CheckResult::Ok));
        assert!(matches!(
            status.checks[1],
            CheckResult::Err { ref message, .. } if message == "downstream error"
        ));
        assert!(matches!(status.checks[2], CheckResult::Ok));
    }

    #[test]
    fn error_display_formats_message() {
        let e = HealthCheckError::new("connection refused");
        assert_eq!(format!("{e}"), "connection refused");
    }

    #[test]
    fn error_from_string_preserves_message() {
        let e = HealthCheckError::from_string("timeout".to_string());
        assert_eq!(e.message, "timeout");
    }
}
