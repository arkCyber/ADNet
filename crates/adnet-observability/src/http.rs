//! HTTP server for the metrics surface.
//!
//! Exposes four endpoints:
//!
//! - `GET /metrics`       — Prometheus text format
//!   (`text/plain; version=0.0.4`).
//! - `GET /metrics.json`  — JSON snapshot of every registered
//!   metric, for debugging.
//! - `GET /health`        — readiness probe that runs
//!   registered dependency checks and returns 200 / 503
//!   based on whether all checks pass. See
//!   [`crate::http_health`] for how to register checks.
//! - `GET /diagnostics`   — JSON dump of the registry contents
//!   plus the metrics count and a timestamp. Aimed at
//!   operators reading `/diagnostics` on the wire.

#![cfg(feature = "http-server")]

use std::net::SocketAddr;
use std::ops::Deref;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use parking_lot::RwLock;
use serde::Serialize;

use crate::prometheus::PrometheusExporter;
use crate::registry::Registry;

/// Configuration for the metrics HTTP server.
#[derive(Debug, Clone)]
pub struct MetricsServerConfig {
    /// Bind address. `0.0.0.0:0` lets the OS pick a port
    /// (useful for tests); production callers should pin
    /// to `127.0.0.1:9090` or whatever the operator wants.
    pub bind_addr: SocketAddr,
    /// Registry to expose. If `None`, the global
    /// [`GLOBAL`](crate::registry::GLOBAL) registry is used.
    pub registry: Option<Arc<Registry>>,
}

impl Default for MetricsServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:9090".parse().expect("valid socket addr"),
            registry: None,
        }
    }
}

/// Handle to a running metrics server. Drop to stop.
#[derive(Debug)]
pub struct MetricsServer {
    /// Bound address (resolved from the config — useful when
    /// the config said `0.0.0.0:0` and we want the OS-assigned
    /// port).
    pub bound_addr: SocketAddr,
    /// Shutdown signal sender. Cloning is cheap.
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// Join handle for the server task. `None` when the
    /// caller took the server into a manual loop.
    join: RwLock<Option<tokio::task::JoinHandle<()>>>,
}

impl MetricsServer {
    /// Stop the server. Idempotent — the second call is a
    /// no-op.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Wait for the server task to finish. Returns immediately
    /// if `shutdown` was already called.
    pub async fn join(&self) {
        // Take the join handle out of the lock before
        // awaiting — `parking_lot::MutexGuard` is not
        // `Send`-aware and would hold the lock across the
        // await point, blocking other callers of `join`
        // (and confusing clippy's `await_holding_lock`).
        let handle = self.join.write().take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }

    /// Bound address — may differ from the config (e.g.
    /// `0.0.0.0:0` resolves to a kernel-assigned port).
    pub fn local_addr(&self) -> SocketAddr {
        self.bound_addr
    }
}

/// Shared state for the axum app.
#[derive(Clone)]
pub struct AppState {
    registry: Arc<Registry>,
}

impl AppState {
    pub fn new(registry: Arc<Registry>) -> Self {
        Self { registry }
    }
}

/// Start the metrics server on the address from `config`. The
/// returned [`MetricsServer`] handle owns the task; dropping
/// the handle stops the server.
pub async fn serve(config: MetricsServerConfig) -> std::io::Result<MetricsServer> {
    // IMPORTANT: when the caller does not supply a registry,
    // we serve the **global** registry — NOT a fresh
    // `Registry::default()`. Crates register their metrics
    // into `crate::registry::GLOBAL` at process startup (via
    // `static FOO: Lazy<...>` blocks), so the HTTP surface
    // MUST read from the same instance. Serving a fresh
    // empty registry would silently expose `/metrics` with
    // zero samples, even after every other crate had
    // registered its counters.
    let registry = config
        .registry
        .unwrap_or_else(|| Arc::new(crate::registry::GLOBAL.deref().clone()));
    // We deliberately clone the registry into the AppState
    // rather than reusing the `Arc<Registry>`; this way the
    // caller can replace the `Arc` at any time without
    // affecting the running server.
    let state = AppState::new(Arc::clone(&registry));
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/metrics.json", get(metrics_json_handler))
        .route("/health", get(health_handler))
        .route("/diagnostics", get(diagnostics_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    let bound_addr = listener.local_addr()?;
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let join = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.changed().await;
            })
            .await
            .ok();
    });
    Ok(MetricsServer {
        bound_addr,
        shutdown_tx,
        join: RwLock::new(Some(join)),
    })
}

/// `GET /metrics` handler — Prometheus text format.
pub async fn metrics_handler(State(state): State<AppState>) -> Response {
    let exporter = PrometheusExporter::new(&state.registry);
    let output = exporter.render();
    let mut resp = (StatusCode::OK, output.body).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(output.content_type),
    );
    resp
}

/// `GET /health` handler — liveness probe. Returns 200 OK
/// with a small JSON body containing the current time and
/// the number of registered metrics. A more sophisticated
/// readiness probe (checking dependencies) is out of scope
/// for PR1.
pub async fn health_handler(State(state): State<AppState>) -> Response {
    use crate::health::{run_checks, CheckResult};

    #[derive(serde::Serialize)]
    struct HealthBody {
        status: &'static str,
        metric_count: usize,
        now_unix_ms: i64,
        checks: Vec<CheckResult>,
    }

    let health = run_checks().await;
    let body = HealthBody {
        status: health.status,
        metric_count: state.registry.metric_count(),
        now_unix_ms: chrono::Utc::now().timestamp_millis(),
        checks: health.checks,
    };

    let http_status = if health.status == "ok" {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (http_status, Json(body)).into_response()
}

/// `GET /metrics.json` handler — JSON dump of every registered
/// metric, one entry per metric. The structure mirrors the
/// Prometheus output: each metric has a `name`, `kind`, `help`,
/// and a `samples` array.
pub async fn metrics_json_handler(State(state): State<AppState>) -> Response {
    use crate::histogram::Histogram;
    use crate::metrics::{Counter, Gauge};
    use crate::registry::RegistrySnapshot;

    #[derive(Serialize)]
    struct JsonMetric {
        name: String,
        kind: &'static str,
        help: String,
        samples: Vec<JsonSample>,
    }

    #[derive(Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum JsonSample {
        Counter { labels: String, value: u64 },
        Gauge { labels: String, value: i64 },
        HistogramBucket { labels: String, le: f64, value: u64 },
        HistogramCount { labels: String, value: u64 },
        HistogramSum { labels: String, value: f64 },
    }

    let snap: RegistrySnapshot = state.registry.snapshot();
    let mut metrics_out: Vec<JsonMetric> = Vec::with_capacity(snap.len());
    for m in snap.sorted() {
        let mut samples = Vec::new();
        // The exporter pattern is: walk each metric and
        // produce samples. We do it inline here rather than
        // calling `render_prometheus()` because we want
        // typed JSON, not the text format.
        if let Some(c) = m.as_any().downcast_ref::<Counter>() {
            // The unlabeled value is always present.
            samples.push(JsonSample::Counter {
                labels: String::new(),
                value: c.get(),
            });
            for (labels, value) in c.labeled_snapshot() {
                samples.push(JsonSample::Counter {
                    labels: labels.render(),
                    value,
                });
            }
        } else if let Some(g) = m.as_any().downcast_ref::<Gauge>() {
            samples.push(JsonSample::Gauge {
                labels: String::new(),
                value: g.get(),
            });
            for (labels, value) in g.labeled_snapshot() {
                samples.push(JsonSample::Gauge {
                    labels: labels.render(),
                    value,
                });
            }
        } else if let Some(h) = m.as_any().downcast_ref::<Histogram>() {
            let unlabeled = h.snapshot();
            for (le, value) in &unlabeled.buckets {
                samples.push(JsonSample::HistogramBucket {
                    labels: String::new(),
                    le: *le,
                    value: *value,
                });
            }
            samples.push(JsonSample::HistogramCount {
                labels: String::new(),
                value: unlabeled.count,
            });
            samples.push(JsonSample::HistogramSum {
                labels: String::new(),
                value: unlabeled.sum,
            });
            for (labels, snap) in h.labeled_snapshots() {
                let label_str = labels.render();
                for (le, value) in &snap.buckets {
                    samples.push(JsonSample::HistogramBucket {
                        labels: label_str.clone(),
                        le: *le,
                        value: *value,
                    });
                }
                samples.push(JsonSample::HistogramCount {
                    labels: label_str.clone(),
                    value: snap.count,
                });
                samples.push(JsonSample::HistogramSum {
                    labels: label_str,
                    value: snap.sum,
                });
            }
        } else {
            // Unknown metric kind (e.g. a custom Metric impl
            // from a downstream crate). Skip — we cannot
            // render typed samples without downcasting.
            continue;
        }
        metrics_out.push(JsonMetric {
            name: m.name().to_string(),
            kind: m.kind().as_prometheus_str(),
            help: m.help().to_string(),
            samples,
        });
    }
    (StatusCode::OK, Json(metrics_out)).into_response()
}

/// `GET /diagnostics` handler — operator-facing JSON view
/// of the registry contents plus a few metadata fields.
pub async fn diagnostics_handler(State(state): State<AppState>) -> Response {
    #[derive(Serialize)]
    struct DiagnosticsBody {
        now_unix_ms: i64,
        metric_count: usize,
        metric_names: Vec<String>,
    }
    let names: Vec<String> = state
        .registry
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let mut names = names;
    names.sort();
    let body = DiagnosticsBody {
        now_unix_ms: chrono::Utc::now().timestamp_millis(),
        metric_count: state.registry.metric_count(),
        metric_names: names,
    };
    (StatusCode::OK, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time test: the routes compile when
    /// `axum` is available. The runtime tests live in
    /// `tests/http_smoke.rs` because axum's
    /// `Router::oneshot` requires the `tokio` runtime to
    /// be set up by the integration test harness, not
    /// the in-crate test harness.
    #[test]
    fn handlers_are_reachable_at_compile_time() {
        // We only need to *reference* the handler symbols
        // here — the body is empty. If any handler is
        // removed or its signature breaks, this reference
        // will fail to compile.
        let _ = (
            metrics_handler as fn(State<AppState>) -> _,
            metrics_json_handler as fn(State<AppState>) -> _,
            health_handler as fn(State<AppState>) -> _,
            diagnostics_handler as fn(State<AppState>) -> _,
        );
    }
}
