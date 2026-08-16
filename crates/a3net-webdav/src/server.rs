//! Minimal tokio+hyper WebDAV transport adapter (DAL-A).
//!
//! Uses hyper 1.x with `http1` only. No tower middleware. Upload
//! bodies are streamed; the hash is supplied by the client in the
//! `X-Content-Hash` header so the namespace layer can record it
//! without re-hashing. The server refuses requests whose
//! Content-Length exceeds `MAX_IMPORT_FILE_BYTES` to keep the
//! audit/manifest FSM deterministic (DAL-A SR-19).

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::watch;

use a3net_blobstore::PathSegments;

use crate::handlers::HandlerState;
#[allow(unused_imports)]
use crate::acl::{CapabilityResolver, StaticCapabilityResolver};
#[allow(unused_imports)]
use crate::token::TokenVerifier;
#[allow(unused_imports)]
use a3net_blobstore::namespace::Nas;

/// Maximum upload size enforced at the transport layer (DAL-A SR-19).
const MAX_UPLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB

#[derive(Debug, Clone)]
pub struct WebdavConfig {
    pub host: String,
    pub port: u16,
}

impl Default for WebdavConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8780,
        }
    }
}

pub struct WebdavServer {
    config: WebdavConfig,
    state: Arc<HandlerState>,
    shutdown: watch::Sender<bool>,
}

impl WebdavServer {
    pub fn new(config: WebdavConfig, state: Arc<HandlerState>) -> Self {
        let (tx, _rx) = watch::channel(false);
        Self {
            config,
            state,
            shutdown: tx,
        }
    }

    pub async fn start(self) -> Result<WebdavServerHandle, std::io::Error> {
        let addr: SocketAddr = format!("{}:{}", self.config.host, self.config.port)
            .parse()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{e}")))?;
        let listener = TcpListener::bind(addr).await?;
        let bound = listener.local_addr()?;
        let state = Arc::clone(&self.state);
        let mut shutdown_rx = self.shutdown.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => break,
                    accept = listener.accept() => {
                        let (stream, _peer) = match accept {
                            Ok(t) => t,
                            Err(_) => continue,
                        };
                        let state = Arc::clone(&state);
                        tokio::spawn(async move {
                            let io = TokioIo::new(stream);
                            let svc = service_fn(move |req| {
                                let s = Arc::clone(&state);
                                async move { dispatch(s, req).await }
                            });
                            let _ = http1::Builder::new()
                                .serve_connection(io, svc)
                                .await;
                        });
                    }
                }
            }
        });
        Ok(WebdavServerHandle {
            bound_addr: bound,
            shutdown: self.shutdown,
        })
    }
}

pub struct WebdavServerHandle {
    pub bound_addr: SocketAddr,
    shutdown: watch::Sender<bool>,
}

impl WebdavServerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.bound_addr
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Wrap raw bytes into the body type used by every response.
#[inline]
fn bytes_body(v: Vec<u8>) -> Full<Bytes> {
    Full::new(Bytes::from(v))
}

/// Wrap a string into the body type.
#[inline]
fn str_body(s: impl Into<String>) -> Full<Bytes> {
    bytes_body(s.into().into_bytes())
}

/// Build an error response. Never panics (DAL-A SR-19).
fn error_response(code: u16, msg: &str) -> Response<Full<Bytes>> {
    let body = format!("{code} {msg}\n");
    Response::builder()
        .status(StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Content-Length", body.len().to_string())
        .body(str_body(body))
        .unwrap_or_else(|_| {
            let mut r = Response::new(str_body("500 internal error\n"));
            *r.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            r
        })
}

/// Build a response with a status code and no body.
fn status_only(code: u16) -> Result<Response<Full<Bytes>>, std::io::Error> {
    Response::builder()
        .status(StatusCode::from_u16(code).unwrap_or(StatusCode::NO_CONTENT))
        .header("Content-Length", "0")
        .body(Full::new(Bytes::new()))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

// ── dispatch ─────────────────────────────────────────────────────────────────

async fn dispatch(
    state: Arc<HandlerState>,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, std::io::Error> {
    let method = req.method().to_string();
    let uri = req.uri().path().to_string();

    let auth = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let user_agent = req
        .headers()
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let content_hash_header = req
        .headers()
        .get("x-content-hash")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| a3net_types::ContentHash::from_hex(s).ok());

    let body_length = body_length_hint(req.headers());

    // SR-19: refuse over-large uploads at the transport layer.
    if body_length > MAX_UPLOAD_BYTES {
        return Ok(error_response(413, "payload too large"));
    }

    let path = match PathSegments::decode_http(&uri) {
        Ok(p) => p,
        Err(e) => return Ok(error_response(400, &e.to_string())),
    };

    let dest_header = req
        .headers()
        .get("destination")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let overwrite_header = req
        .headers()
        .get("overwrite")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_uppercase() == "T")
        .unwrap_or(true); // Default is true

    let range_header = req
        .headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let want_md5 = req
        .headers()
        .get("want-digest")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| s.contains("md5"));

    match method.to_ascii_lowercase().as_str() {
        "options" => {
            let body = state.options().into_bytes();
            let len = body.len();
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("DAV", "1, 2")
                .header("Allow", "OPTIONS, HEAD, GET, PROPFIND, PUT, MKCOL, DELETE, MOVE")
                .header("Content-Length", len.to_string())
                .body(bytes_body(body))
                .unwrap_or_else(|_| error_response(500, "options build failed")))
        }

        "head" => match state.handle_get(&path, auth.as_deref()) {
            Ok(data) => {
                let len = data.len();
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Length", len.to_string())
                    .header("Accept-Ranges", "bytes")
                    .body(Full::new(Bytes::new())) // HEAD: no body
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?)
            }
            Err(e) => Ok(error_response(e.status(), &e.to_string())),
        },

        "get" => {
            if let Some(range_str) = range_header.as_deref() {
                let range = parse_range(range_str)?;
                match state.handle_get_range(&path, range, auth.as_deref()) {
                    Ok((slice, start, end, total)) => {
                        let slice_len = slice.len();
                        let mut resp = Response::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header("Content-Range", format!("bytes {start}-{end}/{total}"))
                            .header("Accept-Ranges", "bytes")
                            .header("Content-Length", slice_len.to_string())
                            .body(bytes_body(slice))
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                        if want_md5.is_some() {
                            // Re-fetch full body for digest — acceptable for small files.
                            if let Ok(data) = state.handle_get(&path, auth.as_deref()) {
                                let digest = format!("{:x}", md5::compute(&data));
                                if let Ok(v) = digest.parse() {
                                    resp.headers_mut().insert("Content-MD5", v);
                                }
                            }
                        }
                        Ok(resp)
                    }
                    Err(e) => Ok(error_response(e.status(), &e.to_string())),
                }
            } else {
                match state.handle_get(&path, auth.as_deref()) {
                    Ok(data) => {
                        let len = data.len();
                        let md5_digest = if want_md5.is_some() {
                            Some(format!("{:x}", md5::compute(&data)))
                        } else {
                            None
                        };
                        let mut builder = Response::builder()
                            .status(StatusCode::OK)
                            .header("Accept-Ranges", "bytes")
                            .header("Content-Length", len.to_string());
                        if let Some(digest) = md5_digest {
                            builder = builder.header("Content-MD5", digest);
                        }
                        Ok(builder
                            .body(bytes_body(data))
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?)
                    }
                    Err(e) => Ok(error_response(e.status(), &e.to_string())),
                }
            }
        }

        "propfind" => {
            // Parse pagination query params: ?offset=0&limit=100
            let offset = query_param(&uri, "offset")
                .and_then(|s| s.parse::<usize>().ok());
            let limit = query_param(&uri, "limit")
                .and_then(|s| s.parse::<usize>().ok());

            // Parse Depth header (RFC 4918)
            let depth_header = req
                .headers()
                .get("depth")
                .and_then(|v| v.to_str().ok());
            // Default to infinity for directories
            let depth = crate::props::parse_depth(depth_header, true);

            match state.handle_propfind(&path, auth.as_deref(), offset, limit, depth) {
                Ok((xml, meta)) => {
                    let body = xml.into_bytes();
                    let len = body.len();
                    let mut builder = Response::builder()
                        .status(StatusCode::MULTI_STATUS)
                        .header("Content-Type", "application/xml; charset=utf-8")
                        .header("DAV", "1, 2")
                        .header("Content-Length", len.to_string())
                        .header("Pagination-Offset", meta.offset.to_string())
                        .header("Pagination-Limit", meta.limit.to_string())
                        .header("Pagination-Total", meta.total.to_string());
                    if meta.has_more {
                        builder = builder.header("Pagination-HasMore", "true");
                    }
                    Ok(builder
                        .body(bytes_body(body))
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?)
                }
                Err(e) => Ok(error_response(e.status(), &e.to_string())),
            }
        }

        "put" => {
            // Read the full request body and persist it into the
            // content-addressed blob store. If the caller supplied
            // `X-Content-Hash`, the computed hash must match it or
            // the request is rejected (SR-15 audit integrity).
            let body = match req.into_body().collect().await {
                Ok(collected) => collected.to_bytes().to_vec(),
                Err(e) => {
                    return Ok(error_response(400, &format!("failed to read body: {e}")));
                }
            };
            match state.handle_put_body(
                &path,
                &body,
                content_hash_header,
                auth.as_deref(),
                user_agent,
            ) {
                Ok(()) => status_only(201),
                Err(e) => Ok(error_response(e.status(), &e.to_string())),
            }
        }

        "mkcol" => match state.handle_mkcol(&path, auth.as_deref(), user_agent) {
            Ok(()) => status_only(201),
            Err(e) => Ok(error_response(e.status(), &e.to_string())),
        },

        "delete" => match state.handle_delete(&path, auth.as_deref(), user_agent) {
            Ok(()) => status_only(204),
            Err(e) => Ok(error_response(e.status(), &e.to_string())),
        },

        "move" => {
            let dest_uri = match dest_header.as_deref() {
                Some(s) => s,
                None => return Ok(error_response(400, "missing Destination header")),
            };
            // Strip query string from Destination.
            let dest_path = dest_uri.split('?').next().unwrap_or(dest_uri);
            let dest = match PathSegments::decode_http(dest_path) {
                Ok(p) => p,
                Err(e) => return Ok(error_response(400, &format!("bad Destination: {e}"))),
            };
            match state.handle_move(&path, &dest, overwrite_header, auth.as_deref(), user_agent) {
                Ok(()) => status_only(201),
                Err(e) => Ok(error_response(e.status(), &e.to_string())),
            }
        }

        "copy" => {
            let dest_uri = match dest_header.as_deref() {
                Some(s) => s,
                None => return Ok(error_response(400, "missing Destination header")),
            };
            let dest_path = dest_uri.split('?').next().unwrap_or(dest_uri);
            let dest = match PathSegments::decode_http(dest_path) {
                Ok(p) => p,
                Err(e) => return Ok(error_response(400, &format!("bad Destination: {e}"))),
            };
            match state.handle_copy(&path, &dest, overwrite_header, auth.as_deref(), user_agent) {
                Ok(()) => status_only(201),
                Err(e) => Ok(error_response(e.status(), &e.to_string())),
            }
        }

        _ => Ok(error_response(405, &format!("method {method} not allowed"))),
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn parse_range(s: &str) -> Result<(u64, u64), std::io::Error> {
    let s = s.trim();
    let prefix = "bytes=";
    if !s.starts_with(prefix) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Range must start with 'bytes='",
        ));
    }
    let parts: Vec<&str> = s[prefix.len()..].split('-').collect();
    if parts.len() != 2 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid range format",
        ));
    }
    let start: u64 = parts[0].trim().parse().map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid range start")
    })?;
    let end: u64 = parts[1].trim().parse().map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid range end")
    })?;
    Ok((start, end))
}

fn body_length_hint(headers: &hyper::HeaderMap) -> u64 {
    headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Extract a query parameter from a URI string.
fn query_param<'a>(uri: &'a str, name: &str) -> Option<&'a str> {
    uri.split('?')
        .nth(1)?
        .split('&')
        .find(|p| p.starts_with(&format!("{name}=")))
        .and_then(|p| p.split('=').nth(1))
}
