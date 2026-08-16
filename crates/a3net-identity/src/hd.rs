//! BIP-32 hierarchical-deterministic derivation for secp256k1 keys.
//!
//! The standard A3Net path is m/44'/60'/0'/0/0 — i.e. BIP-44, account 0,
//! external chain, index 0. The coin type 60 is Ethereum (the wallet
//! is EVM-compatible). We expose this as [`DEFAULT_EVM_PATH`] and a
//! convenience [`HdWallet::derive_default_evm`] so most callers don't
//! need to know the derivation grammar at all.
//!
//! ## Why `bip32` 0.5.x
//!
//! That crate returns a `k256`-backed `SigningKey` directly. We
//! extract the 32-byte scalar via `SigningKey::to_bytes()` and hand
//! it to [`crate::wallet::Wallet`], which already speaks `secp256k1`
//! (the same scalar — `k256` and `secp256k1` are both implementations
//! of the same curve).
//!
//! **Note:** `bip32` 0.5.x only supports 24-word mnemonics. Shorter
//! mnemonics (12 / 15 / 18 / 21) flow through `a3net-identity`'s own
//! [`crate::mnemonic`] (built on the `bip39` crate), and we convert
//! the seed by hand to the 64-byte `Seed` that `bip32` wants.

use std::fmt;

use zeroize::Zeroizing;

use crate::error::{IdentityError, Result};
use crate::wallet::Wallet;

/// A3Net's default EVM derivation path: m/44'/60'/0'/0/0.
///
/// This is the same path MetaMask uses for the first account — picking
/// it makes "import from MetaMask" trivial.
pub const DEFAULT_EVM_PATH: &str = "m/44'/60'/0'/0/0";

/// HD derivation handle. Holds the 64-byte BIP-39 seed in a
/// `Zeroizing`-wrapped buffer so the caller can clone the handle
/// safely and the seed is wiped on drop.
#[derive(Clone)]
pub struct HdWallet {
    seed: Zeroizing<[u8; 64]>,
}

impl HdWallet {
    /// Build from a 64-byte BIP-39 seed. Use
    /// [`crate::mnemonic::Mnemonic::to_seed`] to obtain one.
    pub fn from_seed(seed: &[u8; 64]) -> Result<Self> {
        Ok(Self { seed: Zeroizing::new(*seed) })
    }

    /// Derive the child wallet at the given BIP-32 path.
    pub fn derive(&self, path: &str) -> Result<Wallet> {
        let derived = bip32::XPrv::derive_from_path(self.seed.as_slice(), &path.parse().map_err(
            |e: bip32::Error| IdentityError::HdDerivation(format!("invalid path {path:?}: {e}")),
        )?)
        .map_err(|e| IdentityError::HdDerivation(e.to_string()))?;
        // `bip32::secp256k1::SigningKey::to_bytes()` returns the 32-byte
        // big-endian scalar.
        let sk_bytes = derived.private_key().to_bytes();
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&sk_bytes);
        Wallet::from_bytes(&arr)
    }

    /// Convenience: derive A3Net's default EVM path
    /// ([`DEFAULT_EVM_PATH`]).
    pub fn derive_default_evm(&self) -> Result<Wallet> {
        self.derive(DEFAULT_EVM_PATH)
    }

    /// Derive a non-hardened sequential index under the same BIP-44
    /// account — useful when a UI asks "what's account #1?".
    pub fn derive_account(&self, account: u32, index: u32) -> Result<Wallet> {
        let path = format!("m/44'/60'/{}'/0/{}", account, index);
        self.derive(&path)
    }

    /// Derive the *n*-th child under
    /// `m/44'/60'/0'/0/<index>` (i.e. another address in the default
    /// account). This is the cheapest derivation for "give me a new
    /// address" UIs.
    pub fn derive_address(&self, index: u32) -> Result<Wallet> {
        let path = format!("m/44'/60'/0'/0/{index}");
        self.derive(&path)
    }
}

impl fmt::Debug for HdWallet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HdWallet").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mnemonic::Mnemonic;

    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn derive_default_evm_returns_wallet() {
        let m = Mnemonic::from_phrase(TEST_MNEMONIC).unwrap();
        let seed = m.to_seed("");
        let hd = HdWallet::from_seed(&seed).unwrap();
        let w = hd.derive_default_evm().unwrap();
        let hex = w.public().address().to_hex();
        assert!(hex.starts_with("0x") && hex.len() == 42);
    }

    #[test]
    fn sequential_indexes_give_distinct_addresses() {
        let m = Mnemonic::from_phrase(TEST_MNEMONIC).unwrap();
        let seed = m.to_seed("");
        let hd = HdWallet::from_seed(&seed).unwrap();
        let a0 = hd.derive_address(0).unwrap();
        let a1 = hd.derive_address(1).unwrap();
        assert_ne!(a0.public().address(), a1.public().address());
    }

    #[test]
    fn passphrase_changes_addresses() {
        let m = Mnemonic::from_phrase(TEST_MNEMONIC).unwrap();
        let s1 = m.to_seed("");
        let s2 = m.to_seed("hunter2");
        let hd1 = HdWallet::from_seed(&s1).unwrap();
        let hd2 = HdWallet::from_seed(&s2).unwrap();
        let a = hd1.derive_default_evm().unwrap();
        let b = hd2.derive_default_evm().unwrap();
        assert_ne!(a.public().address(), b.public().address());
    }

    #[test]
    fn rejects_bad_path() {
        let m = Mnemonic::from_phrase(TEST_MNEMONIC).unwrap();
        let seed = m.to_seed("");
        let hd = HdWallet::from_seed(&seed).unwrap();
        assert!(hd.derive("not a path").is_err());
    }
}