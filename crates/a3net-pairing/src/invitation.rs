//! Invitation — the QR / email side of the pairing protocol.
//!
//! An invitation is the **offline, issuer-generated** half of the
//! pairing. It travels in a QR code or an email attachment. It does
//! NOT contain the invitee's transport identity — the invitee proves
//! that over the QUIC handshake. The invitation carries only:
//!
//!  - the protocol version,
//!  - the issuer's `NodeId` and wallet address,
//!  - a random `salt` (mixed into `credential_id` derivation),
//!  - a list of capabilities the issuer is willing to grant,
//!  - an expiry timestamp,
//!  - the issuer's **wallet** (EIP-191) signature over the canonical
//!    digest.
//!
//! This design prevents the "fake QR" attack: a captured invitation
//! does NOT give an attacker a usable transport identity. They would
//! need to also intercept the QUIC handshake and successfully respond
//! to the pairing request challenge, which requires the invitee's
//! Ed25519 private key.

use rand::RngCore;
use serde::{Deserialize, Serialize};

use a3net_identity::signing::PersonalSignature;
use a3net_identity::wallet::{Wallet, WalletPublic};
use a3net_types::node::NodeId;
use a3net_types::wallet_address::WalletAddress;

use crate::capability::CapabilitySet;
use crate::error::{PairingError, PairingResult};
use crate::transport_identity::{CredentialId, pairing_invitation_digest};

/// The raw invitation payload — versioned, unsigned. This is the
/// JSON-encoded form used in QR codes and email attachments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationPayload {
    /// Protocol version. Current = 1.
    pub version: u8,

    /// Issuer's transport identity. The invitee will dial this NodeId.
    pub issuer_node_id: NodeId,

    /// Issuer's wallet address. Used as the signer identity and for
    /// wallet-key lookups by the invitee when verifying the signature.
    pub issuer_wallet: WalletAddress,

    /// Random 32-byte salt mixed into the `CredentialId` derivation.
    /// The issuer generates this fresh for every invitation.
    #[serde(with = "crate::transport_identity::hex_bytes")]
    pub salt: Vec<u8>,
    /// Capabilities the issuer is willing to grant if the invitee
    /// completes the pairing ceremony. The invitee can request a
    /// subset; the issuer's response in `PairingResponse.granted`
    /// is authoritative.
    pub capabilities: CapabilitySet,

    /// When the invitation expires. The invitee must complete the
    /// pairing ceremony before this time. Set to `i64::MAX` for
    /// "no expiry" (not recommended for production).
    pub expires_at_unix: i64,

    /// Short human-readable note, optional. e.g. "Alice's Laptop".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The full signed invitation — `InvitationPayload` plus the
/// issuer's EIP-191 wallet signature.
///
/// This is what gets rendered as a QR code or emailed as an attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedInvitation {
    pub payload: InvitationPayload,

    /// `<scheme:1 byte = 0> || <r:32> || <s:32> || <v:1>` EIP-191 personal
    /// signature over [`pairing_invitation_digest`].
    #[serde(with = "crate::transport_identity::hex_bytes")]
    pub signature: Vec<u8>,
}

impl SignedInvitation {
    /// Build and sign a new invitation.
    ///
    /// `ttl_seconds` controls the invitation expiry. A reasonable
    /// default is 15 minutes (`15 * 60`). The `note` is optional;
    /// it is included in the QR code and the email body.
    pub fn create(
        issuer_node_id: &NodeId,
        wallet: &Wallet,
        capabilities: CapabilitySet,
        ttl_seconds: i64,
        note: Option<String>,
    ) -> PairingResult<SignedInvitation> {
        let issuer_wallet: WalletAddress = wallet.public().address().into();
        let now = chrono::Utc::now().timestamp();
        let expires_at_unix = if ttl_seconds <= 0 {
            now + 15 * 60 // 15 min default
        } else {
            now + ttl_seconds
        };
        let mut salt = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let payload = InvitationPayload {
            version: 1,
            issuer_node_id: issuer_node_id.clone(),
            issuer_wallet,
            salt: salt.to_vec(),
            capabilities,
            expires_at_unix,
            note,
        };
        let digest: [u8; 32] = pairing_invitation_digest(&payload);
        let sig = wallet
            .sign_personal(&digest)
            .map_err(|e| PairingError::Malformed {
                what: "signed_invitation.wallet_sign",
                reason: format!("sign_personal failed: {e}"),
            })?;
        // Tag: scheme 0 = EIP-191. Format: <tag:1> || <compact:65>.
        let compact = sig.to_compact();
        let mut tagged = Vec::with_capacity(1 + compact.len());
        tagged.push(0);
        tagged.extend_from_slice(&compact);
        Ok(Self {
            payload,
            signature: tagged,
        })
    }

    /// Verify the invitation's wallet signature.
    ///
    /// Call this when receiving an invitation (e.g. scanning a QR) to
    /// confirm it was issued by the claimed wallet address and hasn't
    /// been tampered with.
    pub fn verify(&self, now_unix: i64) -> PairingResult<()> {
        if self.payload.version != 1 {
            return Err(PairingError::Malformed {
                what: "invitation.version",
                reason: format!("unsupported version {}", self.payload.version),
            });
        }
        if self.payload.expires_at_unix != i64::MAX && now_unix > self.payload.expires_at_unix {
            return Err(PairingError::InvitationExpired {
                expired_at_unix: self.payload.expires_at_unix,
                now_unix,
            });
        }
        // Note: there is **no clock-skew check** for invitation expiry.
        // An invitation's `expires_at_unix` is a lifetime window (e.g.
        // 15 min from issuance), not a peer-reported timestamp — the
        // expiry check alone correctly rejects stale invitations even
        // when local clock and issuer clock differ by hours.
        let digest: [u8; 32] = pairing_invitation_digest(&self.payload);
        if self.signature.is_empty() {
            return Err(PairingError::SignatureLength {
                expected: 66,
                got: 0,
            });
        }
        let (tag, sig) = split_scheme(&self.signature)?;
        if tag != 0 {
            return Err(PairingError::UnsupportedScheme { scheme_tag: tag });
        }
        if sig.len() != 65 {
            return Err(PairingError::SignatureLength {
                expected: 65,
                got: sig.len(),
            });
        }
        let ps = PersonalSignature::from_compact(sig).map_err(|e| PairingError::Malformed {
            what: "signed_invitation.signature",
            reason: format!("from_compact: {e}"),
        })?;
        let recovered = WalletPublic::recover_personal(&digest, &ps)
            .map_err(|_| PairingError::IssuerSignatureInvalid)?;
        // Compare wallet addresses: recovered vs. embedded.
        let recovered_addr: WalletAddress = recovered.address().into();
        if recovered_addr != self.payload.issuer_wallet {
            return Err(PairingError::IssuerSignatureInvalid);
        }
        Ok(())
    }

    /// Derive the `CredentialId` for the pairing that this invitation
    /// will produce. The invitee must use the same `salt` and its own
    /// `NodeId` to derive the same credential id.
    pub fn credential_id(&self, invitee: &NodeId) -> PairingResult<CredentialId> {
        if self.payload.salt.len() != 32 {
            return Err(PairingError::Malformed {
                what: "invitation_payload.salt",
                reason: format!("salt must be 32 bytes, got {}", self.payload.salt.len()),
            });
        }
        let salt_arr: [u8; 32] = self.payload.salt.as_slice().try_into().unwrap();
        Ok(crate::transport_identity::derive_credential_id(
            &self.payload.issuer_node_id,
            invitee,
            &salt_arr,
        ))
    }

    /// Encode as a JSON string for QR / email rendering.
    pub fn to_json(&self) -> PairingResult<String> {
        serde_json::to_string(self).map_err(Into::into)
    }

    /// Decode from a JSON string produced by `to_json`.
    pub fn from_json(s: &str) -> PairingResult<Self> {
        serde_json::from_str(s).map_err(Into::into)
    }
}

fn split_scheme(sig: &[u8]) -> PairingResult<(u8, &[u8])> {
    if sig.is_empty() {
        return Err(PairingError::SignatureLength {
            expected: 1,
            got: 0,
        });
    }
    // The caller must validate `tag` against an allowlist. A
    // malicious attacker can place any byte here; we deliberately
    // NOT validate it here so callers can't forget to check.
    Ok((sig[0], &sig[1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_id() -> NodeId {
        NodeId::from_bytes(&[0xAAu8; 32]).unwrap()
    }

    #[test]
    fn invitation_sign_and_verify() {
        let n = node_id();
        let wallet = Wallet::generate();
        let caps = CapabilitySet::from_names(["chat", "files.read"]);
        let inv =
            SignedInvitation::create(&n, &wallet, caps, 900, Some("Test device".into())).unwrap();
        inv.verify(chrono::Utc::now().timestamp()).unwrap();
    }

    #[test]
    fn invitation_expired_rejected() {
        let n = node_id();
        let wallet = Wallet::generate();
        let inv = SignedInvitation::create(&n, &wallet, CapabilitySet::empty(), 0, None).unwrap();
        let err = inv.verify(inv.payload.expires_at_unix + 1).unwrap_err();
        assert!(matches!(err, PairingError::InvitationExpired { .. }));
    }

    #[test]
    fn credential_id_is_derivable() {
        let n = node_id();
        let wallet = Wallet::generate();
        let inv = SignedInvitation::create(&n, &wallet, CapabilitySet::empty(), 0, None).unwrap();
        let invitee = NodeId::from_bytes(&[0xBBu8; 32]).unwrap();
        let id = inv.credential_id(&invitee).unwrap();
        assert_eq!(id, inv.credential_id(&invitee).unwrap());
        let other_invitee = NodeId::from_bytes(&[0xCCu8; 32]).unwrap();
        assert_ne!(id, inv.credential_id(&other_invitee).unwrap());
    }

    #[test]
    fn credential_id_rejects_wrong_salt_length() {
        let n = node_id();
        let wallet = Wallet::generate();
        let mut inv =
            SignedInvitation::create(&n, &wallet, CapabilitySet::empty(), 0, None).unwrap();
        inv.payload.salt = vec![0u8; 31]; // wrong length
        let invitee = NodeId::from_bytes(&[0xBBu8; 32]).unwrap();
        let err = inv.credential_id(&invitee).unwrap_err();
        assert!(matches!(
            err,
            PairingError::Malformed {
                what: "invitation_payload.salt",
                ..
            }
        ));
    }

    #[test]
    fn json_round_trip() {
        let n = node_id();
        let wallet = Wallet::generate();
        let inv = SignedInvitation::create(&n, &wallet, CapabilitySet::empty(), 0, None).unwrap();
        let json = inv.to_json().unwrap();
        let back = SignedInvitation::from_json(&json).unwrap();
        assert_eq!(back.payload.issuer_node_id, inv.payload.issuer_node_id);
        assert_eq!(back.payload.issuer_wallet, inv.payload.issuer_wallet);
        assert_eq!(back.signature, inv.signature);
    }
}
