//! `a3net-mesh-coordinator` end-to-end demo.

use a3net_mesh_coordinator::{Coordinator, CoordinatorConfig, InMemoryCoordinator};
use a3net_types::{MeshNetworkId, MeshPolicy, NodeId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let coord = InMemoryCoordinator::new(CoordinatorConfig::default());
    let network = MeshNetworkId::from_bytes(&[1u8; 32])?;
    let creator = NodeId::random();

    // 1. Create a closed network.
    let initial = coord.create(
        network.clone(),
        "gaming".into(),
        MeshPolicy::Closed,
        creator.clone(),
    )?;
    println!(
        "network created: members={}, version={}",
        initial.members.len(),
        initial.version
    );

    // 2. Mint an invite code, hand it to a friend.
    let invite = coord.mint_invite(&network, None)?;
    println!("invite minted: {}", invite.encode_url());

    // 3. Friend redeems it.
    let friend = NodeId::random();
    let member = coord.redeem(&network, &invite.code, friend.clone(), "alice".into())?;
    println!(
        "admitted: hostname={} node_id={}",
        member.hostname,
        member.node_id.short()
    );

    // 4. Another peer tries to join without an invite.
    let other = NodeId::random();
    let req_id = coord.request_join(
        &network,
        other.clone(),
        "bob".into(),
        "hi, can I join?".into(),
    )?;
    println!("join request: {}", req_id);

    // 5. Operator approves.
    let new_member = coord.accept_request(&network, req_id)?;
    println!(
        "approved: hostname={} node_id={}",
        new_member.hostname,
        new_member.node_id.short()
    );

    // 6. Operator kicks the second member.
    coord.kick(&network, &other)?;
    let r = coord.roster(&network).unwrap();
    println!(
        "after kick: members={} version={}",
        r.members.len(),
        r.version
    );

    // 7. Snapshot.
    let snap = coord.snapshot();
    println!(
        "snapshot: {} networks, {} pending invites, {} pending requests, {} resolved",
        snap.networks.len(),
        snap.pending_invites.len(),
        snap.pending_requests.len(),
        snap.resolved_requests.len()
    );

    Ok(())
}
