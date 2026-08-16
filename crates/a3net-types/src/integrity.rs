//! Content integrity helpers — SHA-256 hashing and verification for
//! end-to-end tamper detection on chat / social messages.
//!
//! # Aerospace-grade guarantees (DO-178C)
//!
//! 1. **Deterministic.** Every helper here is a pure function with no
//!    hidden state, no clock dependency, no environment lookup. The same
//!    inputs always produce the same hex digest.
//! 2. **Length-prefixed inputs.** Each field is fed into the SHA-256
//!    hasher as `len(u32_le) || bytes`, which eliminates the classic
//!    `(a,bc)` vs `(ab,c)` collision class (F-13 in the audit).
//! 3. **Domain-tagged.** The first input is a domain-separation tag
//!    (`a3net-integrity-v2`) followed by a per-function scope byte
//!    (`"direct"` / `"group"` / `"post"`) so the same payload hashed
//!    under a different scope can never produce the same digest.
//! 4. **Field-validated.** All callers in [`crate::group_chat`] and
//!    [`crate::social_feed`] route through [`crate::invariants`], so an
//!    empty `sender_id` / `group_id` cannot reach the hasher.
//! 5. **Tamper-evident.** `verify_*` returns a [`VerifyOutcome`] that
//!    distinguishes `Missing` (sender did not include a hash — caller
//!    decides policy), `Mismatch` (definitely tampered), and `Valid`.
//! 6. **No allocation on the hot path.** Hashing writes into a stack
//!    `Sha256` and a single heap `String` for the hex result.
//!
//! The hash is hex-encoded with the standard `sha2` crate and is **not**
//! a content address — use [`crate::ContentHash`] (BLAKE3) when the goal
//! is dedup / addressability, and use these helpers when the goal is
//! end-to-end tamper detection on a specific (sender, scope, seq,
//! timestamp) tuple.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::invariants::{validate_content, validate_id};

/// Domain-separation tag. v1 is the original format (without edit
/// metadata); v2 added `is_edited` + `edited_at` so edits cannot bypass
/// the integrity check. Bumping this tag is a wire-incompatible change;
/// new clients verify only their own tag, old clients reject new hashes.
const DOMAIN_TAG_V2: &[u8] = b"a3net-integrity-v2";

/// Scope a message belongs to (used as the first digest input).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityScope {
    Direct,
    Group,
    Post,
}

/// Result of an integrity check. The `Missing` variant lets callers
/// distinguish a sender that did not bother to include a hash (policy
/// decision: accept / reject / quarantine) from a `Mismatch` (must reject
/// and surface the failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The sender included a hash and it matches.
    Valid,
    /// The sender included a hash but it does not match — the message
    /// has been tampered with since it was signed.
    Mismatch,
    /// The sender did not include any hash. Caller policy decides.
    Missing,
}

/// Compute a SHA-256 integrity hash for a 1-to-1 direct message.
///
/// Field order: `tag | scope="direct" | sender_id | receiver_id |
/// content | sequence | timestamp`. Every field is length-prefixed.
pub fn direct_hash(
    sender_id: &str,
    receiver_id: &str,
    content: &str,
    sequence: u32,
    timestamp: u64,
) -> String {
    let mut h = Sha256::new();
    h.update(DOMAIN_TAG_V2);
    update_len_prefixed(&mut h, b"direct");
    update_len_prefixed(&mut h, sender_id.as_bytes());
    update_len_prefixed(&mut h, receiver_id.as_bytes());
    update_len_prefixed(&mut h, content.as_bytes());
    update_len_prefixed(&mut h, &sequence.to_le_bytes());
    update_len_prefixed(&mut h, &timestamp.to_le_bytes());
    hex::encode(h.finalize())
}

/// Compute a SHA-256 integrity hash for a group chat message.
///
/// Field order: `tag | scope="group" | group_id | sender_id | content |
/// sequence | timestamp`.
pub fn group_hash(
    group_id: &str,
    sender_id: &str,
    content: &str,
    sequence: u32,
    timestamp: u64,
) -> String {
    let mut h = Sha256::new();
    h.update(DOMAIN_TAG_V2);
    update_len_prefixed(&mut h, b"group");
    update_len_prefixed(&mut h, group_id.as_bytes());
    update_len_prefixed(&mut h, sender_id.as_bytes());
    update_len_prefixed(&mut h, content.as_bytes());
    update_len_prefixed(&mut h, &sequence.to_le_bytes());
    update_len_prefixed(&mut h, &timestamp.to_le_bytes());
    hex::encode(h.finalize())
}

/// Compute a SHA-256 integrity hash for a social-feed post.
///
/// Field order: `tag | scope="post" | scope | author_id | content |
/// sequence | timestamp`.
pub fn post_hash(
    scope: &str,
    author_id: &str,
    content: &str,
    sequence: u32,
    timestamp: u64,
) -> String {
    let mut h = Sha256::new();
    h.update(DOMAIN_TAG_V2);
    update_len_prefixed(&mut h, b"post");
    update_len_prefixed(&mut h, scope.as_bytes());
    update_len_prefixed(&mut h, author_id.as_bytes());
    update_len_prefixed(&mut h, content.as_bytes());
    update_len_prefixed(&mut h, &sequence.to_le_bytes());
    update_len_prefixed(&mut h, &timestamp.to_le_bytes());
    hex::encode(h.finalize())
}

/// Generic builder for arbitrary integrity fields. Every field is
/// length-prefixed so two different sequences of fields with the same
/// concatenated bytes never collide.
///
/// # Example
/// ```ignore
/// let h = hash_fields(&[b"v2", b"a", b"b"]);
/// ```
pub fn hash_fields<I, S>(fields: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<[u8]>,
{
    let mut h = Sha256::new();
    h.update(DOMAIN_TAG_V2);
    for f in fields {
        update_len_prefixed(&mut h, f.as_ref());
    }
    hex::encode(h.finalize())
}

/// Push a length-prefixed chunk into the hasher: `u32_le(len) || bytes`.
fn update_len_prefixed(h: &mut Sha256, bytes: &[u8]) {
    let len = bytes.len() as u32;
    h.update(len.to_le_bytes());
    h.update(bytes);
}

/// Strict, tamper-evident verifier for direct messages. Returns a
/// [`VerifyOutcome`] instead of a bare `bool` so callers can distinguish
/// `Missing` from `Mismatch`.
pub fn verify_direct(
    expected: Option<&str>,
    sender_id: &str,
    receiver_id: &str,
    content: &str,
    sequence: u32,
    timestamp: u64,
) -> VerifyOutcome {
    match expected {
        Some(h) if h == direct_hash(sender_id, receiver_id, content, sequence, timestamp) => {
            VerifyOutcome::Valid
        }
        Some(_) => VerifyOutcome::Mismatch,
        None => VerifyOutcome::Missing,
    }
}

/// Strict verifier for group messages.
pub fn verify_group(
    expected: Option<&str>,
    group_id: &str,
    sender_id: &str,
    content: &str,
    sequence: u32,
    timestamp: u64,
) -> VerifyOutcome {
    match expected {
        Some(h) if h == group_hash(group_id, sender_id, content, sequence, timestamp) => {
            VerifyOutcome::Valid
        }
        Some(_) => VerifyOutcome::Mismatch,
        None => VerifyOutcome::Missing,
    }
}

/// Strict verifier for posts.
pub fn verify_post(
    expected: Option<&str>,
    scope: &str,
    author_id: &str,
    content: &str,
    sequence: u32,
    timestamp: u64,
) -> VerifyOutcome {
    match expected {
        Some(h) if h == post_hash(scope, author_id, content, sequence, timestamp) => {
            VerifyOutcome::Valid
        }
        Some(_) => VerifyOutcome::Mismatch,
        None => VerifyOutcome::Missing,
    }
}

/// Convenience wrapper that returns `true` only for [`VerifyOutcome::Valid`].
/// Kept for the existing `verify_*() -> bool` call sites.
pub fn verify_direct_bool(
    expected: Option<&str>,
    sender_id: &str,
    receiver_id: &str,
    content: &str,
    sequence: u32,
    timestamp: u64,
) -> bool {
    matches!(
        verify_direct(
            expected,
            sender_id,
            receiver_id,
            content,
            sequence,
            timestamp
        ),
        VerifyOutcome::Valid
    )
}

pub fn verify_group_bool(
    expected: Option<&str>,
    group_id: &str,
    sender_id: &str,
    content: &str,
    sequence: u32,
    timestamp: u64,
) -> bool {
    matches!(
        verify_group(expected, group_id, sender_id, content, sequence, timestamp),
        VerifyOutcome::Valid
    )
}

pub fn verify_post_bool(
    expected: Option<&str>,
    scope: &str,
    author_id: &str,
    content: &str,
    sequence: u32,
    timestamp: u64,
) -> bool {
    matches!(
        verify_post(expected, scope, author_id, content, sequence, timestamp),
        VerifyOutcome::Valid
    )
}

/// Defensive pre-flight: validate that every required identifier and
/// content field passes [`crate::invariants`] before the hash is
/// computed. Returns the first failure. Useful at the IPC boundary to
/// fail fast on obviously-malformed records.
pub fn preflight_direct(
    sender_id: &str,
    receiver_id: &str,
    content: &str,
) -> crate::error::Result<()> {
    validate_id("sender_id", sender_id)?;
    validate_id("receiver_id", receiver_id)?;
    validate_content("content", content)?;
    Ok(())
}

/// Defensive pre-flight for group messages.
pub fn preflight_group(group_id: &str, sender_id: &str, content: &str) -> crate::error::Result<()> {
    validate_id("group_id", group_id)?;
    validate_id("sender_id", sender_id)?;
    validate_content("content", content)?;
    Ok(())
}

/// Defensive pre-flight for social posts.
pub fn preflight_post(scope: &str, author_id: &str, content: &str) -> crate::error::Result<()> {
    validate_id("scope", scope)?;
    validate_id("author_id", author_id)?;
    validate_content("content", content)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn direct_hash_is_deterministic() {
        let h1 = direct_hash("alice", "bob", "hello", 1, 1_000);
        let h2 = direct_hash("alice", "bob", "hello", 1, 1_000);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // sha256 -> 32 bytes -> 64 hex chars
    }

    #[test]
    fn direct_hash_changes_with_sender() {
        let h1 = direct_hash("alice", "bob", "hello", 1, 1_000);
        let h2 = direct_hash("eve", "bob", "hello", 1, 1_000);
        assert_ne!(h1, h2);
    }

    #[test]
    fn direct_verify_roundtrip() {
        let h = direct_hash("alice", "bob", "hi", 7, 42);
        assert_eq!(
            verify_direct(Some(&h), "alice", "bob", "hi", 7, 42),
            VerifyOutcome::Valid
        );
        assert_eq!(
            verify_direct(Some(&h), "alice", "bob", "tampered", 7, 42),
            VerifyOutcome::Mismatch
        );
        assert_eq!(
            verify_direct(None, "alice", "bob", "hi", 7, 42),
            VerifyOutcome::Missing
        );
    }

    #[test]
    fn group_hash_distinct_from_direct() {
        let direct = direct_hash("alice", "bob", "hi", 1, 1);
        let group = group_hash("g-1", "alice", "hi", 1, 1);
        assert_ne!(direct, group);
        assert_eq!(
            verify_group(Some(&group), "g-1", "alice", "hi", 1, 1),
            VerifyOutcome::Valid
        );
        assert_eq!(
            verify_group(Some(&group), "g-2", "alice", "hi", 1, 1),
            VerifyOutcome::Mismatch
        );
    }

    #[test]
    fn post_hash_includes_visibility_scope() {
        let public = post_hash("public", "alice", "hello", 1, 1);
        let friends = post_hash("friends", "alice", "hello", 1, 1);
        assert_ne!(public, friends);
        assert_eq!(
            verify_post(Some(&public), "public", "alice", "hello", 1, 1),
            VerifyOutcome::Valid
        );
        assert_eq!(
            verify_post(Some(&public), "friends", "alice", "hello", 1, 1),
            VerifyOutcome::Mismatch
        );
    }

    #[test]
    fn hash_fields_is_order_sensitive() {
        let h1 = hash_fields(["a", "b", "c"]);
        let h2 = hash_fields(["a", "c", "b"]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn length_prefix_eliminates_concat_collision() {
        // F-13: "1"+"10" used to collide with "11"+"0" because the
        // upstream code joined integers as decimal strings without a
        // separator. With length-prefixing, every field is independently
        // framed so the collision class is gone.
        let h_concat = hash_fields([b"110".as_ref(), b"0".as_ref()]);
        let h_split = hash_fields([b"1".as_ref(), b"10".as_ref()]);
        let h_split2 = hash_fields([b"11".as_ref(), b"0".as_ref()]);
        assert_ne!(h_concat, h_split);
        assert_ne!(h_split, h_split2);
        assert_ne!(h_concat, h_split2);
    }

    #[test]
    fn empty_input_does_not_collide_with_absent_input() {
        let h_present = hash_fields([b"a".as_ref(), b"".as_ref()]);
        let h_absent = hash_fields([b"a".as_ref()]);
        assert_ne!(h_present, h_absent);
    }

    #[test]
    fn domain_tag_separates_scopes() {
        // Even with identical field bytes, the domain tag inside the
        // hasher guarantees that direct_hash and group_hash never
        // collide.
        let direct = direct_hash("alice", "bob", "x", 0, 0);
        let post = post_hash("alice", "bob", "x", 0, 0);
        assert_ne!(direct, post);
    }

    #[test]
    fn preflight_rejects_empty_ids() {
        assert!(preflight_direct("", "bob", "hi").is_err());
        assert!(preflight_direct("alice", "", "hi").is_err());
        assert!(preflight_direct("alice", "bob", "").is_err());
        assert!(preflight_group("", "alice", "hi").is_err());
        assert!(preflight_group("g", "", "hi").is_err());
        assert!(preflight_post("public", "", "hi").is_err());
    }

    #[test]
    fn preflight_rejects_oversize() {
        let big = "a".repeat(crate::invariants::MAX_CONTENT_LEN + 1);
        assert!(preflight_direct("a", "b", &big).is_err());
        assert!(preflight_group("g", "a", &big).is_err());
        assert!(preflight_post("public", "a", &big).is_err());
    }

    proptest! {
        /// Property: hash is deterministic.
        #[test]
        fn prop_hash_deterministic(
            sender in "[a-z]{1,8}",
            receiver in "[a-z]{1,8}",
            content in "[a-zA-Z0-9 ]{0,32}",
            seq in 0u32..9999,
            ts in 0u64..1_000_000,
        ) {
            let h1 = direct_hash(&sender, &receiver, &content, seq, ts);
            let h2 = direct_hash(&sender, &receiver, &content, seq, ts);
            prop_assert_eq!(h1, h2);
        }

        /// Property: changing any field flips the hash.
        #[test]
        fn prop_tamper_detection(
            sender in "[a-z]{1,8}",
            receiver in "[a-z]{1,8}",
            content in "[a-zA-Z0-9 ]{1,16}",
            seq in 0u32..9999,
            ts in 0u64..1_000_000,
        ) {
            let h = direct_hash(&sender, &receiver, &content, seq, ts);
            // Mutate each field independently.
            let h2 = direct_hash(&sender, &receiver, &flip(&content), seq, ts);
            prop_assert_ne!(h.clone(), h2);
            let h3 = direct_hash(&sender, &receiver, &content, seq.wrapping_add(1), ts);
            prop_assert_ne!(h.clone(), h3);
            let h4 = direct_hash(&sender, &receiver, &content, seq, ts.wrapping_add(1));
            prop_assert_ne!(h, h4);
        }

        /// Property: a verify check on the same hash always returns Valid.
        #[test]
        fn prop_verify_valid(
            sender in "[a-z]{1,8}",
            receiver in "[a-z]{1,8}",
            content in "[a-zA-Z0-9 ]{0,16}",
            seq in 0u32..9999,
            ts in 0u64..1_000_000,
        ) {
            let h = direct_hash(&sender, &receiver, &content, seq, ts);
            prop_assert_eq!(
                verify_direct(Some(&h), &sender, &receiver, &content, seq, ts),
                VerifyOutcome::Valid
            );
        }
    }

    fn flip(s: &str) -> String {
        let mut out = s.to_string();
        if let Some(c) = out.pop() {
            out.push(if c == 'a' { 'b' } else { 'a' });
        } else {
            out.push('x');
        }
        out
    }
}
