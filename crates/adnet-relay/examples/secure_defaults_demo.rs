//! Demo: start the relay with the **default secure** policy and probe
//! `/healthz` to see what the live policy looks like.
//!
//! Run with: `cargo run -p adnet-relay --example secure_defaults_demo`
//!
//! This is the canonical configuration for any production deployment:
//!
//! - `HostPolicy::DefaultBlockPrivate` — refuses to forward to
//!   loopback, RFC1918, link-local, multicast, CGN, or cloud-metadata
//!   addresses (both IP literals and after DNS resolution).
//! - `max_body_bytes` = 16 MiB — caps the relay's per-response memory
//!   usage from upstream.
//! - `upstream_timeout` = 60 s — short enough to detect abuse, long
//!   enough for legitimate slow blobs.
//! - `max_redirects` = 3 — and each destination is re-validated.

use adnet_relay::{BillingMode, RelayServer, ServerPolicy};
use std::time::Duration;

#[tokio::main]
async fn main() {
    let policy = ServerPolicy::default();
    let handle = RelayServer::start_with_policy("127.0.0.1", 18791, BillingMode::Disabled, policy)
        .await
        .expect("relay start");
    println!("relay up at {}", handle.base_url);

    // Probe /healthz for the live policy view.
    let body: serde_json::Value = reqwest::get(format!("{}/healthz", handle.base_url))
        .await
        .expect("healthz req")
        .json()
        .await
        .expect("json");
    println!("active policy: {body:#?}");

    // Sleep briefly so the demo doesn't return immediately.
    tokio::time::sleep(Duration::from_millis(50)).await;

    handle.shutdown();
}
