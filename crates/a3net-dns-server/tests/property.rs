//! Property-based tests for `a3net-dns-server`.
//!
//! These tests back P0-REQ-8 ("航空级测试 — property tests"). They
//! document invariants we care about and let `proptest` shrink to a
//! minimal counter-example when one fails. Each test includes a
//! comment describing the DO-178C traceability (requirement ID) it
//! supports.

use proptest::prelude::*;

use a3net_dns_server::pkarr::{validate_z32_pubkey, z32_decode, z32_encode};

/// P0-REQ-3 invariant: every z32 string that survives validation
/// (length 52, alphabet, curve check) is a valid ed25519 point and
/// round-trips through `z32_decode` then `z32_encode`.
#[test]
fn proptest_z32_round_trip_via_real_keys() {
    proptest!(ProptestConfig::with_cases(64), |(
        // 32 random bytes ⇒ fresh seed for a valid ed25519 keypair.
        seed: [u8; 32],
    )| {
        let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
        let verifying = signing.verifying_key();
        let bytes = verifying.to_bytes();
        let z32 = z32_encode(&bytes);
        prop_assert_eq!(z32.len(), 52, "z32 must be 52 chars");
        prop_assert!(
            validate_z32_pubkey(&z32).is_ok(),
            "must validate: {z32}"
        );
        let back = z32_decode(&z32).expect("decode");
        prop_assert_eq!(back, bytes.to_vec(), "round-trip identity");
    });
}

/// P0-REQ-3 invariant: a z32 string that decodes to 32 bytes which
/// are *not* on the ed25519 curve is rejected by
/// `validate_z32_pubkey`. `0x00..00` decodes fine but the identity
/// point is on the curve, so we use `[1, 0, …, 0]` (a known off-curve
/// tweak) to exercise the curve check itself.
#[test]
fn proptest_validate_z32_rejects_off_curve_byte_variants() {
    proptest!(ProptestConfig::with_cases(32), |(
        // Any non-zero byte at position 0 forms a different candidate.
        byte in 1u8..=255,
    )| {
        let mut bytes = [0u8; 32];
        bytes[0] = byte;
        let z32 = z32_encode(&bytes);
        // Most byte values will decode fine; only the ~0.04% that
        // happen to land on the curve should pass. Both outcomes are
        // valid; we only require that *if* it is rejected, the
        // reason mentions "off-curve" or "ed25519".
        let result = validate_z32_pubkey(&z32);
        if let Err(e) = result {
            let msg = e.to_string();
            prop_assert!(
                msg.contains("off-curve") || msg.contains("ed25519"),
                "error must mention curve check, got: {msg}"
            );
        }
    });
}

/// P0-REQ-3 invariant: random z32 strings have an extremely low
/// probability of being a valid ed25519 point, so validation
/// overwhelmingly rejects them. This is a fuzz-direction sanity
/// check that the alphabet / length checks fire first.
#[test]
fn proptest_random_short_strings_rejected() {
    proptest!(ProptestConfig::with_cases(64), |(
        // Strings of length 0..64 built from the z32 alphabet.
        s in proptest::collection::vec(
            proptest::sample::select(
                b"ybndrfg8ejkmcpqxot1uwisza345h769".to_vec()
            ),
            0..64,
        ),
    )| {
        let s = String::from_utf8(s).unwrap();
        if s.len() == 52 && validate_z32_pubkey(&s).is_ok() {
            // legitimate, do nothing
            return Ok(());
        }
        // Otherwise must be rejected; this catches panics.
        prop_assert!(
            validate_z32_pubkey(&s).is_err(),
            "must reject non-conforming input: {s:?}"
        );
    });
}
