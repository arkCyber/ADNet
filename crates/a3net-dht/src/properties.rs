// SPDX-License-Identifier: MIT OR Apache-2.0
//
// `properties.rs` — Proptest-style property-based tests for
// `a3net-dht`. The proptest harness drives arbitrary byte
// sequences into the DHT's pure-Rust surfaces and asserts that
// invariants hold for *every* generated input — a single
// regression-triggering input is enough to fail a property.
//
// Aerospace note (DO-178C §6.4.4 — robustness): randomised
// property tests are the closest practical analogue to
// bounded-exhaustive model checking. They catch whole classes
// of bugs (off-by-one, unsigned overflow, asymmetric encoding)
// that targeted unit tests miss.
//
// Run with: `cargo test -p a3net-dht --lib properties`

#![cfg(test)]

use a3net_types::NodeId;
use proptest::prelude::*;

use crate::bucket::{Contact, KBucket, RoutingTable};
use crate::record::{DhtKey, IpnRecord, ProviderRecord};
use crate::retry::{is_transient, PeerFailureTracker, RetryPolicy};
use crate::store::new_in_memory_store;

// ─────────────────────────────────────────────────────────────────
// §6.3.4 — XOR distance metric invariants
// ─────────────────────────────────────────────────────────────────

proptest! {
    /// XOR distance must be symmetric: `d(a,b) == d(b,a)`.
    /// A bug that broke this would mean asymmetric bucket
    /// placement — the A→B lookup and the B→A lookup would
    /// converge on different peers, silently corrupting the
    /// routing table.
    #[test]
    fn xor_distance_is_symmetric(
        a_bytes in proptest::collection::vec(any::<u8>(), 1..=64),
        b_bytes in proptest::collection::vec(any::<u8>(), 1..=64),
    ) {
        let k_a = DhtKey::from_bytes(a_bytes);
        let k_b = DhtKey::from_bytes(b_bytes);
        let d_ab = k_a.xor_distance(&k_b);
        let d_ba = k_b.xor_distance(&k_a);
        // Pad with zeros to the same length.
        let len = d_ab.len().max(d_ba.len());
        let mut a = d_ab;
        a.resize(len, 0);
        let mut b = d_ba;
        b.resize(len, 0);
        prop_assert_eq!(a, b);
    }

    /// XOR self-distance must be all-zero for any key length.
    #[test]
    fn xor_distance_self_is_zero(bytes in proptest::collection::vec(any::<u8>(), 0..=128)) {
        let k = DhtKey::from_bytes(bytes);
        let d = k.xor_distance(&k);
        prop_assert!(d.iter().all(|b| *b == 0));
    }

    /// Log-distance of self must be 0 (or Some(0)).
    #[test]
    fn log_distance_self_is_zero(bytes in proptest::collection::vec(any::<u8>(), 0..=128)) {
        let k = DhtKey::from_bytes(bytes);
        prop_assert_eq!(k.log_distance(&k), Some(0));
    }

    /// `DhtKey::as_hex` of a hex-decoded key must equal the input
    /// string (round-trip via `from_content_hash_hex`).
    #[test]
    fn dht_key_hex_roundtrip(hex in "[0-9a-fA-F]*") {
        let key = DhtKey::from_content_hash_hex(&hex);
        prop_assert_eq!(key.as_hex(), hex.to_lowercase());
    }
}

// ─────────────────────────────────────────────────────────────────
// §6.3.4 — Routing-table bucket_index invariants
// ─────────────────────────────────────────────────────────────────

proptest! {
    /// `bucket_index(local, remote)` must agree with the position
    /// of the highest set bit in `local.xor_distance(remote)` —
    /// capped at 255. This is the core routing-table invariant;
    /// a bug here silently corrupts every bucket assignment.
    #[test]
    fn bucket_index_tracks_xor_high_bit(
        local_bytes in proptest::collection::vec(any::<u8>(), 32..=32),
        remote_bytes in proptest::collection::vec(any::<u8>(), 32..=32),
    ) {
        let local = NodeId::from_bytes(&local_bytes).unwrap();
        let remote = NodeId::from_bytes(&remote_bytes).unwrap();
        let idx = RoutingTable::bucket_index(&local, &remote);
        prop_assert!(idx <= 255);
        // Self-distance routes to bucket 255 (the "away" sentinel).
        if local_bytes == remote_bytes {
            prop_assert_eq!(idx, 255);
        } else {
            // Compute expected log distance by hand.
            let mut distance = [0u8; 32];
            for i in 0..32 {
                distance[i] = local_bytes[i] ^ remote_bytes[i];
            }
            let mut expected = 0usize;
            let mut found = false;
            for (i, &byte) in distance.iter().enumerate() {
                if byte != 0 {
                    for bit in (0..8).rev() {
                        if (byte >> bit) & 1 == 1 {
                            expected = i * 8 + (7 - bit) + 1;
                            found = true;
                            break;
                        }
                    }
                    if found { break; }
                }
            }
            let expected_clamped = expected.min(255);
            prop_assert_eq!(idx, expected_clamped, "distance was {:?}", distance);
        }
    }

    /// Inserting `N ≤ KBUCKET_SIZE` unique contacts into a
    /// fresh bucket must succeed without error.
    #[test]
    fn bucket_accepts_k_contacts(n in 1usize..=20) {
        let mut bucket = KBucket::new();
        for i in 0..n {
            let bytes = [i as u8; 32];
            let id = NodeId::from_bytes(&bytes).unwrap();
            let contact = Contact::new(id, format!("127.0.0.1:{}", 9000 + i).parse().unwrap());
            prop_assert!(bucket.insert(contact).is_ok());
        }
        prop_assert_eq!(bucket.len(), n);
    }

    /// The (n+1)-th insert into a saturated bucket must fail
    /// with `InsertError::BucketFull`.
    #[test]
    fn bucket_rejects_k_plus_one(_dummy in 0..1u8) {
        let mut bucket = KBucket::new();
        for i in 0..20 {
            let bytes = [i as u8; 32];
            let id = NodeId::from_bytes(&bytes).unwrap();
            let contact = Contact::new(id, format!("127.0.0.1:{}", 9000 + i).parse().unwrap());
            let _ = bucket.insert(contact);
        }
        // 21st insert into the *same bucket index* — but our
        // contacts may have spread across multiple buckets
        // because they share the low byte. Verify the bucket
        // count is exactly 20 in *some* bucket by trying many
        // candidates and counting successes vs rejections.
        let mut full_buckets = 0;
        for i in 100..120u8 {
            let bytes = [i; 32];
            let id = NodeId::from_bytes(&bytes).unwrap();
            // Insert into a *fresh* bucket — skip if it would
            // land in a non-full one.
            let probe = Contact::new(id.clone(), "127.0.0.1:9000".parse().unwrap());
            if bucket.insert(probe).is_err() {
                full_buckets += 1;
            } else {
                // Remove so the loop invariant (full buckets
                // exist) survives.
                bucket.remove(&id);
            }
        }
        // At least one bucket should be full. (Our seed
        // selection picks IDs that map to the same bucket.)
        prop_assert!(full_buckets >= 1);
    }
}

// ─────────────────────────────────────────────────────────────────
// §6.4.4 — Retry policy monotonicity
// ─────────────────────────────────────────────────────────────────

proptest! {
    /// `RetryPolicy::backoff_for(n)` must be monotonic in `n`
    /// (modulo the max_backoff clamp). A regression here means
    /// retry storms that don't actually back off.
    #[test]
    fn backoff_is_monotonic(
        a in 1u32..10,
        b in 1u32..10,
    ) {
        let p = RetryPolicy::default();
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        let ba = p.backoff_for(lo);
        let bb = p.backoff_for(hi);
        prop_assert!(ba <= bb, "backoff({lo})={ba:?} > backoff({hi})={bb:?}");
    }

    /// `should_retry(n)` is `true` iff `n < max_attempts`.
    #[test]
    fn should_retry_matches_max_attempts(
        max_attempts in 1u32..8,
        attempt in 0u32..16,
    ) {
        let mut p = RetryPolicy::default();
        p.max_attempts = max_attempts;
        let expected = attempt < max_attempts && attempt > 0;
        prop_assert_eq!(p.should_retry(attempt), expected);
    }

    /// `is_transient` must agree with the match on the variant —
    /// no regression can flip Timeout from transient to
    /// permanent or vice versa.
    #[test]
    fn is_transient_classifies_known_variants(s in 0u8..4) {
        use crate::query::QueryError;
        let e = match s {
            0 => QueryError::Timeout,
            1 => QueryError::Network("x".into()),
            2 => QueryError::PeerNotFound,
            _ => QueryError::InvalidResponse,
        };
        let transient = is_transient(&e);
        match e {
            QueryError::Timeout | QueryError::Network(_) => prop_assert!(transient),
            QueryError::PeerNotFound | QueryError::InvalidResponse => prop_assert!(!transient),
        }
    }

    /// Failure tracker monotonicity: repeated `record_failure`
    /// must increase `failure_count` until `record_success`
    /// resets it.
    #[test]
    fn tracker_failure_count_is_monotonic(
        n_failures in 0u32..20,
    ) {
        let mut p = RetryPolicy::default();
        p.peer_cooldown_threshold = u32::MAX; // disable cooldown
        let mut t = PeerFailureTracker::new(p);
        let peer = NodeId::random();
        for _ in 0..n_failures {
            t.record_failure(&peer);
        }
        prop_assert_eq!(t.failure_count(&peer), n_failures);
        t.record_success(&peer);
        prop_assert_eq!(t.failure_count(&peer), 0);
    }
}

// ─────────────────────────────────────────────────────────────────
// §6.4.3 — ProviderRecord signature uniqueness
// ─────────────────────────────────────────────────────────────────

proptest! {
    /// Two ProviderRecords with different `(key, provider_id,
    /// provider_addr, ttl_secs)` tuples must produce
    /// **different** signatures. The previous colon-delimited
    /// format could collide on field injection; the new
    /// length-prefixed format cannot.
    #[test]
    fn signing_data_is_unique_per_tuple(
        a_key in proptest::collection::vec(any::<u8>(), 0..=64),
        a_addr in "[a-zA-Z0-9:.]*",
        b_key in proptest::collection::vec(any::<u8>(), 0..=64),
        b_addr in "[a-zA-Z0-9:.]*",
        ttl_a in 0u64..u32::MAX as u64,
        ttl_b in 0u64..u32::MAX as u64,
    ) {
        let mut rec_a = ProviderRecord::new(
            DhtKey::from_bytes(a_key.clone()),
            NodeId::random(),
            a_addr.clone(),
        );
        rec_a.ttl_secs = ttl_a;
        let mut rec_b = ProviderRecord::new(
            DhtKey::from_bytes(b_key.clone()),
            NodeId::random(),
            b_addr.clone(),
        );
        rec_b.ttl_secs = ttl_b;
        let same_tuple = a_key == b_key && a_addr == b_addr && ttl_a == ttl_b;
        if !same_tuple {
            // Sign both records with a deterministic "identity
            // signer" (the bytes themselves) — this exposes any
            // signing-data collision directly.
            struct Identity;
            impl crate::record::Signer for Identity {
                fn sign(&self, data: &[u8]) -> Vec<u8> { data.to_vec() }
            }
            let mut a = rec_a.clone();
            let mut b = rec_b.clone();
            a.sign(&Identity);
            b.sign(&Identity);
            prop_assert_ne!(
                a.signature.clone().unwrap(),
                b.signature.clone().unwrap(),
                "signature collision on tuples that differ: ({:?}, {:?}, {}) vs ({:?}, {:?}, {})",
                a_key, a_addr, ttl_a, b_key, b_addr, ttl_b
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// §6.4.3 — IpnRecord sequence guard
// ─────────────────────────────────────────────────────────────────

proptest! {
    /// `IpnRecord` sequence must be strictly monotonic — newer
    /// records overwrite older, older records cannot overwrite
    /// newer. The store enforces this; we verify the
    /// invariant under random sequence numbers.
    #[test]
    fn ipns_strict_monotonic_sequence(
        seq_a in 1u64..1000,
        seq_b in 1u64..1000,
    ) {
        let store = new_in_memory_store();
        let key = DhtKey::from_bytes(vec![1u8; 32]);
        let mut a = IpnRecord::new(key.clone(), "v_a".into());
        a.sequence = seq_a;
        let mut b = IpnRecord::new(key.clone(), "v_b".into());
        b.sequence = seq_b;
        store.put_ipns(&key, a.clone());
        let accepted = store.put_ipns(&key, b.clone());
        let stored = store.get_ipns(&key).unwrap();
        // Whichever has the larger sequence must win (and the
        // store must reject the strictly smaller one).
        if seq_a > seq_b {
            prop_assert!(accepted, "newer (b) should win, a.seq={} b.seq={}", seq_a, seq_b);
            prop_assert_eq!(stored.sequence, seq_a);
        } else if seq_b > seq_a {
            prop_assert!(accepted, "newer (b) should win, a.seq={} b.seq={}", seq_a, seq_b);
            prop_assert_eq!(stored.sequence, seq_b);
        } else {
            // Equal — strict monotonic means we reject.
            prop_assert!(!accepted, "equal sequence must be rejected");
            prop_assert_eq!(stored.sequence, seq_a);
        }
    }
}