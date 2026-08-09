//! Relay HTTP server — proxies `/exodus-mesh/fetch` requests across NAT.
//!
//! Mirrors `wan_relay_server.rs` from
//! `Exodus@src-backup/.../wan_relay_server.rs`. Validates the upstream URL
//! (path must start with `/blobs/`, no `..`, host/port must be sane) and
//! forwards the request via `reqwest` with a 1h timeout and at most 3
//! redirects.

use std::net::SocketAddr;
use std::time::Duration;

use axum::{
    Router,
    extract::Query,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::config::RelayServerInfo;

/// Running relay server handle.
pub struct RelayServerHandle {
    pub port: u16,
    pub bind_host: String,
    pub base_url: String,
    shutdown_tx: watch::Sender<bool>,
}

impl RelayServerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    pub fn info(&self) -> RelayServerInfo {
        RelayServerInfo {
            running: true,
            port: self.port,
            base_url: self.base_url.clone(),
            bind_host: self.bind_host.clone(),
        }
    }
}

impl Drop for RelayServerHandle {
    fn drop(&mut self) {
        // Best-effort shutdown signal — ensures the listener task
        // exits even if the user drops the handle without calling
        // `.shutdown()` explicitly.
        let _ = self.shutdown_tx.send(true);
    }
}

/// Query parameters for the proxy endpoint.
#[derive(Debug, Deserialize)]
pub struct MeshFetchQuery {
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// Axum-based relay server.
pub struct RelayServer;

impl RelayServer {
    /// Spawn a relay on `bind_host:port`. Returns a handle that owns the
    /// listener; drop / `.shutdown()` to stop.
    ///
    /// Pass [`crate::billing::BillingMode::Disabled`] (the default) for a
    /// pure forward proxy. To enable the optional billing endpoints pass
    /// [`crate::billing::BillingMode::Enabled`] with the relay's signing
    /// wallet — the `billing` cargo feature must be enabled at build time
    /// for the `Enabled` variant to exist.
    pub async fn start(
        bind_host: &str,
        port: u16,
        #[cfg_attr(not(feature = "billing"), allow(unused_variables))]
        billing_mode: crate::billing::BillingMode,
    ) -> Result<RelayServerHandle, String> {
        let addr: SocketAddr = format!("{bind_host}:{port}")
            .parse()
            .map_err(|e| format!("Invalid relay bind address: {e}"))?;

        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| format!("WAN relay bind failed on {addr}: {e}"))?;
        let bound = listener.local_addr().map_err(|e| e.to_string())?;
        let port = bound.port();
        let bind_host = bound.ip().to_string();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let router = Router::new()
            .route("/health", get(health_handler))
            .route("/exodus-mesh/fetch", get(mesh_fetch_handler));

        #[cfg(feature = "billing")]
        let router = {
            // Use `nest` rather than `merge` so the nested router keeps
            // its own State type (axum 0.7 `merge` requires both routers
            // to share the same State). We only nest when billing is on.
            if matches!(billing_mode, crate::billing::BillingMode::Enabled { .. }) {
                let sub = billing_mode.routes();
                router.nest("/relay/billing", sub)
            } else {
                router
            }
        };

        // axum 0.7 needs a single concrete `State` type. `()` is the
        // empty state; handlers that need shared state declare their own
        // sub-router (see `billing::BillingMode::routes`).
        let router = router.with_state(());

        let shutdown_rx_serve = shutdown_rx.clone();
        tokio::spawn(async move {
            let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
                let mut rx = shutdown_rx_serve;
                loop {
                    if *rx.borrow() {
                        break;
                    }
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            });
            if let Err(e) = serve.await {
                warn!("WAN relay server stopped: {e}");
            }
        });

        let base_url = format!("http://{bind_host}:{port}");
        info!("ADNet WAN relay listening on {base_url}");
        Ok(RelayServerHandle {
            port,
            bind_host,
            base_url,
            shutdown_tx,
        })
    }
}

async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn mesh_fetch_handler(Query(q): Query<MeshFetchQuery>) -> Response {
    match proxy_mesh_fetch(&q).await {
        Ok(resp) => resp,
        Err((status, msg)) => (status, msg).into_response(),
    }
}

async fn proxy_mesh_fetch(q: &MeshFetchQuery) -> Result<Response, (StatusCode, String)> {
    validate_mesh_fetch_query(q)?;
    let path = normalize_mesh_path(&q.path);
    let upstream = format!("http://{}:{}{}", q.host.trim(), q.port, path);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3600))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let resp = client.get(&upstream).send().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("Upstream fetch failed: {e}"),
        )
    })?;

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = resp.headers().clone();
    let bytes = resp.bytes().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("Upstream body read failed: {e}"),
        )
    })?;

    let mut out = Response::builder().status(status);
    if let Some(ct) = headers.get(header::CONTENT_TYPE)
        && let Ok(v) = axum::http::HeaderValue::from_bytes(ct.as_bytes())
    {
        out = out.header(header::CONTENT_TYPE, v);
    }
    if let Some(cl) = headers.get(header::CONTENT_LENGTH)
        && let Ok(v) = axum::http::HeaderValue::from_bytes(cl.as_bytes())
    {
        out = out.header(header::CONTENT_LENGTH, v);
    }
    out.body(axum::body::Body::from(bytes))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn validate_mesh_fetch_query(q: &MeshFetchQuery) -> Result<(), (StatusCode, String)> {
    let host = q.host.trim();
    if host.is_empty() || host.len() > 253 {
        return Err((StatusCode::BAD_REQUEST, "Invalid host".into()));
    }
    if q.port == 0 {
        return Err((StatusCode::BAD_REQUEST, "Invalid port".into()));
    }
    let path = q.path.trim();
    if !path.starts_with("/blobs/") {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must start with /blobs/".into(),
        ));
    }
    // Reject any traversal segment: `..` either as a path component
    // (preceded by `/` or at the start) or as a backslash variant.
    // `..foo` and `foo..` are valid filenames, so we only block the
    // exact `..` segment.
    if path.split('/').any(|seg| seg == "..") || path.contains("\\") {
        return Err((StatusCode::BAD_REQUEST, "Invalid path".into()));
    }
    // Cap the path length so a hostile peer can't blow up our URL.
    if path.len() > 1024 {
        return Err((StatusCode::BAD_REQUEST, "path too long".into()));
    }
    Ok(())
}

fn normalize_mesh_path(path: &str) -> String {
    let p = path.trim();
    if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{p}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_mesh_path() {
        let ok = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 7878,
            path: "/blobs/abc/meta".into(),
        };
        assert!(validate_mesh_fetch_query(&ok).is_ok());

        let bad = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 7878,
            path: "/etc/passwd".into(),
        };
        assert!(validate_mesh_fetch_query(&bad).is_err());
    }

    #[test]
    fn normalizes_path() {
        assert_eq!(normalize_mesh_path("blobs/x"), "/blobs/x");
        assert_eq!(normalize_mesh_path("/blobs/x"), "/blobs/x");
    }

    #[test]
    fn validate_empty_host() {
        let query = MeshFetchQuery {
            host: "".into(),
            port: 7878,
            path: "/blobs/abc".into(),
        };
        assert!(validate_mesh_fetch_query(&query).is_err());
    }

    #[test]
    fn validate_oversized_host() {
        let query = MeshFetchQuery {
            host: "a".repeat(254),
            port: 7878,
            path: "/blobs/abc".into(),
        };
        assert!(validate_mesh_fetch_query(&query).is_err());
    }

    #[test]
    fn validate_zero_port() {
        let query = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 0,
            path: "/blobs/abc".into(),
        };
        assert!(validate_mesh_fetch_query(&query).is_err());
    }

    #[test]
    fn validate_path_traversal_attack() {
        let query = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 7878,
            path: "/blobs/../../etc/passwd".into(),
        };
        assert!(validate_mesh_fetch_query(&query).is_err());
    }

    #[test]
    fn validate_path_with_backslash_rejected() {
        let query = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 7878,
            path: "/blobs/foo\\..\\bar".into(),
        };
        assert!(validate_mesh_fetch_query(&query).is_err());
    }

    #[test]
    fn validate_path_dotted_filename_accepted() {
        // `..foo` is a legal filename component, not a traversal
        // segment. The strict-segment check should let it through.
        let query = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 7878,
            path: "/blobs/..foo/bar".into(),
        };
        assert!(validate_mesh_fetch_query(&query).is_ok());
    }

    #[test]
    fn validate_path_too_long_rejected() {
        let query = MeshFetchQuery {
            host: "10.0.0.5".into(),
            port: 7878,
            path: format!("/blobs/{}", "x".repeat(1100)),
        };
        assert!(validate_mesh_fetch_query(&query).is_err());
    }
}
