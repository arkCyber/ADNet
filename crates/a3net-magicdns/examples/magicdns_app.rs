//! `a3net-magicdns` 应用示例：构造一个 resolver，应用两个网络 roster，
//! 然后批量解析一组名字并展示 `VirtualIp` 的确定性。
//!
//! 运行：`cargo run -p a3net-magicdns --example magicdns_app`

use a3net_magicdns::{Resolver, ResolverConfig};
use a3net_types::{MeshMember, MeshMembership, MeshNetworkId, VirtualIp};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- a3net-magicdns app demo ---");

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

    let resolver = Resolver::new(ResolverConfig::default());
    resolver.apply_roster("gaming", &gaming);
    resolver.apply_roster("infra", &infra);

    let queries = [
        ("alice.gaming.ray", None),
        ("bob.gaming", None),
        ("jumpbox.ray", Some("infra")),
        ("alice.ray", Some("gaming")),
    ];

    for (name, hint) in queries {
        let vip = resolver.resolve_str(name, hint)?;
        let expected = match name {
            "alice.gaming.ray" | "alice.ray" => {
                VirtualIp::from_node_id(&gaming.members[0].node_id)
            }
            "bob.gaming" => VirtualIp::from_node_id(&gaming.members[1].node_id),
            "jumpbox.ray" => VirtualIp::from_node_id(&infra.members[0].node_id),
            _ => unreachable!(),
        };
        println!(
            "{:20} -> {} (matches VirtualIp::from_node_id: {})",
            name, vip, vip == expected
        );
    }

    // Snapshot 用作诊断输出。
    let snap = resolver.snapshot();
    println!(
        "snapshot: {} networks, {} flat entries",
        snap.entries.len(),
        snap.flat_index.len()
    );

    Ok(())
}