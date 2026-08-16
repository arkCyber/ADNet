//! Key-derivation helpers (Argon2id only for now).
//!
//! Kept in a tiny module so callers (and tests) don't have to pull
//! `argon2` directly. The defaults match OWASP's 2024 Password
//! Storage Cheat Sheet for interactive logins:
//!
//! | Parameter | Value |
//! |-----------|-------|
//! | Algorithm | Argon2id |
//! | Memory    | 19 MiB (`m_cost = 19 * 1024`) |
//! | Time      | 2 iterations |
//! | Parallel  | 1 lane |
//! | Salt      | caller-supplied, ≥ 8 bytes |
//! | Output    | 32 bytes |
//!
//! These are also the values persisted in [`crate::store::KeyFileKdf`]
//! so a future boot can re-derive the same key from the same
//! passphrase without out-of-band parameters.

use argon2::{Algorithm, Argon2, Params, Version};

use crate::error::{CryptoError, CryptoResult};

/// Recommended Argon2id parameters (OWASP 2024).
pub const ARGON2_MEM_COST_KIB: u32 = 19 * 1024;
/// Recommended Argon2id time cost.
pub const ARGON2_T_COST: u32 = 2;
/// Recommended Argon2id parallelism.
pub const ARGON2_P_COST: u32 = 1;

/// Derive a 32-byte key from a passphrase + salt using Argon2id
/// with the OWASP-recommended parameters.
///
/// Returns [`CryptoError::InvalidSalt`] if `salt` is shorter than
/// 8 bytes (the Argon2 minimum).
pub fn derive_argon2id(
    passphrase: &[u8],
    salt: &[u8],
) -> CryptoResult<[u8; 32]> {
    if salt.len() < 8 {
        return Err(CryptoError::InvalidSalt);
    }
    let params = Params::new(ARGON2_MEM_COST_KIB, ARGON2_T_COST, ARGON2_P_COST, Some(32))
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    argon
        .hash_password_into(passphrase, salt, &mut out)
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2_derivation_is_deterministic() {
        let salt = b"some-fixed-salt-1234567890";
        let k1 = derive_argon2id(b"hunter2", salt).unwrap();
        let k2 = derive_argon2id(b"hunter2", salt).unwrap();
        assert_eq!(k1, k2);
    }

    #[test]
    fn argon2_derivation_changes_with_salt() {
        let k1 = derive_argon2id(b"hunter2", b"salt-aaaaaaaa-aaa").unwrap();
        let k2 = derive_argon2id(b"hunter2", b"salt-bbbbbbbb-bbb").unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn argon2_derivation_changes_with_passphrase() {
        let salt = b"salt-aaaaaaaaaaaaaa";
        let k1 = derive_argon2id(b"hunter2", salt).unwrap();
        let k2 = derive_argon2id(b"correct horse", salt).unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn argon2_rejects_short_salt() {
        let err = derive_argon2id(b"pw", b"short").unwrap_err();
        assert!(matches!(err, CryptoError::InvalidSalt), "got {:?}", err);
    }
}
