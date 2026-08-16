//! Bitswap Wantlist invariant integration tests (P1-2).
//!
//! These tests exercise the *real* `PeerWantlist` from
//! `a3net_blobstore::bitswap_wantlist` against the same
//! invariant catalogue proven (by Kani) on the abstract model in
//! `a3net_verify::bitswap_invariants`. Running both sides gives
//! end-to-end coverage:
//!
//! - The abstract model is exhaustively verified by `cargo kani`.
//! - The real code is fuzzed by `proptest` here, in normal CI.
//!
//! A regression in either side fails its respective test; this
//! integration suite fails if the real code drifts from the model.
//!
//! Run with:
//!
//! ```bash
//! cargo test -p a3net-blobstore --test bitswap_invariants --features bitswap
//! ```

#![cfg(feature = "bitswap")]

use std::collections::HashSet;

use a3net_blobstore::bitswap::BitswapMessage;
use a3net_blobstore::bitswap_wantlist::PeerWantlist;
use a3net_types::ContentHash;
use proptest::prelude::*;

// ─────────────────────────────────────────────────────────────────
// Invariant catalogue — mirrors `a3net_verify::bitswap_invariants`
// ─────────────────────────────────────────────────────────────────

/// All eight invariants the wantlist must uphold after every
/// operation. Each is checked individually so a failure points at
/// the exact rule that broke.
fn check_pending_subset(wl: &PeerWantlist) -> Result<(), &'static str> {
    for block in wl.pending_blocks() {
        if !wl.contains(&block) {
            return Err("pending-subset-of-wants: a block is in `pending` but not in `wants`");
        }
    }
    Ok(())
}

fn check_len_matches_map(wl: &PeerWantlist) -> Result<(), &'static str> {
    if wl.len() != wl.entries().len() {
        return Err("len-matches-map: wl.len() must equal number of entries");
    }
    Ok(())
}

fn check_remove_clears_both(wl: &PeerWantlist, k: &ContentHash) -> Result<(), &'static str> {
    if wl.contains(k) {
        return Err("remove-clears-both: contains(k) must be false after remove_want(k)");
    }
    if wl.is_pending(k) {
        return Err("remove-clears-both: is_pending(k) must be false after remove_want(k)");
    }
    Ok(())
}

fn check_mark_received_clears_pending(wl: &PeerWantlist, k: &ContentHash) -> Result<(), &'static str> {
    if wl.is_pending(k) {
        return Err("mark-received-clears-pending: is_pending(k) must be false after mark_received(k)");
    }
    // After mark_received, contains(k) may still be true (we may still
    // want the block; we just received a response). So we don't assert
    // !contains(k) here.
    Ok(())
}

fn check_mark_synced_resets_dirty(wl: &PeerWantlist) -> Result<(), &'static str> {
    if wl.is_dirty() {
        return Err("mark-synced-resets-dirty: is_dirty() must be false after mark_synced()");
    }
    Ok(())
}

fn check_add_idempotent_len(
    before_len: usize,
    after_len: usize,
) -> Result<(), &'static str> {
    if before_len != after_len {
        return Err("add-same-key-is-idempotent: re-adding same key must not grow len()");
    }
    Ok(())
}

fn check_want_messages_cover(wl: &PeerWantlist) -> Result<(), &'static str> {
    let msgs = wl.to_want_messages();
    if msgs.len() != wl.len() {
        return Err("want-messages-covers-wants: to_want_messages().len() must equal wl.len()");
    }
    // Each emitted message's block must be a key in the want map.
    let want_keys: HashSet<_> = wl.entries().iter().map(|e| e.block.clone()).collect();
    for msg in &msgs {
        let block = match msg {
            BitswapMessage::WantBlock { block, .. } => block,
            BitswapMessage::WantHave { block, .. } => block,
            _ => continue, // only WantBlock / WantHave matter here
        };
        if !want_keys.contains(block) {
            return Err("want-messages-covers-wants: a want message references an unknown block");
        }
    }
    Ok(())
}

fn check_cancel_messages_cover(wl: &PeerWantlist) -> Result<(), &'static str> {
    let msgs = wl.to_cancel_messages();
    if msgs.len() != wl.len() {
        return Err("cancel-messages-covers-wants: to_cancel_messages().len() must equal wl.len()");
    }
    Ok(())
}

fn check_all(wl: &PeerWantlist) -> Result<(), &'static str> {
    check_pending_subset(wl)?;
    check_len_matches_map(wl)?;
    check_want_messages_cover(wl)?;
    check_cancel_messages_cover(wl)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────
// Operation generator
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Op {
    Add { key: u8, priority: i32, want_block: bool },
    Remove { key: u8 },
    MarkReceived { key: u8 },
    MarkSynced,
    CleanupExpired { key: u8 },
}

fn key_to_hash(k: u8) -> ContentHash {
    // Pad the u8 into a 32-byte buffer so we get distinct BLAKE3 hashes
    // for each u8 value. This keeps the test deterministic.
    let mut buf = [0u8; 32];
    buf[0] = k;
    ContentHash::from_bytes(&buf)
}

/// Apply an operation to the wantlist, returning an optional
/// "witness" (the key that was added/removed/marked-received) so
/// post-conditions specific to that operation can be checked.
fn apply(wl: &mut PeerWantlist, op: &Op) {
    match op {
        Op::Add { key, priority, want_block } => {
            let h = key_to_hash(*key);
            if *want_block {
                let _ = wl.add_want_block(h, *priority);
            } else {
                let _ = wl.add_want_have(h, *priority);
            }
        }
        Op::Remove { key } => {
            let h = key_to_hash(*key);
            wl.remove_want(&h);
        }
        Op::MarkReceived { key } => {
            let h = key_to_hash(*key);
            wl.mark_received(&h);
        }
        Op::MarkSynced => {
            wl.mark_synced();
        }
        Op::CleanupExpired { key } => {
            // We can't truly expire entries without `Instant` plumbing,
            // but we exercise the cleanup path. cleanup_expired() is a
            // no-op on non-expired entries, which is fine — the
            // invariants under test are about the data structure, not
            // the wall-clock.
            let _h = key_to_hash(*key);
            let _ = wl.cleanup_expired();
        }
    }
}

fn arb_op() -> impl Strategy<Value = Op> {
    // Bound the priority range to a sensible window so we don't generate
    // i32::MIN. Bound the key range to a small set so collisions are
    // common (which exercises the idempotency path).
    prop_oneof![
        (0u8..8, -100i32..100, any::<bool>()).prop_map(|(key, priority, want_block)| {
            Op::Add { key, priority, want_block }
        }),
        (0u8..8).prop_map(|key| Op::Remove { key }),
        (0u8..8).prop_map(|key| Op::MarkReceived { key }),
        Just(Op::MarkSynced),
        (0u8..8).prop_map(|key| Op::CleanupExpired { key }),
    ]
}

// Single property: every invariant holds after every operation
// in a random sequence.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn all_invariants_hold_under_random_sequences(ops in proptest::collection::vec(arb_op(), 1..64)) {
        let mut wl = PeerWantlist::new("proptest-peer".to_string());

        // Initial state must satisfy every invariant.
        check_all(&wl).expect("empty wantlist violates invariant");

        // Track which keys we have already added, so the per-op
        // idempotency check only fires when a key is being re-added.
        let mut added_keys: std::collections::HashSet<u8> = std::collections::HashSet::new();

        for op in &ops {
            let prior_len = wl.len();
            apply(&mut wl, op);

            // Per-op post-conditions.
            match op {
                Op::Remove { key } => {
                    let h = key_to_hash(*key);
                    check_remove_clears_both(&wl, &h)
                        .expect("remove_want did not clear both want and pending");
                    added_keys.remove(key);
                }
                Op::MarkReceived { key } => {
                    let h = key_to_hash(*key);
                    check_mark_received_clears_pending(&wl, &h)
                        .expect("mark_received did not clear pending");
                }
                Op::MarkSynced => {
                    check_mark_synced_resets_dirty(&wl)
                        .expect("mark_synced did not reset dirty flag");
                }
                Op::Add { key, .. } => {
                    let after_len = wl.len();
                    let was_present = !added_keys.insert(*key);
                    if was_present {
                        // Re-adding an existing key must not grow len().
                        check_add_idempotent_len(prior_len, after_len)
                            .expect("add_want of same key grew len()");
                    } else {
                        // First-time add: len() may grow by 1 (or 0 if
                        // the cap was hit, in which case add_want
                        // returned Err and len is unchanged — also
                        // valid). The invariant is just "did not
                        // shrink".
                        assert!(
                            after_len >= prior_len,
                            "len() decreased unexpectedly: {prior_len} -> {after_len}"
                        );
                    }
                }
                Op::CleanupExpired { .. } => {}
            }

            // Global invariants must hold after every op.
            check_all(&wl)
                .unwrap_or_else(|e| panic!("invariant violation after op {op:?}: {e}"));
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Deterministic scenarios
// ─────────────────────────────────────────────────────────────────

#[test]
fn add_then_mark_synced_then_add_keeps_dirty() {
    let mut wl = PeerWantlist::new("det-peer".to_string());
    wl.add_want_block(key_to_hash(1), 10).unwrap();
    assert!(wl.is_dirty());
    wl.mark_synced();
    assert!(!wl.is_dirty());
    wl.add_want_block(key_to_hash(2), 20).unwrap();
    assert!(wl.is_dirty());
    check_all(&wl).expect("invariant violated");
}

#[test]
fn remove_then_mark_received_is_no_op() {
    let mut wl = PeerWantlist::new("det-peer".to_string());
    let h = key_to_hash(1);
    wl.add_want_block(h.clone(), 5).unwrap();
    wl.remove_want(&h);
    // mark_received on already-removed key must not crash and must
    // keep the invariant (pending stays a subset of wants).
    wl.mark_received(&h);
    assert!(!wl.contains(&h));
    assert!(!wl.is_pending(&h));
    check_all(&wl).expect("invariant violated");
}

#[test]
fn to_want_messages_round_trip_through_priority() {
    let mut wl = PeerWantlist::new("det-peer".to_string());
    for k in 0..4u8 {
        wl.add_want_block(key_to_hash(k), k as i32).unwrap();
    }
    let msgs = wl.to_want_messages();
    assert_eq!(msgs.len(), wl.len());
    // The emitted blocks must be the same set we put in.
    let want_keys: HashSet<_> = wl
        .entries()
        .iter()
        .map(|e| e.block.clone())
        .collect();
    let emitted: HashSet<_> = msgs
        .iter()
        .filter_map(|m| match m {
            BitswapMessage::WantBlock { block, .. } => Some(block.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(want_keys, emitted);
}

#[test]
fn priority_is_updated_on_re_add() {
    let mut wl = PeerWantlist::new("det-peer".to_string());
    let h = key_to_hash(7);
    wl.add_want_block(h.clone(), 1).unwrap();
    wl.add_want_block(h.clone(), 99).unwrap();
    assert_eq!(wl.len(), 1, "re-adding must not grow len()");
    let entry = wl.get(&h).expect("entry must still be present");
    assert_eq!(entry.priority, 99, "priority must be updated");
    assert!(wl.is_pending(&h));
    check_all(&wl).expect("invariant violated");
}

#[test]
fn cleanup_expired_returns_empty_for_fresh_entries() {
    let mut wl = PeerWantlist::new("det-peer".to_string());
    wl.add_want_block(key_to_hash(1), 5).unwrap();
    wl.add_want_block(key_to_hash(2), 10).unwrap();
    let expired = wl.cleanup_expired();
    assert!(expired.is_empty(), "fresh entries must not be expired");
    assert_eq!(wl.len(), 2);
    check_all(&wl).expect("invariant violated");
}

#[test]
fn invariants_hold_after_each_canonical_sequence() {
    // Canonical sequence: Add, Add (different key), MarkReceived,
    // MarkSynced, Remove (third key).
    let mut wl = PeerWantlist::new("det-peer".to_string());
    let h1 = key_to_hash(1);
    let h2 = key_to_hash(2);
    let h3 = key_to_hash(3);

    wl.add_want_block(h1.clone(), 5).unwrap();
    check_all(&wl).expect("after add #1");

    wl.add_want_block(h2.clone(), 10).unwrap();
    check_all(&wl).expect("after add #2");

    wl.mark_received(&h1);
    check_all(&wl).expect("after mark_received(h1)");

    wl.remove_want(&h3); // never added — no-op
    check_all(&wl).expect("after remove of missing key");

    wl.mark_synced();
    check_all(&wl).expect("after mark_synced");

    // Final assertions on the expected state.
    assert_eq!(wl.len(), 2);
    assert!(wl.contains(&h1));
    assert!(wl.contains(&h2));
    assert!(!wl.is_pending(&h1), "h1 was marked-received earlier");
    assert!(wl.is_pending(&h2), "h2 was never marked-received");
    assert!(!wl.is_dirty());
}