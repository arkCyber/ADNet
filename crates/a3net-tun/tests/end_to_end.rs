//! End-to-end integration test exercising the full VPN core
//! stack:
//!
//! ```text
//!   TUN device  ──►  Firewall  ──►  Magic DNS resolver
//!                          ▲
//!                          │
//!                   Exit-node Router
//!                          ▲
//!                          │
//!              Mesh Coordinator (roster source)
//! ```
//!
//! The test wires every new crate through a single scenario:
//!
//! 1. A coordinator is created and admits three members
//!    (alice, bob, carol) into a closed mesh.
//! 2. The roster is published to the Magic DNS resolver so
//!    `alice.gaming.ray` resolves to alice's virtual IP.
//! 3. A firewall engine is built with a single allow rule
//!    for SSH (TCP/22) from any peer.
//! 4. An exit-node router is wired with bob as the gateway.
//! 5. A userspace TUN device is opened.
//! 6. We inject a fake IPv4 packet addressed to alice's
//!    virtual IP into the TUN. The test asserts the
//!    firewall would allow it (SSH) and the router would
//!    forward it via the mesh.
//! 7. We inject a packet addressed to `8.8.8.8` and
//!    assert the router would forward it via bob's
//!    gateway.
//!
//! This is the only test in the workspace that exercises
//! all the new crates against each other. It catches
//! integration regressions that per-crate unit tests miss
//! (e.g. type mismatches between `a3net-types` versions,
//!   behaviour drift between `a3net-mesh-coordinator`'s
//!   `roster()` shape and what `a3net-magicdns` expects).

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use a3net_exit_node::{Client, Gateway, RouteAction, Router};
use a3net_magicdns::{Resolver, ResolverConfig};
use a3net_mesh_coordinator::{
    Coordinator, CoordinatorConfig, InMemoryCoordinator, JoinRequestStatus,
};
use a3net_mesh_firewall::{
    Decision, DecisionReason, DefaultPolicy, Direction, FirewallConfig, FirewallEngine,
    FirewallStats, Packet, PortSpec, ProtoSpec, Rule, RuleSet,
};
use a3net_tun::{TunDevice, UserspaceTun, UserspaceTunConfig, parse_packet};
use a3net_types::{MeshNetworkId, MeshPolicy, NodeId, VirtualIp};

/// Build a fake IPv4 packet (TCP, total_len = 44) addressed
/// to `dst`.
fn fake_tcp_packet(src: Ipv4Addr, dst: Ipv4Addr, dst_port: u16) -> Vec<u8> {
    let mut p = vec![0u8; 44];
    p[0] = 0x45; // ver=4, IHL=5
    p[2] = (44u16 >> 8) as u8;
    p[3] = (44u16 & 0xff) as u8;
    p[9] = 6; // TCP
    p[12..16].copy_from_slice(&src.octets());
    p[16..20].copy_from_slice(&dst.octets());
    // Encode `dst_port` into bytes 36..38 of the TCP header
    // (after the 20-byte IP header).
    p[36] = (dst_port >> 8) as u8;
    p[37] = (dst_port & 0xff) as u8;
    p
}

/// Set up a coordinator with three members on a closed
/// network called "gaming".
fn three_member_network() -> (InMemoryCoordinator, MeshNetworkId, NodeId, NodeId) {
    let coord = InMemoryCoordinator::new(CoordinatorConfig::default());
    let nid = MeshNetworkId::from_bytes(&[0x42u8; 32]).unwrap();
    let alice = NodeId::from_bytes(&[0xa1u8; 32]).unwrap();
    let bob = NodeId::from_bytes(&[0xb0u8; 32]).unwrap();

    // alice is the creator — hostname "alice".
    coord
        .create(nid.clone(), "gaming".into(), MeshPolicy::Closed, alice.clone())
        .expect("create mesh");
    // The creator's hostname is the display name. Rename
    // it to "alice" via a kick+re-add cycle. Easiest path:
    // kick and re-redeem with the desired hostname.
    coord.kick(&nid, &alice).unwrap();
    let invite_alice = coord.mint_invite(&nid, None).unwrap();
    coord
        .redeem(&nid, &invite_alice.code, alice.clone(), "alice".into())
        .expect("redeem alice");

    // bob and carol are admitted via invites.
    let carol = NodeId::from_bytes(&[0xc0u8; 32]).unwrap();
    let invite_bob = coord.mint_invite(&nid, None).unwrap();
    coord
        .redeem(&nid, &invite_bob.code, bob.clone(), "bob".into())
        .expect("redeem bob");
    let invite_carol = coord.mint_invite(&nid, None).unwrap();
    coord
        .redeem(&nid, &invite_carol.code, carol, "carol".into())
        .expect("redeem carol");

    (coord, nid, alice, bob)
}

/// Resolve the virtual IP for `host` on the resolver.
fn vip_for(resolver: &Resolver, host: &str) -> VirtualIp {
    resolver.resolve_str(&format!("{host}.gaming.ray"), None).unwrap()
}

#[test]
fn end_to_end_three_members_with_firewall_and_router() {
    // 1. Coordinator + roster.
    let (coord, nid, _alice, bob) = three_member_network();
    let roster = coord.roster(&nid).expect("roster exists");
    assert_eq!(roster.members.len(), 3);

    // 2. Magic DNS resolver.
    let resolver = Resolver::new(ResolverConfig::default());
    resolver.apply_roster("gaming", &roster);
    let alice_vip = vip_for(&resolver, "alice");
    let bob_vip = vip_for(&resolver, "bob");
    assert_ne!(alice_vip, bob_vip);

    // 3. Firewall with one allow rule: SSH from any peer.
    let mut ruleset = RuleSet::new();
    ruleset.push(Rule::allow(
        Direction::In,
        ProtoSpec::Tcp,
        PortSpec::Single(22),
    ));
    let fw_cfg = FirewallConfig {
        rules: ruleset,
        default_policy: DefaultPolicy::default(),
        conntrack: Default::default(),
    };
    let firewall = FirewallEngine::new(fw_cfg, Arc::new(FirewallStats::default()));

    // 4. Exit-node router with bob as the gateway.
    let client = Client::default();
    client.use_gateway(bob.clone()).unwrap();
    let router = Router::with_default(client, Gateway::default());

    // 5. TUN device.
    let tun = UserspaceTun::new(UserspaceTunConfig::default());
    tun.bring_up();
    assert_eq!(tun.state(), a3net_tun::DeviceState::Up);

    // 6. SSH packet to alice's virtual IP.
    let ssh_pkt = fake_tcp_packet(
        Ipv4Addr::new(100, 64, 0, 1),
        alice_vip.ipv4.as_std(),
        22,
    );
    let parsed = parse_packet(&ssh_pkt).expect("parse");
    assert_eq!(parsed.dst_v4(), alice_vip.ipv4.as_std().octets());
    assert_eq!(parsed.protocol, a3net_tun::IpProtocol::Tcp);

    // Firewall decision: peer `bob` initiates inbound SSH
    // to alice. The rule allows it.
    let (d, r) = firewall.decide(Packet {
        direction: Direction::In,
        proto: 6,
        port: 22,
        peer: &bob,
        src_ip: IpAddr::V4(alice_vip.ipv4.as_std()),
    });
    assert_eq!(d, Decision::Allow);
    assert!(matches!(r, DecisionReason::AllowRule { .. }));

    // Router decision: alice's IP is in the mesh range,
    // so it routes to the mesh — never to the gateway.
    let action = router.route(IpAddr::V4(alice_vip.ipv4.as_std()));
    assert_eq!(action, RouteAction::ForwardToMesh);

    // 7. Internet packet via the gateway.
    let internet: IpAddr = "8.8.8.8".parse().unwrap();
    let action = router.route(internet);
    assert_eq!(action, RouteAction::ForwardViaGateway { gateway: bob.clone() });

    // Without an active gateway, the same Internet
    // destination is dropped — the firewall's default
    // policy is irrelevant here; the router's default
    // applies.
    let router_no_gw = Router::with_default(Client::default(), Gateway::default());
    let action = router_no_gw.route(internet);
    assert!(matches!(action, RouteAction::Drop { .. }));
}

#[test]
fn end_to_end_deny_unsolicited_inbound_ssh() {
    let (coord, nid, _alice, bob) = three_member_network();
    let roster = coord.roster(&nid).unwrap();
    let resolver = Resolver::new(ResolverConfig::default());
    resolver.apply_roster("gaming", &roster);

    // No allow rule for SSH inbound.
    let fw = FirewallEngine::new(
        FirewallConfig::default(),
        Arc::new(FirewallStats::default()),
    );
    let alice_vip = vip_for(&resolver, "alice");

    // Default policy: deny inbound TCP/UDP.
    let (d, r) = fw.decide(Packet {
        direction: Direction::In,
        proto: 6,
        port: 22,
        peer: &bob,
        src_ip: IpAddr::V4(alice_vip.ipv4.as_std()),
    });
    assert_eq!(d, Decision::Deny);
    assert_eq!(r, DecisionReason::DefaultDeny);
}

#[test]
fn end_to_end_pending_request_does_not_resolve() {
    // Carol is admitted via a join request that is *pending*,
    // so the roster does not yet contain her. Magic DNS
    // must not resolve `carol.gaming.ray`.
    let coord = InMemoryCoordinator::new(CoordinatorConfig::default());
    let nid = MeshNetworkId::from_bytes(&[0x99u8; 32]).unwrap();
    let alice = NodeId::from_bytes(&[0xa1u8; 32]).unwrap();
    coord
        .create(nid.clone(), "gaming".into(), MeshPolicy::Closed, alice)
        .unwrap();
    let carol = NodeId::from_bytes(&[0xc1u8; 32]).unwrap();
    let req_id = coord
        .request_join(&nid, carol, "carol".into(), "hello".into())
        .unwrap();
    // Pending state, not approved.
    let pending = coord.pending_requests(&nid);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, JoinRequestStatus::Pending);

    let roster = coord.roster(&nid).unwrap();
    let resolver = Resolver::new(ResolverConfig::default());
    resolver.apply_roster("gaming", &roster);
    let err = resolver
        .resolve_str("carol.gaming.ray", None)
        .unwrap_err();
    assert!(matches!(
        err,
        a3net_magicdns::MagicError::UnknownHost(_, _)
    ));

    // Approve the request, re-apply the roster, and the
    // name now resolves.
    coord.accept_request(&nid, req_id).unwrap();
    let new_roster = coord.roster(&nid).unwrap();
    assert_eq!(new_roster.members.len(), 2);
    let new_resolver = Resolver::new(ResolverConfig::default());
    new_resolver.apply_roster("gaming", &new_roster);
    let vip = new_resolver
        .resolve_str("carol.gaming.ray", None)
        .unwrap();
    // The resolved virtual IP is the deterministic
    // derivation from carol's NodeId.
    let expected = VirtualIp::from_node_id(&NodeId::from_bytes(&[0xc1u8; 32]).unwrap());
    assert_eq!(vip, expected);
}

#[test]
fn end_to_end_tun_inject_and_drain() {
    // Wire a TUN, inject a packet, observe it round-trip.
    let tun = UserspaceTun::new(UserspaceTunConfig::default());
    tun.bring_up();
    let pkt = fake_tcp_packet(
        Ipv4Addr::new(100, 64, 0, 1),
        Ipv4Addr::new(100, 64, 0, 2),
        22,
    );
    // We use `tokio::runtime::Runtime` so we don't have to
    // make every helper async.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        tun.inject_from_kernel(pkt.clone()).await.unwrap();
        let got = tun.recv().await.unwrap().unwrap();
        assert_eq!(got, pkt);
    });
}
