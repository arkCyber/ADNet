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
//!     { "name": "disk_space", "status": "ok", "message": null },
//!     { "name": "relay_upstream", "status": "err", "message": "connection refused" }
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
//! `RwLock<Vec<_>>` and run on every `/health` request.
//! The check vector is **append-only** — there's no
//! `unregister` API. Unhealthy checks are detected at
//! request time, not registration time.
//!
//! ## `HealthCheck` impl example
//!
//! ```ignore
//! struct RelayUpstreamCheck {
//!     relay_url: url::Url,
//! }
//!
//! #[derive(Debug)]
//! struct RelayError(String);
//!
//! impl std::fmt::Display for RelayError {
//!     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//!         write!(f, "{}", self.0)
//!     }
//! }
//!
//! impl std::error::Error {}
//!
//! #[a3net_observability::http::HealthCheck]
//! impl a3net_observability::http::HealthCheck for RelayUpstreamCheck {
//!     fn name(&self) -> &'static str { "relay_upstream" }
//!
//!     async fn check(&self) -> Result<(), HealthCheckError> {
//!         let client = reqwest::Client::new();
//!         client.get(self.relay_url.clone()).send().await
//!             .map_err(|e| HealthCheckError::new(format!("relay unreachable: {e}")))?;
//!         Ok(())
//!     }
//! }
//! ```

use std::sync::Arc;

use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Errors from a health check. Wraps a plain `String` so
/// implementors don't have to define their own error types.
///
/// The `display` impl formats the inner message so the
/// `/health` JSON response is human-readable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckError {
    message: String,
}

impl HealthCheckError {
    /// Construct from a `String`. Convenience for
    /// `Err(HealthCheckError::new(format!(...)))`.
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

impl std::fmt::Display for HealthCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error {}

/// Trait for a single health check. Implement this for each
/// dependency you want to verify before advertising the
/// node as "ready".
///
/// The trait is `Send + Sync + 'static` so it can live in
/// a `Box<dyn HealthCheck>` stored in the process-global
/// check registry.
pub trait HealthCheck: Send + Sync + 'static {
    /// Stable name for this check. Used as the key in the
    /// `/health` JSON response. Must be a valid JSON string
    /// identifier (no spaces, dots, etc. — use `_` or
    /// camelCase).
    fn name(&self) -> &'static str;

    /// Run the check. Return `Ok(())` if the dependency is
    /// healthy; return `Err(HealthCheckError)` with a human-
    /// readable message if unhealthy.
    ///
    /// The check should be **fast** (network timeouts
    /// should be bounded to a few seconds at most). Slow
    /// checks block the `/health` response.
    fn check(&self) -> impl std::future::Future<Output = Result<(), HealthCheckError>> + Send;
}

/// Result of a single check, suitable for JSON serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum CheckResult {
    /// Check passed.
    Ok,
    /// Check failed — the dependency is unhealthy.
    Err { message: String },
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

/// Run every registered health check and return an aggregate
/// [`HealthStatus`]. Does **not** set the HTTP status code —
/// callers decide 200 vs 503 based on whether any check
/// failed.
///
/// **Locking**: holds the `HEALTH_CHECKS.read()` guard
/// for the duration of all async checks. This is safe
/// because `RwLockReadGuard` is `Send + Sync` and the
/// guard's lifetime is shorter than the async task.
pub async fn run_checks() -> HealthStatus {
    let checks: Vec<Arc<dyn HealthCheck>> = {
        let guard = HEALTH_CHECKS.read();
        guard.clone()
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

// ──────────────────── Registration ─────────────────────────────────────────

type CheckBox = Arc<dyn HealthCheck>;

static HEALTH_CHECKS: Lazy<RwLock<Vec<CheckBox>>> =
    Lazy::new(|| RwLock::new(Vec::new()));

/// Register a health check into the process-global registry.
/// Checks are append-only (no unregister API). The check
/// is wrapped in `Arc` and stored in a `Vec<Arc<dyn HealthCheck>>`
/// so it can be shared across any number of async tasks
/// that call `run_checks()`.
///
/// **Idempotency**: registering the same check struct multiple
/// times adds it multiple times. Callers that want to avoid
/// duplicates should register at most once, typically during
/// application startup.
pub fn register_health_check<C: HealthCheck>(check: C) {
    HEALTH_CHECKS.write().push(Arc::new(check));
}

/// [`Box`]ed version of [`register_health_check`]. Useful when
/// the check is already in an `Arc` or `Box`.
pub fn register_health_check_boxed(check: CheckBox) {
    HEALTH_CHECKS.write().push(check);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysOk;
    impl HealthCheck for AlwaysOk {
        fn name(&self) -> &'static str { "always_ok" }
        fn check(&self) -> impl std::future::Future<Output = Result<(), HealthCheckError>> + Send {
            async { Ok(()) }
        }
    }

    struct AlwaysFail {
        msg: String,
    }
    impl HealthCheck for AlwaysFail {
        fn name(&self) -> &'static str { "always_fail" }
        fn check(&self) -> impl std::future::Future<Output = Result<(), HealthCheckError>> + Send {
            let msg = self.msg.clone();
            async move { Err(HealthCheckError::new(msg)) }
        }
    }

    #[tokio::test]
    async fn all_ok_returns_ok_status() {
        register_health_check(AlwaysOk);
        let status = run_checks().await;
        assert_eq!(status.status, "ok");
        assert_eq!(status.checks.len(), 1);
        assert!(matches!(status.checks[0], CheckResult::Ok));
    }

    #[tokio::test]
    async fn one_fail_returns_err_status() {
        register_health_check(AlwaysFail { msg: "boom".into() });
        let status = run_checks().await;
        assert_eq!(status.status, "err");
        assert_eq!(status.checks.len(), 1);
        assert!(matches!(
            status.checks[0],
            CheckResult::Err { ref message } if message == "boom"
        ));
    }

    #[tokio::test]
    async fn mixed_checks_partial_failure() {
        register_health_check(AlwaysOk);
        register_health_check(AlwaysFail { msg: "downstream error".into() });
        register_health_check(AlwaysOk);
        let status = run_checks().await;
        assert_eq!(status.status, "err");
        assert_eq!(status.checks.len(), 3);
        assert!(matches!(status.checks[0], CheckResult::Ok));
        assert!(matches!(
            status.checks[1],
            CheckResult::Err { ref message } if message == "downstream error"
        ));
        assert!(matches!(status.checks[2], CheckResult::Ok));
    }

    #[test]
    fn error_display_formats_message() {
        let e = HealthCheckError::new("connection refused");
        assert_eq!(format!("{e}"), "connection refused");
        assert_eq!(e.message, "connection refused");
    }
}
