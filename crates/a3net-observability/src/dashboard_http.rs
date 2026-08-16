//! Embedded HTML Dashboard for A3Net observability.
//!
//! Provides a simple web-based dashboard that displays:
//!
//! - Node status and health
//! - Storage usage (private/shared)
//! - Replication metrics
//! - Alerts
//! - All metrics in a searchable table
//!
//! Access at `GET /` or `GET /dashboard`

use axum::{
    response::IntoResponse,
    routing::get,
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::health::{run_checks, CheckResult};
use crate::registry::Registry;
use crate::http::AppState;

/// Dashboard HTML template — self-contained single page.
const DASHBOARD_HTML: &str = include_str!("dashboard.html");

/// Dashboard server configuration.
#[derive(Debug, Clone)]
pub struct DashboardConfig {
    /// Node name for display.
    pub node_name: String,
    /// Bind address for the dashboard server.
    pub bind_addr: SocketAddr,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            node_name: "A3Net Node".to_string(),
            bind_addr: ([127, 0, 0, 1], 9090).into(),
        }
    }
}

/// Shared state for the dashboard.
#[derive(Clone)]
pub struct DashboardState {
    pub node_name: String,
    pub registry: Arc<Registry>,
}

impl From<DashboardState> for AppState {
    fn from(_state: DashboardState) -> Self {
        // Dashboard uses the same registry as the HTTP server
        // We use the registry from the main AppState
        AppState::new(Arc::new(Registry::default()))
    }
}

/// Register dashboard routes with an existing router.
pub fn register_dashboard_routes(router: Router) -> Router {
    router
        .route("/", get(dashboard_handler))
        .route("/dashboard", get(dashboard_handler))
}

/// `GET /` or `GET /dashboard` — serve the dashboard HTML.
pub async fn dashboard_handler() -> axum::response::Html<&'static str> {
    axum::response::Html(DASHBOARD_HTML)
}

/// `GET /dashboard/data` — JSON endpoint for dashboard data.
/// This handler is registered separately with the main app state.
pub async fn dashboard_data_handler(
    state: axum::extract::State<AppState>,
) -> impl IntoResponse {
    #[derive(serde::Serialize)]
    struct DashboardData {
        node_name: String,
        timestamp: i64,
        health: HealthSummary,
        metrics: Vec<MetricEntry>,
    }

    #[derive(serde::Serialize)]
    struct HealthSummary {
        status: String,
        checks: Vec<CheckResult>,
    }

    #[derive(serde::Serialize)]
    struct MetricEntry {
        name: String,
        kind: String,
        help: String,
        value: String,
    }

    let health = run_checks().await;
    let snap = state.registry().snapshot();

    let mut metrics = Vec::new();
    for m in snap.sorted() {
        let value = if let Some(c) = m.as_any().downcast_ref::<crate::metrics::Counter>() {
            c.get().to_string()
        } else if let Some(g) = m.as_any().downcast_ref::<crate::metrics::Gauge>() {
            g.get().to_string()
        } else {
            "N/A".to_string()
        };

        metrics.push(MetricEntry {
            name: m.name().to_string(),
            kind: m.kind().as_prometheus_str().to_string(),
            help: m.help().to_string(),
            value,
        });
    }

    let data = DashboardData {
        node_name: "A3Net Node".to_string(),
        timestamp: chrono::Utc::now().timestamp_millis(),
        health: HealthSummary {
            status: health.status.to_string(),
            checks: health.checks,
        },
        metrics,
    };

    (axum::http::StatusCode::OK, axum::Json(data))
}

/// Start a standalone dashboard server.
pub async fn serve_dashboard(config: DashboardConfig) -> std::io::Result<()> {
    use crate::registry::GLOBAL;

    let app = Router::new()
        .route("/", get(dashboard_handler))
        .route("/dashboard", get(dashboard_handler))
        .route("/dashboard/data", get(dashboard_data_handler))
        .with_state(AppState::new(Arc::new(GLOBAL.clone())));

    println!("Dashboard available at http://{}/", config.bind_addr);
    println!("Press Ctrl+C to stop");

    axum::serve(tokio::net::TcpListener::bind(config.bind_addr).await?, app).await?;

    Ok(())
}
