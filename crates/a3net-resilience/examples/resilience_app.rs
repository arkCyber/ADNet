//! Realistic example: a `ResilientHttpClient` with a circuit breaker +
//! retry policy wrapped around `reqwest`. We point it at a tiny
//! `tokio` HTTP server that fails the first 2 requests with 503 and
//! then succeeds, so the retry-with-backoff loop is exercised
//! end-to-end.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-resilience --example resilience_app
//! ```

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use a3net_resilience::{ResilientHttpClient, ResilientHttpConfig, RetryPolicy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Minimal HTTP/1.1 responder: 503 for the first two hits, then 200.
async fn run_server(hits: Arc<AtomicU32>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let hits = Arc::clone(&hits);
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                let (status, body) = if n < 3 {
                    (503, format!("fail #{n}"))
                } else {
                    (200, format!("ok #{n}"))
                };
                let resp = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    addr
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let hits = Arc::new(AtomicU32::new(0));
    let addr = run_server(Arc::clone(&hits)).await;
    let url = format!("http://{addr}/");

    // 1. Wrap the call with a `ResilientHttpClient` that retries
    //    transient failures with exponential backoff.
    let client = ResilientHttpClient::with_config(ResilientHttpConfig {
        retry: RetryPolicy::Aggressive.to_config(),
        request_timeout: Duration::from_secs(2),
        ..Default::default()
    });

    let body = client
        .get_bytes(&url)
        .await
        .unwrap_or_else(|e| panic!("expected success after retries, got {e}"));
    let body = String::from_utf8_lossy(&body);
    println!("GET {url} -> {body}");

    let total = hits.load(Ordering::SeqCst);
    println!("server saw {total} requests");
    assert!(
        total >= 3,
        "the retry path should have hit the server at least 3 times"
    );

    // 2. Now drive the breaker to Open by hammering a different URL.
    let cfg = ResilientHttpConfig {
        retry: RetryPolicy::None.to_config(),
        ..Default::default()
    };
    let breaker_client = ResilientHttpClient::with_config(cfg);
    for _ in 0..10 {
        let _ = breaker_client.get_bytes("http://127.0.0.1:1/never").await;
    }
    let state = breaker_client.breaker().state().await;
    println!("breaker after 10 failures: {state:?}");
    assert!(format!("{state:?}") == "Open", "expected Open, got {state:?}");
}
