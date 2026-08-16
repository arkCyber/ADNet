//! `a3net-magicdns` end-to-end demo.
//!
//! Builds a fake mesh roster, applies it to a resolver,
//! and resolves a handful of names in the three forms
//! supported by the crate.

use a3net_magicdns::{MagicName, Resolver, ResolverConfig};
use a3net_types::{MeshMember, MeshMembership, MeshNetworkId, VirtualIp};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Build a fake two-network roster.
    let mut gaming =
        MeshMembership::new_unsigned(MeshNetworkId::from_bytes(&[1u8; 32])?, vec![]);
    gaming.members.push(MeshMember::new_member(
        a3net_types::NodeId::random(),
        "alice",
    ));
    gaming.members.push(MeshMember::new_member(
        a3net_types::NodeId::random(),
        "bob",
    ));

    let mut infra =
        MeshMembership::new_unsigned(MeshNetworkId::from_bytes(&[2u8; 32])?, vec![]);
    infra.members.push(MeshMember::new_coordinator(
        a3net_types::NodeId::random(),
        "jumpbox",
    ));

    // 2. Apply both rosters.
    let resolver = Resolver::new(ResolverConfig::default());
    resolver.apply_roster("gaming", &gaming);
    resolver.apply_roster("infra", &infra);

    // 3. Resolve in the three forms.
    let full = resolver.resolve_str("alice.gaming.ray", None)?;
    let short = resolver.resolve_str("bob.gaming", None)?;
    let flat = resolver.resolve_str("jumpbox.ray", Some("infra"))?;

    println!("alice.gaming.ray  -> {}", VirtualIp::from_node_id(&gaming.members[0].node_id));
    println!("  resolved:       {}", full);
    println!("bob.gaming        -> {}", VirtualIp::from_node_id(&gaming.members[1].node_id));
    println!("  resolved:       {}", short);
    println!("jumpbox.ray (infra) -> {}", VirtualIp::from_node_id(&infra.members[0].node_id));
    println!("  resolved:       {}", flat);

    // 4. Parse-only example for diagnostics.
    let parsed = MagicName::parse("alice.gaming.ray")?;
    println!(
        "parsed MagicName: host={:?} network={:?} full={}",
        parsed.hostname,
        parsed.network,
        parsed.full_name()
    );

    Ok(())
}
