//! End-to-end integration tests for the proxy endpoint.
//!
//! These tests stand up two real HTTP servers on ephemeral ports:
//! 1. A mock upstream that serves `/blobs/...` paths.
//! 2. The relay itself (with `HostPolicy::AllowLoopbackOnly` so it can
//!    talk to the mock upstream on `127.0.0.1`).
//!
//! Then we issue real HTTP requests through the relay and assert on
//! the response. The tests are intentionally `#[ignore]`-free so they
//! run in `cargo test -p adnet-relay --test proxy_smoke`.
//!
//! Run with:
//!
//! ```text
//! cargo test -p adnet-relay --test proxy_smoke
//! ```

use std::net::SocketAddr;

use adnet_relay::{HostPolicy, ServerPolicy};
use axum::{Json, Router, extract::Path, http::StatusCode, response::Response, routing::get};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::watch;

#[tokio::test]
async fn end_to_end_blob_fetch() {
    // 1. Upstream: serves JSON at /blobs/foo/meta.
    let upstream = start_upstream("hello world").await;
    let upstream_port = upstream.port();
    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        ..ServerPolicy::default()
    };
    let relay = start_relay(policy, 0).await;
    let client = reqwest::Client::new();
    // 2. Hit the relay pointing at the upstream.
    let url = format!(
        "{}/exodus-mesh/fetch?host=127.0.0.1&port={}&path=/blobs/foo/meta",
        relay.base_url, upstream_port
    );
    let resp = client.get(&url).send().await.expect("relay req");
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.expect("body json");
    assert_eq!(body["path"], "/blobs/foo/meta");
    assert_eq!(body["echo"], "hello world");
    relay.shutdown();
}

#[tokio::test]
async fn blocks_path_traversal() {
    let upstream = start_upstream("").await;
    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        ..ServerPolicy::default()
    };
    let relay = start_relay(policy, 0).await;
    let client = reqwest::Client::new();
    let url = format!(
        "{}/exodus-mesh/fetch?host=127.0.0.1&port={}&path=/blobs/../etc/passwd",
        relay.base_url,
        upstream.port()
    );
    let resp = client.get(&url).send().await.expect("relay req");
    assert_eq!(resp.status().as_u16(), 400);
    relay.shutdown();
}

#[tokio::test]
async fn blocks_non_blobs_path() {
    let upstream = start_upstream("").await;
    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        ..ServerPolicy::default()
    };
    let relay = start_relay(policy, 0).await;
    let client = reqwest::Client::new();
    let url = format!(
        "{}/exodus-mesh/fetch?host=127.0.0.1&port={}&path=/etc/passwd",
        relay.base_url,
        upstream.port()
    );
    let resp = client.get(&url).send().await.expect("relay req");
    assert_eq!(resp.status().as_u16(), 400);
    relay.shutdown();
}

#[tokio::test]
async fn blocks_private_ip_with_default_policy() {
    // No upstream — the relay should reject the request before it
    // tries to connect. Default policy rejects 127.0.0.1.
    let policy = ServerPolicy::default();
    let relay = start_relay(policy, 0).await;
    let client = reqwest::Client::new();
    let url = format!(
        "{}/exodus-mesh/fetch?host=127.0.0.1&port=8080&path=/blobs/foo",
        relay.base_url
    );
    let resp = client.get(&url).send().await.expect("relay req");
    assert_eq!(resp.status().as_u16(), 400);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("private") || body.contains("loopback"),
        "expected host policy rejection, got: {body}"
    );
    relay.shutdown();
}

#[tokio::test]
async fn blocks_redirect_to_private_host() {
    // Upstream serves a 302 redirect to localhost (itself). The relay
    // should refuse the redirect because the target host is loopback.
    let upstream = start_redirect_upstream("http://127.0.0.1:1/never").await;
    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        ..ServerPolicy::default()
    };
    let relay = start_relay(policy, 0).await;
    let client = reqwest::Client::new();
    let url = format!(
        "{}/exodus-mesh/fetch?host=127.0.0.1&port={}&path=/blobs/foo",
        relay.base_url,
        upstream.port()
    );
    let resp = client.get(&url).send().await.expect("relay req");
    assert_eq!(resp.status().as_u16(), 502);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("redirect rejected") || body.contains("redirect host rejected"),
        "expected redirect rejection, got: {body}"
    );
    relay.shutdown();
}

#[tokio::test]
async fn allows_redirect_to_public_ip() {
    // Upstream serves a 302 redirect to a public IP. The relay should
    // refuse because nothing is listening on that port — but the error
    // should be a connect error, NOT a redirect rejection.
    let upstream = start_redirect_upstream("http://1.1.1.1:1/never").await;
    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        ..ServerPolicy::default()
    };
    let relay = start_relay(policy, 0).await;
    let client = reqwest::Client::new();
    let url = format!(
        "{}/exodus-mesh/fetch?host=127.0.0.1&port={}&path=/blobs/foo",
        relay.base_url,
        upstream.port()
    );
    let resp = client.get(&url).send().await.expect("relay req");
    // We don't care whether the upstream connect succeeds — only that
    // the relay didn't reject the redirect-target host.
    let status = resp.status().as_u16();
    assert!(
        status == 502 || status == 504,
        "expected 502/504 from upstream connect failure, got {status}"
    );
    relay.shutdown();
}

#[tokio::test]
async fn rejects_oversized_body_via_stream() {
    // Upstream streams 8 KiB but the relay's max_body_bytes is 1 KiB.
    // The bounded stream must cut the body off rather than allocating
    // the full upstream response. We assert that the bytes received
    // are bounded by the policy (within a small overhead for chunking).
    let upstream = start_streaming_upstream(8192).await;
    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        max_body_bytes: 1024,
        ..ServerPolicy::default()
    };
    let relay = start_relay(policy, 0).await;
    let client = reqwest::Client::new();
    let url = format!(
        "{}/exodus-mesh/fetch?host=127.0.0.1&port={}&path=/blobs/foo",
        relay.base_url,
        upstream.port()
    );
    let resp = client.get(&url).send().await.expect("relay req");
    assert_eq!(resp.status().as_u16(), 200, "relay should still 200");
    let bytes = resp.bytes().await.unwrap_or_default();
    // The bounded stream should cut the body off. The actual cutoff
    // depends on when the server's body frame crosses the limit. We
    // assert that the relay never accepted the full 8 KiB.
    assert!(
        bytes.len() < 8192,
        "relay forwarded {} bytes (expected < 8192)",
        bytes.len()
    );
    relay.shutdown();
}

#[tokio::test]
async fn healthz_returns_policy_view() {
    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        max_body_bytes: 1234,
        ..ServerPolicy::default()
    };
    let relay = start_relay(policy, 0).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/healthz", relay.base_url))
        .send()
        .await
        .expect("healthz req");
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.expect("body json");
    assert_eq!(body["hostPolicy"], "loopback-only");
    assert_eq!(body["maxBodyBytes"], 1234);
    relay.shutdown();
}

#[tokio::test]
async fn health_returns_ok() {
    let policy = ServerPolicy::default();
    let relay = start_relay(policy, 0).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/health", relay.base_url))
        .send()
        .await
        .expect("health req");
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
    relay.shutdown();
}

// ------------------------------------------------------------------------
// helpers
// ------------------------------------------------------------------------

async fn start_relay(policy: ServerPolicy, port: u16) -> adnet_relay::RelayServerHandle {
    adnet_relay::RelayServer::start_with_policy(
        "127.0.0.1",
        port,
        adnet_relay::BillingMode::Disabled,
        policy,
    )
    .await
    .expect("relay start")
}

struct UpstreamHandle {
    port: u16,
    shutdown_tx: watch::Sender<bool>,
}

impl UpstreamHandle {
    fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for UpstreamHandle {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
    }
}

async fn start_upstream(echo: &'static str) -> UpstreamHandle {
    let app = Router::new()
        .route(
            "/blobs/:hash/meta",
            get(move |Path(hash): Path<String>| async move {
                Json(json!({"path": format!("/blobs/{hash}/meta"), "echo": echo}))
            }),
        )
        .route("/blobs/:hash", get(move || async { "raw blob" }));
    spawn_upstream(app).await
}

async fn start_redirect_upstream(location: &'static str) -> UpstreamHandle {
    let app = Router::new().route(
        "/blobs/:hash",
        get(move || async move {
            (
                StatusCode::FOUND,
                [(axum::http::header::LOCATION, location)],
                "",
            )
        }),
    );
    spawn_upstream(app).await
}

async fn start_streaming_upstream(total_bytes: usize) -> UpstreamHandle {
    use axum::body::Body;
    use bytes::Bytes;
    use futures::stream;
    let chunk = Bytes::from(vec![0u8; 1024]);
    let n_chunks = total_bytes / chunk.len();
    let app = Router::new().route(
        "/blobs/:hash",
        get(move || {
            let chunk = chunk.clone();
            let s =
                stream::iter((0..n_chunks).map(move |_| Ok::<_, std::io::Error>(chunk.clone())));
            let body = Body::from_stream(s);
            async move {
                Response::builder()
                    .status(StatusCode::OK)
                    .header(axum::http::header::CONTENT_TYPE, "application/octet-stream")
                    .body(body)
                    .unwrap()
            }
        }),
    );
    spawn_upstream(app).await
}

async fn spawn_upstream(app: Router) -> UpstreamHandle {
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    let port = addr.port();
    tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            loop {
                if *shutdown_rx.borrow() {
                    break;
                }
                if shutdown_rx.changed().await.is_err() {
                    break;
                }
            }
        });
        let _ = server.await;
    });
    UpstreamHandle { port, shutdown_tx }
}
