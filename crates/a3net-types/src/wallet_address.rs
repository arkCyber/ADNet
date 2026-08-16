//! EVM-style wallet address — a 20-byte identifier compatible with any
//! EVM chain (Ethereum, Polygon, Base, Arbitrum, …).
//!
//! We carry this in `a3net-types` as a pure 20-byte value so the protocol
//! layers (`gossip`, `mesh`, `transport`, …) can attach signature metadata
//! to peer tickets without pulling in any crypto dependencies. **We do not
//! re-export `a3net_identity::Address`** — that would force every consumer
//! of `a3net-types` to compile `secp256k1`, `sha3`, `aes-gcm`, and
//! `x25519-dalek`, which contradicts `a3net-types`' "no crypto deps"
//! invariant.
//!
//! Conversion to `a3net_identity::Address` (and back) is provided by the
//! `a3net-identity` crate. The two types are intentionally distinct so
//! the layering stays clean.

use serde::{Deserialize, Serialize};

use crate::error::{AdnetError, Result};

/// Length of a wallet address in bytes.
pub const WALLET_ADDRESS_LEN: usize = 20;

/// A 20-byte EVM-style wallet address. Display: lowercase hex with a `0x`
/// prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WalletAddress([u8; WALLET_ADDRESS_LEN]);

impl WalletAddress {
    /// Construct from a 20-byte array.
    pub const fn from_bytes(bytes: [u8; WALLET_ADDRESS_LEN]) -> Self {
        Self(bytes)
    }

    /// Construct from a 20-byte slice. Returns [`AdnetError::Validation`]
    /// if the slice is the wrong length.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != WALLET_ADDRESS_LEN {
            return Err(AdnetError::Validation(format!(
                "wallet_address: expected {} bytes, got {}",
                WALLET_ADDRESS_LEN,
                bytes.len()
            )));
        }
        let mut out = [0u8; WALLET_ADDRESS_LEN];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    /// Borrow the raw bytes.
    pub fn as_bytes(&self) -> &[u8; WALLET_ADDRESS_LEN] {
        &self.0
    }

    /// Lowercase hex with `0x` prefix.
    pub fn to_hex(&self) -> String {
        format!("0x{}", hex::encode(self.0))
    }

    /// Parse a `0x…` or bare-hex string. Matching is case-insensitive.
    pub fn from_hex(s: &str) -> Result<Self> {
        let trimmed = s.strip_prefix("0x").unwrap_or(s);
        let bytes = hex::decode(trimmed)
            .map_err(|e| AdnetError::Validation(format!("wallet_address hex: {e}")))?;
        Self::from_slice(&bytes)
    }
}

impl std::fmt::Display for WalletAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl AsRef<[u8]> for WalletAddress {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<[u8; WALLET_ADDRESS_LEN]> for WalletAddress {
    fn from(b: [u8; WALLET_ADDRESS_LEN]) -> Self {
        Self(b)
    }
}

impl From<WalletAddress> for String {
    fn from(a: WalletAddress) -> Self {
        a.to_hex()
    }
}

impl TryFrom<String> for WalletAddress {
    type Error = AdnetError;
    fn try_from(s: String) -> Result<Self> {
        Self::from_hex(&s)
    }
}

impl TryFrom<&str> for WalletAddress {
    type Error = AdnetError;
    fn try_from(s: &str) -> Result<Self> {
        Self::from_hex(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_hex() {
        let bytes = [0x42u8; 20];
        let a = WalletAddress::from_bytes(bytes);
        let s = a.to_hex();
        let b = WalletAddress::from_hex(&s).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn parses_with_and_without_prefix() {
        let bytes = [0xab; 20];
        let s = hex::encode(bytes);
        let a = WalletAddress::from_hex(&s).unwrap();
        let b = WalletAddress::from_hex(&format!("0x{s}")).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.as_bytes(), &bytes);
    }

    #[test]
    fn rejects_wrong_length() {
        let err = WalletAddress::from_slice(&[0u8; 19]).unwrap_err();
        assert!(err.to_string().contains("expected 20 bytes"));
    }

    #[test]
    fn serde_round_trip() {
        let a = WalletAddress::from_bytes([0x11; 20]);
        let json = serde_json::to_string(&a).unwrap();
        let b: WalletAddress = serde_json::from_str(&json).unwrap();
        assert_eq!(a, b);
    }
}
