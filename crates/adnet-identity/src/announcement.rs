//! Helpers that sign and verify [`adnet_types::Announcement`] payloads.
//!
//! The wire format for a signed announcement:
//!
//! - The signature is `[u8]` whose first byte is the scheme tag
//!   (see [`crate::SIG_SCHEME_EIP191_SECP256K1`]) and whose remainder
//!   is the raw signature bytes (65 bytes for EIP-191 over secp256k1:
//!   `r || s || v`).
//! - The signing preimage is `blake3(signing_preimage_bytes)` where
//!   `signing_preimage_bytes` is the canonical JSON produced by
//!   [`adnet_types::Announcement::signing_preimage`]. BLAKE3 is the
//!   ADNet-wide hashing choice (see `adnet-types` and `adnet-blobstore`).
//! - The recovered address must equal the wallet's address, and that
//!   address (lowercase hex) is what the announcement carries in its
//!   `signer` field.

use adnet_types::Announcement;

use crate::ProtocolWalletAddress;
use crate::error::{IdentityError, Result};
use crate::wallet::{Wallet, WalletPublic};

/// Tag prepended to the raw signature bytes so the wire format can
/// evolve to multiple schemes without breaking old verifiers.
pub fn signature_with_scheme(scheme: u8, raw_signature: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + raw_signature.len());
    out.push(scheme);
    out.extend_from_slice(raw_signature);
    out
}

/// Compute the signing digest for an announcement.
pub fn announcement_digest(ann: &Announcement) -> Result<[u8; 32]> {
    let preimage = ann
        .signing_preimage()
        .map_err(|e| IdentityError::InvalidSignature(format!("{e}")))?;
    let mut h = blake3::Hasher::new();
    h.update(&preimage);
    let out = h.finalize();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(out.as_bytes());
    Ok(digest)
}

/// Sign an announcement in place. The wallet's address is recorded in
/// `ann.signer` and the signature (scheme tag + `r||s||v`) goes into
/// `ann.signature`.
pub fn sign_announcement(ann: &mut Announcement, wallet: &Wallet) -> Result<()> {
    let digest = announcement_digest(ann)?;
    let sig = wallet.sign_personal(&digest)?;
    let wallet_addr: ProtocolWalletAddress = wallet.public().address().into();
    ann.attach_signature(
        wallet_addr,
        signature_with_scheme(crate::SIG_SCHEME_EIP191_SECP256K1, &sig.to_compact()),
    );
    Ok(())
}

/// Verify the signature on an announcement.
///
/// **Callers must run [`Announcement::validate`] first** — this
/// function deliberately does *not* check field-level bounds
/// (`signature.len()`, `timestamp` clock skew, partially-signed
/// fields) so the wire-format guard stays in `adnet-types`. The
/// identity crate only owns cryptographic verification.
///
/// Returns `Ok(())` if the announcement is unsigned (no `signer` field
/// AND no `signature` field). Returns an error when exactly one of the
/// two is present — a partially-signed announcement is malformed and
/// must not be silently accepted.
///
/// Returns an error if a signature is malformed, fails to recover, or
/// the recovered address doesn't match `ann.signer`.
pub fn verify_announcement(ann: &Announcement) -> Result<()> {
    let (signer, sig) = match (&ann.signer, &ann.signature) {
        (Some(s), Some(sig)) => (s, sig),
        (None, None) => return Ok(()), // unsigned — nothing to check.
        (Some(_), None) => {
            return Err(IdentityError::InvalidSignature(
                "announcement has signer but no signature".into(),
            ));
        }
        (None, Some(_)) => {
            return Err(IdentityError::InvalidSignature(
                "announcement has signature but no signer".into(),
            ));
        }
    };
    if sig.is_empty() {
        return Err(IdentityError::InvalidSignature(
            "announcement signature is empty".into(),
        ));
    }
    let scheme = sig[0];
    let raw = &sig[1..];
    if scheme == crate::SIG_SCHEME_EIP191_SECP256K1 {
        if raw.len() != 65 {
            return Err(IdentityError::InvalidSignature(format!(
                "EIP-191 signature must be 65 bytes, got {}",
                raw.len()
            )));
        }
        let compact: [u8; 65] = raw.try_into().expect("length checked above");
        let parsed = crate::signing::PersonalSignature::from_compact(&compact)?;
        let digest = announcement_digest(ann)?;
        let recovered = WalletPublic::recover_personal(&digest, &parsed)?;
        let recovered_addr: ProtocolWalletAddress = recovered.address().into();
        if recovered_addr != *signer {
            return Err(IdentityError::InvalidSignature(
                "announcement signer does not match recovered address".into(),
            ));
        }
        Ok(())
    } else {
        Err(IdentityError::InvalidSignature(format!(
            "unsupported signature scheme tag {scheme}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_announcement() -> Announcement {
        use adnet_types::{CdnContentKind, ContentHash, NodeId, RoomId};
        Announcement {
            room_id: RoomId::new("lobby"),
            content_hash: ContentHash::from_bytes(b"hello"),
            node_id: NodeId::random(),
            title: "t".into(),
            kind: CdnContentKind::Article,
            size_bytes: 5,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: chrono::Utc::now(),
            signer: None,
            signature: None,
        }
    }

    #[test]
    fn unsigned_passes() {
        let ann = sample_announcement();
        verify_announcement(&ann).unwrap();
    }

    #[test]
    fn partially_signed_signer_only_fails() {
        // `signer` set but `signature` missing — this used to slip
        // through the old wildcard match. It must now be rejected.
        let mut ann = sample_announcement();
        ann.signer = Some(ProtocolWalletAddress::from_bytes([0x33u8; 20]));
        let err = verify_announcement(&ann).unwrap_err();
        assert!(err.to_string().contains("no signature"), "got {err}");
    }

    #[test]
    fn partially_signed_signature_only_fails() {
        // `signature` set but `signer` missing — likewise must be
        // rejected so we never accept an anonymous signature.
        let mut ann = sample_announcement();
        ann.signature = Some(vec![crate::SIG_SCHEME_EIP191_SECP256K1; 66]);
        let err = verify_announcement(&ann).unwrap_err();
        assert!(err.to_string().contains("no signer"), "got {err}");
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let wallet = Wallet::generate();
        let mut ann = sample_announcement();
        sign_announcement(&mut ann, &wallet).unwrap();
        assert!(ann.is_signed());
        verify_announcement(&ann).unwrap();
    }

    #[test]
    fn tampered_announcement_fails() {
        let wallet = Wallet::generate();
        let mut ann = sample_announcement();
        sign_announcement(&mut ann, &wallet).unwrap();
        // Tamper with the title after signing.
        ann.title = "tampered".into();
        let err = verify_announcement(&ann).unwrap_err();
        assert!(err.to_string().contains("does not match"), "got {err}");
    }

    #[test]
    fn wrong_signer_field_fails() {
        let wallet = Wallet::generate();
        let mut ann = sample_announcement();
        sign_announcement(&mut ann, &wallet).unwrap();
        // Replace the announced signer with a different wallet address.
        let other = ProtocolWalletAddress::from_bytes([0x99u8; 20]);
        ann.signer = Some(other);
        let err = verify_announcement(&ann).unwrap_err();
        assert!(err.to_string().contains("does not match"), "got {err}");
    }

    #[test]
    fn rejects_unknown_scheme_tag() {
        let wallet = Wallet::generate();
        let mut ann = sample_announcement();
        sign_announcement(&mut ann, &wallet).unwrap();
        ann.signature.as_mut().unwrap()[0] = 0x7f;
        let err = verify_announcement(&ann).unwrap_err();
        assert!(err.to_string().contains("unsupported"), "got {err}");
    }

    #[test]
    fn rejects_malformed_signature_length() {
        let mut ann = sample_announcement();
        ann.attach_signature(
            ProtocolWalletAddress::from_bytes([0x11u8; 20]),
            vec![crate::SIG_SCHEME_EIP191_SECP256K1, 0, 0, 0], // 3 raw bytes
        );
        let err = verify_announcement(&ann).unwrap_err();
        assert!(err.to_string().contains("65 bytes"), "got {err}");
    }
}
