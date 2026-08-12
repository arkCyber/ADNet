//! `adnet-mesh-firewall` 应用示例：模拟一个典型的小型 mesh 网络防火墙策略。
//! 演示 SSH/ICMP 默认放行、HTTP 受 peer 限制、conntrack 回流放行。
//!
//! 运行：`cargo run -p adnet-mesh-firewall --example firewall_app`

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use adnet_mesh_firewall::{
    ConnProto, DefaultPolicy, Decision, FirewallConfig, FirewallEngine, FirewallStats, Packet,
    rule::{Action, Direction, PortSpec, ProtoSpec, Rule, RuleSet},
};
use adnet_types::NodeId;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- adnet-mesh-firewall app demo ---");

    let alice = NodeId::random();
    let bob = NodeId::random();
    let mallory = NodeId::random();
    let _stats = Arc::new(FirewallStats::default());

    let mut rs = RuleSet::new();
    rs.push(Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Single(22)));
    rs.push(Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Single(80))
        .with_peer(bob.clone()));
    rs.push(Rule::allow(Direction::In, ProtoSpec::Icmp, PortSpec::Any));

    let cfg = FirewallConfig {
        default_policy: DefaultPolicy {
            inbound_tcp_udp: Action::Deny,
            inbound_icmp: Action::Allow,
            outbound: Action::Allow,
        },
        rules: rs,
        ..Default::default()
    };
    let engine = FirewallEngine::new(cfg, Arc::new(FirewallStats::default()));

    let any_src = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 5));

    let decide = |peer: &NodeId, port: u16, proto: u8| -> (Decision, _) {
        engine.decide(Packet {
            direction: Direction::In,
            proto,
            port,
            peer,
            src_ip: any_src,
        })
    };

    println!("alice ssh  in -> {:?}", decide(&alice, 22, 6).0);
    println!("alice http in -> {:?}", decide(&alice, 80, 6).0);
    println!("bob   http in -> {:?}", decide(&bob, 80, 6).0);
    println!("bob   ssh  in -> {:?}", decide(&bob, 22, 6).0);
    println!(
        "mall ssh  in -> {:?}",
        decide(&mallory, 22, 6).0
    );

    // conntrack：alice 开了到 bob 的 443，bob 的回流允许。
    engine.open_outbound(ConnProto::Tcp, alice.clone(), 443, any_src, 50000)?;
    let (d, _r) = engine.decide(Packet {
        direction: Direction::In,
        proto: 6,
        port: 443,
        peer: &alice,
        src_ip: any_src,
    });
    println!("conntrack: alice→bob:443 return -> {:?}", d);
    assert_eq!(d, Decision::Allow);

    Ok(())
}