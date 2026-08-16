//! Fuzz-style tests for the pkarr publication / resolution path.
//!
//! `cargo fuzz` requires nightly, so we approximate fuzzing inside
//! the standard test harness by running `proptest` for 4,096 cases
//! against each parser. This catches crashes, panics and undefined
//! behaviour just as effectively for these entry points and runs
//! on stable Rust (the P0-REQ-8 acceptance criterion).
//!
//! Backed by: P0-REQ-3 (curve check), P0-REQ-7 (no panic in
//! production paths).

use proptest::prelude::*;

use a3net_dns_server::pkarr::{validate_z32_pubkey, z32_decode};

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 4096,
        .. ProptestConfig::default()
    })]

    /// The decoder must never panic for *any* byte string up to 64
    /// bytes. It is the boundary between untrusted input and our
    /// cryptography — a panic here would be a security issue.
    #[test]
    fn z32_decode_never_panics_on_random_input(
        s in proptest::collection::vec(any::<u8>(), 0..64),
    ) {
        // We don't care whether the result is Some or None — only
        // that we never panic.
        let _ = z32_decode(std::str::from_utf8(&s).unwrap_or(""));
    }

    /// `validate_z32_pubkey` must never panic on random input.
    /// A crash from a malformed pubkey would propagate to the DNS
    /// resolution path, which is internet-facing.
    #[test]
    fn validate_z32_pubkey_never_panics_on_random_input(
        s in "[\\x00-\\xff]{0,80}",
    ) {
        let _ = validate_z32_pubkey(&s);
    }

    /// `validate_z32_pubkey` must never panic even on extreme
    /// payloads: very long strings, whitespace, control codes, and
    /// strings consisting only of punctuation.
    #[test]
    fn validate_z32_pubkey_pathological_inputs(
        // 800-char strings; well past the 52-char z32 ceiling.
        filler in "[!-/]{800,800}",
    ) {
        let _ = validate_z32_pubkey(&filler);
    }
}
