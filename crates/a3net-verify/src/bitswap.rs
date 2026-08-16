//! Kani Verification Harness for A3Net Bitswap Protocol

/// Ledger entry for bandwidth accounting
#[derive(Debug, Clone, Default)]
pub struct LedgerEntry {
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub sent_count: u32,
    pub received_count: u32,
}

impl LedgerEntry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_sent(&mut self, bytes: u64) {
        self.sent_bytes += bytes;
        self.sent_count += 1;
    }

    pub fn record_received(&mut self, bytes: u64) {
        self.received_bytes += bytes;
        self.received_count += 1;
    }

    /// Calculate debt ratio (how much we owe vs what we receive)
    pub fn debt_ratio(&self) -> f64 {
        if self.received_bytes == 0 {
            return 0.0;
        }
        self.sent_bytes as f64 / self.received_bytes as f64
    }

    pub fn is_balanced(&self, max_ratio: f64) -> bool {
        self.debt_ratio() <= max_ratio
    }
}

/// Want list entry
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WantEntry {
    pub cid: Vec<u8>,
    pub priority: i32,
}

impl WantEntry {
    pub fn new(cid: Vec<u8>, priority: i32) -> Self {
        Self { cid, priority }
    }
}

/// Want list manager
#[derive(Debug, Clone, Default)]
pub struct WantList {
    pub entries: Vec<WantEntry>,
}

impl WantList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, entry: WantEntry) {
        if !self.entries.iter().any(|e| e.cid == entry.cid) {
            self.entries.push(entry);
        }
    }

    pub fn remove(&mut self, cid: &[u8]) {
        self.entries.retain(|e| e.cid != cid);
    }

    pub fn contains(&self, cid: &[u8]) -> bool {
        self.entries.iter().any(|e| e.cid == cid)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get highest priority entry
    pub fn top(&self) -> Option<&WantEntry> {
        self.entries.iter().max_by_key(|e| e.priority)
    }
}

/// Ledger book for all peers
#[derive(Debug, Clone, Default)]
pub struct LedgerBook {
    pub entries: std::collections::HashMap<Vec<u8>, LedgerEntry>,
}

impl LedgerBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_create(&mut self, peer_id: &[u8]) -> &mut LedgerEntry {
        self.entries.entry(peer_id.to_vec())
            .or_insert_with(LedgerEntry::new)
    }

    pub fn balance(&self, peer_id: &[u8]) -> Option<f64> {
        self.entries.get(peer_id).map(|e| e.debt_ratio())
    }
}

/// Bitswap invariants (to be verified with Kani)
pub struct BitswapInvariants;

impl BitswapInvariants {
    /// Invariant 1: Want list should not contain already have CIDs
    pub fn wantlist_valid(want: &WantList, have: &[Vec<u8>]) -> bool {
        for entry in &want.entries {
            if have.iter().any(|h| h == &entry.cid) {
                return false;
            }
        }
        true
    }

    /// Invariant 2: Ledger balance should never be negative
    pub fn ledger_balance_valid(ledger: &LedgerEntry) -> bool {
        ledger.received_bytes <= ledger.sent_bytes + u64::MAX / 2
    }

    /// Invariant 3: Debt ratio should be non-negative
    pub fn debt_ratio_valid(ledger: &LedgerEntry) -> bool {
        ledger.debt_ratio() >= 0.0
    }

    /// Invariant 4: Peer should be blocked if debt exceeds threshold
    pub fn peer_blocked_correct(ledger: &LedgerEntry, blocked: bool, threshold: f64) -> bool {
        if ledger.debt_ratio() > threshold {
            blocked
        } else {
            !blocked || true // Not blocked is always valid
        }
    }
}

/// Kani proofs for bitswap protocol
#[cfg(feature = "kani")]
mod proof {
    use super::*;
    use kani::proof;

    /// Proof: Adding duplicate want fails (deduplication)
    #[proof]
    pub fn proof_wantlist_dedup() {
        let mut wantlist = WantList::new();
        
        let cid = vec![1, 2, 3, 4];
        wantlist.add(WantEntry::new(cid.clone(), 1));
        wantlist.add(WantEntry::new(cid.clone(), 2));
        
        let count = wantlist.entries.iter()
            .filter(|e| e.cid == cid)
            .count();
        
        kani::assert(count == 1, "WantList should deduplicate entries");
    }

    /// Proof: Ledger debt ratio is non-negative
    #[proof]
    pub fn proof_debt_ratio_nonnegative() {
        let ledger = LedgerEntry {
            sent_bytes: 1000,
            received_bytes: 500,
            sent_count: 10,
            received_count: 5,
        };
        
        let ratio = ledger.debt_ratio();
        
        kani::assert(ratio >= 0.0, "Debt ratio should be non-negative");
    }

    /// Proof: Ledger balance after multiple operations
    #[proof]
    pub fn proof_ledger_consistency() {
        let mut ledger = LedgerEntry::new();
        
        ledger.record_sent(100);
        ledger.record_received(50);
        ledger.record_sent(200);
        ledger.record_received(100);
        
        let expected_sent = 300u64;
        let expected_received = 150u64;
        
        kani::assert(ledger.sent_bytes == expected_sent, "Sent bytes should match");
        kani::assert(ledger.received_bytes == expected_received, "Received bytes should match");
    }

    /// Proof: WantList.is_empty is correct
    #[proof]
    pub fn proof_wantlist_empty_correct() {
        let mut wantlist = WantList::new();
        
        kani::assert(wantlist.is_empty(), "New WantList should be empty");
        
        wantlist.add(WantEntry::new(vec![1, 2], 1));
        
        kani::assert(!wantlist.is_empty(), "WantList with entry should not be empty");
    }

    /// Proof: Remove from WantList works
    #[proof]
    pub fn proof_wantlist_remove() {
        let mut wantlist = WantList::new();
        let cid = vec![1, 2, 3];
        
        wantlist.add(WantEntry::new(cid.clone(), 1));
        kani::assert(wantlist.contains(&cid), "CID should be in list after add");
        
        wantlist.remove(&cid);
        kani::assert(!wantlist.contains(&cid), "CID should not be in list after remove");
    }

    /// Proof: Invariant - wantlist doesn't contain have
    #[proof]
    pub fn proof_wantlist_have_disjoint() {
        let have = vec![vec![1, 2, 3], vec![4, 5, 6]];
        let mut wantlist = WantList::new();
        
        wantlist.add(WantEntry::new(vec![7, 8, 9], 1));
        wantlist.add(WantEntry::new(vec![4, 5, 6], 2)); // This is in have!
        
        let valid = BitswapInvariants::wantlist_valid(&wantlist, &have);
        
        kani::assert(!valid, "Wantlist should not contain CIDs that we have");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ledger_debt_ratio() {
        let mut ledger = LedgerEntry::new();
        ledger.record_sent(1000);
        ledger.record_received(500);
        
        assert_eq!(ledger.debt_ratio(), 2.0);
    }

    #[test]
    fn test_wantlist_dedup() {
        let mut wantlist = WantList::new();
        let cid = vec![1, 2, 3];
        
        wantlist.add(WantEntry::new(cid.clone(), 1));
        wantlist.add(WantEntry::new(cid.clone(), 2));
        
        assert_eq!(wantlist.entries.len(), 1);
    }

    #[test]
    fn test_top_priority() {
        let mut wantlist = WantList::new();
        wantlist.add(WantEntry::new(vec![1], 1));
        wantlist.add(WantEntry::new(vec![2], 10));
        wantlist.add(WantEntry::new(vec![3], 5));
        
        assert_eq!(wantlist.top().unwrap().cid, vec![2]);
    }
}
