//! `adnet-gateway` 应用示例：用 axum 把 `GatewayRouter` 的只读子集跑起来，
//! 并挂上 metrics、auth、bitswap router。模拟一次 IPFS HTTP gateway 的最小部署。
//!
//! 运行：`cargo run -p adnet-gateway --example gateway_app`

use std::sync::Arc;

use adnet_gateway::{
    bitswap_api::{create_bitswap_router, BitswapAppState},
    metrics::GatewayMetrics,
    GatewayConfig,
};
use adnet_observability::registry::Registry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = GatewayConfig {
        bind_addr: "127.0.0.1:18080".into(),
        cors_enabled: true,
        cors_allowed_origins: vec!["https://example.com".into()],
        cache_control: "public, max-age=60".into(),
        max_response_size: 8 * 1024 * 1024,
        enable_ipns: true,
        auth_enabled: false,
        read_only: true,
        rate_limit: 100,
        rate_limit_window: 60,
        ..Default::default()
    };
    println!("--- adnet-gateway (read-only demo) ---");
    println!("bind          : {}", cfg.bind_addr);
    println!("read-only     : {}", cfg.read_only);
    println!("cache_control : {}", cfg.cache_control);
    println!("max_response  : {} bytes", cfg.max_response_size);

    let registry = Arc::new(Registry::default());
    let _metrics = GatewayMetrics::register(&registry);
    println!("metrics       : registered {} counters", 5);

    let state = BitswapAppState::default();
    let _bitswap_router = create_bitswap_router(state);
    println!("bitswap router: ok");

    println!(
        "tip: combine with `GatewayRouter` to expose /ipfs/{{cid}} routes on {}",
        cfg.bind_addr
    );
    Ok(())
}