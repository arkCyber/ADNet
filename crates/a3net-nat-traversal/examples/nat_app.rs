//! `a3net-nat-traversal` 应用示例：构造 manager、打印 NAT 类型决策表，
//! 演示把 NAT 类型映射到 A3Net 应当采用的链路（direct / hole-punch / TURN）。
//! **不**真正发起网络探测（CI 友好）。
//!
//! 运行：`cargo run -p a3net-nat-traversal --example nat_app`

use a3net_nat_traversal::{NatConfig, NatTraversalManager, NatType};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- a3net-nat-traversal app demo ---");

    let cfg = NatConfig {
        stun_enabled: true,
        upnp_enabled: true,
        turn_enabled: true,
        hole_punch_enabled: true,
        detect_nat_type: true,
        ..Default::default()
    };
    let _mgr = NatTraversalManager::new(cfg)?;
    println!("manager ready");

    for nt in [
        NatType::OpenInternet,
        NatType::FullCone,
        NatType::RestrictedCone,
        NatType::PortRestrictedCone,
        NatType::Symmetric,
        NatType::Unknown,
    ] {
        let link = if nt.supports_direct_p2p() {
            "direct"
        } else if nt.supports_hole_punching() {
            "hole-punch"
        } else {
            "TURN relay"
        };
        println!("  {:>20} -> {}", format!("{:?}", nt), link);
    }
    Ok(())
}