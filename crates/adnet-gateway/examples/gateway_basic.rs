//! `adnet-gateway` 最小示例：构造一份 `GatewayConfig`，挂上 metrics，
//! 展示如何把它和 `BitswapAppState` 组合起来（不实际启动 axum 监听）。
//!
//! 运行：`cargo run -p adnet-gateway --example gateway_basic`

use adnet_gateway::{bitswap_api::BitswapAppState, metrics::GatewayMetrics, GatewayConfig};
use adnet_observability::metrics::Metric;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = GatewayConfig {
        bind_addr: "127.0.0.1:8080".into(),
        cors_enabled: true,
        writable: false,
        enable_ipns: true,
        rate_limit: 100,
        ..Default::default()
    };
    println!("bind          : {}", cfg.bind_addr);
    println!("writable      : {}", cfg.writable);
    println!("ipns enabled  : {}", cfg.enable_ipns);
    println!("rate limit    : {} req / {}s", cfg.rate_limit, cfg.rate_limit_window);

    let registry = std::sync::Arc::new(adnet_observability::registry::Registry::default());
    let metrics = GatewayMetrics::register(&registry);
    println!("metrics       : {}", metrics.requests_total.name());

    // BitswapAppState 演示构造 — 真实部署会传入 BitswapApi 实例。
    let _state = BitswapAppState::default();
    println!("bitswap state : ok");

    Ok(())
}