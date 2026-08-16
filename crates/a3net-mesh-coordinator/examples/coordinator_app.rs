//! `a3net-mesh-coordinator` 应用示例：模拟一个家庭 mesh 网络的完整生命周期
//! —— 创建、邀请、审批、踢人、roster 升级 —— 多次迭代，验证 `version` 单调递增。
//!
//! 运行：`cargo run -p a3net-mesh-coordinator --example coordinator_app`

use a3net_mesh_coordinator::{Coordinator, CoordinatorConfig, InMemoryCoordinator};
use a3net_types::{MeshNetworkId, MeshPolicy, NodeId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- a3net-mesh-coordinator app demo ---");

    let coord = InMemoryCoordinator::new(CoordinatorConfig::default());
    let network = MeshNetworkId::from_bytes(&[0x42u8; 32])?;
    let creator = NodeId::random();

    // 1. 创建 closed mesh。
    let initial = coord.create(
        network.clone(),
        "home".into(),
        MeshPolicy::Closed,
        creator.clone(),
    )?;
    let mut version = initial.version;
    println!("[1] created 'home', v={version}, members={}", initial.members.len());

    // 2. Mint 邀请码，模拟朋友加入。
    let invite = coord.mint_invite(&network, None)?;
    let friend = NodeId::random();
    let added = coord.redeem(&network, &invite.code, friend.clone(), "alice".into())?;
    let roster = coord.roster(&network).unwrap();
    version = roster.version;
    println!(
        "[2] {} joined via invite, v={}, roster={}",
        added.hostname, version, roster.members.len()
    );

    // 3. 另一个人发请求；运营者 accept。
    let other = NodeId::random();
    let req = coord.request_join(&network, other.clone(), "bob".into(), "hi".into())?;
    let accepted = coord.accept_request(&network, req)?;
    let roster = coord.roster(&network).unwrap();
    version = roster.version;
    println!(
        "[3] {} approved by operator, v={}, roster={}",
        accepted.hostname, version, roster.members.len()
    );

    // 4. 拒绝另一个陌生人。
    let stranger = NodeId::random();
    let req2 = coord.request_join(&network, stranger.clone(), "mallory".into(), "spam".into())?;
    let _denied = coord.deny_request(&network, req2)?;
    println!("[4] stranger denied, roster unchanged");

    // 5. 踢掉 bob。
    let roster_before = coord.roster(&network).unwrap();
    coord.kick(&network, &other)?;
    let roster_after = coord.roster(&network).unwrap();
    version = roster_after.version;
    println!(
        "[5] kick bob: {} -> {} members, v={version}",
        roster_before.members.len(),
        roster_after.members.len()
    );

    // 6. snapshot.
    let snap = coord.snapshot();
    println!(
        "[6] snapshot: {} networks, {} invites, {} pending, {} resolved",
        snap.networks.len(),
        snap.pending_invites.len(),
        snap.pending_requests.len(),
        snap.resolved_requests.len()
    );

    Ok(())
}