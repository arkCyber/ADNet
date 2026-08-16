//! Tiny embedded HTTP server that exposes the local node's
//! identity profile.
//!
//! The server is intentionally minimal — it speaks just enough
//! HTTP/1.1 to serve three endpoints:
//!
//! - `GET /profile` → `text/html` (the rendered profile page)
//! - `GET /api/node/profile` → `application/json` (the
//!   [`NodeIdentityCard`] for programmatic consumers)
//! - `GET /health` → `200 OK text/plain` (liveness probe)
//!
//! Everything else returns `404 Not Found`. The server binds to
//! `127.0.0.1:0` by default (random ephemeral port, localhost
//! only) so it cannot accidentally expose the profile to the
//! public internet. Operators who *do* want to expose the profile
//! publicly should put a reverse proxy (nginx, caddy) in front
//! and bind this server to a `0.0.0.0` address via
//! [`ProfileServerBuilder::bind_addr`].
//!
//! ## Concurrency model
//!
//! One task per accepted connection, with a hard cap of 16
//! concurrent connections via a [`tokio::sync::Semaphore`].
//! The handler is read-only against the [`Node`]: it borrows the
//! [`NodeIdentityStore`] and [`ContactsManager`] snapshots, never
//! mutates them, so concurrent profile requests are safe.
//!
//! ## Why no `axum` / `hyper`?
//!
//! The dependency footprint matters for FFI consumers (mobile
//! bindings, embedded). A 150-line manual HTTP/1.1 parser is
//! enough for three GET endpoints and avoids pulling `hyper` /
//! `http-body-util` / `tower` into the `a3net-node` build graph.
//! If we ever need full HTTP/2, websockets, or TLS termination
//! we'll revisit.

#![forbid(unsafe_code)]

use std::{net::SocketAddr, sync::Arc, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};
use tracing::{debug, error, info, warn};

use crate::node::Node;

/// Hard cap on concurrent in-flight HTTP connections.
const MAX_CONCURRENT_CONNECTIONS: usize = 16;

/// Read deadline per request — a slow / malicious client can't
/// pin a handler task forever.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Write deadline per response.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Server-side header length cap. Nobody sane sends more than
/// 8 KiB on the request line + headers.
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// Handle to the running profile HTTP server. Cheap to clone
/// (internally `Arc`).
#[derive(Debug, Clone)]
pub struct ProfileServerHandle {
    inner: Arc<ProfileServerInner>,
}

#[derive(Debug)]
struct ProfileServerInner {
    /// Local address the server is bound to (useful for tests
    /// that need to know the random port).
    local_addr: SocketAddr,
    /// Shutdown signal sender.
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl ProfileServerHandle {
    /// Bound local address — useful for `a3net profile serve`
    /// CLI to print `http://127.0.0.1:43210/profile`.
    pub fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr
    }

    /// Trigger a graceful shutdown. The listener stops accepting
    /// new connections; in-flight handlers are allowed to finish.
    pub async fn shutdown(&self) {
        let _ = self.inner.shutdown_tx.send(true);
    }
}

/// Builder for the embedded profile server.
#[derive(Debug)]
pub struct ProfileServerBuilder {
    bind_addr: SocketAddr,
}

impl ProfileServerBuilder {
    /// Default: bind to `127.0.0.1:0` (random port, localhost
    /// only).
    pub fn new() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        }
    }

    /// Override the bind address. Common uses:
    /// - `0.0.0.0:8080` to expose publicly (behind a reverse
    ///   proxy you trust)
    pub fn bind_addr(mut self, addr: SocketAddr) -> Self {
        self.bind_addr = addr;
        self
    }

    /// Start the server bound to the configured address.
    pub async fn start(
        self,
        node: Arc<Node>,
    ) -> std::io::Result<(ProfileServerHandle, tokio::task::JoinHandle<()>)> {
        let listener = TcpListener::bind(self.bind_addr).await?;
        let local_addr = listener.local_addr()?;
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let handle = ProfileServerHandle {
            inner: Arc::new(ProfileServerInner {
                local_addr,
                shutdown_tx,
            }),
        };
        let handle_for_task = handle.clone();
        let join = tokio::spawn(async move {
            let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
            info!(addr = %local_addr, "profile HTTP server listening");
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("profile HTTP server shutting down");
                            break;
                        }
                    }
                    accept = listener.accept() => {
                        match accept {
                            Ok((stream, peer)) => {
                                let permit = match sem.clone().try_acquire_owned() {
                                    Ok(p) => p,
                                    Err(_) => {
                                        warn!(%peer, "profile server at capacity; dropping");
                                        continue;
                                    }
                                };
                                let node = node.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handle_conn(stream, &node).await {
                                        debug!(error = %e, "profile conn ended with error");
                                    }
                                    drop(permit);
                                });
                            }
                            Err(e) => {
                                error!(error = %e, "profile server accept failed");
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        }
                    }
                }
            }
            drop(handle_for_task); // keep handle alive until end
        });
        Ok((handle, join))
    }
}

impl Default for ProfileServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle one connection to completion.
async fn handle_conn(mut stream: TcpStream, node: &Node) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(512);
    let mut tmp = [0u8; 1024];
    loop {
        let n = timeout(READ_TIMEOUT, stream.read(&mut tmp))
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "header read deadline")
            })??;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return write_response(
                &mut stream,
                431,
                "Request Header Fields Too Large",
                "text/plain",
                b"headers too large",
            )
            .await;
        }
    }
    let request = match std::str::from_utf8(&buf) {
        Ok(s) => s,
        Err(_) => {
            return write_response(&mut stream, 400, "Bad Request", "text/plain", b"non-utf8")
                .await;
        }
    };
    let response = dispatch(request, node);
    timeout(
        WRITE_TIMEOUT,
        write_response(
            &mut stream,
            response.status,
            response.reason,
            response.content_type,
            &response.body,
        ),
    )
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "write deadline"))??;
    Ok(())
}

#[derive(Debug)]
struct ResponseParts {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

/// Match the request line + path and produce a response.
fn dispatch(request: &str, node: &Node) -> ResponseParts {
    let mut lines = request.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    match (method, path) {
        ("GET", "/profile") | ("GET", "/profile/") => {
            let html = node.render_profile_html();
            ResponseParts {
                status: 200,
                reason: "OK",
                content_type: "text/html; charset=utf-8",
                body: html.into_bytes(),
            }
        }
        ("GET", "/api/node/profile") | ("GET", "/api/node/profile/") => {
            let card = node.identity_card();
            let body = match serde_json::to_vec(&card) {
                Ok(b) => b,
                Err(e) => {
                    return ResponseParts {
                        status: 500,
                        reason: "Internal Server Error",
                        content_type: "text/plain",
                        body: format!("serialise: {e}").into_bytes(),
                    };
                }
            };
            ResponseParts {
                status: 200,
                reason: "OK",
                content_type: "application/json",
                body,
            }
        }
        ("GET", "/health") => ResponseParts {
            status: 200,
            reason: "OK",
            content_type: "text/plain",
            body: b"ok".to_vec(),
        },
        ("GET", "/") => ResponseParts {
            status: 200,
            reason: "OK",
            content_type: "text/plain",
            body: b"A3Net profile server - see /profile\n".to_vec(),
        },
        _ => ResponseParts {
            status: 404,
            reason: "Not Found",
            content_type: "text/plain",
            body: b"not found\n".to_vec(),
        },
    }
}

/// Minimal HTTP/1.1 response writer. Always sends
/// `Connection: close` because we don't keep-alive.
async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         Cache-Control: no-store\r\n\
         \r\n",
        len = body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    stream.shutdown().await.ok();
    Ok(())
}

/// Convenience: start the server on `127.0.0.1:0` and return
/// just the handle. Used by the CLI and tests.
pub async fn start_default(
    node: Arc<Node>,
) -> std::io::Result<(ProfileServerHandle, tokio::task::JoinHandle<()>)> {
    ProfileServerBuilder::new().start(node).await
}

/// Errors returned by the profile server. Currently unused
/// outside this module but kept for forward compatibility.
#[derive(Debug, thiserror::Error)]
pub enum ProfileServerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeConfig;
    use a3net_types::NodeId;
    use std::sync::Arc;
    use tokio::net::TcpStream;

    /// Build a [`Node`] in a tempdir, populate the identity so
    /// the profile page has content, and return both. Lives in
    /// the test module so production builds don't pull in the
    /// test-only setup.
    async fn build_node_with_identity() -> (tempfile::TempDir, Arc<Node>) {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(tmp.path(), NodeId::random());
        let node = Arc::new(Node::builder(cfg).build().await.unwrap());
        node.identity().set_nickname("tester").unwrap();
        node.identity().set_email("tester@example.com").unwrap();
        (tmp, node)
    }

    #[tokio::test]
    async fn dispatch_profile_html_via_real_server() {
        let (_tmp, node) = build_node_with_identity().await;
        let resp = dispatch("GET /profile HTTP/1.1\r\nHost: x\r\n\r\n", &node);
        assert_eq!(resp.status, 200);
        assert!(resp.content_type.starts_with("text/html"));
        let body = std::str::from_utf8(&resp.body).unwrap();
        assert!(body.contains("<!DOCTYPE html>"));
        assert!(body.contains("tester"));
    }

    #[test]
    fn dispatch_api_profile_json() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_tmp, node) = rt.block_on(build_node_with_identity());
        let resp = dispatch("GET /api/node/profile HTTP/1.1\r\nHost: x\r\n\r\n", &node);
        assert_eq!(resp.status, 200);
        assert_eq!(resp.content_type, "application/json");
        let _: serde_json::Value =
            serde_json::from_slice(&resp.body).expect("json body");
    }

    #[test]
    fn dispatch_health() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_tmp, node) = rt.block_on(build_node_with_identity());
        let resp = dispatch("GET /health HTTP/1.1\r\n\r\n", &node);
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"ok");
    }

    #[test]
    fn dispatch_unknown_path() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_tmp, node) = rt.block_on(build_node_with_identity());
        let resp = dispatch("GET /nope HTTP/1.1\r\n\r\n", &node);
        assert_eq!(resp.status, 404);
    }

    #[test]
    fn dispatch_post_rejected() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_tmp, node) = rt.block_on(build_node_with_identity());
        let resp = dispatch("POST /profile HTTP/1.1\r\n\r\n", &node);
        assert_eq!(resp.status, 404);
    }

    #[tokio::test]
    async fn profile_server_end_to_end() {
        let (_tmp, node) = build_node_with_identity().await;
        let (handle, _join) =
            ProfileServerBuilder::new().start(node.clone()).await.unwrap();
        let addr = handle.local_addr();

        // GET /profile
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"GET /profile HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let s = std::str::from_utf8(&buf).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK"));
        assert!(s.contains("text/html"));
        assert!(s.contains("<!DOCTYPE html>"));
        assert!(s.contains("tester"));

        // GET /health
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let s = std::str::from_utf8(&buf).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK"));
        assert!(s.contains("ok"));

        // GET /api/node/profile
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"GET /api/node/profile HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let s = std::str::from_utf8(&buf).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK"));
        assert!(s.contains("application/json"));

        // GET /nope → 404
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"GET /nope HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let s = std::str::from_utf8(&buf).unwrap();
        assert!(s.starts_with("HTTP/1.1 404 Not Found"));

        handle.shutdown().await;
    }
}
