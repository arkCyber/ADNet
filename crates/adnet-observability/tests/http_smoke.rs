//! HTTP smoke test for the metrics server.
//!
//! Verifies the four routes (`/metrics`, `/metrics.json`,
//! `/health`, `/diagnostics`) work end-to-end: bind the
//! server, register a metric, hit each route, assert the
//! response body.

#![cfg(feature = "http-server")]

use std::net::SocketAddr;
use std::time::Duration;

use adnet_observability::http::{MetricsServer, MetricsServerConfig, serve};
use adnet_observability::labels::LabelSet;
use adnet_observability::registry::Registry;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn read_http_response(stream: &mut TcpStream) -> (String, String) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut header_end = None;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), stream.read(&mut tmp)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = find_double_crlf(&buf) {
                    header_end = Some(pos);
                    break;
                }
            }
            _ => continue,
        }
    }
    let header_end = header_end.unwrap_or(buf.len());
    let raw = String::from_utf8_lossy(&buf).to_string();
    let headers = raw[..header_end].to_string();
    let body_start = header_end + 4; // skip "\r\n\r\n"
    let mut body = raw[body_start..].to_string();
    // Drain any remaining body bytes.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(50), stream.read(&mut tmp)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => body.push_str(&String::from_utf8_lossy(&tmp[..n])),
            _ => break,
        }
    }
    (headers, body)
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

async fn http_get(addr: SocketAddr, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.expect("write");
    read_http_response(&mut stream).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_endpoint_returns_prometheus_text() {
    let registry = std::sync::Arc::new(Registry::default());
    let counter = registry.register_counter("adnet_smoke_total", "smoke");
    counter.inc_by(11);
    let label = LabelSet::new([("topic".into(), "lobby".into())]).unwrap();
    counter.inc_labels(&label);

    let server: MetricsServer = serve(MetricsServerConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        registry: Some(std::sync::Arc::clone(&registry)),
    })
    .await
    .expect("server starts");
    let addr = server.local_addr();

    let (headers, body) = http_get(addr, "/metrics").await;
    assert!(
        headers.contains("200 OK"),
        "expected 200 OK, got: {headers}"
    );
    assert!(
        headers.contains("text/plain"),
        "expected Prometheus content type, got: {headers}"
    );
    assert!(
        body.contains("# HELP adnet_smoke_total smoke"),
        "missing HELP line: {body}"
    );
    assert!(body.contains("adnet_smoke_total 11"));
    assert!(body.contains(r#"adnet_smoke_total{topic="lobby"} 1"#));

    server.shutdown();
    server.join().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_endpoint_returns_ok_with_metric_count() {
    let registry = std::sync::Arc::new(Registry::default());
    let _c = registry.register_counter("a_total", "a");
    let _g = registry.register_gauge("b", "b");
    let server = serve(MetricsServerConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        registry: Some(std::sync::Arc::clone(&registry)),
    })
    .await
    .expect("server starts");
    let (headers, body) = http_get(server.local_addr(), "/health").await;
    assert!(headers.contains("200 OK"));
    assert!(headers.contains("application/json"), "headers: {headers}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["metric_count"], 2);
    assert!(json["now_unix_ms"].is_i64());
    server.shutdown();
    server.join().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnostics_endpoint_lists_metric_names() {
    let registry = std::sync::Arc::new(Registry::default());
    let _ = registry.register_counter("zzz_total", "z");
    let _ = registry.register_counter("aaa_total", "a");
    let server = serve(MetricsServerConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        registry: Some(std::sync::Arc::clone(&registry)),
    })
    .await
    .expect("server starts");
    let (headers, body) = http_get(server.local_addr(), "/diagnostics").await;
    assert!(headers.contains("200 OK"));
    let json: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    let names = json["metric_names"].as_array().expect("array");
    let names: Vec<String> = names
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    // Names are sorted in the handler.
    assert_eq!(
        names,
        vec!["aaa_total".to_string(), "zzz_total".to_string()]
    );
    assert_eq!(json["metric_count"], 2);
    server.shutdown();
    server.join().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_json_endpoint_returns_typed_samples() {
    let registry = std::sync::Arc::new(Registry::default());
    let c = registry.register_counter("c_total", "c");
    c.inc_by(4);
    let l = LabelSet::new([("topic".into(), "lobby".into())]).unwrap();
    c.inc_labels(&l);
    let server = serve(MetricsServerConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        registry: Some(std::sync::Arc::clone(&registry)),
    })
    .await
    .expect("server starts");
    let (headers, body) = http_get(server.local_addr(), "/metrics.json").await;
    assert!(headers.contains("200 OK"));
    let json: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    let arr = json.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    let metric = &arr[0];
    assert_eq!(metric["name"], "c_total");
    assert_eq!(metric["kind"], "counter");
    let samples = metric["samples"].as_array().expect("samples array");
    // Two samples: unlabeled (4) + labeled (1).
    assert_eq!(samples.len(), 2);
    let unlabeled = samples
        .iter()
        .find(|s| s["labels"] == "")
        .expect("unlabeled sample present");
    assert_eq!(unlabeled["value"], 4);
    let labeled = samples
        .iter()
        .find(|s| s["labels"].as_str().unwrap_or("").contains("lobby"))
        .expect("labeled sample present");
    assert_eq!(labeled["value"], 1);
    server.shutdown();
    server.join().await;
}

/// Regression: when no `registry` is passed to
/// `MetricsServerConfig`, the server must serve the global
/// registry — NOT a fresh empty `Registry::default()`. Without
/// this, every crate's eagerly-registered metric is invisible
/// through `/metrics` and an operator running `adnet metrics`
/// sees a blank page even after the crate's main code path has
/// already incremented its counters.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_registry_serves_global() {
    // Register something directly into the global registry,
    // then start the server with NO `registry` field set on
    // the config. The server must observe that same handle.
    let counter = adnet_observability::registry::GLOBAL
        .register_counter("smoke_global_test_total", "smoke test counter");
    counter.inc_by(7);

    let server = serve(MetricsServerConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        registry: None, // <-- the regression trigger
    })
    .await
    .expect("server starts");
    let (_headers, body) = http_get(server.local_addr(), "/metrics").await;
    assert!(
        body.contains("smoke_global_test_total 7"),
        "expected the global-registered counter in /metrics, got:\n{body}",
    );
    server.shutdown();
    server.join().await;
}
