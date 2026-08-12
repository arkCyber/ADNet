//! HTTP request handler for the IPFS-compatible Gateway.

use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;

use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

use adnet_blobstore::BlobStore;
use adnet_types::ContentHash;

use crate::config::GatewayConfig;
use crate::dag::DagService;
use crate::dht::DhtService;
use crate::ipns::IpnService;
use crate::metrics::GatewayMetrics;
use crate::pin::PinService;

/// Errors that can occur in the gateway handler.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("content not found: {0}")]
    NotFound(String),

    #[error("invalid path: {0}")]
    InvalidPath(String),

    #[error("invalid CID: {0}")]
    InvalidCid(String),

    #[error("request timeout")]
    Timeout,

    #[error("payload too large: {0} bytes")]
    PayloadTooLarge(u64),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("method not allowed: {0}")]
    MethodNotAllowed(String),

    #[error("CORS not allowed")]
    CorsNotAllowed,
}

impl GatewayError {
    /// Convert to HTTP status code.
    pub fn status_code(&self) -> StatusCode {
        match self {
            GatewayError::NotFound(_) => StatusCode::NOT_FOUND,
            GatewayError::InvalidPath(_) => StatusCode::BAD_REQUEST,
            GatewayError::InvalidCid(_) => StatusCode::BAD_REQUEST,
            GatewayError::Timeout => StatusCode::GATEWAY_TIMEOUT,
            GatewayError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            GatewayError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            GatewayError::MethodNotAllowed(_) => StatusCode::METHOD_NOT_ALLOWED,
            GatewayError::CorsNotAllowed => StatusCode::FORBIDDEN,
        }
    }

    /// Create a JSON error response body.
    pub fn to_json_response(&self) -> serde_json::Value {
        serde_json::json!({
            "Message": self.to_string(),
            "Code": self.status_code().as_u16(),
            "Type": "error"
        })
    }
}

/// Parsed IPFS/IPNS path.
#[derive(Debug, Clone)]
pub struct IpfsPath {
    /// Whether this is an IPNS path.
    pub is_ipns: bool,
    /// The root CID or name.
    pub root: String,
    /// Path segments within the DAG.
    pub segments: Vec<String>,
}

impl IpfsPath {
    /// Parse an IPFS or IPNS path.
    pub fn parse(path: &str) -> Result<Self, GatewayError> {
        let path = path.trim_start_matches('/');
        let parts: Vec<&str> = path.splitn(2, '/').collect();

        if parts.is_empty() || parts[0].is_empty() {
            return Err(GatewayError::InvalidPath("empty path".to_string()));
        }

        let (is_ipns, root, remainder) = if parts[0] == "ipns" {
            if parts.len() < 2 {
                return Err(GatewayError::InvalidPath("missing IPNS name".to_string()));
            }
            // Split the rest into root and remainder
            let rest = parts[1];
            let rest_parts: Vec<&str> = rest.splitn(2, '/').collect();
            let root = rest_parts[0].to_string();
            let remainder = rest_parts.get(1).copied();
            (true, root, remainder)
        } else if parts[0] == "ipfs" {
            if parts.len() < 2 {
                return Err(GatewayError::InvalidPath("missing CID".to_string()));
            }
            // Split the rest into root and remainder
            let rest = parts[1];
            let rest_parts: Vec<&str> = rest.splitn(2, '/').collect();
            let root = rest_parts[0].to_string();
            let remainder = rest_parts.get(1).copied();
            (false, root, remainder)
        } else {
            // Assume CID if it looks like one
            let rest_parts: Vec<&str> = parts[0].splitn(2, '/').collect();
            let root = rest_parts[0].to_string();
            let remainder = rest_parts.get(1).copied();
            (false, root, remainder)
        };

        let segments: Vec<String> = remainder
            .map(|r| r.split('/').filter(|s| !s.is_empty()).map(String::from).collect())
            .unwrap_or_default();

        Ok(Self {
            is_ipns,
            root,
            segments,
        })
    }

    /// Get the CID as a content hash.
    /// 
    /// Note: For IPNS paths, this will return an error because IPNS
    /// requires async resolution. Use `ipns_service.resolve()` instead
    /// for IPNS paths.
    pub fn to_content_hash(&self) -> Result<ContentHash, GatewayError> {
        if self.is_ipns {
            // IPNS requires async resolution via ipns_service.resolve()
            return Err(GatewayError::InvalidPath(
                "IPNS paths require async resolution via /ipns/ endpoint".to_string()
            ));
        }

        // Try to parse as hex hash
        ContentHash::from_hex(&self.root)
            .map_err(|_| GatewayError::InvalidCid(format!("invalid CID format: {}", self.root)))
    }
}

/// IPFS-compatible response types.
#[derive(Debug, Clone)]
pub enum IpfsResponse {
    /// Raw bytes response.
    Bytes(Bytes),
    /// JSON response.
    Json(serde_json::Value),
    /// Empty response.
    Empty,
}

/// The main gateway request handler.
pub struct GatewayHandler {
    config: GatewayConfig,
    blob_store: Arc<BlobStore>,
    dag_service: Arc<DagService>,
    pin_service: Arc<PinService>,
    dht_service: Arc<DhtService>,
    ipns_service: Arc<IpnService>,
    metrics: GatewayMetrics,
}

impl GatewayHandler {
    /// Create a new gateway handler.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: GatewayConfig,
        blob_store: Arc<BlobStore>,
        dag_service: Arc<DagService>,
        pin_service: Arc<PinService>,
        dht_service: Arc<DhtService>,
        ipns_service: Arc<IpnService>,
    ) -> Self {
        Self {
            config,
            blob_store,
            dag_service,
            pin_service,
            dht_service,
            ipns_service,
            metrics: GatewayMetrics::default(),
        }
    }

    /// Handle an incoming HTTP request.
    pub async fn handle(
        &self,
        req: Request<Incoming>,
    ) -> Result<Response<Full<Bytes>>, GatewayError> {
        let start = SystemTime::now();
        self.metrics.requests_total.inc();

        // Extract path and method
        let path = req.uri().path().to_string();
        let method = req.method().clone();

        debug!("Gateway request: {} {}", method, path);

        // Route the request
        let response = match (method.as_str(), path.as_str()) {
            // CORS preflight
            ("OPTIONS", _) => self.handle_cors_preflight(&req).await,

            // Health and readiness endpoints
            ("GET", "/health") => self.handle_health().await,
            ("GET", "/ready") => self.handle_ready().await,
            ("GET", "/live") => self.handle_liveness().await,

            // Gateway endpoints
            ("GET", p) if p.starts_with("/ipfs/") => {
                self.handle_get_ipfs(p).await
            }
            ("GET", p) if p.starts_with("/ipns/") => {
                self.handle_get_ipns(p).await
            }
            ("GET", "/") | ("GET", "") => {
                self.handle_root().await
            }

            // API v0 endpoints
            ("POST", "/api/v0/dag/put") => {
                self.handle_dag_put(req).await
            }
            ("POST", "/api/v0/dag/get") => {
                self.handle_dag_get(req).await
            }
            ("POST", "/api/v0/dag/resolve") => {
                self.handle_dag_resolve(req).await
            }
            ("POST", "/api/v0/cat") => {
                self.handle_cat(req).await
            }
            ("POST", "/api/v0/block/put") => {
                self.handle_block_put(req).await
            }
            ("POST", "/api/v0/block/get") => {
                self.handle_block_get(req).await
            }
            ("POST", "/api/v0/block/stat") => {
                self.handle_block_stat(req).await
            }
            ("POST", "/api/v0/pin/add") => {
                self.handle_pin_add(req).await
            }
            ("POST", "/api/v0/pin/rm") => {
                self.handle_pin_remove(req).await
            }
            ("POST", "/api/v0/pin/ls") => {
                self.handle_pin_list(req).await
            }
            ("GET", "/api/v0/id") => {
                self.handle_node_id().await
            }
            ("GET", "/api/v0/version") => {
                self.handle_version().await
            }

            // Object API endpoints
            ("POST", "/api/v0/object/get") => {
                self.handle_object_get(req).await
            }
            ("POST", "/api/v0/object/put") => {
                self.handle_object_put(req).await
            }
            ("POST", "/api/v0/object/stat") => {
                self.handle_object_stat(req).await
            }
            ("POST", "/api/v0/object/new") => {
                self.handle_object_new(req).await
            }

            // Refs API endpoints
            ("POST", "/api/v0/refs") => {
                self.handle_refs(req).await
            }
            ("POST", "/api/v0/refs/local") => {
                self.handle_refs_local(req).await
            }

            // Stats API endpoints
            ("POST", "/api/v0/stats/stat") => {
                self.handle_stats_repo(req).await
            }
            ("POST", "/api/v0/stats/bw") => {
                self.handle_stats_bw(req).await
            }

            // DHT API endpoints
            ("POST", "/api/v0/dht/findprovs") => {
                self.handle_dht_findprovs(req).await
            }
            ("POST", "/api/v0/dht/provide") => {
                self.handle_dht_provide(req).await
            }
            ("POST", "/api/v0/dht/findprovs-local") => {
                self.handle_dht_findprovs_local(req).await
            }

            // IPNS API endpoints
            ("POST", "/api/v0/name/publish") => {
                self.handle_ipns_publish(req).await
            }
            ("POST", "/api/v0/name/resolve") => {
                self.handle_ipns_resolve(req).await
            }
            ("GET", "/api/v0/name/local") => {
                self.handle_ipns_local().await
            }
            ("POST", "/api/v0/name/export") => {
                self.handle_ipns_export(req).await
            }
            ("POST", "/api/v0/name/import") => {
                self.handle_ipns_import(req).await
            }

            // Additional block endpoints
            ("POST", "/api/v0/block/rm") => {
                self.handle_block_rm(req).await
            }

            // Swarm API endpoints
            ("GET", "/api/v0/swarm/peers") => {
                self.handle_swarm_peers(req).await
            }
            ("POST", "/api/v0/swarm/connect") => {
                self.handle_swarm_connect(req).await
            }
            ("POST", "/api/v0/swarm/disconnect") => {
                self.handle_swarm_disconnect(req).await
            }
            ("GET", "/api/v0/swarm/addrs") => {
                self.handle_swarm_addrs(req).await
            }
            ("GET", "/api/v0/swarm/addrs/local") => {
                self.handle_swarm_addrs_local().await
            }
            ("GET", "/api/v0/swarm/filters") => {
                self.handle_swarm_filters().await
            }
            ("POST", "/api/v0/swarm/filters/add") => {
                self.handle_swarm_filters_add(req).await
            }
            ("POST", "/api/v0/swarm/filters/rm") => {
                self.handle_swarm_filters_rm(req).await
            }

            // Repo API endpoints
            ("POST", "/api/v0/repo/gc") => {
                self.handle_repo_gc(req).await
            }
            ("POST", "/api/v0/repo/verify") => {
                self.handle_repo_verify(req).await
            }
            ("GET", "/api/v0/repo/stat") => {
                self.handle_repo_stat().await
            }
            ("GET", "/api/v0/repo/ls") => {
                self.handle_repo_ls().await
            }

            // Pin verify endpoint
            ("POST", "/api/v0/pin/verify") => {
                self.handle_pin_verify(req).await
            }

            _ => Err(GatewayError::NotFound(format!(
                "route not found: {} {}",
                method, path
            ))),
        };

        // Record metrics
        let elapsed = start.elapsed();
        match &response {
            Ok(r) => {
                if r.status().as_u16() == 200 {
                    self.metrics.requests_success.inc();
                }
                debug!("Gateway response: {} in {:?}", r.status().as_u16(), elapsed);
            }
            Err(e) => {
                self.metrics.requests_error.inc();
                if e.status_code() == StatusCode::NOT_FOUND {
                    self.metrics.requests_not_found.inc();
                }
                error!("Gateway error: {} in {:?}", e, elapsed);
            }
        }

        response
    }

    /// Handle GET request for /ipfs/<cid> path.
    async fn handle_get_ipfs(&self, path: &str) -> Result<Response<Full<Bytes>>, GatewayError> {
        let parsed = IpfsPath::parse(path)?;

        if parsed.is_ipns {
            return Err(GatewayError::InvalidPath("expected /ipfs/, got /ipns/".to_string()));
        }

        let hash = parsed.to_content_hash()?;
        debug!("IPFS resolve: {} -> {}", path, hash.as_hex());

        // Check if we have the content
        if !self.blob_store.has_complete(&hash) {
            debug!("IPFS content not found: {}", hash.as_hex());
            return Err(GatewayError::NotFound(format!(
                "content not found: {}",
                hash.as_hex()
            )));
        }

        // If there are path segments, resolve them through the DAG
        if parsed.segments.is_empty() {
            // Return the raw content
            let data = self.blob_store.get_sync(&hash)
                .ok_or_else(|| GatewayError::NotFound(hash.as_hex().to_string()))?;

            self.metrics.bytes_served.inc_by(data.len() as u64);
            debug!("IPFS served {} bytes for {}", data.len(), hash.as_hex());

            return self.ok_response(data.into(), "application/octet-stream");
        }

        // Resolve path through DAG
        let data = self.dag_service
            .resolve_path(&hash, &parsed.segments)
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        self.metrics.bytes_served.inc_by(data.len() as u64);
        debug!("IPFS served {} bytes for {} (path: {:?})", data.len(), hash.as_hex(), parsed.segments);

        // Try to detect content type
        let content_type = detect_content_type(&data, parsed.segments.last());
        self.ok_response(data.into(), &content_type)
    }

    /// Handle GET request for /ipns/<name> path.
    async fn handle_get_ipns(&self, path: &str) -> Result<Response<Full<Bytes>>, GatewayError> {
        if !self.config.enable_ipns {
            debug!("IPNS disabled, rejecting request for {}", path);
            return Err(GatewayError::NotFound("IPNS disabled".to_string()));
        }

        // Parse the IPNS name
        let mut name = path.trim_start_matches("/ipns/").trim_start_matches('/').to_string();
        debug!("IPNS resolution starting for: {}", name);

        // Maximum depth to prevent infinite loops
        const MAX_DEPTH: usize = 10;

        for i in 0..MAX_DEPTH {
            // Resolve via IPNS service
            let resolved = self.ipns_service.resolve(&name).await
                .map_err(|e| {
                    warn!("IPNS resolution failed for {}: {}", name, e);
                    GatewayError::Internal(e.to_string())
                })?;

            debug!("IPNS resolved {} -> {} (depth {}/{})", name, resolved.path, i + 1, MAX_DEPTH);

            if resolved.path.starts_with("/ipfs/") {
                let ipfs_path = resolved.path.trim_start_matches("/ipfs/");
                return self.handle_get_ipfs(&format!("/ipfs/{}", ipfs_path)).await;
            } else if resolved.path.starts_with("/ipns/") {
                // Chain resolution
                name = resolved.path.trim_start_matches("/ipns/").to_string();
            } else {
                // Return the resolved value directly
                return self.ok_response(Bytes::from(resolved.path), "text/plain");
            }
        }

        warn!("IPNS resolution exceeded maximum depth for: {}", path);
        Err(GatewayError::InvalidPath("IPNS resolution exceeded maximum depth".to_string()))
    }

    /// Handle root path request.
    async fn handle_root(&self) -> Result<Response<Full<Bytes>>, GatewayError> {
        if let Some(ref redirect) = self.config.root_redirect {
            return self.redirect_response(redirect);
        }

        let html = r#"<!DOCTYPE html>
<html>
<head>
    <title>ADNet Gateway</title>
    <style>
        body { font-family: system-ui, sans-serif; max-width: 800px; margin: 50px auto; padding: 20px; }
        h1 { color: #333; }
        code { background: #f5f5f5; padding: 2px 6px; border-radius: 3px; }
        pre { background: #f5f5f5; padding: 15px; border-radius: 5px; overflow-x: auto; }
        .endpoint { margin: 15px 0; }
    </style>
</head>
<body>
    <h1>ADNet IPFS Gateway</h1>
    <p>Welcome to the ADNet HTTP Gateway. This gateway provides IPFS-compatible access to content on the ADNet network.</p>

    <h2>Gateway Endpoints</h2>
    <div class="endpoint">
        <h3>Retrieve Content</h3>
        <pre>GET /ipfs/{cid}
GET /ipfs/{cid}/{path}</pre>
        <p>Retrieve content by CID, optionally following a path within a DAG.</p>
    </div>

    <div class="endpoint">
        <h3>IPNS Resolution</h3>
        <pre>GET /ipns/{name}</pre>
        <p>Resolve an IPNS name to content (requires DHT).</p>
    </div>

    <h2>API v0 Endpoints</h2>
    <div class="endpoint">
        <h3>DAG Operations</h3>
        <pre>POST /api/v0/dag/put - Add a DAG node
POST /api/v0/dag/get - Get a DAG node
POST /api/v0/dag/resolve - Resolve a DAG path</pre>
    </div>

    <div class="endpoint">
        <h3>Block Operations</h3>
        <pre>POST /api/v0/block/put - Add a block
POST /api/v0/block/get - Get a block
POST /api/v0/block/stat - Get block stats</pre>
    </div>

    <div class="endpoint">
        <h3>Pin Operations</h3>
        <pre>POST /api/v0/pin/add - Pin content
POST /api/v0/pin/rm - Unpin content
POST /api/v0/pin/ls - List pins</pre>
    </div>

    <div class="endpoint">
        <h3>Content Retrieval</h3>
        <pre>POST /api/v0/cat - Get content (equivalent to /ipfs/{cid})</pre>
    </div>

    <h2>Node Info</h2>
    <pre>GET /api/v0/id - Node ID and info
GET /api/v0/version - Gateway version</pre>
</body>
</html>"#;

        self.html_response(html)
    }

    /// Handle DAG put operation.
    async fn handle_dag_put(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        if !self.config.writable {
            warn!("DAG put rejected: gateway is read-only");
            return Err(GatewayError::MethodNotAllowed("gateway is read-only".to_string()));
        }

        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let result = self.dag_service
            .put(&body)
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        info!("DAG put: size={} bytes, cid={}", result.size, result.cid);

        let response = serde_json::json!({
            "Cid": {
                "/": result.cid
            },
            "Size": result.size
        });

        self.json_response(response)
    }

    /// Handle DAG get operation.
    async fn handle_dag_get(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        // Parse arg parameter
        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        // Parse path parameter
        let path: Vec<String> = parse_multipart_arg_array(&body, "path")
            .unwrap_or_default();

        // Resolve the CID
        let hash = ContentHash::from_hex(arg.trim_start_matches('/'))
            .map_err(|_| GatewayError::InvalidCid(arg.to_string()))?;

        debug!("DAG get: cid={}, path={:?}", hash.as_hex(), path);

        let result = self.dag_service
            .get(&hash, &path)
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        Response::builder()
            .status(200)
            .header("Content-Type", "application/json")
            .header("X-Content-Type-Options", "nosniff")
            .body(Full::new(Bytes::from(result.data)))
            .map_err(|e| GatewayError::Internal(e.to_string()))
    }

    /// Handle DAG resolve operation.
    async fn handle_dag_resolve(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        let result = self.dag_service
            .resolve(&arg)
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let response = serde_json::json!({
            "Cid": {
                "/": result.cid
            },
            "RemPath": result.path
        });

        self.json_response(response)
    }

    /// Handle cat operation (equivalent to /ipfs/<cid>).
    async fn handle_cat(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        let path = IpfsPath::parse(&arg)?;
        self.handle_get_ipfs(&format!("/ipfs/{}", path.root)).await
    }

    /// Handle block put operation.
    async fn handle_block_put(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        if !self.config.writable {
            warn!("Block put rejected: gateway is read-only");
            return Err(GatewayError::MethodNotAllowed("gateway is read-only".to_string()));
        }

        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let hash = self.blob_store.put_bytes_sync(&body)
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .0;

        info!("Block put: {} bytes, hash={}", body.len(), hash.as_hex());

        let response = serde_json::json!({
            "Key": hash.as_hex(),
            "Size": body.len()
        });

        self.json_response(response)
    }

    /// Handle block get operation.
    async fn handle_block_get(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        let hash = ContentHash::from_hex(arg.trim_start_matches('/'))
            .map_err(|_| GatewayError::InvalidCid(arg.to_string()))?;

        debug!("Block get: hash={}", hash.as_hex());

        let data = self.blob_store.get_sync(&hash)
            .ok_or_else(|| GatewayError::NotFound(hash.as_hex().to_string()))?;

        self.ok_response(data.into(), "application/octet-stream")
    }

    /// Handle block stat operation.
    async fn handle_block_stat(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        let hash = ContentHash::from_hex(arg.trim_start_matches('/'))
            .map_err(|_| GatewayError::InvalidCid(arg.to_string()))?;

        let (size, _) = self.blob_store.meta(&hash)
            .map_err(|_| GatewayError::NotFound(hash.as_hex().to_string()))?;

        let response = serde_json::json!({
            "Key": hash.as_hex(),
            "Size": size,
            "Cid": hash.as_hex()
        });

        self.json_response(response)
    }

    /// Handle pin add operation.
    async fn handle_pin_add(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        if !self.config.writable {
            warn!("Pin add rejected: gateway is read-only");
            return Err(GatewayError::MethodNotAllowed("gateway is read-only".to_string()));
        }

        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        // Parse recursive parameter
        let recursive = parse_multipart_arg(&body, "recursive")
            .map(|v| v != "false")
            .unwrap_or(true);

        let hash = ContentHash::from_hex(arg.trim_start_matches('/'))
            .map_err(|_| GatewayError::InvalidCid(arg.to_string()))?;

        info!("Pin add: cid={}, recursive={}", hash.as_hex(), recursive);

        self.pin_service
            .add_pin(&hash, recursive)
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let response = serde_json::json!({
            "Pins": [hash.as_hex()]
        });

        self.json_response(response)
    }

    /// Handle pin remove operation.
    async fn handle_pin_remove(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        if !self.config.writable {
            warn!("Pin remove rejected: gateway is read-only");
            return Err(GatewayError::MethodNotAllowed("gateway is read-only".to_string()));
        }

        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        let hash = ContentHash::from_hex(arg.trim_start_matches('/'))
            .map_err(|_| GatewayError::InvalidCid(arg.to_string()))?;

        info!("Pin remove: cid={}", hash.as_hex());

        self.pin_service
            .remove_pin(&hash)
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let response = serde_json::json!({
            "Pins": [hash.as_hex()]
        });

        self.json_response(response)
    }

    /// Handle pin list operation.
    async fn handle_pin_list(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        // Check for specific CID filter
        let filter_hash = parse_multipart_arg(&body, "arg")
            .and_then(|arg| ContentHash::from_hex(arg.trim_start_matches('/')).ok());

        let pins = self.pin_service
            .list_pins(filter_hash.as_ref())
            .await;

        let mut pins_json = serde_json::Map::new();
        for pin in pins {
            pins_json.insert(
                pin.cid.as_hex().to_string(),
                serde_json::json!({
                    "Type": pin.pin_type.to_string()
                })
            );
        }

        let response = serde_json::json!({
            "Keys": pins_json
        });

        self.json_response(response)
    }

    /// Handle node ID request.
    async fn handle_node_id(&self) -> Result<Response<Full<Bytes>>, GatewayError> {
        // For now, return a placeholder
        let response = serde_json::json!({
            "ID": "adnet-gateway",
            "PublicKey": "",
            "Addresses": [],
            "AgentVersion": "adnet-gateway/0.1.0",
            "ProtocolVersion": "ipfs/0.1.0"
        });

        self.json_response(response)
    }

    /// Handle version request.
    async fn handle_version(&self) -> Result<Response<Full<Bytes>>, GatewayError> {
        let response = serde_json::json!({
            "Version": "0.1.0",
            "Commit": "",
            "Repo": "10",
            "System": "adnet-gateway",
            "Golang": "1.21"
        });

        self.json_response(response)
    }

    /// Handle CORS preflight request.
    async fn handle_cors_preflight(&self, req: &Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let origin = req.headers()
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("*");

        debug!("CORS preflight from origin: {}", origin);

        if !self.config.is_cors_allowed(origin) {
            warn!("CORS rejected for origin: {}", origin);
            return Err(GatewayError::CorsNotAllowed);
        }

        debug!("CORS allowed for origin: {}", origin);

        Response::builder()
            .status(204)
            .header("Access-Control-Allow-Origin", origin)
            .header("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS")
            .header("Access-Control-Allow-Headers", "Content-Type, Authorization")
            .header("Access-Control-Max-Age", "86400")
            .body(Full::new(Bytes::new()))
            .map_err(|e| GatewayError::Internal(e.to_string()))
    }

    // ─────────────────────────────────────────────────────────────────
    // Health, Readiness, and Liveness Endpoints (Kubernetes-compatible)
    // ─────────────────────────────────────────────────────────────────

    /// Handle GET /health - detailed health check with component status.
    /// Returns 200 if the gateway is healthy, 503 if any component is unhealthy.
    async fn handle_health(&self) -> Result<Response<Full<Bytes>>, GatewayError> {
        use std::time::SystemTime;

        let blob_store_status = if self.blob_store.is_healthy() {
            "healthy"
        } else {
            "unhealthy"
        };

        let dht_peers = self.dht_service.num_peers().await;
        let dht_status = if dht_peers > 0 { "healthy" } else { "degraded" };

        let ipns_records = self.ipns_service.list_local().await.len();
        let ipns_status = "healthy";

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let response = serde_json::json!({
            "status": "healthy",
            "timestamp": timestamp,
            "version": env!("CARGO_PKG_VERSION"),
            "components": {
                "blob_store": {
                    "status": blob_store_status
                },
                "dht": {
                    "status": dht_status,
                    "peers": dht_peers
                },
                "ipns": {
                    "status": ipns_status,
                    "local_records": ipns_records
                }
            }
        });

        self.json_response(response)
    }

    /// Handle GET /ready - readiness probe for Kubernetes.
    /// Returns 200 if ready to serve traffic, 503 if still initializing.
    async fn handle_ready(&self) -> Result<Response<Full<Bytes>>, GatewayError> {
        use std::time::SystemTime;

        let blob_store_ready = self.blob_store.is_healthy();
        
        // Gateway is ready if blob store is ready
        let is_ready = blob_store_ready;

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let status = if is_ready { "ready" } else { "not_ready" };

        let response = serde_json::json!({
            "status": status,
            "timestamp": timestamp,
            "ready": is_ready
        });

        if is_ready {
            self.json_response(response)
        } else {
            // Return 503 Service Unavailable when not ready
            Response::builder()
                .status(503)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(serde_json::to_string(&response).unwrap_or_default())))
                .map_err(|e| GatewayError::Internal(e.to_string()))
        }
    }

    /// Handle GET /live - liveness probe for Kubernetes.
    /// Returns 200 if the process is alive, regardless of component status.
    async fn handle_liveness(&self) -> Result<Response<Full<Bytes>>, GatewayError> {
        use std::time::SystemTime;

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let response = serde_json::json!({
            "status": "alive",
            "timestamp": timestamp
        });

        // Always return 200 for liveness - this just confirms the process is running
        Response::builder()
            .status(200)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(serde_json::to_string(&response).unwrap_or_default())))
            .map_err(|e| GatewayError::Internal(e.to_string()))
    }

    // ─────────────────────────────────────────────────────────────────
    // Response builders
    // ─────────────────────────────────────────────────────────────────

    fn ok_response(&self, body: Bytes, content_type: &str) -> Result<Response<Full<Bytes>>, GatewayError> {
        let mut builder = Response::builder()
            .status(200)
            .header("Content-Type", content_type)
            .header("X-Content-Type-Options", "nosniff")
            .header("Cache-Control", &self.config.cache_control);

        if self.config.cors_enabled {
            builder = builder.header("Access-Control-Allow-Origin", "*");
        }

        builder
            .body(Full::new(body))
            .map_err(|e| GatewayError::Internal(e.to_string()))
    }

    fn json_response(&self, value: serde_json::Value) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = serde_json::to_vec(&value)
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .into();

        let mut builder = Response::builder()
            .status(200)
            .header("Content-Type", "application/json")
            .header("X-Content-Type-Options", "nosniff");

        if self.config.cors_enabled {
            builder = builder.header("Access-Control-Allow-Origin", "*");
        }

        builder
            .body(Full::new(body))
            .map_err(|e| GatewayError::Internal(e.to_string()))
    }

    fn html_response(&self, html: &str) -> Result<Response<Full<Bytes>>, GatewayError> {
        self.ok_response(Bytes::from(html.to_string()), "text/html; charset=utf-8")
    }

    fn redirect_response(&self, location: &str) -> Result<Response<Full<Bytes>>, GatewayError> {
        Response::builder()
            .status(301)
            .header("Location", location)
            .body(Full::new(Bytes::new()))
            .map_err(|e| GatewayError::Internal(e.to_string()))
    }

    #[allow(dead_code)]
    fn error_response(&self, error: &GatewayError) -> Result<Response<Full<Bytes>>, GatewayError> {
        let status = error.status_code();
        let body = serde_json::to_vec(&error.to_json_response())
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .into();

        let mut builder = Response::builder()
            .status(status)
            .header("Content-Type", "application/json")
            .header("X-Content-Type-Options", "nosniff");

        if self.config.cors_enabled {
            builder = builder.header("Access-Control-Allow-Origin", "*");
        }

        builder
            .body(Full::new(body))
            .map_err(|e| GatewayError::Internal(e.to_string()))
    }

    // ─────────────────────────────────────────────────────────────────
    // Object API handlers
    // ─────────────────────────────────────────────────────────────────

    /// Handle object/get operation.
    async fn handle_object_get(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        let hash = ContentHash::from_hex(arg.trim_start_matches('/'))
            .map_err(|_| GatewayError::InvalidCid(arg.to_string()))?;

        let data = self.blob_store.get_sync(&hash)
            .ok_or_else(|| GatewayError::NotFound(hash.as_hex().to_string()))?;

        // Try to decode as DAG node
        if let Ok(node) = serde_cbor::from_slice::<crate::dag::DagNode>(&data) {
            let response = serde_json::json!({
                "Data": node.data.unwrap_or_default(),
                "Links": node.links
            });
            return self.json_response(response);
        }

        // Return raw data if not a DAG node
        self.ok_response(data.into(), "application/octet-stream")
    }

    /// Handle object/put operation.
    async fn handle_object_put(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        if !self.config.writable {
            return Err(GatewayError::MethodNotAllowed("gateway is read-only".to_string()));
        }

        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let node = crate::dag::DagNode {
            data: Some(body.to_vec()),
            links: Vec::new(),
            unixfs: None,
        };

        let encoded = serde_cbor::to_vec(&node)
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let (hash, _) = self.blob_store.put_bytes_sync(&encoded)
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let response = serde_json::json!({
            "Hash": hash.as_hex()
        });

        self.json_response(response)
    }

    /// Handle object/stat operation.
    async fn handle_object_stat(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        let hash = ContentHash::from_hex(arg.trim_start_matches('/'))
            .map_err(|_| GatewayError::InvalidCid(arg.to_string()))?;

        let data = self.blob_store.get_sync(&hash)
            .ok_or_else(|| GatewayError::NotFound(hash.as_hex().to_string()))?;

        let node: crate::dag::DagNode = serde_cbor::from_slice(&data)
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let links_size: u64 = node.links.iter().map(|l| l.size).sum();
        let data_size = node.data.as_ref().map(|d| d.len() as u64).unwrap_or(0);
        let block_size = data.len() as u64;

        let response = serde_json::json!({
            "Hash": hash.as_hex(),
            "NumLinks": node.links.len(),
            "BlockSize": block_size,
            "LinksSize": links_size,
            "DataSize": data_size,
            "CumulativeSize": block_size + links_size
        });

        self.json_response(response)
    }

    /// Handle object/new operation.
    async fn handle_object_new(&self, _req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        if !self.config.writable {
            return Err(GatewayError::MethodNotAllowed("gateway is read-only".to_string()));
        }

        let node = crate::dag::DagNode {
            data: Some(Vec::new()),
            links: Vec::new(),
            unixfs: Some(crate::dag::UnixFsNode::Directory { mtime: None }),
        };

        let encoded = serde_cbor::to_vec(&node)
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let (hash, _) = self.blob_store.put_bytes_sync(&encoded)
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let response = serde_json::json!({
            "Hash": hash.as_hex()
        });

        self.json_response(response)
    }

    // ─────────────────────────────────────────────────────────────────
    // Refs API handlers
    // ─────────────────────────────────────────────────────────────────

    /// Handle refs operation (non-recursive, single level).
    async fn handle_refs(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        let hash = ContentHash::from_hex(arg.trim_start_matches('/'))
            .map_err(|_| GatewayError::InvalidCid(arg.to_string()))?;

        if !self.blob_store.has_complete(&hash) {
            return Err(GatewayError::NotFound(hash.as_hex().to_string()));
        }

        let data = self.blob_store.get_sync(&hash)
            .ok_or_else(|| GatewayError::NotFound(hash.as_hex().to_string()))?;

        let node: crate::dag::DagNode = serde_cbor::from_slice(&data)
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        // Build newline-separated output for direct links only
        let output: String = node.links.iter()
            .map(|link| format!("{} {}", hash.as_hex(), link.hash))
            .collect::<Vec<_>>()
            .join("\n");

        self.ok_response(output.into(), "text/plain")
    }

    /// Handle refs/local operation.
    async fn handle_refs_local(&self, _req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let hashes = self.blob_store.list_complete()
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let output: String = hashes.iter()
            .map(|h| h.as_hex().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        self.ok_response(output.into(), "text/plain")
    }

    // ─────────────────────────────────────────────────────────────────
    // Stats API handlers
    // ─────────────────────────────────────────────────────────────────

    /// Handle stats/stat operation.
    async fn handle_stats_repo(&self, _req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let repo_size = self.blob_store.total_size().unwrap_or(0);
        let num_objects = self.blob_store.list_complete()
            .map(|v| v.len() as u64)
            .unwrap_or(0);

        let response = serde_json::json!({
            "RepoSize": repo_size,
            "StorageMax": 0u64,
            "NumObjects": num_objects,
            "RepoPath": self.blob_store.data_dir().to_string_lossy(),
            "Version": "10"
        });

        self.json_response(response)
    }

    /// Handle stats/bw operation.
    async fn handle_stats_bw(&self, _req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        // Placeholder for bandwidth stats
        let response = serde_json::json!({
            "TotalIn": 0u64,
            "TotalOut": 0u64,
            "RateIn": 0f64,
            "RateOut": 0f64
        });

        self.json_response(response)
    }

    // ─────────────────────────────────────────────────────────────────
    // Additional block handlers
    // ─────────────────────────────────────────────────────────────────

    /// Handle block/rm operation.
    async fn handle_block_rm(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        if !self.config.writable {
            return Err(GatewayError::MethodNotAllowed("gateway is read-only".to_string()));
        }

        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        let hash = ContentHash::from_hex(arg.trim_start_matches('/'))
            .map_err(|_| GatewayError::InvalidCid(arg.to_string()))?;

        let removed = self.blob_store.remove(&hash)
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let response = serde_json::json!({
            "Hash": hash.as_hex(),
            "Removed": removed
        });

        self.json_response(response)
    }

    // ─────────────────────────────────────────────────────────────────
    // IPNS handlers
    // ─────────────────────────────────────────────────────────────────

    /// Handle name/publish operation.
    /// Publishes an IPNS name with the given value.
    async fn handle_ipns_publish(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        if !self.config.writable {
            return Err(GatewayError::MethodNotAllowed("gateway is read-only".to_string()));
        }

        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        // Parse TTL if provided
        let ttl = parse_multipart_arg(&body, "ttl")
            .and_then(|v| v.parse::<u64>().ok())
            .map(std::time::Duration::from_secs);

        // Parse lifetime if provided
        let lifetime = parse_multipart_arg(&body, "lifetime")
            .and_then(|v| v.parse::<u64>().ok())
            .map(std::time::Duration::from_secs);

        // Publish via IPNS service
        let result = self.ipns_service.publish(arg, ttl.or(lifetime)).await
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let response = serde_json::json!({
            "Name": result.name,
            "Value": result.value
        });

        self.json_response(response)
    }

    /// Handle name/resolve operation.
    /// Resolves an IPNS name to its current value.
    async fn handle_ipns_resolve(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        // Parse recursive flag
        let recursive = parse_multipart_arg(&body, "recursive")
            .map(|v| v == "true")
            .unwrap_or(true);

        // Resolve via IPNS service
        let mut current = arg;
        let mut visited = std::collections::HashSet::new();
        const MAX_DEPTH: usize = 32;

        for _ in 0..MAX_DEPTH {
            if visited.contains(&current) {
                return Err(GatewayError::InvalidPath("IPNS resolution loop detected".to_string()));
            }
            visited.insert(current.clone());

            let result = self.ipns_service.resolve(&current).await
                .map_err(|e| GatewayError::Internal(e.to_string()))?;

            if result.path.starts_with("/ipfs/") {
                let response = serde_json::json!({
                    "Path": result.path
                });
                return self.json_response(response);
            } else if result.path.starts_with("/ipns/") {
                if !recursive {
                    return Err(GatewayError::InvalidPath("recursive resolution disabled".to_string()));
                }
                current = result.path.trim_start_matches("/ipns/").to_string();
            } else {
                let response = serde_json::json!({
                    "Path": result.path
                });
                return self.json_response(response);
            }
        }

        Err(GatewayError::InvalidPath("IPNS resolution exceeded maximum depth".to_string()))
    }

    /// Handle name/local operation.
    /// Lists all local IPNS records.
    async fn handle_ipns_local(&self) -> Result<Response<Full<Bytes>>, GatewayError> {
        let records = self.ipns_service.list_local().await;

        let local_info: Vec<_> = records.iter().map(|r| {
            serde_json::json!({
                "Name": r.name,
                "Value": r.value,
                "Sequence": r.sequence,
                "TTL": r.ttl_secs,
                "Validity": r.validity
            })
        }).collect();

        let response = serde_json::json!({
            "Keys": local_info
        });

        self.json_response(response)
    }

    /// Handle name/export operation.
    /// Exports an IPNS record for backup/sharing.
    async fn handle_ipns_export(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        if let Some(record) = self.ipns_service.get_record(&arg).await {
            let record_json = serde_json::to_vec_pretty(&serde_json::json!({
                "name": record.name,
                "value": record.value,
                "sequence": record.sequence,
                "ttl_secs": record.ttl_secs,
                "created": record.created,
                "expires": record.expires,
                "validity": record.validity
            })).map_err(|e| GatewayError::Internal(e.to_string()))?;

            return self.ok_response(record_json.into(), "application/json");
        }

        Err(GatewayError::NotFound(format!("IPNS record not found: {}", arg)))
    }

    /// Handle name/import operation.
    /// Imports an IPNS record from backup.
    async fn handle_ipns_import(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        if !self.config.writable {
            return Err(GatewayError::MethodNotAllowed("gateway is read-only".to_string()));
        }

        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let data_str = String::from_utf8_lossy(&body);

        // Parse the record JSON
        let record: serde_json::Value = serde_json::from_str(&data_str)
            .map_err(|e| GatewayError::InvalidPath(format!("invalid record JSON: {}", e)))?;

        let value = record["value"].as_str()
            .ok_or_else(|| GatewayError::InvalidPath("missing 'value' field".to_string()))?;
        let ttl_secs = record["ttl_secs"].as_u64().unwrap_or(86400);

        let result = self.ipns_service.publish(
            value.to_string(),
            Some(std::time::Duration::from_secs(ttl_secs))
        ).await
        .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let response = serde_json::json!({
            "Name": result.name,
            "Value": result.value,
            "Imported": true
        });

        self.json_response(response)
    }

    // ─────────────────────────────────────────────────────────────────
    // DHT handlers
    // ─────────────────────────────────────────────────────────────────

    /// Handle dht/findprovs operation.
    async fn handle_dht_findprovs(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        let result = self.dht_service.find_providers(&arg).await
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let response = serde_json::json!({
            "Cid": result.cid,
            "Providers": result.providers
        });

        self.json_response(response)
    }

    /// Handle dht/provide operation.
    async fn handle_dht_provide(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        if !self.config.writable {
            return Err(GatewayError::MethodNotAllowed("gateway is read-only".to_string()));
        }

        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter".to_string()))?;

        let result = self.dht_service.provide(&arg).await
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let response = serde_json::json!({
            "Cid": result.cid
        });

        self.json_response(response)
    }

    /// Handle dht/findprovs-local operation.
    async fn handle_dht_findprovs_local(&self, _req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let providers = self.dht_service.list_local_providers().await;

        let provider_infos: Vec<_> = providers.iter().map(|(cid, infos)| {
            serde_json::json!({
                "Cid": cid,
                "Providers": infos
            })
        }).collect();

        let response = serde_json::json!({
            "Providers": provider_infos
        });

        self.json_response(response)
    }

    // ─────────────────────────────────────────────────────────────────
    // Swarm API handlers
    // ─────────────────────────────────────────────────────────────────

    /// Handle swarm/peers operation.
    /// Lists currently connected peers.
    async fn handle_swarm_peers(&self, _req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let peers: Vec<serde_json::Value> = Vec::new();

        let response = serde_json::json!({
            "Peers": peers
        });

        self.json_response(response)
    }

    /// Handle swarm/connect operation.
    /// Dials a peer by multiaddr.
    async fn handle_swarm_connect(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let addr = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter (multiaddr)".to_string()))?;

        // Placeholder: actual connection would be handled by libp2p
        let response = serde_json::json!({
            "Strings": [format!("connect {}: success (not implemented)", addr)]
        });

        self.json_response(response)
    }

    /// Handle swarm/disconnect operation.
    /// Closes connection to a peer.
    async fn handle_swarm_disconnect(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let peer_id = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter (peer id)".to_string()))?;

        // Placeholder: actual disconnection would be handled by libp2p
        let response = serde_json::json!({
            "Strings": [format!("disconnect {}: success (not implemented)", peer_id)]
        });

        self.json_response(response)
    }

    /// Handle swarm/addrs operation.
    /// Lists addresses of connected peers.
    async fn handle_swarm_addrs(&self, _req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let addrs: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

        let response = serde_json::json!({
            "Addrs": addrs
        });

        self.json_response(response)
    }

    /// Handle swarm/addrs/local operation.
    /// Lists our own listen addresses.
    async fn handle_swarm_addrs_local(&self) -> Result<Response<Full<Bytes>>, GatewayError> {
        let response = serde_json::json!({
            "Strings": []
        });

        self.json_response(response)
    }

    /// Handle swarm/filters operation.
    /// Lists connection filters.
    async fn handle_swarm_filters(&self) -> Result<Response<Full<Bytes>>, GatewayError> {
        let response = serde_json::json!({
            "Strings": []
        });

        self.json_response(response)
    }

    /// Handle swarm/filters/add operation.
    /// Adds a connection filter.
    async fn handle_swarm_filters_add(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let filter = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter (multiaddr filter)".to_string()))?;

        let response = serde_json::json!({
            "Strings": [format!("filter added: {}", filter)]
        });

        self.json_response(response)
    }

    /// Handle swarm/filters/rm operation.
    /// Removes a connection filter.
    async fn handle_swarm_filters_rm(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let filter = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter (multiaddr filter)".to_string()))?;

        let response = serde_json::json!({
            "Strings": [format!("filter removed: {}", filter)]
        });

        self.json_response(response)
    }

    // ─────────────────────────────────────────────────────────────────
    // Repo API handlers
    // ─────────────────────────────────────────────────────────────────

    /// Handle repo/gc operation.
    /// Garbage collects orphaned blocks.
    async fn handle_repo_gc(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        if !self.config.writable {
            return Err(GatewayError::MethodNotAllowed("gateway is read-only".to_string()));
        }

        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let dry_run = parse_multipart_arg(&body, "dry-run")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let gc_service = crate::pin::GcService::new(
            self.blob_store.clone(),
            self.pin_service.clone()
        );

        if dry_run {
            let count = gc_service.dry_run().await
                .map_err(|e| GatewayError::Internal(e.to_string()))?;
            let response = serde_json::json!({
                "Keys": [],
                "Meta": {
                    "GCDryRun": true,
                    "Count": count
                }
            });
            self.json_response(response)
        } else {
            let result = gc_service.run().await
                .map_err(|e| GatewayError::Internal(e.to_string()))?;
            let response = serde_json::json!({
                "Keys": [],
                "Meta": {
                    "GCDryRun": false,
                    "Removed": result.removed,
                    "Failed": result.failed
                }
            });
            self.json_response(response)
        }
    }

    /// Handle repo/verify operation.
    /// Verifies all blocks in the local store.
    async fn handle_repo_verify(&self, _req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let all_cids = self.blob_store.list_complete()
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let mut verified = 0u64;
        let mut failed = 0u64;
        let mut failed_keys: Vec<String> = Vec::new();

        for cid in &all_cids {
            match self.blob_store.meta(cid) {
                Ok((size, _)) => {
                    if size > 0 && self.blob_store.has_complete(cid) {
                        verified += 1;
                    } else {
                        failed += 1;
                        failed_keys.push(cid.as_hex().to_string());
                    }
                }
                Err(_) => {
                    failed += 1;
                    failed_keys.push(cid.as_hex().to_string());
                }
            }
        }

        let response = serde_json::json!({
            "Key": "",
            "Errors": failed_keys,
            "Success": failed == 0,
            "Stats": {
                "Verified": verified,
                "Failed": failed,
                "Total": all_cids.len() as u64
            }
        });

        self.json_response(response)
    }

    /// Handle repo/stat operation.
    /// Returns repository statistics.
    async fn handle_repo_stat(&self) -> Result<Response<Full<Bytes>>, GatewayError> {
        let repo_size = self.blob_store.total_size().unwrap_or(0);
        let num_objects = self.blob_store.list_complete()
            .map(|v| v.len() as u64)
            .unwrap_or(0);
        let pin_stats = self.pin_service.stats().await;

        let response = serde_json::json!({
            "RepoSize": repo_size,
            "StorageMax": 0u64,
            "NumObjects": num_objects,
            "RepoPath": self.blob_store.data_dir().to_string_lossy(),
            "Version": "10",
            "PinStats": {
                "Direct": pin_stats.direct,
                "Recursive": pin_stats.recursive,
                "Indirect": pin_stats.indirect,
                "Total": pin_stats.total
            }
        });

        self.json_response(response)
    }

    /// Handle repo/ls operation.
    /// Lists all blocks in the local store.
    async fn handle_repo_ls(&self) -> Result<Response<Full<Bytes>>, GatewayError> {
        let all_cids = self.blob_store.list_complete()
            .map_err(|e| GatewayError::Internal(e.to_string()))?;

        let mut objects: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();
        for cid in &all_cids {
            if let Ok((size, _chunk_count)) = self.blob_store.meta(cid) {
                objects.insert(
                    cid.as_hex().to_string(),
                    serde_json::json!({
                        "Hash": cid.as_hex(),
                        "Size": size,
                        "SizeStat": size,
                        "CumulativeSize": size,
                        "Links": []
                    })
                );
            }
        }

        let response = serde_json::json!({
            "Objects": objects
        });

        self.json_response(response)
    }

    // ─────────────────────────────────────────────────────────────────
    // Pin verify handler
    // ─────────────────────────────────────────────────────────────────

    /// Handle pin/verify operation.
    /// Verifies that a pinned CID still exists in the local store.
    async fn handle_pin_verify(&self, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, GatewayError> {
        let body = req.into_body()
            .collect()
            .await
            .map_err(|e| GatewayError::Internal(e.to_string()))?
            .to_bytes();

        let arg = parse_multipart_arg(&body, "arg")
            .ok_or_else(|| GatewayError::InvalidPath("missing 'arg' parameter (CID)".to_string()))?;

        let cid = ContentHash::from_hex(arg.trim_start_matches('/'))
            .map_err(|_| GatewayError::InvalidCid(arg.to_string()))?;

        let pins = self.pin_service.list_pins(Some(&cid)).await;

        if pins.is_empty() {
            let response = serde_json::json!({
                "Cid": {
                    "/": cid.as_hex()
                },
                "PinStatus": {
                    "Error": {
                        "Code": "礁_NOT_PINNED",
                        "Message": "Not pinned or not found"
                    }
                },
                "Bad": false
            });
            return self.json_response(response);
        }

        let pin_info = &pins[0];
        let exists = self.blob_store.has_complete(&cid);

        let response = serde_json::json!({
            "Cid": {
                "/": cid.as_hex()
            },
            "PinStatus": {
                "Cid": {
                    "/": cid.as_hex()
                },
                "Type": pin_info.pin_type.to_string(),
                "Err": if exists {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String("content not found in store".to_string())
                }
            },
            "Bad": !exists
        });

        self.json_response(response)
    }
}

// ─────────────────────────────────────────────────────────────────
// Helper functions
// ─────────────────────────────────────────────────────────────────

/// Parse multipart form data argument.
fn parse_multipart_arg(body: &[u8], name: &str) -> Option<String> {
    // Simple multipart parser for IPFS API format
    // Format: "arg=<value>" or "arg=\\\"<value>\\\""
    let search = format!("{}=", name);
    if let Some(pos) = body.windows(search.len()).position(|w| w == search.as_bytes()) {
        let start = pos + search.len();
        let end = body[start..].iter().position(|&b| b == b'&' || b == b'\r' || b == b'\n').unwrap_or(body.len() - start);
        let value = &body[start..start + end];

        // Remove leading/trailing whitespace
        let mut start = 0;
        let mut end = value.len();
        while start < value.len() && (value[start] == b' ' || value[start] == b'\t') {
            start += 1;
        }
        while end > start && (value[end - 1] == b' ' || value[end - 1] == b'\t') {
            end -= 1;
        }
        let value = &value[start..end];

        // Remove quotes if present
        if (value.starts_with(b"\"") && value.ends_with(b"\"")) ||
           (value.starts_with(b"'") && value.ends_with(b"'")) {
            return Some(String::from_utf8_lossy(&value[1..value.len()-1]).to_string());
        }
        Some(String::from_utf8_lossy(value).to_string())
    } else {
        None
    }
}
fn parse_multipart_arg_array(body: &[u8], name: &str) -> Option<Vec<String>> {
    // For path parameters, just return empty for now
    // A full implementation would need proper multipart parsing
    let _ = (body, name);
    None
}

/// Detect content type from data.
fn detect_content_type(data: &[u8], path: Option<&String>) -> String {
    // Check for common magic bytes
    if data.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        return "image/png".to_string();
    }
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg".to_string();
    }
    if data.starts_with(&[0x47, 0x49, 0x46]) {
        return "image/gif".to_string();
    }
    if data.starts_with(&[0x25, 0x50, 0x44, 0x46]) {
        return "application/pdf".to_string();
    }
    if data.starts_with(&[0x50, 0x4B, 0x03, 0x04]) {
        return "application/zip".to_string();
    }
    if data.starts_with(b"<?xml") || data.starts_with(b"<") {
        if path.map(|p| p.ends_with(".svg")).unwrap_or(false) {
            return "image/svg+xml".to_string();
        }
        return "application/xml".to_string();
    }

    // Check if it looks like text
    if data.iter().take(1000).all(|&b| b.is_ascii_graphic() || b.is_ascii_whitespace()) {
        if path.map(|p| p.ends_with(".json")).unwrap_or(false) {
            return "application/json".to_string();
        }
        if path.map(|p| p.ends_with(".html")).unwrap_or(false) {
            return "text/html; charset=utf-8".to_string();
        }
        if path.map(|p| p.ends_with(".css")).unwrap_or(false) {
            return "text/css".to_string();
        }
        if path.map(|p| p.ends_with(".js")).unwrap_or(false) {
            return "application/javascript".to_string();
        }
        if path.map(|p| p.ends_with(".txt")).unwrap_or(false) {
            return "text/plain; charset=utf-8".to_string();
        }
        return "text/plain".to_string();
    }

    "application/octet-stream".to_string()
}

/// Start the gateway server.
pub async fn start_gateway(
    config: GatewayConfig,
    blob_store: Arc<BlobStore>,
    dag_service: Arc<DagService>,
    pin_service: Arc<PinService>,
    dht_service: Arc<DhtService>,
    ipns_service: Arc<IpnService>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr: SocketAddr = config.bind_addr.parse()?;
    let listener = TcpListener::bind(addr).await?;

    info!("Starting ADNet IPFS Gateway on {}", addr);

    // Build a StatsService so the gateway and the IPC surface share
    // a single counter store. We hold an Arc so the IPC handler can
    // outlive this function.
    let stats_service = Arc::new(crate::stats::StatsService::new(
        blob_store.clone(),
        format!("in-memory://{}", addr),
        config.max_response_size,
    ));

    // Spin up the internal IPC server if the configuration asks for
    // it. The handle is intentionally dropped at the end of this
    // scope — `start_gateway` only returns when the HTTP listener
    // exits, so the IPC handle is leaked during normal operation.
    // Callers that want the IPC handle should use
    // [`crate::ipc::GatewayIpcService::start`] directly and pass the
    // handle around.
    let _ipc_handle = if let Some(socket_path) = config.ipc_socket.clone() {
        let ipc_service = crate::ipc::GatewayIpcService::new(
            blob_store.clone(),
            dag_service.clone(),
            pin_service.clone(),
            dht_service.clone(),
            ipns_service.clone(),
            stats_service.clone(),
        );
        let cfg = crate::ipc::GatewayIpcConfig {
            socket_path,
            notification_capacity: 64,
        };
        match ipc_service.start(cfg).await {
            Ok(h) => Some(h),
            Err(e) => {
                error!("failed to bind internal IPC server: {e}");
                None
            }
        }
    } else {
        None
    };

    let handler = Arc::new(GatewayHandler::new(
        config,
        blob_store,
        dag_service,
        pin_service,
        dht_service,
        ipns_service,
    ));

    loop {
        let (conn, _remote_addr) = listener.accept().await?;
        let handler = handler.clone();

        tokio::spawn(async move {
            let io = TokioIo::new(conn);
            let service = hyper::service::service_fn(move |req| {
                let handler = handler.clone();
                async move {
                    let result: Result<Response<Full<Bytes>>, GatewayError> = handler.handle(req).await;
                    match result {
                        Ok(resp) => Ok::<_, std::convert::Infallible>(resp),
                        Err(e) => {
                            let body = serde_json::to_vec(&e.to_json_response())
                                .unwrap_or_default()
                                .into();
                            Ok(Response::builder()
                                .status(e.status_code())
                                .header("Content-Type", "application/json")
                                .body(Full::new(body))
                                .unwrap())
                        }
                    }
                }
            });

            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service)
                .await
            {
                error!("Gateway connection error: {}", e);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipfs_path_parsing() {
        let path = IpfsPath::parse("/ipfs/QmHash").unwrap();
        assert!(!path.is_ipns);
        assert_eq!(path.root, "QmHash");
        assert!(path.segments.is_empty());

        let path = IpfsPath::parse("/ipfs/QmHash/some/path").unwrap();
        assert_eq!(path.root, "QmHash");
        assert_eq!(path.segments, vec!["some", "path"]);

        let path = IpfsPath::parse("/ipns/example.com").unwrap();
        assert!(path.is_ipns);
        assert_eq!(path.root, "example.com");
    }

    #[test]
    fn test_content_type_detection() {
        assert_eq!(detect_content_type(b"hello world", None), "text/plain");
        assert_eq!(detect_content_type(&[0x89, 0x50, 0x4E, 0x47], None), "image/png");
        assert_eq!(detect_content_type(&[0xFF, 0xD8, 0xFF], None), "image/jpeg");
        assert_eq!(detect_content_type(&[0x47, 0x49, 0x46], None), "image/gif");
        assert_eq!(detect_content_type(&[0x25, 0x50, 0x44, 0x46], None), "application/pdf");
    }

    #[tokio::test]
    async fn test_ipns_path_parsing() {
        let path = IpfsPath::parse("/ipns/k51qzi5m93nua").unwrap();
        assert!(path.is_ipns);
        assert_eq!(path.root, "k51qzi5m93nua");
        assert!(path.segments.is_empty());

        let path = IpfsPath::parse("/ipns/k51qzi5m93nua/some/path").unwrap();
        assert!(path.is_ipns);
        assert_eq!(path.root, "k51qzi5m93nua");
        assert_eq!(path.segments, vec!["some", "path"]);
    }
}
