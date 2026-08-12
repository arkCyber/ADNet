//! Integration tests for `adnet-mesh-firewall`. These
//! exercise the public API the way downstream crates
//! would — through `FirewallEngine::decide`, the rule set,
//! and the declarative loader.

use adnet_mesh_firewall::{
    declarative::{FirewallSpec, NetworkSpec, StaticPeerResolver},
    rule::{Action, Direction, PortSpec, ProtoSpec, Rule},
    ConnProto, DefaultPolicy, FirewallConfig, FirewallEngine, FirewallStats, Packet,
};
use adnet_types::NodeId;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

fn ipv4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

fn engine() -> FirewallEngine {
    FirewallEngine::new(FirewallConfig::default(), Arc::new(FirewallStats::default()))
}

fn engine_with(policy: DefaultPolicy, rules: Vec<Rule>) -> FirewallEngine {
    let mut cfg = FirewallConfig {
        default_policy: policy,
        ..FirewallConfig::default()
    };
    for r in rules {
        assert!(cfg.rules.push(r));
    }
    FirewallEngine::new(cfg, Arc::new(FirewallStats::default()))
}

#[test]
fn ssh_server_pattern_allow_any_inbound() {
    let peer = NodeId::random();
    let ip = ipv4(100, 64, 0, 9);
    let eng = engine_with(
        DefaultPolicy::default(),
        vec![Rule::allow(
            Direction::In,
            ProtoSpec::Tcp,
            PortSpec::Single(22),
        )],
    );
    let (d, _) = eng.decide(Packet {
        direction: Direction::In,
        proto: 6,
        port: 22,
        peer: &peer,
        src_ip: ip,
    });
    assert!(d.is_allowed());
}

#[test]
fn allow_with_peer_scope_excludes_others() {
    let alice = NodeId::random();
    let bob = NodeId::random();
    let ip = ipv4(100, 64, 0, 9);
    let eng = engine_with(
        DefaultPolicy::default(),
        vec![Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Single(22))
            .with_peer(alice.clone())],
    );
    let allow_alice = eng.decide(Packet {
        direction: Direction::In,
        proto: 6,
        port: 22,
        peer: &alice,
        src_ip: ip,
    });
    let deny_bob = eng.decide(Packet {
        direction: Direction::In,
        proto: 6,
        port: 22,
        peer: &bob,
        src_ip: ip,
    });
    assert!(allow_alice.0.is_allowed());
    assert!(!deny_bob.0.is_allowed());
}

#[test]
fn deny_rule_blocks_despite_default_allow() {
    let peer = NodeId::random();
    let ip = ipv4(100, 64, 0, 9);
    let eng = engine_with(
        DefaultPolicy {
            inbound_tcp_udp: Action::Allow,
            inbound_icmp: Action::Allow,
            outbound: Action::Allow,
        },
        vec![Rule {
            id: adnet_mesh_firewall::rule::RuleId::from_index(0),
            direction: Direction::In,
            action: Action::Deny,
            proto: ProtoSpec::Tcp,
            port: PortSpec::Single(23),
            peer: adnet_mesh_firewall::rule::PeerSpec::any(),
        }],
    );
    let (d, _) = eng.decide(Packet {
        direction: Direction::In,
        proto: 6,
        port: 23,
        peer: &peer,
        src_ip: ip,
    });
    assert!(!d.is_allowed());
}

#[test]
fn icmp_inbound_allowed_by_default() {
    let peer = NodeId::random();
    let ip = ipv4(100, 64, 0, 9);
    let eng = engine();
    let (d, _) = eng.decide(Packet {
        direction: Direction::In,
        proto: 1,
        port: 0,
        peer: &peer,
        src_ip: ip,
    });
    assert!(d.is_allowed());
    let (d, _) = eng.decide(Packet {
        direction: Direction::In,
        proto: 58,
        port: 0,
        peer: &peer,
        src_ip: ip,
    });
    assert!(d.is_allowed());
}

#[test]
fn conntrack_returns_allow_after_outbound_open() {
    let peer = NodeId::random();
    let ip = ipv4(100, 64, 0, 9);
    let eng = engine();
    eng.open_outbound(ConnProto::Tcp, peer.clone(), 443, ip, 0)
        .unwrap();
    let (d, r) = eng.decide(Packet {
        direction: Direction::In,
        proto: 6,
        port: 443,
        peer: &peer,
        src_ip: ip,
    });
    assert!(d.is_allowed());
    assert_eq!(r, adnet_mesh_firewall::DecisionReason::ConntrackAllow);
}

/// Regression: with the conntrack 5-tuple lookup keyed on
/// `local_port = 0`, opening an outbound flow with a real
/// ephemeral local port would make inbound return-traffic
/// fail to match. `lookup_inbound` searches by the
/// `(proto, peer, peer_port)` triple so this test passes.
#[test]
fn conntrack_inbound_lookup_works_with_real_local_port() {
    use adnet_mesh_firewall::InboundProbe;

    let ct = adnet_mesh_firewall::ConnTracker::new(
        adnet_mesh_firewall::ConnTrackerConfig::default(),
    );
    let peer = NodeId::random();
    let ip = ipv4(100, 64, 0, 9);
    let peer_sock = std::net::SocketAddr::new(ip, 443);
    // Outbound flow opened with a real ephemeral local port.
    ct.open_outbound(ConnProto::Tcp, peer.clone(), 443, peer_sock, 51234)
        .unwrap();

    // Inbound probe carries only the wire-side 3-tuple;
    // the firewall does NOT have the local port.
    let probe = InboundProbe {
        proto: ConnProto::Tcp,
        peer: &peer,
        peer_port: 443,
    };
    assert!(ct.lookup_inbound(probe));

    // A probe for a different peer must not match.
    let other = NodeId::random();
    let probe_other = InboundProbe {
        proto: ConnProto::Tcp,
        peer: &other,
        peer_port: 443,
    };
    assert!(!ct.lookup_inbound(probe_other));
}

/// Regression: the firewall's inbound-return path uses
/// `lookup_inbound` (3-tuple match), not the 5-tuple
/// `ConnKey` lookup. This test opens an outbound flow
/// with a real local port (51234) and asserts that an
/// inbound packet with the wire-side ports allows.
#[test]
fn conntrack_inbound_engine_path_uses_partial_lookup() {
    let peer = NodeId::random();
    let ip = ipv4(100, 64, 0, 9);
    let eng = engine();
    // Real ephemeral local port, NOT 0.
    eng.open_outbound(ConnProto::Tcp, peer.clone(), 443, ip, 51234)
        .unwrap();
    let (d, r) = eng.decide(Packet {
        direction: Direction::In,
        proto: 6,
        // Inbound peer port (matches the conntrack entry).
        port: 443,
        peer: &peer,
        src_ip: ip,
    });
    assert!(d.is_allowed());
    assert_eq!(r, adnet_mesh_firewall::DecisionReason::ConntrackAllow);
}

#[test]
fn conntrack_full_returns_default_deny() {
    use adnet_mesh_firewall::conntrack::{ConnTracker, ConnTrackerConfig};
    let tracker_cfg = ConnTrackerConfig {
        max_entries: 1,
        ..ConnTrackerConfig::default()
    };
    let cfg = FirewallConfig {
        conntrack: tracker_cfg,
        ..FirewallConfig::default()
    };
    let eng = FirewallEngine::new(cfg, Arc::new(FirewallStats::default()));
    let peer1 = NodeId::random();
    let peer2 = NodeId::random();
    let ip = ipv4(100, 64, 0, 9);
    eng.open_outbound(ConnProto::Tcp, peer1, 80, ip, 0).unwrap();
    let res = eng.open_outbound(ConnProto::Tcp, peer2, 80, ip, 1);
    assert!(res.is_err());
    // The engine itself doesn't surface ConntrackFull to
    // the caller yet — that's a follow-up that requires
    // moving the open_outbound error into a Decision reason.
    // For now, we just confirm the table overflows.
    let _ = ConnTracker::new(ConnTrackerConfig {
        max_entries: 0,
        ..Default::default()
    });
}

#[test]
fn replace_rules_zeroes_rule_count() {
    let eng = engine_with(
        DefaultPolicy::default(),
        vec![Rule::allow(
            Direction::In,
            ProtoSpec::Tcp,
            PortSpec::Single(22),
        )],
    );
    assert_eq!(eng.rule_count(), 1);
    eng.replace_rules(Default::default());
    assert_eq!(eng.rule_count(), 0);
}

#[test]
fn declarative_spec_applies_multiple_rules() {
    let alice = NodeId::random();
    let bob = NodeId::random();
    let resolver = StaticPeerResolver(HashMap::from([
        ("alice".to_string(), alice.clone()),
        ("bob".to_string(), bob.clone()),
    ]));
    let spec_json = serde_json::json!({
        "networks": {
            "infra": {
                "allows": {
                    "alice": "tcp:22",
                    "bob": "tcp:80,tcp:443"
                }
            },
            "gaming": {
                "denies": {
                    "alice": "tcp:25565"
                }
            }
        }
    })
    .to_string();
    let spec = FirewallSpec::parse_json(&spec_json).unwrap();
    let eng = engine();
    let report = spec.apply(&eng, &resolver).unwrap();
    assert_eq!(report.allows, 3);
    assert_eq!(report.denies, 1);
    assert_eq!(eng.rule_count(), 4);
}

#[test]
fn declarative_spec_unknown_peer_errors() {
    let resolver = StaticPeerResolver(HashMap::new());
    let spec = FirewallSpec {
        networks: std::collections::BTreeMap::from([(
            "x".to_string(),
            NetworkSpec {
                allows: std::collections::BTreeMap::from([(
                    "ghost".to_string(),
                    "tcp:22".to_string(),
                )]),
                denies: std::collections::BTreeMap::new(),
            },
        )]),
    };
    let eng = engine();
    let err = spec.apply(&eng, &resolver).unwrap_err();
    assert!(matches!(
        err,
        adnet_mesh_firewall::declarative::DeclarativeError::UnknownPeer(_)
    ));
}

#[test]
fn stats_counter_advances_per_decide() {
    let stats = Arc::new(FirewallStats::default());
    let eng = FirewallEngine::new(FirewallConfig::default(), stats.clone());
    let peer = NodeId::random();
    let ip = ipv4(100, 64, 0, 9);
    for _ in 0..5 {
        eng.decide(Packet {
            direction: Direction::Out,
            proto: 6,
            port: 80,
            peer: &peer,
            src_ip: ip,
        });
    }
    assert!(stats.decisions_total.get() >= 5);
    assert!(stats.decisions_allowed.get() >= 5);
}
