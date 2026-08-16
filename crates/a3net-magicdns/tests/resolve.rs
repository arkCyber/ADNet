//! Integration tests for `a3net-magicdns`.

use a3net_magicdns::{MagicError, MagicName, Resolver, ResolverConfig};
use a3net_types::{MeshMember, MeshMembership, MeshNetworkId, NodeId, VirtualIp};

fn roster_with(network: &[u8], hosts: &[&str]) -> (MeshNetworkId, MeshMembership) {
    let nid = MeshNetworkId::from_bytes(network).unwrap();
    let mut roster = MeshMembership::new_unsigned(nid.clone(), vec![]);
    for (i, host) in hosts.iter().enumerate() {
        let id_bytes = [i as u8; 32];
        let node_id = NodeId::from_bytes(&id_bytes).unwrap();
        roster
            .members
            .push(MeshMember::new_member(node_id, *host));
    }
    (nid, roster)
}

#[test]
fn full_form_resolves_to_member_vip() {
    let (nid, roster) = roster_with(&[1u8; 32], &["alice", "bob"]);
    let resolver = Resolver::new(ResolverConfig::default());
    resolver.apply_roster("gaming", &roster);
    let vip = resolver.resolve_str("alice.gaming.ray", None).unwrap();
    let alice_node = roster.members.iter().find(|m| m.hostname == "alice").unwrap();
    assert_eq!(vip, VirtualIp::from_node_id(&alice_node.node_id));
    let _ = nid;
}

#[test]
fn short_form_resolves() {
    let (_nid, roster) = roster_with(&[2u8; 32], &["alice"]);
    let resolver = Resolver::new(ResolverConfig::default());
    resolver.apply_roster("gaming", &roster);
    let vip = resolver.resolve_str("alice.gaming", None).unwrap();
    assert_eq!(
        vip,
        VirtualIp::from_node_id(&roster.members[0].node_id)
    );
}

#[test]
fn flat_form_walks_all_networks() {
    let (_, r1) = roster_with(&[1u8; 32], &["alice"]);
    let (_, r2) = roster_with(&[2u8; 32], &["bob"]);
    let resolver = Resolver::new(ResolverConfig::default());
    resolver.apply_roster("net1", &r1);
    resolver.apply_roster("net2", &r2);
    let vip1 = resolver.resolve_str("alice.ray", None).unwrap();
    let vip2 = resolver.resolve_str("bob.ray", None).unwrap();
    assert_eq!(vip1, VirtualIp::from_node_id(&r1.members[0].node_id));
    assert_eq!(vip2, VirtualIp::from_node_id(&r2.members[0].node_id));
}

#[test]
fn unknown_network_returns_typed_error() {
    let resolver = Resolver::new(ResolverConfig::default());
    let err = resolver.resolve_str("alice.gaming.ray", None).unwrap_err();
    match err {
        MagicError::UnknownNetwork(name) => assert_eq!(name, "gaming"),
        _ => panic!("expected UnknownNetwork"),
    }
}

#[test]
fn unknown_host_returns_typed_error() {
    let (_, roster) = roster_with(&[1u8; 32], &["alice"]);
    let resolver = Resolver::new(ResolverConfig::default());
    resolver.apply_roster("gaming", &roster);
    let err = resolver.resolve_str("ghost.gaming.ray", None).unwrap_err();
    match err {
        MagicError::UnknownHost(host, net) => {
            assert_eq!(host, "ghost");
            assert_eq!(net, "gaming");
        }
        _ => panic!("expected UnknownHost"),
    }
}

#[test]
fn replace_roster_purges_old_hosts() {
    let resolver = Resolver::new(ResolverConfig::default());
    let (_, r1) = roster_with(&[1u8; 32], &["alice", "bob"]);
    resolver.apply_roster("gaming", &r1);
    let (_, r2) = roster_with(&[1u8; 32], &["carol"]);
    resolver.apply_roster("gaming", &r2);
    let snap = resolver.snapshot();
    assert_eq!(snap.entries.len(), 1);
    assert_eq!(snap.entries[0].entries.len(), 1);
    assert_eq!(snap.entries[0].entries[0].0, "carol");
}

#[test]
fn snapshot_roundtrips_via_serde() {
    let resolver = Resolver::new(ResolverConfig::default());
    let (_, roster) = roster_with(&[1u8; 32], &["alice"]);
    resolver.apply_roster("gaming", &roster);
    let snap = resolver.snapshot();
    let json = serde_json::to_string(&snap).unwrap();
    let back: a3net_magicdns::ResolverSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back.entries.len(), snap.entries.len());
    assert_eq!(back.flat_index.len(), snap.flat_index.len());
}

#[test]
fn parse_full_and_render_back() {
    let n = MagicName::parse("alice.gaming.ray").unwrap();
    let back = n.full_name();
    assert_eq!(back, "alice.gaming.ray");
}

#[test]
fn parse_with_punycode_like_labels() {
    // Hyphens are allowed mid-label.
    let n = MagicName::parse("web-1.gaming.ray").unwrap();
    assert_eq!(n.hostname, "web-1");
}

#[test]
fn parse_rejects_underscore() {
    assert!(MagicName::parse("web_1.gaming.ray").is_err());
}

#[test]
fn parse_accepts_numeric_only_hostname() {
    let n = MagicName::parse("42.gaming.ray").unwrap();
    assert_eq!(n.hostname, "42");
}

#[test]
fn resolve_after_apply_does_not_leak_across_resolvers() {
    // Two independent resolvers should not see each
    // other's rosters.
    let r1 = Resolver::new(ResolverConfig::default());
    let r2 = Resolver::new(ResolverConfig::default());
    let (_, roster) = roster_with(&[1u8; 32], &["alice"]);
    r1.apply_roster("gaming", &roster);
    assert!(r1.resolve_str("alice.gaming.ray", None).is_ok());
    let err = r2.resolve_str("alice.gaming.ray", None).unwrap_err();
    assert!(matches!(err, MagicError::UnknownNetwork(_)));
}
