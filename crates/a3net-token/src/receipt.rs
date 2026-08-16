//! Receipt — a relay's acknowledgement that a [`Pledge`] was accepted and
//! how much service was rendered against it.
//!
//! A receipt is *not* a chain transaction. It is a signed record that the
//! relay will redeem the pledge for `charged_atomic` units of work. The
//! receipt is the artifact a user shows to the relay (or a settlement
//! service) to claim credit for past relay traffic.

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::pledge::Pledge;
use a3net_identity::{Address, PersonalSignature, Wallet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub version: u8,
    /// Mirror of the pledge's nonce. Lets the relay re-derive the same
    /// pledge identifier we used at accept time.
    pub pledge_nonce: String,
    pub pledgor: Address,
    pub recipient: Address,
    /// Total amount of the pledge that this receipt consumes.
    pub charged_atomic: u128,
    pub issued_unix: i64,
    pub relay: Address,
    pub signature: PersonalSignature,
}

/// Unsigned body of a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptBody {
    pub version: u8,
    pub pledge_nonce: String,
    pub pledgor: Address,
    pub recipient: Address,
    pub charged_atomic: u128,
    pub issued_unix: i64,
    pub relay: Address,
}

impl ReceiptBody {
    pub fn digest(&self) -> [u8; 32] {
        let bytes = serde_json::to_vec(self).expect("ReceiptBody is always serializable");
        let mut h = blake3::Hasher::new();
        h.update(&bytes);
        let out = h.finalize();
        let mut out32 = [0u8; 32];
        out32.copy_from_slice(out.as_bytes());
        out32
    }
}

impl Receipt {
    pub fn version() -> u8 {
        1
    }

    /// Issue a receipt consuming `charged_atomic` of the given pledge.
    /// `relay_wallet` is the relay's signing wallet.
    pub fn issue(pledge: &Pledge, charged_atomic: u128, relay_wallet: &Wallet) -> Result<Self> {
        if charged_atomic == 0 {
            return Err(crate::error::TokenError::InvalidAmount(
                "charged_atomic must be > 0".into(),
            ));
        }
        if charged_atomic > pledge.amount_atomic {
            return Err(crate::error::TokenError::InvalidAmount(format!(
                "charged_atomic ({charged_atomic}) exceeds pledge.amount_atomic ({})",
                pledge.amount_atomic
            )));
        }
        let body = ReceiptBody {
            version: Self::version(),
            pledge_nonce: pledge.nonce.clone(),
            pledgor: pledge.pledgor,
            recipient: relay_wallet.public().address(),
            charged_atomic,
            issued_unix: Utc::now().timestamp(),
            relay: relay_wallet.public().address(),
        };
        let digest = body.digest();
        let signature = relay_wallet.sign_personal(&digest)?;
        Ok(Self {
            version: body.version,
            pledge_nonce: body.pledge_nonce,
            pledgor: body.pledgor,
            recipient: body.recipient,
            charged_atomic: body.charged_atomic,
            issued_unix: body.issued_unix,
            relay: body.relay,
            signature,
        })
    }

    /// Verify the receipt's signature. Use [`Receipt::verify_for_pledge`]
    /// when you also have the original pledge available.
    pub fn verify(&self) -> Result<()> {
        let body = ReceiptBody {
            version: self.version,
            pledge_nonce: self.pledge_nonce.clone(),
            pledgor: self.pledgor,
            recipient: self.recipient,
            charged_atomic: self.charged_atomic,
            issued_unix: self.issued_unix,
            relay: self.relay,
        };
        let digest = body.digest();
        let recovered = a3net_identity::WalletPublic::recover_personal(&digest, &self.signature)?;
        if recovered.address() != self.relay {
            return Err(crate::error::TokenError::RecoveredWrongSigner);
        }
        Ok(())
    }

    /// Verify and also confirm that the receipt matches the given pledge.
    pub fn verify_for_pledge(&self, pledge: &Pledge) -> Result<()> {
        self.verify()?;
        if self.pledge_nonce != pledge.nonce {
            return Err(crate::error::TokenError::InvalidNonce(format!(
                "receipt nonce {} does not match pledge nonce {}",
                self.pledge_nonce, pledge.nonce
            )));
        }
        if self.pledgor != pledge.pledgor {
            return Err(crate::error::TokenError::RecoveredWrongSigner);
        }
        if self.recipient != pledge.recipient {
            return Err(crate::error::TokenError::RecoveredWrongSigner);
        }
        if self.charged_atomic > pledge.amount_atomic {
            return Err(crate::error::TokenError::InvalidAmount(
                "charged_atomic exceeds pledge.amount_atomic".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pledgor() -> Wallet {
        Wallet::generate()
    }

    fn test_relay() -> Wallet {
        Wallet::generate()
    }

    fn test_body(_wallet: &Wallet) -> crate::pledge::PledgeBody {
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
    fn issue_and_verify() {
        let pledgor = test_pledgor();
        let relay = test_relay();
        let body = test_body(&pledgor);
        let pledge = Pledge::sign(body, &pledgor).unwrap();
        let receipt = Receipt::issue(&pledge, 500_000, &relay).unwrap();
        receipt.verify().unwrap();
    }

    #[test]
    fn rejects_overcharge() {
        let pledgor = test_pledgor();
        let relay = test_relay();
        let body = test_body(&pledgor);
        let pledge = Pledge::sign(body, &pledgor).unwrap();
        let err = Receipt::issue(&pledge, 1_000_001, &relay).unwrap_err();
        assert!(matches!(err, crate::error::TokenError::InvalidAmount(_)));
    }

    #[test]
    fn verify_for_pledge_matches() {
        let pledgor = test_pledgor();
        let relay = test_relay();
        // Pledge must be addressed to the relay for correct cross-validation.
        let body = crate::pledge::Pledge::body(
            1,
            "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
            "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
            1_000_000,
            relay.public().address(),
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".into(),
            Utc::now().timestamp() + 3600,
        )
        .unwrap();
        let pledge = Pledge::sign(body, &pledgor).unwrap();
        let receipt = Receipt::issue(&pledge, 100_000, &relay).unwrap();
        receipt.verify_for_pledge(&pledge).unwrap();
    }
}
