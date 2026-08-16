//! `a3net-mesh-firewall` end-to-end demo.
//!
//! Demonstrates the rule pipeline: default-deny inbound TCP,
//! default-allow outbound, explicit allow rule override,
//! conntrack return-traffic, and the declarative YAML loader.

use a3net_mesh_firewall::{
    declarative::{FirewallSpec, StaticPeerResolver},
    rule::{Action, Direction, PortSpec, ProtoSpec, Rule},
    ConnProto, DefaultPolicy, FirewallConfig, FirewallEngine, FirewallStats, Packet,
};
use a3net_types::NodeId;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let alice = NodeId::random();
    let bob = NodeId::random();

    // 1. Build the engine with explicit rules.
    let mut config = FirewallConfig {
        default_policy: DefaultPolicy {
            inbound_tcp_udp: Action::Deny,
            inbound_icmp: Action::Allow,
            outbound: Action::Allow,
        },
        ..FirewallConfig::default()
    };
    config.rules.push(Rule::allow(
        Direction::In,
        ProtoSpec::Tcp,
        PortSpec::Single(22),
    ));
    config.rules.push(
        Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Single(80))
            .with_peer(bob.clone()),
    );

    let stats = Arc::new(FirewallStats::default());
    let engine = FirewallEngine::new(config, stats.clone());

    let ip_alice = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 5));
    let ip_bob = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 6));

    // 2. Inbound SSH from anyone → allow (rule 0).
    let (d, r) = engine.decide(Packet {
        direction: Direction::In,
        proto: 6,
        port: 22,
        peer: &alice,
        src_ip: ip_alice,
    });
    println!("alice ssh in  -> {:?} ({})", d, r);

    // 3. Inbound HTTP from alice → default-deny.
    let (d, r) = engine.decide(Packet {
        direction: Direction::In,
        proto: 6,
        port: 80,
        peer: &alice,
        src_ip: ip_alice,
    });
    println!("alice http in -> {:?} ({})", d, r);

    // 4. Inbound HTTP from bob → allow (rule 1, peer-scoped).
    let (d, r) = engine.decide(Packet {
        direction: Direction::In,
        proto: 6,
        port: 80,
        peer: &bob,
        src_ip: ip_bob,
    });
    println!("bob   http in -> {:?} ({})", d, r);

    // 5. Open an outbound flow; return traffic is allow-listed.
    engine.open_outbound(ConnProto::Tcp, bob.clone(), 443, ip_bob, 44444)?;
    let (d, r) = engine.decide(Packet {
        direction: Direction::In,
        proto: 6,
        port: 443,
        peer: &bob,
        src_ip: ip_bob,
    });
    println!("bob   tls ret -> {:?} ({})", d, r);

    // 6. Apply a declarative spec.
    let mut resolver_map = HashMap::new();
    resolver_map.insert("alice".to_string(), alice.clone());
    let resolver = StaticPeerResolver(resolver_map);
    let spec_json = serde_json::json!({
        "networks": {
            "infra": {
                "allows": { "alice": "tcp:22,udp:53" }
            }
        }
    })
    .to_string();
    let spec = FirewallSpec::parse_json(&spec_json)?;
    let report = spec.apply(&engine, &resolver)?;
    println!(
        "declarative apply: allows={}, denies={}",
        report.allows, report.denies
    );

    Ok(())
}
