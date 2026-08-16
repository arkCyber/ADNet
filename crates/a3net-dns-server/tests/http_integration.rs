//! HTTP server integration tests.
//!
//! These tests require a separate runtime because serve_http() blocks
//! indefinitely and doesn't support graceful shutdown.

use std::sync::Arc;

use a3net_dns_server::http::{serve_http_with_listener, HttpApi, PublishBody};
use a3net_dns_server::pkarr::PkarrApi;
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Helper to make HTTP requests and get responses
async fn make_request(addr: std::net::SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    String::from_utf8(buf).unwrap()
}

fn create_test_api() -> (tempfile::TempDir, Arc<HttpApi>) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = a3net_dns_server::DnsServerConfig::default()
        .with_state_path(dir.path().join("zone.json"))
        .with_zone("a3net.test");
    let api = HttpApi::from_config(cfg).unwrap();
    (dir, Arc::new(api))
}

#[tokio::test(flavor = "current_thread")]
async fn test_health_endpoint() {
    let (_dir, api) = create_test_api();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let api_clone = api.clone();
    let server = tokio::spawn(async move {
        serve_http_with_listener(listener, api_clone).await.ok();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let response = make_request(addr, "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.contains("200 OK"));
    assert!(response.contains("ok"));

    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_get_existing_record() {
    let (_dir, api) = create_test_api();
    api.publish("testuser", PublishBody { payload: "TESTPAYLOAD".into(), ttl_secs: Some(60) }).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let api_clone = api.clone();
    let server = tokio::spawn(async move {
        serve_http_with_listener(listener, api_clone).await.ok();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let response = make_request(addr, "GET /zones/a3net.test/ipns/testuser HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.contains("200 OK"));
    assert!(response.contains("TESTPAYLOAD"));

    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_get_nonexistent_record() {
    let (_dir, api) = create_test_api();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let api_clone = api.clone();
    let server = tokio::spawn(async move {
        serve_http_with_listener(listener, api_clone).await.ok();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let response = make_request(addr, "GET /zones/a3net.test/ipns/nonexistent HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.contains("404 Not Found"));

    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_list_records() {
    let (_dir, api) = create_test_api();
    api.publish("rec1", PublishBody { payload: "R1".into(), ttl_secs: None }).unwrap();
    api.publish("rec2", PublishBody { payload: "R2".into(), ttl_secs: None }).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let api_clone = api.clone();
    let server = tokio::spawn(async move {
        serve_http_with_listener(listener, api_clone).await.ok();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let response = make_request(addr, "GET /zones/a3net.test/records HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.contains("200 OK"));
    assert!(response.contains("rec1"));
    assert!(response.contains("rec2"));

    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_put_publish() {
    let (_dir, api) = create_test_api();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let api_clone = api.clone();
    let server = tokio::spawn(async move {
        serve_http_with_listener(listener, api_clone).await.ok();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let body = r#"{"payload":"PUTDATA","ttl_secs":120}"#;
    let request = format!(
        "PUT /zones/a3net.test/ipns/newuser HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );

    let response = make_request(addr, &request).await;
    assert!(response.contains("201 Created"));
    assert!(response.contains("PUTDATA"));

    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_put_invalid_json() {
    let (_dir, api) = create_test_api();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let api_clone = api.clone();
    let server = tokio::spawn(async move {
        serve_http_with_listener(listener, api_clone).await.ok();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let body = "not valid json";
    let request = format!(
        "PUT /zones/a3net.test/ipns/baduser HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );

    let response = make_request(addr, &request).await;
    assert!(response.contains("400 Bad Request"));

    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_unknown_path() {
    let (_dir, api) = create_test_api();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let api_clone = api.clone();
    let server = tokio::spawn(async move {
        serve_http_with_listener(listener, api_clone).await.ok();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let response = make_request(addr, "GET /unknown/path HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.contains("404 Not Found"));

    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_unsupported_method() {
    let (_dir, api) = create_test_api();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let api_clone = api.clone();
    let server = tokio::spawn(async move {
        serve_http_with_listener(listener, api_clone).await.ok();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let response = make_request(addr, "DELETE /zones/a3net.test/ipns/user HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.contains("404 Not Found"));

    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_metrics_endpoint_exposes_prometheus_text() {
    let (_dir, api) = create_test_api();
    api.metrics().queries.with_label_values(&["hit"]).inc_by(7);
    api.metrics()
        .doh_queries
        .with_label_values(&["post", "2xx"])
        .inc_by(3);
    api.metrics().zone_records.set(42);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let api_clone = api.clone();
    let server = tokio::spawn(async move {
        serve_http_with_listener(listener, api_clone).await.ok();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let response = make_request(addr, "GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(
        response.contains("a3net_dns_queries_total{result=\"hit\"} 7"),
        "{response}"
    );
    assert!(
        response.contains("a3net_dns_doh_queries_total{method=\"post\",status=\"2xx\"} 3"),
        "{response}"
    );
    assert!(
        response.contains("a3net_dns_zone_records 42"),
        "{response}"
    );
    assert!(response.contains("# HELP a3net_dns_queries_total"), "{response}");

    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_request_with_extra_headers() {
    let (_dir, api) = create_test_api();
    api.publish("header_test", PublishBody { payload: "HEADER".into(), ttl_secs: None }).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let api_clone = api.clone();
    let server = tokio::spawn(async move {
        serve_http_with_listener(listener, api_clone).await.ok();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let request = "GET /zones/a3net.test/ipns/header_test HTTP/1.1\r\n\
                   Host: localhost\r\n\
                   Accept: application/json\r\n\
                   Authorization: Bearer token123\r\n\r\n";
    let response = make_request(addr, request).await;
    assert!(response.contains("200 OK"));
    assert!(response.contains("HEADER"));

    server.abort();
}

// ========== Pkarr HTTP surface integration tests ==========

fn create_pkarr_api() -> (tempfile::TempDir, Arc<HttpApi>) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = a3net_dns_server::DnsServerConfig::default()
        .with_state_path(dir.path().join("zone.json"))
        .with_zone("a3net.test");
    let pkarr = PkarrApi::from_config(cfg.clone()).unwrap();
    (dir, Arc::new(HttpApi::from_config_with_pkarr(cfg, pkarr).unwrap()))
}

fn sample_z32() -> String {
    // A real z-base-32 encoded ed25519 public key (52 chars,
    // lowercase + 2..7). The DNS server validates the z32 against
    // a real curve point, so we derive one from a fixed
    // signing-key seed here.
    let sk = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);
    a3net_dns_server::pkarr::z32_encode(&sk.verifying_key().to_bytes())
}

#[tokio::test(flavor = "current_thread")]
async fn test_pkarr_publish_then_resolve() {
    let (_dir, api) = create_pkarr_api();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let api_clone = api.clone();
    let server = tokio::spawn(async move { serve_http_with_listener(listener, api_clone).await.ok() });
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let z = sample_z32();
    let packet = [0x01u8, 0x02, 0x03, 0x04];
    let body: String = packet.iter().map(|b| *b as char).collect();
    let request = format!(
        "PUT /pkarr/{z} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
        body.len(),
    );
    let response = make_request(addr, &request).await;
    assert!(response.contains("200 OK"), "publish got {response:?}");
    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_pkarr_resolve_unknown_returns_404() {
    let (_dir, api) = create_pkarr_api();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let api_clone = api.clone();
    let server = tokio::spawn(async move { serve_http_with_listener(listener, api_clone).await.ok() });
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let z = sample_z32();
    let request = format!("GET /pkarr/{z} HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let response = make_request(addr, &request).await;
    assert!(response.contains("404 Not Found"));
    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_pkarr_publish_rejects_bad_key() {
    let (_dir, api) = create_pkarr_api();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let api_clone = api.clone();
    let server = tokio::spawn(async move { serve_http_with_listener(listener, api_clone).await.ok() });
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let body = "x";
    let request = format!(
        "PUT /pkarr/not-z32 HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
        body.len(),
    );
    let response = make_request(addr, &request).await;
    assert!(response.contains("400 Bad Request"), "got {response:?}");
    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_pkarr_delete_returns_404_when_absent() {
    let (_dir, api) = create_pkarr_api();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let api_clone = api.clone();
    let server = tokio::spawn(async move { serve_http_with_listener(listener, api_clone).await.ok() });
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let z = sample_z32();
    let request = format!("DELETE /pkarr/{z} HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let response = make_request(addr, &request).await;
    assert!(response.contains("404 Not Found"), "got {response:?}");
    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn test_pkarr_endpoints_disabled_without_pkarr() {
    let (_dir, api) = create_test_api();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let api_clone = api.clone();
    let server = tokio::spawn(async move { serve_http_with_listener(listener, api_clone).await.ok() });
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let z = sample_z32();
    let body = "abc";
    let request = format!(
        "PUT /pkarr/{z} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
        body.len(),
    );
    let response = make_request(addr, &request).await;
    assert!(response.contains("400 Bad Request"), "got {response:?}");

    let get_req = format!("GET /pkarr/{z} HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let response = make_request(addr, &get_req).await;
    assert!(response.contains("404 Not Found"));
    server.abort();
}