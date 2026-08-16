//! Demo: start the relay server and verify the URL helpers.
//!
//! Run with: `cargo run -p a3net-relay --example relay_local_demo`

use a3net_relay::{BillingMode, RelayClient, RelayConfig, RelayServer};
use std::time::Duration;

#[tokio::main]
async fn main() {
    let dir = tempfile::tempdir().expect("tempdir");
    let port = 18790;
    let cfg = RelayConfig {
        enabled: true,
        relay_base_url: Some(format!("http://127.0.0.1:{port}")),
        serve_enabled: true,
        serve_port: port,
        serve_bind: "127.0.0.1".into(),
        billing_secret_path: None,
    };
    cfg.save(dir.path()).expect("save config");

    let handle = RelayServer::start("127.0.0.1", port, BillingMode::Disabled)
        .await
        .expect("start");
    println!("relay up at {}", handle.base_url);

    let client = RelayClient::new(cfg.clone());
    let proxy_url = client
        .proxy_url("10.0.0.1", 7878, "/blobs/abc/meta")
        .expect("proxy url");
    println!("proxy URL: {proxy_url}");

    let info = handle.info();
    println!("info: {info:?}");

    // Let the health handler answer a probe.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let health = reqwest::get(format!("{}/health", handle.base_url))
        .await
        .expect("health req")
        .text()
        .await
        .expect("health body");
    assert_eq!(health, "ok");
    println!("health ok");

    handle.shutdown();
}
