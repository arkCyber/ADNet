//! Claim — a user-side aggregation of receipts to present to the settlement
//! service.
//!
//! A `Claim` is the bundle of [`Receipt`]s the user pushes to the
//! settlement layer (e.g. a relayer, a chain transaction, or a plain CSV
//! for manual reconciliation). It does not change the receipt contents; it
//! just groups them under a single signed envelope so the settlement
//! service can verify it received everything at once.

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::error::{Result, TokenError};
use crate::receipt::Receipt;
use a3net_identity::{Address, PersonalSignature, Wallet};

/// Default cap on receipts per claim. Bigger claims are split; smaller
/// accepted as-is.
pub const MAX_RECEIPTS_PER_CLAIM: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub version: u8,
    pub claimer: Address,
    pub issued_unix: i64,
    pub receipts: Vec<Receipt>,
    pub signature: PersonalSignature,
}

impl Claim {
    pub fn version() -> u8 {
        1
    }

    /// Build a claim from a set of receipts. The claimer (the user) signs
    /// the bundle so the settlement service can authenticate the bundle
    /// itself; the per-receipt signatures are checked downstream.
    pub fn build(claim_wallet: &Wallet, receipts: Vec<Receipt>) -> Result<Self> {
        if receipts.is_empty() {
            return Err(TokenError::InvalidAmount(
                "claim must include at least one receipt".into(),
            ));
        }
        if receipts.len() > MAX_RECEIPTS_PER_CLAIM {
            return Err(TokenError::InvalidAmount(format!(
                "claim has {} receipts, max is {MAX_RECEIPTS_PER_CLAIM}",
                receipts.len()
            )));
        }
        let body = serde_json::json!({
            "version": Self::version(),
            "claimer": claim_wallet.public().address().to_checksum(),
            "issued_unix": Utc::now().timestamp(),
            "receipts": receipts,
        });
        let bytes = serde_json::to_vec(&body).expect("claim body is serializable");
        let mut h = blake3::Hasher::new();
        h.update(&bytes);
        let out = h.finalize();
        let mut digest = [0u8; 32];
        digest.copy_from_slice(out.as_bytes());

        let signature = claim_wallet.sign_personal(&digest)?;
        Ok(Self {
            version: Self::version(),
            claimer: claim_wallet.public().address(),
            issued_unix: Utc::now().timestamp(),
            receipts,
            signature,
        })
    }

    /// Verify the claim envelope signature. Each receipt inside is verified
    /// separately by the settlement service.
    pub fn verify(&self) -> Result<()> {
        let body = serde_json::json!({
            "version": self.version,
            "claimer": self.claimer.to_checksum(),
            "issued_unix": self.issued_unix,
            "receipts": self.receipts,
        });
        let bytes = serde_json::to_vec(&body).expect("claim body is serializable");
        let mut h = blake3::Hasher::new();
        h.update(&bytes);
        let out = h.finalize();
        let mut digest = [0u8; 32];
        digest.copy_from_slice(out.as_bytes());

        let recovered = a3net_identity::WalletPublic::recover_personal(&digest, &self.signature)?;
        if recovered.address() != self.claimer {
            return Err(TokenError::RecoveredWrongSigner);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pledge::Pledge;
    use a3net_identity::Address;
    use chrono::Utc;

    fn make_pledge(pledgor: &Wallet) -> Pledge {
        let body = Pledge::body(
            1,
            "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
            "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
            1_000_000,
            Address::from_hex("0x52908400098527886E0F7030069857D2E4169EE7").unwrap(),
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".into(),
            Utc::now().timestamp() + 3600,
        )
        .unwrap();
        Pledge::sign(body, pledgor).unwrap()
    }

    #[test]
    fn build_and_verify() {
        let pledgor = Wallet::generate();
        let relay = Wallet::generate();
        let pledge = make_pledge(&pledgor);
        let r1 = Receipt::issue(&pledge, 100_000, &relay).unwrap();
        let r2 = Receipt::issue(&pledge, 250_000, &relay).unwrap();
        let claim = Claim::build(&pledgor, vec![r1, r2]).unwrap();
        claim.verify().unwrap();
    }

    #[test]
    fn rejects_empty() {
        let pledgor = Wallet::generate();
        let err = Claim::build(&pledgor, vec![]).unwrap_err();
        assert!(matches!(err, TokenError::InvalidAmount(_)));
    }
}
