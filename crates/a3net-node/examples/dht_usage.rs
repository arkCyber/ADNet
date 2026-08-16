//! Example: Using DHT routing in a3net-node.
//!
//! This example demonstrates how to:
//! 1. Create a DHT handle attached to a node
//! 2. Announce content when imported
//! 3. Find providers for content via DHT
//! 4. Use IPNS for mutable naming
//!
//! Run with: `cargo run --example dht_usage --features a3net-node/dht`

#![cfg(feature = "dht")]

use std::sync::Arc;
use std::time::Duration;

use a3net_node::{DhtConfig, DhtHandle, IpnConfig};
use a3net_namespace::Ed25519SecretKey;
use a3net_types::{ContentHash, NodeId};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let node_id = NodeId::random();

    // DHT configuration
    let dht_config = DhtConfig {
        bootstrap_nodes: Vec::new(),
        provider_interval: Duration::from_secs(3600),
        refresh_interval: Duration::from_secs(300),
        contact_timeout: Duration::from_secs(600),
        k_bucket_size: 20,
        local_id: node_id.clone(),
    };

    // IPNS configuration
    let ipns_config = IpnConfig {
        cache_ttl_secs: 3600,
        record_ttl_secs: 3600,
    };

    let secret_key: Arc<dyn a3net_namespace::SecretKey> = Arc::new(Ed25519SecretKey::generate());

    println!("Node ID: {}", node_id.short());

    let dht = DhtHandle::new(dht_config).await;

    println!("DHT handle created for: {}", dht.local_id());

    let content_hash = ContentHash::from_bytes(b"example content");
    dht.provide(&content_hash).await;

    println!("Announced content: {}", content_hash.short());

    let providers = dht.find_providers(&content_hash).await;
    println!("Found {} providers", providers.len());

    // Get DHT stats
    let stats = dht.stats();
    println!("DHT Stats: {}", stats);

    Ok(())
}
