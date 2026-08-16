//! Realistic example: a small Bitswap-style ledger and a want list
//! that the runtime would maintain per peer. The verification
//! invariants are the same that the Kani proofs in this crate
//! formalise (the proofs are gated behind the `kani` feature).
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-verify --example verify_app
//! ```

use a3net_verify::bitswap::{BitswapInvariants, LedgerBook, LedgerEntry, WantEntry, WantList};

fn main() {
    // 1. Want list — peers we are asking for blocks from.
    let mut want = WantList::new();
    want.add(WantEntry::new(b"cid-1".to_vec(), 1));
    want.add(WantEntry::new(b"cid-2".to_vec(), 5));
    want.add(WantEntry::new(b"cid-3".to_vec(), 2));
    // Duplicate `cid-1` is ignored — same invariant the Kani proof
    // `proof_wantlist_dedup` checks.
    want.add(WantEntry::new(b"cid-1".to_vec(), 9));
    assert_eq!(want.entries.len(), 3);
    println!("want list size: {}", want.entries.len());
    let top = want.top().expect("top");
    assert_eq!(top.cid, b"cid-2");
    println!("top priority: {} (priority={})",
        String::from_utf8_lossy(&top.cid), top.priority);

    // 2. The want list must not contain any CIDs we already have.
    let have = vec![b"cid-3".to_vec()];
    let valid = BitswapInvariants::wantlist_valid(&want, &have);
    assert!(!valid, "want list contains a CID we already have");
    println!("invariant (wantlist ⊆ complement(have)): ok");

    // 3. Ledger book — bandwidth accounting per peer.
    let mut book = LedgerBook::new();
    {
        let peer = book.get_or_create(b"peer-A");
        peer.record_sent(1000);
        peer.record_received(500);
    }
    {
        let peer = book.get_or_create(b"peer-B");
        peer.record_sent(50);
        peer.record_received(200);
    }

    let debt_a = book.balance(b"peer-A").unwrap_or(0.0);
    let debt_b = book.balance(b"peer-B").unwrap_or(0.0);
    println!("peer-A debt ratio: {debt_a:.2}");
    println!("peer-B debt ratio: {debt_b:.2}");

    // peer-A: 1000 sent / 500 received = 2.0
    // peer-B:  50 sent / 200 received = 0.25
    assert!((debt_a - 2.0).abs() < 1e-9);
    assert!((debt_b - 0.25).abs() < 1e-9);

    // 4. `is_balanced` is the per-peer gating the gossip layer uses
    //    to decide whether to keep the connection open.
    let peer_a: LedgerEntry = book.get_or_create(b"peer-A").clone();
    println!("peer-A is balanced at 1.0? {}", peer_a.is_balanced(1.0));
    assert!(!peer_a.is_balanced(1.0));
    println!("peer-A is balanced at 2.0? {}", peer_a.is_balanced(2.0));
    assert!(peer_a.is_balanced(2.0));

    // 5. The non-negative debt ratio invariant holds.
    assert!(BitswapInvariants::debt_ratio_valid(&peer_a));
    assert!(BitswapInvariants::ledger_balance_valid(&peer_a));
    println!("debt invariants: ok");
}
