//! Tiny example: build a small Kademlia-style routing table, add a
//! handful of peers, and ask for the k closest to a target.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-verify --example verify_basic
//! ```

use adnet_types::NodeId;
use adnet_verify::{RoutingTable, xor_distance};

fn main() {
    let me = NodeId::random();
    let mut table = RoutingTable::new(me.clone(), 3);

    // 1. Populate the table with random peers.
    let peers: Vec<NodeId> = (0..5).map(|_| NodeId::random()).collect();
    for p in &peers {
        let added = table.add_peer(p.clone());
        println!("add_peer({}) -> {}", p.short(), added);
        assert!(added, "first insert of a fresh peer should succeed");
    }

    // 2. Duplicate insertion is rejected.
    let dup = table.add_peer(peers[0].clone());
    assert!(!dup, "duplicate insert should be rejected");
    println!("duplicate rejected: ok");

    // 3. Get the k closest to a target.
    let target = NodeId::random();
    let closest = table.get_k_closest(&target, 3);
    println!("\nk=3 closest to {}:", target.short());
    for c in &closest {
        let dist = xor_distance(c, &target);
        println!("  {}  distance={dist}", c.short());
    }
    assert!(closest.len() <= 3);

    // 4. XOR distance is symmetric and zero against self.
    let a = NodeId::random();
    let b = NodeId::random();
    assert_eq!(xor_distance(&a, &a), 0);
    assert_eq!(xor_distance(&a, &b), xor_distance(&b, &a));
    println!("xor properties: ok");
}
