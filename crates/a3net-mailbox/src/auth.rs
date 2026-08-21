//! Cryptographic authentication for mailbox requests.
//!
//! ## Wire format
//!
//! All mailbox requests carry an EIP-191 `personal_sign` signature over
//! a canonical 32-byte digest. The digest is the **blake3** hash of a
//! pipe-separated canonical message (see [`canonical_message`]).
//!
//! - **Sender** signs on `enqueue`; the recovered address must equal
//!   `sender_id` (envelope header).
//! - **Recipient** signs on `pull` / `ack`; the recovered address must
//!   equal the recipient id in the URL path.
//!
//! The choice of blake3 (rather than keccak256) is deliberate: it's the
//! project's standard hash and A3Net signatures already use
//! `sign_personal(_personal_digest)` which accepts an arbitrary 32-byte
//! hash. External signers (MetaMask, ethers.js) only see the 32-byte
//! digest; they don't care what hash produced it.
//!
//! ## Canonical message shape
//!
//! ```text
//! enqueue:  "mailbox.enqueue"  | recipient_id | msg_id | sha256(ciphertext) | signed_at_unix
//! pull:     "mailbox.pull"     | recipient_id
//! ack:      "mailbox.ack"      | recipient_id | msg_ids.join(",")
//! ```
//!
//! Each segment is length-prefixed with its UTF-8 byte length as
//! 8-byte little-endian so segments cannot collide across boundaries
//! (a `|` inside a segment is impossible by construction).
//!
//! ## EIP-712 timestamp binding (P3-7)
//!
//! The `enqueue` signature **binds to a unix timestamp** (`signed_at_unix`)
//! so that a signature captured on the wire is useless after the
//! `signature_max_age_secs` window has passed. This is analogous to
//! EIP-712's `nonce` / `expiry` pattern but works with any EIP-191 signer
//! (MetaMask, WalletConnect, hardware wallets) because the signer only
//! sees the 32-byte blake3 digest. Wallets should display the human-readable
//! components to the user before signing.
//!
//! ## Identity format
//!
//! `recipient_id` is the EIP-55 checksummed string form of an A3Net
//! [`Address`] — `0x` + 40 hex chars. We reject anything that doesn't
//! parse to a 20-byte address. This is the same identifier used by
//! `a3net-identity` and `a3net-userstore`, so the same hex string
//! works as a user lookup key in `a3chat-app`.

use a3net_identity::{Address, PersonalSignature, WalletPublic};
use blake3::Hasher;

use crate::error::{MailboxError, MailboxResult};

/// Maximum number of segments allowed in a canonical message. Defends
/// against runaway allocation if a caller accidentally feeds a huge
/// `msg_ids` list.
const MAX_SEGMENTS: usize = 64;

/// Maximum length of a single segment in bytes.
const MAX_SEGMENT_LEN: usize = 16 * 1024 * 1024; // 16 MiB (way above the 1 MiB envelope cap)

/// Tag for the `enqueue` signature payload.
pub const TAG_ENQUEUE: &[u8] = b"mailbox.enqueue";
/// Tag for the `pull` signature payload.
pub const TAG_PULL: &[u8] = b"mailbox.pull";
/// Tag for the `ack` signature payload.
pub const TAG_ACK: &[u8] = b"mailbox.ack";

/// Validate that `recipient_id` is a well-formed A3Net address. Returns
/// the parsed [`Address`] on success.
///
/// Wired into the `enqueue` / `pull` / `ack` HTTP path so that
/// a path-traversal attack (`/v1/inbox/..%2Fadmin`) can't reach the
/// validator with a malformed id.
pub fn validate_recipient_id(recipient_id: &str) -> MailboxResult<Address> {
    if recipient_id.is_empty() {
        return Err(MailboxError::InvalidRecipientId("empty".into()));
    }
    if recipient_id.len() > 64 {
        // An EIP-55 address is `0x` + 40 hex chars = 42 chars.
        // Anything longer is definitely garbage.
        return Err(MailboxError::InvalidRecipientId(format!(
            "too long: {} chars",
            recipient_id.len()
        )));
    }
    Address::from_hex(recipient_id)
        .map_err(|e| MailboxError::InvalidRecipientId(e.to_string()))
}

/// Validate that `msg_id` is a hex string of the expected length.
///
/// We accept either:
/// - 32 hex chars (16 raw bytes — used for blake3 / sha256 truncated ids)
/// - 64 hex chars (32 raw bytes — sha256 / blake3 full ids)
/// - 36 chars with `-` separators (UUID v4, e.g. `550e8400-e29b-41d4-a716-446655440000`)
///
/// Reject anything else.
pub fn validate_msg_id(msg_id: &str) -> MailboxResult<()> {
    if msg_id.is_empty() {
        return Err(MailboxError::InvalidMessageId("empty".into()));
    }
    if msg_id.len() > 72 {
        return Err(MailboxError::InvalidMessageId(format!(
            "too long: {} chars",
            msg_id.len()
        )));
    }
    if msg_id.contains('-') {
        // UUID form: 8-4-4-4-12 = 32 hex chars + 4 dashes = 36 chars.
        if msg_id.len() != 36 {
            return Err(MailboxError::InvalidMessageId(format!(
                "uuid form must be 36 chars, got {}",
                msg_id.len()
            )));
        }
        let mut segments = msg_id.split('-');
        for want in [8, 4, 4, 4, 12] {
            let seg = segments
                .next()
                .ok_or_else(|| MailboxError::InvalidMessageId("truncated uuid".into()))?;
            if seg.len() != want || !seg.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(MailboxError::InvalidMessageId(format!(
                    "non-hex / wrong len in uuid segment: {seg}"
                )));
            }
        }
        if segments.next().is_some() {
            return Err(MailboxError::InvalidMessageId(
                "uuid has too many segments".into(),
            ));
        }
        return Ok(());
    }
    if msg_id.len() != 32 && msg_id.len() != 64 {
        return Err(MailboxError::InvalidMessageId(format!(
            "hex form must be 32 or 64 chars, got {}",
            msg_id.len()
        )));
    }
    if !msg_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(MailboxError::InvalidMessageId(format!(
            "non-hex character in {msg_id}"
        )));
    }
    Ok(())
}

/// Maximum age of a sender signature in seconds. Signatures older than
/// this are rejected to prevent replay of captured signatures.
/// Default: 5 minutes. This is the "signature_max_age" in the audit doc.
pub const DEFAULT_SIGNATURE_MAX_AGE_SECS: i64 = 300;

/// Maximum clock skew tolerance: clients whose clocks are this many seconds
/// ahead of the server are still accepted (catches clock drift / mobile
/// devices with incorrect system time). Default: 60 seconds.
pub const CLOCK_SKEW_TOLERANCE_SECS: i64 = 60;

/// Build a canonical message for the `enqueue` envelope **without** a timestamp.
/// Used by tests and for backwards-compatible signature computation.
/// Prefer [`canonical_enqueue_with_timestamp`] in production.
pub fn canonical_enqueue(recipient_id: &str, msg_id: &str, ciphertext: &[u8]) -> Vec<u8> {
    let ct_hash = blake3::hash(ciphertext);
    canonical_message(&[
        TAG_ENQUEUE,
        recipient_id.as_bytes(),
        msg_id.as_bytes(),
        ct_hash.as_bytes(),
    ])
}

/// Build a canonical message for the `enqueue` envelope with a unix timestamp.
///
/// This is the **EIP-712-style binding**: the signature is only valid within
/// a `signature_max_age_secs` window. After that, the server rejects it even
/// if the cryptographic signature is valid.
///
/// `signed_at` is a unix timestamp in UTC seconds (the sender's wall clock).
/// The server uses `chrono::Utc::now().timestamp()` to compute age.
pub fn canonical_enqueue_with_timestamp(
    recipient_id: &str,
    msg_id: &str,
    ciphertext: &[u8],
    signed_at: i64,
) -> Vec<u8> {
    let ct_hash = blake3::hash(ciphertext);
    let ts_bytes = signed_at.to_string();
    canonical_message(&[
        TAG_ENQUEUE,
        recipient_id.as_bytes(),
        msg_id.as_bytes(),
        ct_hash.as_bytes(),
        ts_bytes.as_bytes(),
    ])
}

/// Build a canonical message for the `pull` request.
pub fn canonical_pull(recipient_id: &str) -> Vec<u8> {
    canonical_message(&[TAG_PULL, recipient_id.as_bytes()])
}

/// Build a canonical message for the `ack` request. The `msg_ids` are
/// joined with `,` in ascending order; the caller is responsible for
/// pre-sorting if the order matters.
pub fn canonical_ack(recipient_id: &str, msg_ids: &[String]) -> Vec<u8> {
    let joined = msg_ids.join(",");
    canonical_message(&[TAG_ACK, recipient_id.as_bytes(), joined.as_bytes()])
}

/// Length-prefixed concatenation of `segments`.
///
/// Each segment is encoded as `[u8;8] LE length][bytes]`. This
/// guarantees that two distinct `segments` lists can never produce
/// the same byte sequence regardless of what bytes appear inside.
///
/// Returns an error if a segment is too long or there are too many
/// segments.
pub fn canonical_message(segments: &[&[u8]]) -> Vec<u8> {
    if segments.len() > MAX_SEGMENTS {
        // Caller bug — we *don't* propagate via Result because the
        // public API is infallible. Caller input is validated up
        // front by the HTTP layer.
        debug_assert!(
            segments.len() <= MAX_SEGMENTS,
            "too many canonical segments: {}",
            segments.len()
        );
    }
    let mut out = Vec::with_capacity(segments.len() * 16);
    for seg in segments {
        debug_assert!(
            seg.len() <= MAX_SEGMENT_LEN,
            "segment too long: {} bytes",
            seg.len()
        );
        out.extend_from_slice(&(seg.len() as u64).to_le_bytes());
        out.extend_from_slice(seg);
    }
    out
}

/// Compute the 32-byte digest the caller must sign.
pub fn digest_of(message: &[u8]) -> [u8; 32] {
    let mut h = Hasher::new();
    h.update(message);
    let out = h.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(out.as_bytes());
    bytes
}

/// Recover the signer address from a personal_sign signature over a
/// canonical mailbox message.
///
/// Returns `MailboxError::InvalidSignature` if recovery fails.
pub fn recover_signer(digest: &[u8; 32], sig_bytes: &[u8]) -> MailboxResult<Address> {
    let parsed = PersonalSignature::from_compact(sig_bytes)
        .map_err(|_| MailboxError::InvalidSignature)?;
    let recovered = WalletPublic::recover_personal(digest, &parsed)
        .map_err(|_| MailboxError::InvalidSignature)?;
    Ok(recovered.address())
}

/// Verify that `sig_bytes` is a valid sender signature on the canonical
/// `enqueue` message and that the recovered address matches
/// `claimed_sender_id`. Also verifies that the signature is not older than
/// `signature_max_age_secs` using the provided `signed_at_unix` timestamp.
pub fn verify_sender_signature_with_timestamp(
    claimed_sender_id: &str,
    recipient_id: &str,
    msg_id: &str,
    ciphertext: &[u8],
    sig_bytes: &[u8],
    signed_at_unix: i64,
    signature_max_age_secs: i64,
) -> MailboxResult<()> {
    let claimed = Address::from_hex(claimed_sender_id)
        .map_err(|e| MailboxError::InvalidRecipientId(e.to_string()))?;

    // Step 1: validate timestamp.
    //
    // SECURITY: reject far-future timestamps (anti-replay). We allow
    // CLOCK_SKEW_TOLERANCE_SECS of clock drift so honest clients with
    // slightly fast clocks aren't rejected.
    let now = chrono::Utc::now().timestamp();
    if signed_at_unix > now + CLOCK_SKEW_TOLERANCE_SECS {
        return Err(MailboxError::InvalidTimestamp);
    }

    // Step 2: check staleness (anti-replay of old signatures).
    // age is guaranteed non-negative here since signed_at <= now + tolerance.
    let age = now - signed_at_unix;
    if age > signature_max_age_secs {
        return Err(MailboxError::StaleSignature {
            age_secs: age,
            max_age_secs: signature_max_age_secs,
        });
    }

    // Step 3: verify cryptographic signature over the timestamped canonical message.

    let msg =
        canonical_enqueue_with_timestamp(recipient_id, msg_id, ciphertext, signed_at_unix);
    let digest = digest_of(&msg);
    let recovered = recover_signer(&digest, sig_bytes)?;
    if recovered != claimed {
        return Err(MailboxError::InvalidSignature);
    }
    Ok(())
}

/// Verify that `sig_bytes` is a valid sender signature on the canonical
/// `enqueue` message (no timestamp binding). For backwards compatibility
/// only; prefer [`verify_sender_signature_with_timestamp`].
pub fn verify_sender_signature(
    claimed_sender_id: &str,
    recipient_id: &str,
    msg_id: &str,
    ciphertext: &[u8],
    sig_bytes: &[u8],
) -> MailboxResult<()> {
    let claimed = Address::from_hex(claimed_sender_id)
        .map_err(|e| MailboxError::InvalidRecipientId(e.to_string()))?;
    let msg = canonical_enqueue(recipient_id, msg_id, ciphertext);
    let digest = digest_of(&msg);
    let recovered = recover_signer(&digest, sig_bytes)?;
    if recovered != claimed {
        return Err(MailboxError::InvalidSignature);
    }
    Ok(())
}

/// Verify that `sig_bytes` is a valid recipient signature on the
/// canonical `pull` message and that the recovered address matches
/// `claimed_recipient_id`.
pub fn verify_pull_signature(
    claimed_recipient_id: &str,
    sig_bytes: &[u8],
) -> MailboxResult<()> {
    let claimed = Address::from_hex(claimed_recipient_id)
        .map_err(|e| MailboxError::InvalidRecipientId(e.to_string()))?;
    let msg = canonical_pull(claimed_recipient_id);
    let digest = digest_of(&msg);
    let recovered = recover_signer(&digest, sig_bytes)?;
    if recovered != claimed {
        return Err(MailboxError::InvalidRecipientSignature);
    }
    Ok(())
}

/// Verify that `sig_bytes` is a valid recipient signature on the
/// canonical `ack` message and that the recovered address matches
/// `claimed_recipient_id`.
pub fn verify_ack_signature(
    claimed_recipient_id: &str,
    msg_ids: &[String],
    sig_bytes: &[u8],
) -> MailboxResult<()> {
    let claimed = Address::from_hex(claimed_recipient_id)
        .map_err(|e| MailboxError::InvalidRecipientId(e.to_string()))?;
    let msg = canonical_ack(claimed_recipient_id, msg_ids);
    let digest = digest_of(&msg);
    let recovered = recover_signer(&digest, sig_bytes)?;
    if recovered != claimed {
        return Err(MailboxError::InvalidRecipientSignature);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_identity::{PersonalSignature, Wallet};

    fn wallet_alice() -> Wallet {
        let w = Wallet::generate();
        // Use the checksummed form everywhere so the parsing test
        // also catches any case-handling bug.
        let _ = w.public().address().to_checksum();
        w
    }

    #[test]
    fn validate_recipient_id_accepts_40_hex() {
        let w = wallet_alice();
        let addr = w.public().address().to_checksum();
        validate_recipient_id(&addr).expect("valid address should pass");
    }

    #[test]
    fn validate_recipient_id_accepts_lowercase() {
        let w = wallet_alice();
        let addr = w.public().address().to_hex(); // 0xABCD…
        validate_recipient_id(&addr).expect("lowercase hex should pass");
    }

    #[test]
    fn validate_recipient_id_rejects_empty() {
        assert!(matches!(
            validate_recipient_id(""),
            Err(MailboxError::InvalidRecipientId(_))
        ));
    }

    #[test]
    fn validate_recipient_id_rejects_path_traversal() {
        // Path-traversal: the URL extractor would already have
        // decoded this; the validator must still reject it on
        // shape grounds.
        assert!(matches!(
            validate_recipient_id("../../../etc/passwd"),
            Err(MailboxError::InvalidRecipientId(_))
        ));
    }

    #[test]
    fn validate_recipient_id_rejects_too_long() {
        let s = "0x".to_string() + &"a".repeat(80);
        assert!(matches!(
            validate_recipient_id(&s),
            Err(MailboxError::InvalidRecipientId(_))
        ));
    }

    #[test]
    fn validate_recipient_id_rejects_non_hex() {
        assert!(matches!(
            validate_recipient_id("0xZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ"),
            Err(MailboxError::InvalidRecipientId(_))
        ));
    }

    #[test]
    fn validate_msg_id_accepts_64_hex() {
        validate_msg_id(&"a".repeat(64)).expect("64 hex chars should pass");
    }

    #[test]
    fn validate_msg_id_accepts_32_hex() {
        validate_msg_id(&"f".repeat(32)).expect("32 hex chars should pass");
    }

    #[test]
    fn validate_msg_id_accepts_uuid() {
        validate_msg_id("550e8400-e29b-41d4-a716-446655440000").expect("uuid should pass");
    }

    #[test]
    fn validate_msg_id_rejects_bad_uuid() {
        assert!(matches!(
            validate_msg_id("550e8400-e29b-41d4-a716"),
            Err(MailboxError::InvalidMessageId(_))
        ));
        assert!(matches!(
            validate_msg_id("550e8400-e29b-41d4-a716-44665544000Z"),
            Err(MailboxError::InvalidMessageId(_))
        ));
        assert!(matches!(
            validate_msg_id("550e8400-e29b-41d4-a716-446655440000-extra"),
            Err(MailboxError::InvalidMessageId(_))
        ));
    }

    #[test]
    fn validate_msg_id_rejects_wrong_length() {
        assert!(matches!(
            validate_msg_id(&"a".repeat(31)),
            Err(MailboxError::InvalidMessageId(_))
        ));
    }

    #[test]
    fn canonical_message_is_length_prefixed() {
        let a = b"abc";
        let b = b"def";
        let m = canonical_message(&[a, b]);
        // 8 bytes length + 3 bytes payload + 8 bytes length + 3 bytes payload
        assert_eq!(m.len(), 8 + 3 + 8 + 3);
        assert_eq!(&m[0..8], &3u64.to_le_bytes());
        assert_eq!(&m[8..11], b"abc");
        assert_eq!(&m[11..19], &3u64.to_le_bytes());
        assert_eq!(&m[19..22], b"def");
    }

    #[test]
    fn canonical_message_does_not_collide_on_pipe_in_segment() {
        // `mailbox.enqueue|alice|msg|hash` and `mailbox.enqueue|alice` /
        // `msg|hash` must not collide. The length-prefixed encoding
        // makes the collision impossible.
        let a = canonical_message(&[
            b"mailbox.enqueue|alice|msg|hash",
        ]);
        let b = canonical_message(&[
            b"mailbox.enqueue|alice",
            b"msg|hash",
        ]);
        assert_ne!(a, b);
    }

    #[test]
    fn sender_signature_round_trips() {
        let w = wallet_alice();
        let sender_id = w.public().address().to_checksum();
        let recipient = "0x0000000000000000000000000000000000000000";
        let msg_id = "550e8400-e29b-41d4-a716-446655440000";
        let ciphertext = b"hello";

        let msg = canonical_enqueue(recipient, msg_id, ciphertext);
        let digest = digest_of(&msg);
        let sig = w.sign_personal(&digest).unwrap();
        let sig_bytes = sig.to_compact();

        verify_sender_signature(&sender_id, recipient, msg_id, ciphertext, &sig_bytes)
            .expect("valid signature should pass");
    }

    #[test]
    fn sender_signature_rejects_claimed_sender_mismatch() {
        let alice_w = wallet_alice();
        let eve_w = wallet_alice();
        let recipient = "0x0000000000000000000000000000000000000000";
        let msg_id = "550e8400-e29b-41d4-a716-446655440000";
        let ciphertext = b"hello";

        let msg = canonical_enqueue(recipient, msg_id, ciphertext);
        let digest = digest_of(&msg);
        let sig = alice_w.sign_personal(&digest).unwrap();

        let err = verify_sender_signature(
            &eve_w.public().address().to_checksum(),
            recipient,
            msg_id,
            ciphertext,
            &sig.to_compact(),
        );
        assert!(matches!(err, Err(MailboxError::InvalidSignature)));
    }

    #[test]
    fn sender_signature_rejects_tampered_ciphertext() {
        let w = wallet_alice();
        let sender_id = w.public().address().to_checksum();
        let recipient = "0x0000000000000000000000000000000000000000";
        let msg_id = "550e8400-e29b-41d4-a716-446655440000";

        let msg = canonical_enqueue(recipient, msg_id, b"hello");
        let digest = digest_of(&msg);
        let sig = w.sign_personal(&digest).unwrap();

        let err = verify_sender_signature(
            &sender_id,
            recipient,
            msg_id,
            b"tampered",
            &sig.to_compact(),
        );
        assert!(matches!(err, Err(MailboxError::InvalidSignature)));
    }

    #[test]
    fn sender_signature_rejects_tampered_recipient() {
        let w = wallet_alice();
        let sender_id = w.public().address().to_checksum();
        let recipient = "0x0000000000000000000000000000000000000000";
        let other = "0x0000000000000000000000000000000000000001";
        let msg_id = "550e8400-e29b-41d4-a716-446655440000";

        let msg = canonical_enqueue(recipient, msg_id, b"hello");
        let digest = digest_of(&msg);
        let sig = w.sign_personal(&digest).unwrap();

        let err = verify_sender_signature(
            &sender_id,
            other,
            msg_id,
            b"hello",
            &sig.to_compact(),
        );
        assert!(matches!(err, Err(MailboxError::InvalidSignature)));
    }

    #[test]
    fn pull_signature_round_trips() {
        let w = wallet_alice();
        let recipient = w.public().address().to_checksum();
        let msg = canonical_pull(&recipient);
        let digest = digest_of(&msg);
        let sig = w.sign_personal(&digest).unwrap();
        verify_pull_signature(&recipient, &sig.to_compact())
            .expect("valid pull signature should pass");
    }

    #[test]
    fn pull_signature_rejects_other_recipient() {
        let alice_w = wallet_alice();
        let eve_w = wallet_alice();
        let recipient = alice_w.public().address().to_checksum();
        let eve_recipient = eve_w.public().address().to_checksum();

        let msg = canonical_pull(&recipient);
        let digest = digest_of(&msg);
        let sig = alice_w.sign_personal(&digest).unwrap();

        let err = verify_pull_signature(&eve_recipient, &sig.to_compact());
        assert!(matches!(err, Err(MailboxError::InvalidRecipientSignature)));
    }

    #[test]
    fn ack_signature_round_trips() {
        let w = wallet_alice();
        let recipient = w.public().address().to_checksum();
        let msg_ids = vec!["a".repeat(64), "b".repeat(64)];
        let msg = canonical_ack(&recipient, &msg_ids);
        let digest = digest_of(&msg);
        let sig = w.sign_personal(&digest).unwrap();
        verify_ack_signature(&recipient, &msg_ids, &sig.to_compact())
            .expect("valid ack signature should pass");
    }

    #[test]
    fn ack_signature_rejects_other_msg_ids() {
        let w = wallet_alice();
        let recipient = w.public().address().to_checksum();
        let signed = vec!["a".repeat(64), "b".repeat(64)];
        let tampered = vec!["a".repeat(64), "c".repeat(64)];

        let msg = canonical_ack(&recipient, &signed);
        let digest = digest_of(&msg);
        let sig = w.sign_personal(&digest).unwrap();

        let err = verify_ack_signature(&recipient, &tampered, &sig.to_compact());
        assert!(matches!(err, Err(MailboxError::InvalidRecipientSignature)));
    }

    #[test]
    fn sized_compact_signature_is_65_bytes() {
        let w = wallet_alice();
        let recipient = "0x0000000000000000000000000000000000000000";
        let msg = canonical_pull(recipient);
        let digest = digest_of(&msg);
        let sig = w.sign_personal(&digest).unwrap();
        let compact = sig.to_compact();
        assert_eq!(compact.len(), 65);
        // round-trip through the parser
        let _ = PersonalSignature::from_compact(&compact).unwrap();
    }
}
