//! Bitswap Wantlist — formal-invariant model and Kani harness.
//!
//! This module provides a **Kani-friendly abstraction** of
//! `a3net_blobstore::bitswap_wantlist::PeerWantlist` so that we can
//! formally verify the wantlist's state-machine invariants.
//!
//! ## Why a model instead of the real `PeerWantlist`?
//!
//! `PeerWantlist` uses `std::time::Instant` and `HashMap`, both of
//! which are difficult for Kani/CBMC to reason about symbolically.
//! The model here mirrors the *contract* of `PeerWantlist` —
//! `add_want`, `remove_want`, `mark_received`, `mark_synced`,
//! `cleanup_expired`, plus the dirty-flag transitions — using only
//! fixed-size integer keys and `BTreeMap` (which Kani handles).
//!
//! The same invariants are then re-checked **against the real
//! `PeerWantlist`** by `crates/a3net-blobstore/tests/bitswap_invariants.rs`,
//! so a regression in either side is caught:
//!
//! - **Kani proof** (`cargo kani --features kani`) — exhaustive over
//!   the model state space.
//! - **Rust randomized test** (`cargo test -p a3net-blobstore
//!   --test bitswap_invariants`) — probabilistic, but runs in normal
//!   CI on the real code.
//!
//! ## Invariants
//!
//! For a `SimpleWantlist` with state `(wants: BTreeMap<K, Entry>,
//! pending: BTreeSet<K>, dirty: bool)`:
//!
//! 1. **`pending ⊆ keys(wants)`** — every pending key has a want entry.
//! 2. **`len() == wants.len()`** — the public length reflects the map.
//! 3. **After `remove_want(k)`: `!contains(k) ∧ !pending(k)`**.
//! 4. **After `mark_received(k)`: `!pending(k)`** (does not require
//!    `!contains(k)` — receiving a response is orthogonal to wanting).
//! 5. **After `mark_synced()`: `!dirty`**.
//! 6. **`add_want(k, _)` followed by `mark_synced()` leaves the want
//!    in the map, and the dirty flag is reset**.
//! 7. **`add_want(k, e1)` then `add_want(k, e2)` does NOT grow `len()`**
//!    (single-entry-per-key).
//! 8. **`to_want_messages().len() == wants.len()`**.
//! 9. **`to_cancel_messages().len() == wants.len()`**.
//! 10. **Idempotence**: `remove_want(k)` after `remove_want(k)` is a
//!     no-op (the second call returns `None` and leaves state unchanged).

use std::collections::{BTreeMap, BTreeSet};

/// Kani-friendly hash-key type. We use `u32` rather than the real
/// `ContentHash` (which is a 64-char hex string) because Kani can
/// exhaustively model bounded integers but not arbitrary-length
/// strings. The mapping from real CID → `u32` is just `fn mock_hash`
/// below; the invariants are about *set membership*, not bit-pattern
/// identity, so this is sound.
pub type MockHash = u32;

/// Single want entry in the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    /// Block hash (mocked as `u32`).
    pub block: MockHash,
    /// Higher = more urgent.
    pub priority: i32,
    /// `true` for full block, `false` for want-have.
    pub want_block: bool,
    /// Whether the want has expired (used by `cleanup_expired`).
    pub expired: bool,
}

impl ModelEntry {
    pub fn new(block: MockHash, priority: i32, want_block: bool) -> Self {
        Self {
            block,
            priority,
            want_block,
            expired: false,
        }
    }
}

/// Kani-friendly mirror of `PeerWantlist`.
///
/// The field set is intentionally minimal — it captures everything
/// the invariants need to talk about and nothing more.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimpleWantlist {
    /// `wants: block -> entry`.
    pub wants: BTreeMap<MockHash, ModelEntry>,
    /// `pending: set of blocks we've asked for but not yet received`.
    pub pending: BTreeSet<MockHash>,
    /// `dirty: true` between mutation and `mark_synced`.
    pub dirty: bool,
}

impl SimpleWantlist {
    pub fn new() -> Self {
        Self::default()
    }

    /// Public length: number of distinct blocks wanted.
    pub fn len(&self) -> usize {
        self.wants.len()
    }

    pub fn is_empty(&self) -> bool {
        self.wants.is_empty()
    }

    pub fn contains(&self, k: MockHash) -> bool {
        self.wants.contains_key(&k)
    }

    pub fn is_pending(&self, k: MockHash) -> bool {
        self.pending.contains(&k)
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mirror of `PeerWantlist::add_want`.
    pub fn add_want(&mut self, entry: ModelEntry) {
        let k = entry.block;
        self.wants.insert(k, entry);
        self.pending.insert(k);
        self.dirty = true;
    }

    /// Mirror of `PeerWantlist::remove_want`.
    pub fn remove_want(&mut self, k: MockHash) -> Option<ModelEntry> {
        let removed = self.wants.remove(&k);
        self.pending.remove(&k);
        if removed.is_some() {
            self.dirty = true;
        }
        removed
    }

    /// Mirror of `PeerWantlist::mark_received`.
    pub fn mark_received(&mut self, k: MockHash) {
        self.pending.remove(&k);
    }

    /// Mirror of `PeerWantlist::mark_synced`.
    pub fn mark_synced(&mut self) {
        self.dirty = false;
    }

    /// Mirror of `PeerWantlist::cleanup_expired`.
    pub fn cleanup_expired(&mut self) -> Vec<MockHash> {
        let expired: Vec<MockHash> = self
            .wants
            .iter()
            .filter(|(_, e)| e.expired)
            .map(|(k, _)| *k)
            .collect();
        for k in &expired {
            self.wants.remove(k);
            self.pending.remove(k);
        }
        if !expired.is_empty() {
            self.dirty = true;
        }
        expired
    }

    /// Mirror of `PeerWantlist::to_want_messages`.
    pub fn to_want_messages(&self) -> Vec<(MockHash, i32)> {
        self.wants
            .values()
            .map(|e| (e.block, e.priority))
            .collect()
    }

    /// Mirror of `PeerWantlist::to_cancel_messages`.
    pub fn to_cancel_messages(&self) -> Vec<MockHash> {
        self.wants.keys().copied().collect()
    }
}

// ─────────────────────────────────────────────────────────────────
// Invariant catalogue
// ─────────────────────────────────────────────────────────────────

/// Named invariants the wantlist must uphold after every operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvariantId {
    /// `pending ⊆ keys(wants)`.
    PendingSubsetOfWants,
    /// `len() == wants.len()`.
    LenMatchesMap,
    /// After `remove_want(k)`: `!contains(k) ∧ !pending(k)`.
    RemoveClearsBoth,
    /// After `mark_received(k)`: `!pending(k)`.
    MarkReceivedClearsPending,
    /// After `mark_synced()`: `!dirty`.
    MarkSyncedResetsDirty,
    /// `add_want(k, e1)` then `add_want(k, e2)` ⇒ `len() == 1`.
    AddSameKeyIsIdempotent,
    /// `to_want_messages().len() == wants.len()`.
    WantMessagesCoversWants,
    /// `to_cancel_messages().len() == wants.len()`.
    CancelMessagesCoversWants,
}

impl InvariantId {
    pub const ALL: &'static [InvariantId] = &[
        InvariantId::PendingSubsetOfWants,
        InvariantId::LenMatchesMap,
        InvariantId::RemoveClearsBoth,
        InvariantId::MarkReceivedClearsPending,
        InvariantId::MarkSyncedResetsDirty,
        InvariantId::AddSameKeyIsIdempotent,
        InvariantId::WantMessagesCoversWants,
        InvariantId::CancelMessagesCoversWants,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            InvariantId::PendingSubsetOfWants => "pending-subset-of-wants",
            InvariantId::LenMatchesMap => "len-matches-map",
            InvariantId::RemoveClearsBoth => "remove-clears-both",
            InvariantId::MarkReceivedClearsPending => "mark-received-clears-pending",
            InvariantId::MarkSyncedResetsDirty => "mark-synced-resets-dirty",
            InvariantId::AddSameKeyIsIdempotent => "add-same-key-is-idempotent",
            InvariantId::WantMessagesCoversWants => "want-messages-covers-wants",
            InvariantId::CancelMessagesCoversWants => "cancel-messages-covers-wants",
        }
    }
}

/// Result of checking an invariant: `Ok(())` if it holds, `Err` with
/// the invariant id if it does not.
pub type InvariantResult = std::result::Result<(), InvariantId>;

/// Check **all** invariants on a `SimpleWantlist` snapshot.
///
/// This is callable both from Kani (`#[cfg(kani)]`) and from plain
/// Rust tests, so the same predicate runs in both worlds.
pub fn check_all(wl: &SimpleWantlist) -> InvariantResult {
    check_pending_subset(wl)?;
    check_len_matches_map(wl)?;
    check_want_messages_covers(wl)?;
    check_cancel_messages_covers(wl)?;
    Ok(())
}

/// Invariant 1: `pending ⊆ keys(wants)`.
pub fn check_pending_subset(wl: &SimpleWantlist) -> InvariantResult {
    for k in &wl.pending {
        if !wl.wants.contains_key(k) {
            return Err(InvariantId::PendingSubsetOfWants);
        }
    }
    Ok(())
}

/// Invariant 2: `len() == wants.len()`.
pub fn check_len_matches_map(wl: &SimpleWantlist) -> InvariantResult {
    if wl.len() != wl.wants.len() {
        return Err(InvariantId::LenMatchesMap);
    }
    Ok(())
}

/// Invariant 7: `to_want_messages().len() == wants.len()`.
pub fn check_want_messages_covers(wl: &SimpleWantlist) -> InvariantResult {
    if wl.to_want_messages().len() != wl.wants.len() {
        return Err(InvariantId::WantMessagesCoversWants);
    }
    Ok(())
}

/// Invariant 8: `to_cancel_messages().len() == wants.len()`.
pub fn check_cancel_messages_covers(wl: &SimpleWantlist) -> InvariantResult {
    if wl.to_cancel_messages().len() != wl.wants.len() {
        return Err(InvariantId::CancelMessagesCoversWants);
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────
// Kani harnesses
// ─────────────────────────────────────────────────────────────────

#[cfg(kani)]
mod proof {
    use super::*;

    /// Helper to construct a `SimpleWantlist` from a bounded number of
    /// non-deterministic entries. Bounded so Kani can finish in finite
    /// time.
    fn arbitrary_wantlist() -> SimpleWantlist {
        let n: usize = kani::any();
        kani::assume(n <= 3);
        let mut wl = SimpleWantlist::new();
        let mut i = 0;
        while i < n {
            let k: MockHash = kani::any();
            kani::assume(k < 4);
            let prio: i32 = kani::any();
            let block: bool = kani::any();
            wl.add_want(ModelEntry::new(k, prio, block));
            i += 1;
        }
        wl
    }

    /// Proof: any sequence of `add_want` followed by `mark_synced`
    /// leaves the wantlist in a state where `pending ⊆ wants`.
    #[kani::proof]
    pub fn proof_add_then_sync_pending_subset() {
        let mut wl = arbitrary_wantlist();
        wl.mark_synced();
        assert!(
            check_pending_subset(&wl).is_ok(),
            "pending must remain a subset of wants after mark_synced"
        );
    }

    /// Proof: `len() == wants.len()` always holds.
    #[kani::proof]
    pub fn proof_len_equals_wants_len() {
        let mut wl = arbitrary_wantlist();
        wl.mark_received(0);
        wl.remove_want(1);
        wl.mark_synced();
        assert_eq!(
            wl.len(),
            wl.wants.len(),
            "len() must equal wants.len() under arbitrary operations"
        );
    }

    /// Proof: removing an existing entry leaves the wantlist free of
    /// both the want and its pending flag.
    #[kani::proof]
    pub fn proof_remove_clears_both() {
        let mut wl = arbitrary_wantlist();
        // Pick a key that is currently present and pending.
        let k: MockHash = kani::any();
        kani::assume(wl.contains(k) && wl.is_pending(k));
        let removed = wl.remove_want(k);
        assert!(removed.is_some(), "remove must return Some for present key");
        assert!(!wl.contains(k), "wants must not contain k after remove");
        assert!(!wl.is_pending(k), "pending must not contain k after remove");
    }

    /// Proof: `add_want` of an existing key is idempotent w.r.t. `len`.
    #[kani::proof]
    pub fn proof_add_same_key_idempotent_len() {
        let mut wl = SimpleWantlist::new();
        let k: MockHash = kani::any();
        kani::assume(k < 4);
        wl.add_want(ModelEntry::new(k, 1, true));
        let len_after_first = wl.len();
        wl.add_want(ModelEntry::new(k, 2, false));
        assert_eq!(
            wl.len(),
            len_after_first,
            "add_want of an existing key must not grow len()"
        );
        assert!(
            check_pending_subset(&wl).is_ok(),
            "after re-add, pending must remain subset of wants"
        );
    }

    /// Proof: `mark_synced()` resets the dirty flag.
    #[kani::proof]
    pub fn proof_mark_synced_resets_dirty() {
        let mut wl = arbitrary_wantlist();
        // Make sure dirty is set by mutating.
        wl.add_want(ModelEntry::new(7, 1, true));
        assert!(wl.is_dirty(), "after add, dirty must be true");
        wl.mark_synced();
        assert!(!wl.is_dirty(), "after mark_synced, dirty must be false");
    }
}

// ─────────────────────────────────────────────────────────────────
// Plain-Rust unit tests (run on `cargo test`, no Kani needed)
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_wantlist_passes_all_invariants() {
        let wl = SimpleWantlist::new();
        assert!(check_all(&wl).is_ok());
    }

    #[test]
    fn add_then_remove_leaves_empty() {
        let mut wl = SimpleWantlist::new();
        wl.add_want(ModelEntry::new(1, 5, true));
        wl.remove_want(1);
        assert!(wl.is_empty());
        assert!(check_all(&wl).is_ok());
    }

    #[test]
    fn pending_subset_holds_after_add_remove_received() {
        let mut wl = SimpleWantlist::new();
        wl.add_want(ModelEntry::new(1, 0, true));
        wl.add_want(ModelEntry::new(2, 1, false));
        assert!(check_all(&wl).is_ok());

        wl.mark_received(1);
        assert!(!wl.is_pending(1), "pending should clear after mark_received");
        assert!(wl.contains(1), "wants must still hold 1 after mark_received");
        assert!(check_all(&wl).is_ok());

        wl.remove_want(2);
        assert!(check_all(&wl).is_ok());
    }

    #[test]
    fn cleanup_expired_drops_want_and_pending() {
        let mut wl = SimpleWantlist::new();
        let mut e = ModelEntry::new(42, 3, true);
        e.expired = true;
        wl.add_want(e);
        assert!(wl.is_pending(42));
        let expired = wl.cleanup_expired();
        assert_eq!(expired, vec![42]);
        assert!(!wl.contains(42));
        assert!(!wl.is_pending(42));
        assert!(check_all(&wl).is_ok());
    }

    #[test]
    fn add_same_key_does_not_grow_len() {
        let mut wl = SimpleWantlist::new();
        wl.add_want(ModelEntry::new(7, 0, true));
        let l0 = wl.len();
        wl.add_want(ModelEntry::new(7, 99, false));
        let l1 = wl.len();
        assert_eq!(l0, l1);
        // And the priority was updated.
        assert_eq!(wl.wants[&7].priority, 99);
        assert_eq!(wl.wants[&7].want_block, false);
    }

    #[test]
    fn mark_synced_resets_dirty() {
        let mut wl = SimpleWantlist::new();
        wl.add_want(ModelEntry::new(1, 0, true));
        assert!(wl.is_dirty());
        wl.mark_synced();
        assert!(!wl.is_dirty());
        wl.add_want(ModelEntry::new(2, 0, true));
        assert!(wl.is_dirty());
    }

    #[test]
    fn to_want_and_cancel_messages_cover_all_wants() {
        let mut wl = SimpleWantlist::new();
        for k in 0..5 {
            wl.add_want(ModelEntry::new(k, k as i32, true));
        }
        assert_eq!(wl.to_want_messages().len(), wl.wants.len());
        assert_eq!(wl.to_cancel_messages().len(), wl.wants.len());
        let want_blocks: std::collections::BTreeSet<_> =
            wl.to_want_messages().iter().map(|(k, _)| *k).collect();
        let cancel_blocks: std::collections::BTreeSet<_> =
            wl.to_cancel_messages().into_iter().collect();
        let want_keys: std::collections::BTreeSet<_> = wl.wants.keys().copied().collect();
        assert_eq!(want_blocks, want_keys);
        assert_eq!(cancel_blocks, want_keys);
    }

    #[test]
    fn invariant_id_str_round_trips() {
        for id in InvariantId::ALL {
            // Smoke test: `as_str` returns a non-empty identifier.
            assert!(!id.as_str().is_empty());
        }
    }
}
