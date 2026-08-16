//! `a3net-nat-traversal` 最小示例：构造 `NatConfig` 与 `NatTraversalManager`，
//! 打印默认 STUN servers 与 NAT 类型分类，**不**真正发起网络探测（CI 不希望打洞）。
//!
//! 运行：`cargo run -p a3net-nat-traversal --example nat_basic`

use a3net_nat_traversal::{NatConfig, NatTraversalManager, NatType};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = NatConfig::default();
    println!("stun enabled : {}", cfg.stun_enabled);
    println!("stun servers : {}", cfg.stun_servers.len());
    println!("upnp enabled : {}", cfg.upnp_enabled);
    println!("turn enabled : {}", cfg.turn_enabled);
    println!("hole punch   : {}", cfg.hole_punch_enabled);

    let _mgr = NatTraversalManager::new(cfg);

    for nt in [
        NatType::OpenInternet,
        NatType::FullCone,
        NatType::Symmetric,
        NatType::Unknown,
    ] {
        println!(
            "{:?}: direct={} hole_punch={} turn={}",
            nt,
            nt.supports_direct_p2p(),
            nt.supports_hole_punching(),
            nt.requires_turn()
        );
    }
    Ok(())
}