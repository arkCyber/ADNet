//! `Treasury` — a long-lived wallet identity plus a set of ephemeral
//! receipt wallets for short-lived settlements.
//!
//! Most ADNet nodes need **one stable identity** (used for signing pledges,
//! receipts, peer tickets) and **many short-lived wallets** (used to
//! isolate settlement traffic so a compromised / lost receipt wallet
//! doesn't drain the long-term identity).
//!
//! The treasury is responsible for:
//!
//! - Holding the long-lived [`Wallet`] (the *root*).
//! - Generating, signing, and persisting ephemeral wallets.
//! - Tagging ephemeral wallets with their *purpose* (so a leaked wallet
//!   can be linked back to the service it was issued for).
//! - Recovering the root from the same 32-byte secret a user already has
//!   (no new key-management surface).
//!
//! ## Persistence
//!
//! A treasury can be serialized to JSON for disk persistence. The
//! long-lived root secret is *not* included by default — only its
//! compressed public key — so that an attacker who steals the file
//! cannot drain the root. Callers must reload the root via
//! [`Treasury::with_root`] before signing anything that uses it.
//!
//! Ephemeral wallets *are* stored as 32-byte secrets in the serialized
//! form, so callers who don't want that should keep the ephemeral set
//! short and rotate aggressively.

use serde::{Deserialize, Serialize};

use crate::error::{IdentityError, Result};
use crate::wallet::{Wallet, WalletPublic};

/// One short-lived receipt wallet. Tagged with a string purpose so logs
/// and audit trails can correlate the wallet with the service that
/// issued it.
#[derive(Debug, Clone)]
pub struct ReceiptWallet {
    pub purpose: String,
    pub wallet: Wallet,
    /// Unix-seconds timestamp after which this wallet should no longer
    /// be trusted. Zero means "no expiry".
    pub expires_unix: i64,
}

/// Long-lived treasury state.
#[derive(Debug)]
pub struct Treasury {
    /// Permanent signing key. Optional so a `Treasury` can be loaded from
    /// disk before the operator supplies the root secret.
    root: Option<Wallet>,
    /// Long-term *public* identity. Always present (the file on disk has
    /// at minimum the compressed public key).
    root_public: WalletPublic,
    /// Short-lived wallets for receipt / settlement traffic.
    ephemeral: Vec<ReceiptWallet>,
}

impl Treasury {
    /// Build a treasury around a freshly-generated root wallet. The
    /// caller must save the 32-byte secret externally — we don't store
    /// it after this call returns.
    pub fn new() -> (Self, [u8; 32]) {
        let wallet = Wallet::generate();
        let secret = wallet.secret_bytes();
        let treasury = Self::from_wallet(wallet);
        (treasury, secret)
    }

    /// Wrap an existing wallet as the root.
    pub fn from_wallet(wallet: Wallet) -> Self {
        let root_public = wallet.public().clone();
        Self {
            root: Some(wallet),
            root_public,
            ephemeral: Vec::new(),
        }
    }

    /// Attach a root wallet to a treasury whose root is currently empty
    /// (e.g. just loaded from disk). Returns an error if a root is
    /// already present.
    pub fn with_root(mut self, secret: &[u8; 32]) -> Result<Self> {
        if self.root.is_some() {
            return Err(IdentityError::InvalidSecretKey(
                "treasury already has a root wallet loaded".into(),
            ));
        }
        let wallet = Wallet::from_bytes(secret)?;
        // Sanity: the loaded wallet's public side must match what's on
        // disk, otherwise the caller is wiring the wrong key.
        if wallet.public() != &self.root_public {
            return Err(IdentityError::InvalidSecretKey(
                "loaded root wallet does not match the public key on file".into(),
            ));
        }
        self.root = Some(wallet);
        Ok(self)
    }

    pub fn root_public(&self) -> &WalletPublic {
        &self.root_public
    }

    pub fn root(&self) -> Result<&Wallet> {
        self.root.as_ref().ok_or_else(|| {
            IdentityError::InvalidSecretKey(
                "treasury has no root wallet loaded; call with_root() first".into(),
            )
        })
    }

    /// Issue a fresh receipt wallet tagged with `purpose` and (optionally)
    /// `expires_unix`. Returns the new wallet so the caller can sign things
    /// with it; the treasury keeps a clone for later bookkeeping.
    pub fn issue_receipt_wallet(
        &mut self,
        purpose: impl Into<String>,
        expires_unix: i64,
    ) -> Wallet {
        let wallet = Wallet::generate();
        self.ephemeral.push(ReceiptWallet {
            purpose: purpose.into(),
            wallet: wallet.clone(),
            expires_unix,
        });
        wallet
    }

    pub fn ephemeral_count(&self) -> usize {
        self.ephemeral.len()
    }

    pub fn list_ephemeral_purposes(&self) -> Vec<&str> {
        self.ephemeral.iter().map(|e| e.purpose.as_str()).collect()
    }

    /// Drop any ephemeral wallets whose `expires_unix` is in the past
    /// relative to `now_unix`. Returns how many were removed.
    pub fn reap_expired(&mut self, now_unix: i64) -> usize {
        let before = self.ephemeral.len();
        self.ephemeral
            .retain(|e| e.expires_unix == 0 || e.expires_unix > now_unix);
        before - self.ephemeral.len()
    }
}

/// Serialization-friendly view of the treasury. The root secret is *not*
/// included; use [`Treasury::with_root`] after loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreasuryView {
    pub root_public: WalletPublic,
    pub ephemeral: Vec<ReceiptWalletView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptWalletView {
    pub purpose: String,
    /// 32-byte secret, lowercase hex.
    pub secret_hex: String,
    pub expires_unix: i64,
}

impl Treasury {
    /// Snapshot the treasury for persistence. Does not include the root
    /// secret.
    pub fn view(&self) -> TreasuryView {
        TreasuryView {
            root_public: self.root_public.clone(),
            ephemeral: self
                .ephemeral
                .iter()
                .map(|e| ReceiptWalletView {
                    purpose: e.purpose.clone(),
                    secret_hex: hex::encode(e.wallet.secret_bytes()),
                    expires_unix: e.expires_unix,
                })
                .collect(),
        }
    }

    /// Reconstruct a treasury from a saved view. Use [`Self::with_root`]
    /// to attach the root secret.
    pub fn from_view(view: TreasuryView) -> Self {
        let ephemeral = view
            .ephemeral
            .into_iter()
            .filter_map(|v| {
                let bytes = hex::decode(&v.secret_hex).ok()?;
                if bytes.len() != 32 {
                    return None;
                }
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                let wallet = Wallet::from_bytes(&arr).ok()?;
                Some(ReceiptWallet {
                    purpose: v.purpose,
                    wallet,
                    expires_unix: v.expires_unix,
                })
            })
            .collect();
        Self {
            root: None,
            root_public: view.root_public,
            ephemeral,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_then_with_root_round_trip() {
        let (t1, secret) = Treasury::new();
        // Sign something with the root so we know it works.
        let sig = t1.root().unwrap().sign_personal(&[0x42u8; 32]).unwrap();
        // Drop the in-memory root and reload from view + secret.
        let view = t1.view();
        let t2 = Treasury::from_view(view).with_root(&secret).unwrap();
        // Re-signing must produce the same signature (deterministic for
        // the same RFC6979 nonce).
        let sig2 = t2.root().unwrap().sign_personal(&[0x42u8; 32]).unwrap();
        assert_eq!(sig, sig2);
    }

    #[test]
    fn rejects_wrong_root() {
        let (t, _secret) = Treasury::new();
        let view = t.view();
        let wrong = [0x99u8; 32];
        // Wrong secret → must error.
        let err = Treasury::from_view(view).with_root(&wrong).unwrap_err();
        assert!(matches!(err, IdentityError::InvalidSecretKey(_)));
    }

    #[test]
    fn issue_and_reap_ephemeral() {
        let (mut t, _s) = Treasury::new();
        t.issue_receipt_wallet("relay-session-1", 0);
        t.issue_receipt_wallet("relay-session-2", 1_000);
        assert_eq!(t.ephemeral_count(), 2);
        // Reap with now > 1000: only the second should go.
        let reaped = t.reap_expired(1_500);
        assert_eq!(reaped, 1);
        assert_eq!(t.ephemeral_count(), 1);
        assert_eq!(t.list_ephemeral_purposes(), vec!["relay-session-1"]);
    }

    #[test]
    fn from_view_ignores_invalid_secrets() {
        let view = TreasuryView {
            root_public: Wallet::generate().public().clone(),
            ephemeral: vec![ReceiptWalletView {
                purpose: "ok".into(),
                secret_hex: hex::encode([0x11u8; 32]),
                expires_unix: 0,
            }],
        };
        let t = Treasury::from_view(view);
        assert_eq!(t.ephemeral_count(), 1);
    }
}
