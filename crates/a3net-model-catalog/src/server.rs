//! Web Server - HTTP server for the model catalog web interface
//!
//! Provides:
//! - REST API for catalog operations
//! - Web UI for model browsing
//! - Download ticket management

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{info, error};

use crate::catalog::ModelCatalog;
use crate::downloader::ModelDownloader;
use crate::error::ModelCatalogError;
use crate::manifest::{format_size, ModelManifest};
use crate::reputation::{
    ProviderReputation, ProviderReputationTracker, ReportReason, TrustFlag,
};
use crate::types::{CatalogStats, ModelFilter, ModelType, PaginatedModels};

/// Application state
#[derive(Clone)]
pub struct AppState {
    pub catalog: Arc<ModelCatalog>,
    pub downloader: Arc<ModelDownloader>,
    pub static_path: Option<PathBuf>,
    pub reputation: Arc<ProviderReputationTracker>,
}

/// Server configuration
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub catalog_path: PathBuf,
    pub download_dir: PathBuf,
    pub static_path: Option<PathBuf>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
            catalog_path: PathBuf::from("model-catalog.db"),
            download_dir: PathBuf::from("./downloads"),
            static_path: None,
        }
    }
}

impl ServerConfig {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            ..Default::default()
        }
    }

    pub fn with_catalog_path<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.catalog_path = path.into();
        self
    }

    pub fn with_download_dir<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.download_dir = path.into();
        self
    }
}

/// Start the web server
pub async fn start(config: ServerConfig) -> Result<(), ModelCatalogError> {
    // Initialize catalog
    let catalog = ModelCatalog::open(&config.catalog_path).await?;
    let catalog = Arc::new(catalog);

    // Initialize downloader
    let downloader = ModelDownloader::new(
        catalog.clone(),
        config.download_dir.clone(),
    );
    let downloader = Arc::new(downloader);

    // Hydrate the reputation tracker from on-disk snapshots.
    let reputation = Arc::new(ProviderReputationTracker::new());
    if let Ok(snapshots) = catalog.list_provider_reputation().await {
        reputation.hydrate(snapshots);
    }

    // Create app state
    let state = AppState {
        catalog,
        downloader,
        static_path: config.static_path,
        reputation,
    };

    // Configure CORS
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Build router
    let app = Router::new()
        // API routes
        .route("/api/models", get(list_models_api))
        .route("/api/models/{id}", get(get_model_api))
        .route("/api/models/{id}/ticket", get(get_ticket_api))
        .route("/api/models", post(add_model_api))
        .route("/api/stats", get(stats_api))
        .route("/api/tags", get(tags_api))
        .route("/api/search", get(search_api))
        // Provider reputation API
        .route("/api/providers", get(list_providers_api))
        .route("/api/providers/stats", get(provider_stats_api))
        .route("/api/providers/{node_id}", get(provider_info_api))
        .route("/api/providers/{node_id}/flag", post(flag_provider_api))
        .route("/api/providers/{node_id}/report", post(report_provider_api))
        // Web UI routes
        .route("/", get(index_handler))
        .route("/models", get(models_page_handler))
        .route("/models/{id}", get(model_detail_handler))
        .route("/search", get(search_handler))
        .route("/download/{id}", get(download_handler))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Start server
    let addr = format!("{}:{}", config.host, config.port);
    info!("Starting model catalog server on http://{}", addr);
    
    let listener = tokio::net::TcpListener::bind(&addr).await
        .map_err(|e| ModelCatalogError::ServerError(format!("Failed to bind: {}", e)))?;

    axum::serve(listener, app).await
        .map_err(|e| ModelCatalogError::ServerError(format!("Server error: {}", e)))?;

    Ok(())
}

// ============================================================================
// API Handlers
// ============================================================================

/// List models (API)
async fn list_models_api(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Json<PaginatedModels<ModelManifest>> {
    let filter = ModelFilter {
        model_type: params.model_type.as_ref().map(|t| ModelType::parse(t)),
        tags: params.tags,
        architecture: params.architecture,
        query: params.q,
        offset: Some(params.offset.unwrap_or(0)),
        limit: Some(params.limit.unwrap_or(20)),
        ..Default::default()
    };

    match state.catalog.list(filter).await {
        Ok(result) => Json(result),
        Err(e) => {
            error!("Failed to list models: {}", e);
            Json(PaginatedModels::new(vec![], 0, 0, 20))
        }
    }
}

/// Get single model (API)
async fn get_model_api(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ModelManifest>, StatusCode> {
    state.catalog.get(&id).await
        .map_err(|e| {
            error!("Failed to get model: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })
        .and_then(|opt| opt.map(Json).ok_or(StatusCode::NOT_FOUND))
}

/// Get model ticket (API)
async fn get_ticket_api(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TicketResponse>, StatusCode> {
    // Increment download count
    let _ = state.catalog.increment_downloads(&id).await;
    
    state.catalog.get_ticket(&id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        .and_then(|opt| {
            opt.map(|t| Json(TicketResponse { ticket: t }))
              .ok_or(StatusCode::NOT_FOUND)
        })
}

/// Add model (API)
async fn add_model_api(
    State(state): State<AppState>,
    Json(manifest): Json<ModelManifest>,
) -> Result<Json<AddModelResponse>, StatusCode> {
    manifest.validate().map_err(|e| {
        error!("Validation error: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    state.catalog.add(manifest).await
        .map(|_| Json(AddModelResponse { success: true }))
        .map_err(|e| {
            error!("Failed to add model: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// Get catalog statistics (API)
async fn stats_api(State(state): State<AppState>) -> Json<CatalogStats> {
    match state.catalog.stats().await {
        Ok(stats) => Json(stats),
        Err(e) => {
            error!("Failed to get stats: {}", e);
            Json(CatalogStats::default())
        }
    }
}

/// Get all tags (API)
async fn tags_api(State(state): State<AppState>) -> Json<Vec<(String, u64)>> {
    match state.catalog.get_all_tags().await {
        Ok(tags) => Json(tags),
        Err(e) => {
            error!("Failed to get tags: {}", e);
            Json(vec![])
        }
    }
}

/// Search models (API)
async fn search_api(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Json<Vec<ModelManifest>> {
    match state.catalog.search(&params.q).await {
        Ok(results) => Json(results),
        Err(e) => {
            error!("Search error: {}", e);
            Json(vec![])
        }
    }
}

// ── Provider Reputation API ──────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ProviderListParams {
    /// Filter: "trusted", "blocked", or absent for all.
    tier: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FlagProviderRequest {
    /// "trusted", "blocked", or "neutral".
    flag: String,
}

#[derive(Debug, Deserialize)]
struct ReportProviderRequest {
    /// "spam", "harassment", "impersonation", "phishing",
    /// "misleading_metadata", "license_violation",
    /// "malicious_content", or "other".
    reason: String,
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReportProviderResponse {
    success: bool,
    reports_count: u64,
}

async fn list_providers_api(
    State(state): State<AppState>,
    Query(params): Query<ProviderListParams>,
) -> Json<Vec<ProviderReputation>> {
    let mut all = state.reputation.snapshots();
    if let Some(tier) = params.tier.as_deref() {
        all.retain(|p| match tier {
            "trusted" => {
                matches!(
                    p.trust_tier(),
                    crate::reputation::TrustTier::Trusted
                )
            }
            "blocked" => {
                matches!(
                    p.trust_tier(),
                    crate::reputation::TrustTier::Blocked
                )
            }
            "risky" => {
                matches!(p.trust_tier(), crate::reputation::TrustTier::Risky)
            }
            "neutral" => {
                matches!(p.trust_tier(), crate::reputation::TrustTier::Neutral)
            }
            _ => true,
        });
    }
    all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    Json(all)
}

async fn provider_stats_api(
    State(state): State<AppState>,
) -> Json<crate::reputation::ReputationStats> {
    Json(state.reputation.stats())
}

async fn provider_info_api(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
) -> Result<Json<ProviderReputation>, StatusCode> {
    state
        .reputation
        .get(&node_id)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn flag_provider_api(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    Json(req): Json<FlagProviderRequest>,
) -> Result<Json<ProviderReputation>, (StatusCode, String)> {
    let flag = match req.flag.as_str() {
        "trusted" => TrustFlag::Trusted,
        "blocked" => TrustFlag::Blocked,
        "neutral" => TrustFlag::Neutral,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("unknown flag '{}'", other),
            ))
        }
    };
    state
        .reputation
        .set_trust_flag(&node_id, flag)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    // Persist snapshot
    if let Some(snap) = state.reputation.get(&node_id) {
        let _ = state.catalog.upsert_provider_reputation(&snap).await;
    }
    state
        .reputation
        .get(&node_id)
        .map(Json)
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "missing".to_string()))
}

async fn report_provider_api(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    Json(req): Json<ReportProviderRequest>,
) -> Result<Json<ReportProviderResponse>, (StatusCode, String)> {
    let reason = match req.reason.as_str() {
        "spam" => ReportReason::Spam,
        "harassment" => ReportReason::Harassment,
        "impersonation" => ReportReason::Impersonation,
        "phishing" => ReportReason::Phishing,
        "misleading_metadata" => ReportReason::MisleadingMetadata,
        "license_violation" => ReportReason::LicenseViolation,
        "malicious_content" => ReportReason::MaliciousContent,
        "other" => ReportReason::Other,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("unknown reason '{}'", other),
            ))
        }
    };
    state
        .reputation
        .report_provider(&node_id, reason, 0, req.detail.clone())
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let snap = state.reputation.get(&node_id);
    let reports_count = snap.as_ref().map(|s| s.reports_count).unwrap_or(0);
    // Persist snapshot
    if let Some(snap) = snap {
        let _ = state.catalog.upsert_provider_reputation(&snap).await;
    }
    Ok(Json(ReportProviderResponse {
        success: true,
        reports_count,
    }))
}

// ============================================================================
// Web UI Handlers
// ============================================================================

/// Index page
async fn index_handler() -> Html<String> {
    Html(get_index_html())
}

/// Models listing page
async fn models_page_handler(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Html<String> {
    let filter = ModelFilter {
        model_type: params.model_type.as_ref().map(|t| ModelType::parse(t)),
        tags: params.tags,
        offset: Some(params.offset.unwrap_or(0)),
        limit: Some(params.limit.unwrap_or(20)),
        ..Default::default()
    };

    let models = match state.catalog.list(filter).await {
        Ok(m) => m,
        Err(e) => {
            error!("Failed to list models: {}", e);
            PaginatedModels::new(vec![], 0, 0, 20)
        }
    };

    let stats = state.catalog.stats().await.unwrap_or_default();
    let tags = state.catalog.get_all_tags().await.unwrap_or_default();

    Html(get_models_page_html(&models, &stats, &tags))
}

/// Model detail page
async fn model_detail_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.catalog.get(&id).await {
        Ok(Some(manifest)) => Html(get_model_detail_html(&manifest)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "Model not found").into_response(),
        Err(e) => {
            error!("Failed to get model: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()
        }
    }
}

/// Search page
async fn search_handler(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Html<String> {
    let results = if params.q.is_empty() {
        vec![]
    } else {
        state.catalog.search(&params.q).await.unwrap_or_default()
    };

    Html(get_search_results_html(&params.q, &results))
}

/// Download redirect handler
async fn download_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.catalog.get_ticket(&id).await {
        Ok(Some(ticket)) => {
            let _ = state.catalog.increment_downloads(&id).await;
            Html(format!(r#"<html><head><title>Downloading...</title></head>
<body onload="copyTicket()">
<h2>Starting Download...</h2>
<p>Model ID: {}</p>
<script>
function copyTicket() {{
    navigator.clipboard.writeText('{}').then(() => {{
        alert('Ticket copied! Your A3Net client will start downloading.');
        window.close();
    }});
}}
</script>
</body></html>"#, id, ticket)).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "Model not found").into_response(),
        Err(e) => {
            error!("Download error: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Download failed").into_response()
        }
    }
}

// ============================================================================
// Query Parameters
// ============================================================================

#[derive(Debug, Deserialize)]
struct ListParams {
    #[serde(rename = "type")]
    model_type: Option<String>,
    tags: Option<Vec<String>>,
    architecture: Option<String>,
    q: Option<String>,
    offset: Option<u64>,
    limit: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: String,
}

// ============================================================================
// Response Types
// ============================================================================

#[derive(Serialize)]
struct TicketResponse {
    ticket: String,
}

#[derive(Serialize)]
struct AddModelResponse {
    success: bool,
}

// ============================================================================
// HTML Templates
// ============================================================================

fn get_index_html() -> String {
    r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>A3Net Model Store</title>
    <style>
        * { margin: 0; padding: 0; box-sizing: border-box; }
        body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%); color: #e0e0e0; min-height: 100vh; }
        .container { max-width: 1200px; margin: 0 auto; padding: 2rem; }
        header { text-align: center; padding: 3rem 0; }
        h1 { font-size: 3rem; background: linear-gradient(90deg, #00d9ff, #00ff88); -webkit-background-clip: text; -webkit-text-fill-color: transparent; margin-bottom: 1rem; }
        .subtitle { font-size: 1.2rem; color: #888; }
        .hero { background: rgba(255,255,255,0.05); border-radius: 20px; padding: 3rem; margin: 2rem 0; border: 1px solid rgba(255,255,255,0.1); }
        .search-box { display: flex; gap: 1rem; margin: 2rem 0; }
        .search-input { flex: 1; padding: 1rem 1.5rem; font-size: 1.1rem; border: none; border-radius: 50px; background: rgba(255,255,255,0.1); color: white; outline: none; }
        .search-input::placeholder { color: #888; }
        .btn { padding: 1rem 2rem; font-size: 1rem; border: none; border-radius: 50px; cursor: pointer; font-weight: 600; transition: all 0.3s; }
        .btn-primary { background: linear-gradient(90deg, #00d9ff, #00ff88); color: #1a1a2e; }
        .btn-primary:hover { transform: scale(1.05); box-shadow: 0 0 20px rgba(0,217,255,0.4); }
        .categories { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 1.5rem; margin: 3rem 0; }
        .category-card { background: rgba(255,255,255,0.05); border-radius: 15px; padding: 2rem; text-align: center; border: 1px solid rgba(255,255,255,0.1); transition: all 0.3s; cursor: pointer; }
        .category-card:hover { transform: translateY(-5px); background: rgba(255,255,255,0.08); border-color: #00d9ff; }
        .category-icon { font-size: 3rem; margin-bottom: 1rem; }
        .category-name { font-size: 1.2rem; font-weight: 600; }
        .features { display: grid; grid-template-columns: repeat(auto-fit, minmax(250px, 1fr)); gap: 2rem; margin: 3rem 0; }
        .feature { text-align: center; padding: 1.5rem; }
        .feature-icon { font-size: 2rem; margin-bottom: 1rem; }
        footer { text-align: center; padding: 2rem; color: #666; border-top: 1px solid rgba(255,255,255,0.1); margin-top: 3rem; }
    </style>
</head>
<body>
    <div class="container">
        <header>
            <h1>A3Net Model Store</h1>
            <p class="subtitle">Decentralized AI Model Distribution Network</p>
        </header>

        <div class="hero">
            <h2>Discover, Download, and Share AI Models</h2>
            <p style="color: #888; margin: 1rem 0 2rem;">Powered by P2P technology - fast, secure, and distributed.</p>
            
            <form action="/search" method="get" class="search-box">
                <input type="text" name="q" class="search-input" placeholder="Search models by name, tag, or architecture...">
                <button type="submit" class="btn btn-primary">Search</button>
            </form>
        </div>

        <h3 style="margin: 2rem 0;">Browse Categories</h3>
        <div class="categories">
            <a href="/models?type=llm" class="category-card">
                <div class="category-icon">🤖</div>
                <div class="category-name">LLMs</div>
            </a>
            <a href="/models?type=lora" class="category-card">
                <div class="category-icon">🎨</div>
                <div class="category-name">LoRA Adapters</div>
            </a>
            <a href="/models?type=text_to_image" class="category-card">
                <div class="category-icon">🖼️</div>
                <div class="category-name">Text-to-Image</div>
            </a>
            <a href="/models?type=embedding" class="category-card">
                <div class="category-icon">📊</div>
                <div class="category-name">Embeddings</div>
            </a>
            <a href="/models?type=speech_to_text" class="category-card">
                <div class="category-icon">🎤</div>
                <div class="category-name">Speech AI</div>
            </a>
            <a href="/models?type=vision" class="category-card">
                <div class="category-icon">👁️</div>
                <div class="category-name">Vision Models</div>
            </a>
        </div>

        <h3 style="margin: 2rem 0;">Features</h3>
        <div class="features">
            <div class="feature">
                <div class="feature-icon">⚡</div>
                <h4>P2P Speed</h4>
                <p style="color: #888;">Download from multiple peers simultaneously</p>
            </div>
            <div class="feature">
                <div class="feature-icon">🔒</div>
                <h4>Secure</h4>
                <p style="color: #888;">Bao-verified content integrity</p>
            </div>
            <div class="feature">
                <div class="feature-icon">🌐</div>
                <h4>Decentralized</h4>
                <p style="color: #888;">No central server, fully distributed</p>
            </div>
            <div class="feature">
                <div class="feature-icon">💾</div>
                <h4>Edge Cached</h4>
                <p style="color: #888;">Automatic caching at the network edge</p>
            </div>
        </div>

        <footer>
            <p>A3Net Model Catalog &mdash; Powered by Iroh P2P</p>
            <p style="margin-top: 0.5rem;"><a href="/models" style="color: #00d9ff;">View All Models →</a></p>
        </footer>
    </div>
</body>
</html>"#.to_string()
}

fn get_models_page_html(
    models: &PaginatedModels<ModelManifest>,
    stats: &CatalogStats,
    tags: &[(String, u64)],
) -> String {
    let models_html: String = models.items.iter().map(|m| {
        let tags_html: String = m.tags.iter().take(3).map(|t| 
            format!("<span class=\"tag\">{}</span>", t)
        ).collect::<Vec<_>>().join(" ");
        
        format!(r#"
        <div class="model-card">
            <div class="model-header">
                <h3><a href="/models/{}">{}</a></h3>
                <span class="model-type">{}</span>
            </div>
            <p class="model-desc">{}</p>
            <div class="model-tags">{}</div>
            <div class="model-meta">
                <span>📦 {}</span>
                <span>👤 {}</span>
                <span>⬇️ {} downloads</span>
            </div>
            <a href="/models/{}" class="btn btn-small">View Details</a>
        </div>
        "#,
        m.id, m.name, m.model_type, 
        if m.description.len() > 100 { format!("{}...", &m.description[..100]) } else { m.description.clone() },
        tags_html,
        m.size_display(), m.author, m.download_count,
        m.id
        )
    }).collect::<Vec<_>>().join("\n");

    let tags_html: String = tags.iter().take(20).map(|(tag, count)| {
        format!("<a href=\"/search?q={}\" class=\"tag\">{} ({})</a>", tag, tag, count)
    }).collect::<Vec<_>>().join(" ");

    format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Browse Models - A3Net Model Store</title>
    <style>
        * {{ margin: 0; padding: 0; box-sizing: border-box; }}
        body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%); color: #e0e0e0; min-height: 100vh; }}
        .container {{ max-width: 1200px; margin: 0 auto; padding: 2rem; }}
        header {{ display: flex; justify-content: space-between; align-items: center; padding: 1rem 0; border-bottom: 1px solid rgba(255,255,255,0.1); }}
        .logo {{ font-size: 1.5rem; font-weight: 700; background: linear-gradient(90deg, #00d9ff, #00ff88); -webkit-background-clip: text; -webkit-text-fill-color: transparent; }}
        nav a {{ color: #888; text-decoration: none; margin-left: 2rem; }}
        nav a:hover {{ color: #00d9ff; }}
        h1 {{ font-size: 2rem; margin: 2rem 0 1rem; }}
        .stats {{ display: flex; gap: 2rem; margin-bottom: 2rem; }}
        .stat {{ background: rgba(255,255,255,0.05); padding: 1rem 2rem; border-radius: 10px; }}
        .stat-value {{ font-size: 1.5rem; font-weight: 700; color: #00d9ff; }}
        .stat-label {{ color: #888; font-size: 0.9rem; }}
        .filters {{ display: flex; gap: 1rem; margin-bottom: 2rem; flex-wrap: wrap; }}
        .filter-btn {{ padding: 0.5rem 1rem; background: rgba(255,255,255,0.1); border: none; border-radius: 20px; color: #e0e0e0; cursor: pointer; }}
        .filter-btn:hover, .filter-btn.active {{ background: #00d9ff; color: #1a1a2e; }}
        .models-grid {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(350px, 1fr)); gap: 1.5rem; }}
        .model-card {{ background: rgba(255,255,255,0.05); border-radius: 15px; padding: 1.5rem; border: 1px solid rgba(255,255,255,0.1); transition: all 0.3s; }}
        .model-card:hover {{ transform: translateY(-3px); border-color: rgba(0,217,255,0.3); }}
        .model-header {{ display: flex; justify-content: space-between; align-items: flex-start; margin-bottom: 0.5rem; }}
        .model-header h3 {{ font-size: 1.2rem; }}
        .model-header h3 a {{ color: #e0e0e0; text-decoration: none; }}
        .model-header h3 a:hover {{ color: #00d9ff; }}
        .model-type {{ background: rgba(0,217,255,0.2); color: #00d9ff; padding: 0.2rem 0.5rem; border-radius: 5px; font-size: 0.8rem; }}
        .model-desc {{ color: #888; font-size: 0.9rem; margin: 0.5rem 0; line-height: 1.4; }}
        .model-tags {{ display: flex; gap: 0.5rem; flex-wrap: wrap; margin: 0.5rem 0; }}
        .tag {{ background: rgba(255,255,255,0.1); padding: 0.2rem 0.5rem; border-radius: 5px; font-size: 0.8rem; color: #888; text-decoration: none; }}
        .tag:hover {{ background: rgba(0,217,255,0.2); color: #00d9ff; }}
        .model-meta {{ display: flex; gap: 1rem; font-size: 0.85rem; color: #666; margin: 1rem 0; }}
        .btn {{ display: inline-block; padding: 0.8rem 1.5rem; background: linear-gradient(90deg, #00d9ff, #00ff88); color: #1a1a2e; text-decoration: none; border-radius: 8px; font-weight: 600; border: none; cursor: pointer; }}
        .btn:hover {{ transform: scale(1.02); }}
        .btn-small {{ padding: 0.5rem 1rem; font-size: 0.9rem; }}
        .pagination {{ display: flex; justify-content: center; gap: 0.5rem; margin-top: 2rem; }}
        .pagination a {{ padding: 0.5rem 1rem; background: rgba(255,255,255,0.1); color: #e0e0e0; text-decoration: none; border-radius: 5px; }}
        .pagination a:hover {{ background: #00d9ff; color: #1a1a2e; }}
        .pagination .active {{ background: #00d9ff; color: #1a1a2e; }}
        .sidebar {{ position: fixed; right: 2rem; top: 100px; width: 250px; background: rgba(255,255,255,0.05); border-radius: 15px; padding: 1.5rem; }}
        .sidebar h3 {{ margin-bottom: 1rem; font-size: 1rem; }}
        .empty {{ text-align: center; padding: 4rem; color: #666; }}
    </style>
</head>
<body>
    <div class="container">
        <header>
            <div class="logo">A3Net Models</div>
            <nav>
                <a href="/">Home</a>
                <a href="/models">Browse</a>
            </nav>
        </header>

        <div class="stats">
            <div class="stat">
                <div class="stat-value">{}</div>
                <div class="stat-label">Total Models</div>
            </div>
            <div class="stat">
                <div class="stat-value">{}</div>
                <div class="stat-label">Storage Used</div>
            </div>
        </div>

        <h1>Browse Models</h1>
        
        <div class="models-grid">
            {}
        </div>
        
        {}
        
        <div class="sidebar">
            <h3>Popular Tags</h3>
            <div class="model-tags">{}</div>
        </div>
    </div>
</body>
</html>"#,
    stats.total_models,
    format_size(stats.total_size_bytes),
    models_html,
    if models.total > models.limit {
        format!(r#"
        <div class="pagination">
            <a href="/models?offset={}">&larr; Previous</a>
            <a href="/models?offset={}">Next &rarr;</a>
        </div>
        "#, 
        models.offset.saturating_sub(models.limit), 
        models.offset + models.limit)
    } else { String::new() },
    tags_html
    )
}

fn get_model_detail_html(manifest: &ModelManifest) -> String {
    let tags_html: String = manifest.tags.iter().map(|t| 
        format!("<span class=\"tag\">{}</span>", t)
    ).collect::<Vec<_>>().join(" ");

    format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{} - A3Net Model Store</title>
    <style>
        * {{ margin: 0; padding: 0; box-sizing: border-box; }}
        body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%); color: #e0e0e0; min-height: 100vh; }}
        .container {{ max-width: 900px; margin: 0 auto; padding: 2rem; }}
        header {{ display: flex; justify-content: space-between; align-items: center; padding: 1rem 0; border-bottom: 1px solid rgba(255,255,255,0.1); }}
        .logo {{ font-size: 1.5rem; font-weight: 700; background: linear-gradient(90deg, #00d9ff, #00ff88); -webkit-background-clip: text; -webkit-text-fill-color: transparent; }}
        nav a {{ color: #888; text-decoration: none; margin-left: 2rem; }}
        nav a:hover {{ color: #00d9ff; }}
        .back {{ color: #888; text-decoration: none; margin-bottom: 1rem; display: inline-block; }}
        .back:hover {{ color: #00d9ff; }}
        .model-header {{ background: rgba(255,255,255,0.05); border-radius: 20px; padding: 2rem; margin: 1rem 0; border: 1px solid rgba(255,255,255,0.1); }}
        h1 {{ font-size: 2.5rem; margin-bottom: 0.5rem; }}
        .meta {{ display: flex; gap: 2rem; margin: 1rem 0; color: #888; }}
        .badge {{ background: rgba(0,217,255,0.2); color: #00d9ff; padding: 0.3rem 0.8rem; border-radius: 20px; font-size: 0.9rem; }}
        .description {{ color: #ccc; line-height: 1.6; margin: 1.5rem 0; }}
        .details {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 1rem; margin: 2rem 0; }}
        .detail-card {{ background: rgba(255,255,255,0.05); padding: 1rem; border-radius: 10px; }}
        .detail-label {{ color: #888; font-size: 0.85rem; margin-bottom: 0.3rem; }}
        .detail-value {{ font-weight: 600; }}
        .tags {{ display: flex; gap: 0.5rem; flex-wrap: wrap; margin: 1rem 0; }}
        .tag {{ background: rgba(255,255,255,0.1); padding: 0.3rem 0.8rem; border-radius: 20px; font-size: 0.9rem; }}
        .download-section {{ background: linear-gradient(135deg, rgba(0,217,255,0.1), rgba(0,255,136,0.1)); border-radius: 20px; padding: 2rem; margin: 2rem 0; border: 1px solid rgba(0,217,255,0.3); text-align: center; }}
        .btn {{ display: inline-block; padding: 1rem 3rem; background: linear-gradient(90deg, #00d9ff, #00ff88); color: #1a1a2e; text-decoration: none; border-radius: 50px; font-weight: 700; font-size: 1.2rem; border: none; cursor: pointer; transition: all 0.3s; }}
        .btn:hover {{ transform: scale(1.05); box-shadow: 0 0 30px rgba(0,217,255,0.4); }}
        .hash {{ font-family: monospace; color: #888; font-size: 0.85rem; word-break: break-all; }}
        footer {{ text-align: center; padding: 2rem; color: #666; border-top: 1px solid rgba(255,255,255,0.1); margin-top: 3rem; }}
    </style>
</head>
<body>
    <div class="container">
        <header>
            <div class="logo">A3Net Models</div>
            <nav>
                <a href="/">Home</a>
                <a href="/models">Browse</a>
            </nav>
        </header>

        <a href="/models" class="back">&larr; Back to models</a>

        <div class="model-header">
            <h1>{}</h1>
            <div class="meta">
                <span class="badge">{}</span>
                <span>v{}</span>
                <span>by {}</span>
            </div>
            <div class="tags">{}</div>
            <p class="description">{}</p>
        </div>

        <div class="details">
            <div class="detail-card">
                <div class="detail-label">Size</div>
                <div class="detail-value">{}</div>
            </div>
            <div class="detail-card">
                <div class="detail-label">Architecture</div>
                <div class="detail-value">{}</div>
            </div>
            <div class="detail-card">
                <div class="detail-label">Quantization</div>
                <div class="detail-value">{}</div>
            </div>
            <div class="detail-card">
                <div class="detail-label">License</div>
                <div class="detail-value">{}</div>
            </div>
            <div class="detail-card">
                <div class="detail-label">Downloads</div>
                <div class="detail-value">{}</div>
            </div>
            <div class="detail-card">
                <div class="detail-label">Added</div>
                <div class="detail-value">{}</div>
            </div>
        </div>

        <div class="detail-card" style="margin: 2rem 0;">
            <div class="detail-label">Content Hash (BLAKE3)</div>
            <div class="hash">{}</div>
        </div>

        <div class="download-section">
            <h2>Download Model</h2>
            <p style="color: #888; margin: 1rem 0;">Download via P2P network - fast and secure</p>
            <a href="/download/{}" class="btn">Download Model</a>
        </div>

        <footer>
            <p>A3Net Model Catalog &mdash; Powered by Iroh P2P</p>
        </footer>
    </div>
</body>
</html>"#,
    manifest.name, manifest.name, manifest.model_type, manifest.version, manifest.author,
    tags_html, manifest.description,
    manifest.size_display(), manifest.architecture, manifest.quantization.display_name(),
    manifest.license, manifest.download_count, manifest.created_at.format("%Y-%m-%d").to_string(),
    manifest.content_hash, manifest.id
    )
}

fn get_search_results_html(query: &str, results: &[ModelManifest]) -> String {
    let results_html: String = if results.is_empty() {
        r#"<div class="empty">No models found matching your search.</div>"#.to_string()
    } else {
        results.iter().map(|m| {
            format!(r#"
            <div class="model-card">
                <h3><a href="/models/{}">{}</a></h3>
                <p>{} - {} ({} downloads)</p>
                <a href="/models/{}" class="btn btn-small">View Details</a>
            </div>
            "#, m.id, m.name, m.model_type, m.size_display(), m.download_count, m.id)
        }).collect::<Vec<_>>().join("")
    };

    format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Search: {} - A3Net Model Store</title>
    <style>
        * {{ margin: 0; padding: 0; box-sizing: border-box; }}
        body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%); color: #e0e0e0; min-height: 100vh; }}
        .container {{ max-width: 900px; margin: 0 auto; padding: 2rem; }}
        header {{ display: flex; justify-content: space-between; align-items: center; padding: 1rem 0; border-bottom: 1px solid rgba(255,255,255,0.1); }}
        .logo {{ font-size: 1.5rem; font-weight: 700; background: linear-gradient(90deg, #00d9ff, #00ff88); -webkit-background-clip: text; -webkit-text-fill-color: transparent; }}
        nav a {{ color: #888; text-decoration: none; margin-left: 2rem; }}
        nav a:hover {{ color: #00d9ff; }}
        h1 {{ margin: 2rem 0; }}
        .results-count {{ color: #888; margin-bottom: 2rem; }}
        .results {{ display: flex; flex-direction: column; gap: 1rem; }}
        .model-card {{ background: rgba(255,255,255,0.05); border-radius: 15px; padding: 1.5rem; border: 1px solid rgba(255,255,255,0.1); }}
        .model-card h3 a {{ color: #e0e0e0; text-decoration: none; }}
        .model-card h3 a:hover {{ color: #00d9ff; }}
        .model-card p {{ color: #888; margin: 0.5rem 0; }}
        .btn {{ display: inline-block; padding: 0.8rem 1.5rem; background: linear-gradient(90deg, #00d9ff, #00ff88); color: #1a1a2e; text-decoration: none; border-radius: 8px; font-weight: 600; border: none; cursor: pointer; }}
        .btn-small {{ padding: 0.5rem 1rem; font-size: 0.9rem; }}
        .search-form {{ margin: 2rem 0; }}
        .search-input {{ padding: 1rem; font-size: 1rem; border: none; border-radius: 10px; background: rgba(255,255,255,0.1); color: white; width: 400px; outline: none; }}
    </style>
</head>
<body>
    <div class="container">
        <header>
            <div class="logo">A3Net Models</div>
            <nav>
                <a href="/">Home</a>
                <a href="/models">Browse</a>
            </nav>
        </header>

        <h1>Search Results</h1>
        <p class="results-count">Found {} results for "{}"</p>
        
        <form action="/search" method="get" class="search-form">
            <input type="text" name="q" class="search-input" value="{}" placeholder="Search models...">
            <button type="submit" class="btn">Search</button>
        </form>

        <div class="results">
            {}
        </div>
    </div>
</body>
</html>"#,
    query, results.len(), query, query, results_html
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_params_deserializes_from_query_string() {
        // Use a JSON body since ListParams only impls Deserialize; we
        // exercise serde-renaming of `model_type`.
        let json = r#"{
            "type": "llm",
            "tags": ["chat", "instruct"],
            "architecture": "llama3",
            "q": "foo",
            "offset": 10,
            "limit": 5
        }"#;
        let parsed: ListParams = serde_json::from_str(json).expect("valid json");
        assert_eq!(parsed.model_type.as_deref(), Some("llm"));
        assert_eq!(parsed.tags.as_ref().unwrap().len(), 2);
        assert_eq!(parsed.architecture.as_deref(), Some("llama3"));
        assert_eq!(parsed.q.as_deref(), Some("foo"));
        assert_eq!(parsed.offset, Some(10));
        assert_eq!(parsed.limit, Some(5));
    }

    #[test]
    fn search_params_require_query() {
        let ok = serde_json::from_str::<SearchParams>(r#"{"q": "hi"}"#);
        assert!(ok.is_ok());
        let missing = serde_json::from_str::<SearchParams>(r#"{}"#);
        assert!(missing.is_err());
    }

    #[test]
    fn ticket_response_serializes_ticket_field() {
        let r = TicketResponse { ticket: "iroh://blob/abc".into() };
        let v = serde_json::to_string(&r).unwrap();
        assert!(v.contains("\"ticket\":\"iroh://blob/abc\""));
    }

    #[test]
    fn add_model_response_serializes_success() {
        let r = AddModelResponse { success: true };
        let v = serde_json::to_string(&r).unwrap();
        assert!(v.contains("\"success\":true"));
    }

    #[test]
    fn server_config_defaults_are_sane() {
        let cfg = ServerConfig::default();
        assert_eq!(cfg.port, 8080);
        assert!(cfg.catalog_path.ends_with("model-catalog.db"));
    }

    #[test]
    fn server_config_builder_chain() {
        let cfg = ServerConfig::new("127.0.0.1", 9090)
            .with_catalog_path("/tmp/c.db")
            .with_download_dir("/tmp/dl");
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 9090);
        assert_eq!(cfg.catalog_path, PathBuf::from("/tmp/c.db"));
        assert_eq!(cfg.download_dir, PathBuf::from("/tmp/dl"));
    }

    #[test]
    fn index_html_contains_branding() {
        let html = get_index_html();
        assert!(html.contains("A3Net Model Store"));
        assert!(html.contains("Decentralized"));
    }

    // ── Integration tests via axum Router ───────────────────────

    use crate::catalog::ModelCatalog;
    use crate::downloader::ModelDownloader;
    use crate::manifest::ModelManifest;
    use crate::types::{ModelType, Quantization};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn sample_manifest(name: &str) -> ModelManifest {
        ModelManifest::new(
            name.to_string(),
            "1.0.0".to_string(),
            ModelType::Llm,
            1024 * 1024,
            "a".repeat(64),
            "iroh://blob/abc".to_string(),
            "Test Author".to_string(),
            "A test model".to_string(),
            vec!["test".to_string()],
            "llama3".to_string(),
            Quantization::None,
            "MIT".to_string(),
        )
    }

    fn test_app_state() -> AppState {
        let catalog = Arc::new(ModelCatalog::memory().unwrap());
        let temp_dir = tempfile::tempdir().unwrap();
        let downloader = ModelDownloader::new(
            catalog.clone(),
            temp_dir.path().to_path_buf(),
        );
        AppState {
            catalog,
            downloader: Arc::new(downloader),
            static_path: None,
            reputation: Arc::new(crate::reputation::ProviderReputationTracker::new()),
        }
    }

    async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
        to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    fn build_router(state: AppState) -> axum::Router {
        Router::new()
            .route("/api/models", get(list_models_api))
            .route("/api/models/{id}", get(get_model_api))
            .route("/api/models/{id}/ticket", get(get_ticket_api))
            .route("/api/models", post(add_model_api))
            .route("/api/stats", get(stats_api))
            .route("/api/tags", get(tags_api))
            .route("/api/search", get(search_api))
            .route("/api/providers", get(list_providers_api))
            .route("/api/providers/stats", get(provider_stats_api))
            .route("/api/providers/{node_id}", get(provider_info_api))
            .route(
                "/api/providers/{node_id}/flag",
                post(flag_provider_api),
            )
            .route(
                "/api/providers/{node_id}/report",
                post(report_provider_api),
            )
            .with_state(state)
    }

    #[tokio::test]
    async fn list_models_api_returns_empty_initially() {
        let state = test_app_state();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = body_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["total"], 0);
        assert_eq!(body["items"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn add_model_then_list_returns_it() {
        let state = test_app_state();
        let m = sample_manifest("alpha");
        let m_json = serde_json::to_string(&m).unwrap();
        let app = build_router(state.clone());

        // POST /api/models
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/models")
                    .header("content-type", "application/json")
                    .body(Body::from(m_json))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // GET /api/models
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = body_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body["total"].as_u64().unwrap() >= 1);
        let items = body["items"].as_array().unwrap();
        assert!(items
            .iter()
            .any(|v| v["name"] == "alpha"));
    }

    #[tokio::test]
    async fn get_unknown_model_returns_404() {
        let state = test_app_state();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/models/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn add_model_rejects_invalid_manifest() {
        let state = test_app_state();
        let app = build_router(state);
        // Empty name should fail validation
        let mut m = sample_manifest("ignored");
        m.name = "".into();
        let body = serde_json::to_string(&m).unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/models")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn stats_api_returns_zero_initially() {
        let state = test_app_state();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = body_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["total_models"], 0);
    }

    #[tokio::test]
    async fn search_api_finds_match() {
        let state = test_app_state();
        let m = sample_manifest("llama3-chat");
        state.catalog.add(m).await.unwrap();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/search?q=llama")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = body_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = body.as_array().unwrap();
        assert!(!arr.is_empty());
    }

    // ── Provider reputation API integration tests ──────────────

    #[tokio::test]
    async fn list_providers_starts_empty() {
        let state = test_app_state();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = body_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn provider_stats_starts_zero() {
        let state = test_app_state();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/providers/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = body_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["total_providers"], 0);
    }

    #[tokio::test]
    async fn flag_provider_then_get_info() {
        let state = test_app_state();
        let node = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        // Flag the provider
        let flag_body = serde_json::json!({ "flag": "trusted" }).to_string();
        let app = build_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&format!("/api/providers/{}/flag", node))
                    .header("content-type", "application/json")
                    .body(Body::from(flag_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Read it back
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(&format!("/api/providers/{}", node))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = body_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["node_id"], node);
        assert_eq!(body["trust_flag"], "trusted");
    }

    #[tokio::test]
    async fn flag_provider_with_invalid_flag_returns_400() {
        let state = test_app_state();
        let node = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let app = build_router(state);
        let body = serde_json::json!({ "flag": "bogus" }).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&format!("/api/providers/{}/flag", node))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn report_provider_increments_count() {
        let state = test_app_state();
        let node = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let app = build_router(state.clone());
        let body = serde_json::json!({
            "reason": "spam",
            "detail": "duplicate uploads"
        })
        .to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&format!("/api/providers/{}/report", node))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = body_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["success"], true);
        assert_eq!(body["reports_count"], 1);
    }

    #[tokio::test]
    async fn report_provider_with_invalid_reason_returns_400() {
        let state = test_app_state();
        let node = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let app = build_router(state);
        let body = serde_json::json!({ "reason": "bogus_reason" }).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&format!("/api/providers/{}/report", node))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn provider_info_returns_404_when_unknown() {
        let state = test_app_state();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/providers/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_providers_filters_by_tier() {
        let state = test_app_state();
        let trusted = "1111111111111111111111111111111111111111111111111111111111111111";
        let blocked = "2222222222222222222222222222222222222222222222222222222222222222";
        state
            .reputation
            .set_trust_flag(trusted, crate::reputation::TrustFlag::Trusted)
            .unwrap();
        state
            .reputation
            .set_trust_flag(blocked, crate::reputation::TrustFlag::Blocked)
            .unwrap();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/providers?tier=trusted")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = body_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = body.as_array().unwrap();
        // Only the trusted provider should be returned
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["node_id"], trusted);
    }

    #[tokio::test]
    async fn ticket_api_returns_ticket_for_known_model() {
        let state = test_app_state();
        let m = sample_manifest("ticket-test");
        state.catalog.add(m.clone()).await.unwrap();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(&format!("/api/models/{}/ticket", m.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = body_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body["ticket"]
            .as_str()
            .unwrap()
            .starts_with("iroh://"));
    }

    #[tokio::test]
    async fn ticket_api_returns_404_for_unknown_model() {
        let state = test_app_state();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/models/no-such-id/ticket")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
