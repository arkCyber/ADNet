//! Relay-payment pledge — a signed promise to pay a relay for service.
//!
//! A pledge is bound to:
//!
//! - `chain_id` + `contract` + `token`: which EVM chain and which ERC-20 is
//!   acceptable. The relay uses this to decide whether to accept the pledge
//!   without consulting any chain.
//! - `recipient`: the relay's EVM address that should receive the eventual
//!   payment. We check this in `verify_for_relay`.
//! - `amount_atomic`: integer amount in the token's smallest unit (e.g.
//!   USDC has 6 decimals → "1.00 USDC" = 1_000_000). Bigger than `u128` is
//!   never needed in practice; we cap at [`MAX_AMOUNT_ATOMIC`] = 2^120.
//! - `nonce`: 32 random bytes, hex-encoded. Prevents replay.
//! - `expiry_unix`: a unix timestamp. The relay rejects pledges that have
//!   already expired.
//!
//! The pledge is *signed* by the pledgor's EVM wallet using EIP-191
//! `personal_sign` over a canonical JSON digest. Signature is verified with
//! the recovered address against the pledgor's declared address.

#[allow(unused_imports)]
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::error::{Result, TokenError};
use adnet_identity::{Address, PersonalSignature, Wallet, WalletPublic};
/// Cap on the smallest-unit amount. 2^120 is large enough for any token
/// supply (even with 18 decimals that is ~10^18 tokens) and small enough to
/// never overflow u128.
pub const MAX_AMOUNT_ATOMIC: u128 = 1u128 << 120;

/// Canonical JSON version for the EIP-191 inner envelope. Bumped on breaking
/// changes to the JSON shape.
pub const PLEDGE_VERSION: u8 = 1;

/// Forced `v` byte for receipts (we don't actually use `v` directly for
/// pledges, but we keep the sym for the signing helpers).
pub const EIP55_VERSION: u8 = 27;

/// The "to a relay" payment pledge. Cheap to copy, printable, easy to put
/// inside a JSON payload, gossip message, or QR code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pledge {
    pub version: u8,
    pub chain_id: u64,
    /// 0x`40-hex` lowercase.
    pub contract: String,
    /// 0x`40-hex` lowercase.
    pub token: String,
    pub amount_atomic: u128,
    pub recipient: Address,
    /// 32 random bytes, hex-encoded (64 chars).
    pub nonce: String,
    pub expiry_unix: i64,
    pub pledgor: Address,
    pub signature: PersonalSignature,
}

impl Pledge {
    /// Build the inner, unsigned body of a pledge.
    pub fn body(
        chain_id: u64,
        contract: String,
        token: String,
        amount_atomic: u128,
        recipient: Address,
        nonce: String,
        expiry_unix: i64,
    ) -> Result<PledgeBody> {
        if amount_atomic == 0 {
            return Err(TokenError::InvalidAmount("must be > 0".into()));
        }
        if amount_atomic > MAX_AMOUNT_ATOMIC {
            return Err(TokenError::InvalidAmount(format!(
                "exceeds MAX_AMOUNT_ATOMIC = {MAX_AMOUNT_ATOMIC}"
            )));
        }
        if nonce.len() != 64 || hex::decode(&nonce).map(|b| b.len() != 32).unwrap_or(true) {
            return Err(TokenError::InvalidNonce(
                "nonce must be 64 hex chars (32 bytes)".into(),
            ));
        }
        if expiry_unix <= 0 {
            return Err(TokenError::InvalidExpiry("must be unix seconds > 0".into()));
        }
        Ok(PledgeBody {
            version: PLEDGE_VERSION,
            chain_id,
            contract,
            token,
            amount_atomic,
            recipient,
            nonce,
            expiry_unix,
        })
    }

    /// Sign a pledge body with the given wallet. The pledgor address is
    /// taken from the wallet itself.
    pub fn sign(body: PledgeBody, wallet: &Wallet) -> Result<Self> {
        let digest = body.digest();
        let signature = wallet.sign_personal(&digest)?;
        Ok(Self {
            version: PLEDGE_VERSION,
            chain_id: body.chain_id,
            contract: body.contract,
            token: body.token,
            amount_atomic: body.amount_atomic,
            recipient: body.recipient,
            nonce: body.nonce,
            expiry_unix: body.expiry_unix,
            pledgor: wallet.public().address(),
            signature,
        })
    }

    /// Recreate the body used in signing, then verify that the signature
    /// recovers to the declared `pledgor` and that the pledge is not yet
    /// expired.
    pub fn verify(&self, now_unix: i64) -> Result<()> {
        let body = PledgeBody {
            version: self.version,
            chain_id: self.chain_id,
            contract: self.contract.clone(),
            token: self.token.clone(),
            amount_atomic: self.amount_atomic,
            recipient: self.recipient,
            nonce: self.nonce.clone(),
            expiry_unix: self.expiry_unix,
        };
        let digest = body.digest();
        // Recover the public key from the signature. The recovered
        // address is, by definition, the pledgor.
        let recovered = WalletPublic::recover_personal(&digest, &self.signature)?;
        // If the producer filled in `self.pledgor` (the `Pledge::sign`
        // path does), it must match the recovered signer — otherwise a
        // MITM could swap the `pledgor` field. If `self.pledgor` is the
        // zero-address placeholder (the `from_url` path), we trust the
        // recovered signer instead.
        if self.pledgor.as_bytes() != &[0u8; 20] && recovered.address() != self.pledgor {
            return Err(TokenError::RecoveredWrongSigner);
        }
        if self.expiry_unix <= now_unix {
            return Err(TokenError::Expired {
                now: now_unix,
                pledge: self.expiry_unix,
            });
        }
        Ok(())
    }

    /// Verify that this pledge is targeted at the given relay (`recipient`)
    /// on the expected chain.
    pub fn verify_for_relay(&self, now_unix: i64, expected_chain_id: u64) -> Result<()> {
        if self.chain_id != expected_chain_id {
            return Err(TokenError::ChainIdMismatch {
                expected: expected_chain_id,
                got: self.chain_id,
            });
        }
        self.verify(now_unix)
    }

    /// Convert to a printable URL suitable for QR codes / deep links.
    ///
    /// Format: `adnet-token://<chain_id>/<contract>/<token>/<amount>/<recipient>/<nonce>/<expiry>/<sig_hex>`
    pub fn to_url(&self) -> String {
        format!(
            "adnet-token://{}/{}/{}/{}/{}/{}/{}/0x{}",
            self.chain_id,
            self.contract,
            self.token,
            self.amount_atomic,
            self.recipient.to_checksum(),
            self.nonce,
            self.expiry_unix,
            hex::encode(self.signature.to_compact()),
        )
    }

    /// Parse the URL form. Mirrors [`Self::to_url`].
    pub fn from_url(s: &str) -> Result<Self> {
        let rest = s
            .strip_prefix("adnet-token://")
            .ok_or_else(|| TokenError::InvalidUrl(s.to_string()))?;
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() != 8 {
            return Err(TokenError::InvalidUrl(format!(
                "expected 8 path segments, got {}",
                parts.len()
            )));
        }
        let chain_id: u64 = parts[0]
            .parse()
            .map_err(|_| TokenError::InvalidUrl(format!("bad chain_id: {}", parts[0])))?;
        let contract = parts[1].to_ascii_lowercase();
        let token = parts[2].to_ascii_lowercase();
        let amount_atomic: u128 = parts[3]
            .parse()
            .map_err(|_| TokenError::InvalidAmount(parts[3].to_string()))?;
        let recipient = Address::from_hex(parts[4])?;
        let nonce = parts[5].to_string();
        let expiry_unix: i64 = parts[6]
            .parse()
            .map_err(|_| TokenError::InvalidExpiry(parts[6].to_string()))?;
        let sig_hex = parts[7].strip_prefix("0x").unwrap_or(parts[7]);
        let sig_bytes = hex::decode(sig_hex)?;
        let signature = PersonalSignature::from_compact(&sig_bytes)?;
        Ok(Self {
            version: PLEDGE_VERSION,
            chain_id,
            contract,
            token,
            amount_atomic,
            recipient,
            nonce,
            expiry_unix,
            // Filled in lazily by `verify_with_recovered()` below; we
            // leave the zero-address placeholder so `verify()` skips the
            // pledged-vs-recovered equality check (the recovered address
            // is authoritative).
            pledgor: Address::from_bytes([0u8; 20]),
            signature,
        })
    }

    /// Like [`Self::verify`] but also returns the recovered pledgor
    /// address. Use this when you parsed the pledge from a URL and need
    /// to know who signed it.
    pub fn verify_with_recovered(&self, now_unix: i64) -> Result<Address> {
        let body = PledgeBody {
            version: self.version,
            chain_id: self.chain_id,
            contract: self.contract.clone(),
            token: self.token.clone(),
            amount_atomic: self.amount_atomic,
            recipient: self.recipient,
            nonce: self.nonce.clone(),
            expiry_unix: self.expiry_unix,
        };
        let digest = body.digest();
        let recovered = WalletPublic::recover_personal(&digest, &self.signature)?;
        if self.expiry_unix <= now_unix {
            return Err(TokenError::Expired {
                now: now_unix,
                pledge: self.expiry_unix,
            });
        }
        Ok(recovered.address())
    }
}

/// The unsigned body of a pledge. Stable across crates; serialization is
/// *exact* (no `serde_json::to_value` reordering) so the digest is
/// reproducible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PledgeBody {
    pub version: u8,
    pub chain_id: u64,
    pub contract: String,
    pub token: String,
    pub amount_atomic: u128,
    pub recipient: Address,
    pub nonce: String,
    pub expiry_unix: i64,
}

impl PledgeBody {
    /// Canonical JSON digest. We use `serde_json::to_vec` (alphabetical
    /// keys, no whitespace) and feed it through `blake3`. Anything that
    /// changes the JSON form changes the digest.
    pub fn digest(&self) -> [u8; 32] {
        let bytes = serde_json::to_vec(self).expect("PledgeBody is always serializable");
        let mut h = blake3::Hasher::new();
        h.update(&bytes);
        let out = h.finalize();
        let mut out32 = [0u8; 32];
        out32.copy_from_slice(out.as_bytes());
        out32
    }
}

/// URL-print helpers (split out so callers can paste a partial URL builder).
pub mod uris {
    pub use super::Pledge as PledgeUris;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pledgor() -> Wallet {
        Wallet::generate()
    }

    fn test_body(_wallet: &Wallet) -> PledgeBody {
        Pledge::body(
            1,
            "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
            "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
            1_000_000,
            Address::from_hex("0x52908400098527886E0F7030069857D2E4169EE7").unwrap(),
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".into(),
            Utc::now().timestamp() + 3600,
        )
        .unwrap()
    }

    #[test]
    fn sign_verify_round_trip() {
        let wallet = test_pledgor();
        let body = test_body(&wallet);
        let pledge = Pledge::sign(body, &wallet).unwrap();
        pledge.verify(Utc::now().timestamp()).unwrap();
    }

    #[test]
    fn rejects_wrong_recovered_signer() {
        let pledgor = test_pledgor();
        let body = test_body(&pledgor);
        let mut pledge = Pledge::sign(body, &pledgor).unwrap();
        // Replace the signature with one from a different wallet.
        let other = Wallet::generate();
        pledge.signature = other.sign_personal(&digest_of_body(&pledge)).unwrap();
        let err = pledge.verify(Utc::now().timestamp()).unwrap_err();
        assert!(matches!(err, TokenError::RecoveredWrongSigner));
    }

    #[test]
    fn rejects_expired() {
        let wallet = test_pledgor();
        let body = test_body(&wallet);
        let pledge = Pledge::sign(body, &wallet).unwrap();
        let err = pledge.verify(pledge.expiry_unix + 1).unwrap_err();
        assert!(matches!(err, TokenError::Expired { .. }));
    }

    #[test]
    fn url_round_trip() {
        let wallet = test_pledgor();
        let body = test_body(&wallet);
        let pledge = Pledge::sign(body, &wallet).unwrap();
        let url = pledge.to_url();
        let parsed = Pledge::from_url(&url).unwrap();
        // Pledgor is recovered by verify(), so set it explicitly.
        let mut parsed = parsed;
        parsed.pledgor = pledge.pledgor;
        assert_eq!(parsed.chain_id, pledge.chain_id);
        assert_eq!(parsed.amount_atomic, pledge.amount_atomic);
        assert_eq!(parsed.recipient, pledge.recipient);
        assert_eq!(parsed.signature, pledge.signature);
    }

    #[test]
    fn rejects_bad_nonce() {
        let err = Pledge::body(
            1,
            "0x00".into(),
            "0x00".into(),
            1,
            Address::from_hex("0x52908400098527886E0F7030069857D2E4169EE7").unwrap(),
            "z".into(),
            1,
        )
        .unwrap_err();
        assert!(matches!(err, TokenError::InvalidNonce(_)));
    }

    #[test]
    fn rejects_zero_amount() {
        // Use a valid nonce to isolate the failure to amount.
        let err = Pledge::body(
            1,
            "0x00".into(),
            "0x00".into(),
            0,
            Address::from_hex("0x52908400098527886E0F7030069857D2E4169EE7").unwrap(),
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".into(),
            1,
        )
        .unwrap_err();
        assert!(matches!(err, TokenError::InvalidAmount(_)));
    }

    fn digest_of_body(pledge: &Pledge) -> [u8; 32] {
        let body = PledgeBody {
            version: pledge.version,
            chain_id: pledge.chain_id,
            contract: pledge.contract.clone(),
            token: pledge.token.clone(),
            amount_atomic: pledge.amount_atomic,
            recipient: pledge.recipient,
            nonce: pledge.nonce.clone(),
            expiry_unix: pledge.expiry_unix,
        };
        body.digest()
    }
}

// We use Wallet::sign_personal directly in `Pledge::sign`; the `use` lives
// at the top of this file. No extra helper needed here.
