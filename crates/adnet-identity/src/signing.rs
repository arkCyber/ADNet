//! EIP-191 `personal_sign` signature wrapper.
//!
//! We standardize on the 65-byte `r || s || v` form so that any EVM verifier
//! (`eth_call ecrecover`, ethers.js, viem) can recover the same address. The
//! `v` byte is constrained to `27` or `28` per EIP-191; we force [`PERSONAL_SIGN_VERSION`]
//! = `27` to keep the wire format unambiguous.

use serde::{Deserialize, Serialize};

/// The EIP-191 prefix used for `personal_sign`:
/// `"\x19Ethereum Signed Message:\n"`.
pub const EIP191_PREFIX: &[u8] = b"\x19Ethereum Signed Message:\n";

/// The fixed `v` value we emit (EIP-191 specifies `27` or `28`; we never
/// emit `28` so consumers can treat `v` as a deliberately chosen byte).
pub const PERSONAL_SIGN_VERSION: u8 = 27;

/// A 65-byte EIP-191 signature with `v` pinned to 27.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalSignature {
    pub r: [u8; 32],
    pub s: [u8; 32],
    pub v: u8,
}

impl PersonalSignature {
    /// 65-byte compact form (`r || s || v`). Compatible with `ecrecover`.
    pub fn to_compact(&self) -> [u8; 65] {
        let mut out = [0u8; 65];
        out[..32].copy_from_slice(&self.r);
        out[32..64].copy_from_slice(&self.s);
        out[64] = self.v;
        out
    }

    /// Parse from a 65-byte slice. Returns [`crate::error::IdentityError::InvalidSignature`]
    /// if the slice is wrong or `v` is not 27 or 28.
    pub fn from_compact(bytes: &[u8]) -> crate::error::Result<Self> {
        if bytes.len() != 65 {
            return Err(crate::error::IdentityError::InvalidSignature(format!(
                "expected 65 bytes, got {}",
                bytes.len()
            )));
        }
        let mut r = [0u8; 32];
        let mut s = [0u8; 32];
        r.copy_from_slice(&bytes[..32]);
        s.copy_from_slice(&bytes[32..64]);
        let v = bytes[64];
        if v != 27 && v != 28 {
            return Err(crate::error::IdentityError::InvalidSignature(format!(
                "v must be 27 or 28, got {}",
                v
            )));
        }
        Ok(Self { r, s, v })
    }

    /// `0x` + 130 hex chars (`r` + `s`).
    pub fn to_hex(&self) -> String {
        format!("0x{}", hex::encode(self.to_compact()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_round_trip() {
        let sig = PersonalSignature {
            r: [0x11; 32],
            s: [0x22; 32],
            v: 27,
        };
        let bytes = sig.to_compact();
        let back = PersonalSignature::from_compact(&bytes).unwrap();
        assert_eq!(sig, back);
    }

    #[test]
    fn rejects_bad_v() {
        let mut bytes = [0u8; 65];
        bytes[64] = 99;
        assert!(PersonalSignature::from_compact(&bytes).is_err());
    }
}
