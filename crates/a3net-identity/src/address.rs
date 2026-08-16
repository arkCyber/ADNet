//! EVM-style 20-byte address.
//!
//! An `Address` is `keccak256(uncompressed_pubkey[1..])[12..32]`, exactly the
//! derivation used by Ethereum (`eth_accounts`). We pin this format so that
//! any external EVM wallet / block explorer can verify an A3Net wallet's
//! address.

use serde::{Deserialize, Serialize};

use crate::error::{IdentityError, Result};

/// Length of an EVM address in bytes.
pub const EVM_ADDRESS_LEN: usize = 20;

/// 20-byte EVM-style address, displayed as `0x` + 40 hex chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Address([u8; EVM_ADDRESS_LEN]);

impl Address {
    /// Construct from a raw 20-byte array.
    pub const fn from_bytes(bytes: [u8; EVM_ADDRESS_LEN]) -> Self {
        Self(bytes)
    }

    /// Construct from a 20-byte slice. Returns an error if the slice is the
    /// wrong length.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != EVM_ADDRESS_LEN {
            return Err(IdentityError::InvalidAddressLength(bytes.len()));
        }
        let mut out = [0u8; EVM_ADDRESS_LEN];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    /// Borrow the raw bytes.
    pub fn as_bytes(&self) -> &[u8; EVM_ADDRESS_LEN] {
        &self.0
    }

    /// `0x` + 40 hex characters — never `0x`-prefixed twice.
    pub fn to_hex(&self) -> String {
        format!("0x{}", hex::encode(self.0))
    }

    /// Parse a `0x…` or bare-hex string. Matching is case-insensitive.
    pub fn from_hex(s: &str) -> Result<Self> {
        // Accept both `0x` and `0X` prefixes — case is purely
        // cosmetic at the wire boundary, and case-insensitive
        // matching is what ethers.js / viem do.
        let trimmed = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);
        let bytes = hex::decode(trimmed)?;
        Self::from_slice(&bytes)
    }

    /// EIP-55-style checksummed address (mixed case by hash nibble).
    ///
    /// This is the canonical display form for EVM addresses and avoids
    /// confusion between `0x…abc…` and `0x…ABC…`. We use the same hash
    /// algorithm as EIP-55 so the output is interoperable with MetaMask,
    /// Etherscan, etc.
    pub fn to_checksum(&self) -> String {
        let hex_lower = hex::encode(self.0);
        let hash = tiny_keccak_keccak256(hex_lower.as_bytes());
        let mut out = String::with_capacity(2 + EVM_ADDRESS_LEN * 2);
        out.push_str("0x");
        for (i, ch) in hex_lower.chars().enumerate() {
            let nibble = (hash[i / 2] >> (if i % 2 == 0 { 4 } else { 0 })) & 0x0f;
            // EIP-55: only uppercase letters, and only when the nibble >= 8.
            if ch.is_ascii_alphabetic() && nibble >= 8 {
                out.push(ch.to_ascii_uppercase());
            } else {
                out.push(ch);
            }
        }
        out
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_checksum())
    }
}

impl AsRef<[u8]> for Address {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<[u8; EVM_ADDRESS_LEN]> for Address {
    fn from(b: [u8; EVM_ADDRESS_LEN]) -> Self {
        Self(b)
    }
}

impl From<Address> for String {
    fn from(a: Address) -> Self {
        a.to_checksum()
    }
}

impl TryFrom<String> for Address {
    type Error = IdentityError;
    fn try_from(s: String) -> Result<Self> {
        Self::from_hex(&s)
    }
}

impl TryFrom<&str> for Address {
    type Error = IdentityError;
    fn try_from(s: &str) -> Result<Self> {
        Self::from_hex(s)
    }
}

/// Local keccak256 helper. Tiny-Keccak's `Hasher` API is the most ergonomic
/// way to express this without dragging in `sha3`'s Digest trait.
fn tiny_keccak_keccak256(data: &[u8]) -> [u8; 32] {
    use tiny_keccak::Hasher;
    let mut k = tiny_keccak::Keccak::v256();
    k.update(data);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical EIP-55 fixtures. These are the *known-good* addresses from
    /// the EIP-55 spec doc — we test that `to_checksum` reproduces them.
    /// The bytes are exactly the lowercase hex the fixture addresses start
    /// with, so we don't have to drag in a public-key for each one.
    #[test]
    fn eip55_fixtures_match() {
        // Fixture: 0x52908400098527886E0F7030069857D2E4169EE7
        let a = Address::from_hex("52908400098527886E0F7030069857D2E4169EE7").unwrap();
        assert_eq!(
            a.to_checksum(),
            "0x52908400098527886E0F7030069857D2E4169EE7"
        );

        // Fixture: 0x8617E340B3D01FA5F11F306F4090FD50E238070D
        let b = Address::from_hex("8617E340B3D01FA5F11F306F4090FD50E238070D").unwrap();
        assert_eq!(
            b.to_checksum(),
            "0x8617E340B3D01FA5F11F306F4090FD50E238070D"
        );

        // Fixture: 0xde709f2102306220921060314715629080e2fb77 (all lowercase)
        let c = Address::from_hex("de709f2102306220921060314715629080e2fb77").unwrap();
        assert_eq!(
            c.to_checksum(),
            "0xde709f2102306220921060314715629080e2fb77"
        );

        // Fixture: 0x27b1fdb04752bbc536007a920d24acb045561c26 (all uppercase)
        let d = Address::from_hex("27B1FDB04752BBC536007A920D24ACB045561C26").unwrap();
        assert_eq!(
            d.to_checksum(),
            "0x27b1fdb04752bbc536007a920d24acb045561c26"
        );
    }

    #[test]
    fn round_trip_hex() {
        let bytes = [0x42u8; 20];
        let a = Address::from_bytes(bytes);
        let s = a.to_hex();
        let b = Address::from_hex(&s).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn to_checksum_is_input_case_insensitive() {
        // The canonical EIP-55 checksum form must not depend on the
        // case of the input — feeding the same address in upper,
        // lower, or mixed case must yield the same checksummed
        // string. This guards against an implementation that
        // accidentally re-derives the hash from the *input* string
        // instead of the lowercase hex.
        let bytes: [u8; 20] = [
            0x5a, 0xae, 0xb6, 0x05, 0x3f, 0x3e, 0x94, 0xc9, 0xb9, 0xa0,
            0x9f, 0x33, 0x66, 0x94, 0x35, 0xe7, 0xef, 0x1b, 0xea, 0xed,
        ];
        let a = Address::from_bytes(bytes);
        let canonical = a.to_checksum();
        for variant in [
            canonical.to_lowercase(),
            canonical.to_uppercase(),
            // Deliberately alternate the case of each hex digit so
            // we exercise the mixed-case input path. The leading
            // `0x` stays put because `Address::from_hex` strips it.
            mix_hex_case(&canonical),
        ] {
            let parsed = Address::from_hex(&variant).unwrap();
            assert_eq!(parsed, a);
            assert_eq!(parsed.to_checksum(), canonical);
        }
    }

    /// Alternate upper/lower case on each hex digit, leaving the
    /// `0x` prefix alone.
    fn mix_hex_case(s: &str) -> String {
        let rest = s.strip_prefix("0x").unwrap_or(s);
        let mut out = String::with_capacity(s.len());
        out.push_str("0x");
        for (i, c) in rest.chars().enumerate() {
            if i % 2 == 0 {
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c.to_ascii_uppercase());
            }
        }
        out
    }

    #[test]
    fn rejects_wrong_length() {
        let err = Address::from_slice(&[0u8; 19]).unwrap_err();
        assert!(matches!(err, IdentityError::InvalidAddressLength(19)));
    }
}
