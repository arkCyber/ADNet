//! Encrypted on-disk wallet keystore (Argon2id + AES-256-GCM).
//!
//! ## Threat model
//!
//! The keystore protects against an attacker who can read the file but
//! does not have the passphrase. It does **not** defend against:
//!
//! - an attacker who has the passphrase (nothing does);
//! - an attacker who can read live process memory;
//! - side-channels / Spectre-class attacks.
//!
//! ## Wire format
//!
//! The JSON file is intentionally simple — single record, no
//! metadata bloat. Schema version byte is the leading integer.
//!
//! ```text
//! {
//!   "v": 1,
//!   "kdf": { "name": "argon2id", "m_kib": 64, "t": 3, "p": 1,
//!            "salt": "0x..." },
//!   "cipher": { "name": "aes-256-gcm", "nonce": "0x..." },
//!   "ct": "0x..."   // sealed 32-byte secp256k1 secret
//! }
//! ```
//!
//! ## `Debug` / `Display`
//!
//! [`Keystore`] never prints the ciphertext, salt, or nonce. The
//! passphrase is the only secret not on disk; the user types it.

use std::fmt;

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{IdentityError, Result};
use crate::wallet::Wallet;

/// Current keystore schema version.
pub const KEYSTORE_VERSION: u8 = 1;

/// Argon2id parameters — chosen to take ~250ms on a modern laptop.
const ARGON2_MEM_KIB: u32 = 64 * 1024;
const ARGON2_TIME_COST: u32 = 3;
const ARGON2_PARALLELISM: u32 = 1;

/// JSON record persisted to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeystoreFile {
    /// Schema version. Bump on breaking changes.
    pub v: u8,
    pub kdf: KdfParams,
    pub cipher: CipherParams,
    /// AES-GCM ciphertext (sealed 32-byte secp256k1 secret).
    pub ct: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KdfParams {
    pub name: String,
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
    pub salt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CipherParams {
    pub name: String,
    pub nonce: String,
}

/// In-memory representation of an encrypted wallet on disk.
pub struct Keystore;

impl Keystore {
    /// Encrypt a wallet's 32-byte secret under `passphrase`.
    ///
    /// Returns the JSON-serializable [`KeystoreFile`] plus a fresh
    /// random salt + nonce — both must be persisted.
    pub fn encrypt(wallet: &Wallet, passphrase: &str) -> Result<KeystoreFile> {
        if passphrase.is_empty() {
            return Err(IdentityError::Keystore(
                "passphrase must not be empty".into(),
            ));
        }
        // Random 16-byte salt for Argon2id.
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        // Random 12-byte nonce for AES-GCM.
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);

        let key = derive_kek(passphrase.as_bytes(), &salt)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_ref()));
        let nonce = Nonce::from_slice(&nonce_bytes);
        let plaintext = Zeroizing::new(wallet.secret_bytes());
        let ct = cipher
            .encrypt(nonce, plaintext.as_ref())
            .map_err(|e| IdentityError::Keystore(format!("encrypt: {e}")))?;

        Ok(KeystoreFile {
            v: KEYSTORE_VERSION,
            kdf: KdfParams {
                name: "argon2id".into(),
                m_kib: ARGON2_MEM_KIB,
                t: ARGON2_TIME_COST,
                p: ARGON2_PARALLELISM,
                salt: hex::encode(salt),
            },
            cipher: CipherParams {
                name: "aes-256-gcm".into(),
                nonce: hex::encode(nonce_bytes),
            },
            ct: hex::encode(ct),
        })
    }

    /// Decrypt a [`KeystoreFile`] using `passphrase`. Returns the
    /// unlocked [`Wallet`].
    ///
    /// **Migration:** older schema versions are first run through
    /// [`Self::migrate_to_current`]. A v1 file today passes through
    /// unchanged; the hook exists so a v2 schema can land without
    /// forcing a coordinated migration of every existing keystore.
    pub fn decrypt(file: &KeystoreFile, passphrase: &str) -> Result<Wallet> {
        let migrated = Self::migrate_to_current(file.clone());
        if migrated.v != KEYSTORE_VERSION {
            return Err(IdentityError::UnsupportedKeystoreVersion(migrated.v));
        }
        if migrated.kdf.name != "argon2id" {
            return Err(IdentityError::InvalidKdf(format!(
                "unsupported kdf {:?}",
                migrated.kdf.name
            )));
        }
        if migrated.cipher.name != "aes-256-gcm" {
            return Err(IdentityError::Keystore(format!(
                "unsupported cipher {:?}",
                migrated.cipher.name
            )));
        }
        let salt = hex::decode(&migrated.kdf.salt)
            .map_err(|e| IdentityError::Keystore(format!("bad salt hex: {e}")))?;
        let nonce_bytes = hex::decode(&migrated.cipher.nonce)
            .map_err(|e| IdentityError::Keystore(format!("bad nonce hex: {e}")))?;
        let ct = hex::decode(&migrated.ct)
            .map_err(|e| IdentityError::Keystore(format!("bad ct hex: {e}")))?;
        if salt.len() != 16 {
            return Err(IdentityError::InvalidKdf(format!(
                "salt must be 16 bytes, got {}",
                salt.len()
            )));
        }
        if nonce_bytes.len() != 12 {
            return Err(IdentityError::InvalidKdf(format!(
                "nonce must be 12 bytes, got {}",
                nonce_bytes.len()
            )));
        }
        let params = Params::new(
            migrated.kdf.m_kib,
            migrated.kdf.t,
            migrated.kdf.p,
            Some(32),
        )
        .map_err(|e| IdentityError::InvalidKdf(e.to_string()))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut key = Zeroizing::new([0u8; 32]);
        argon
            .hash_password_into(passphrase.as_bytes(), &salt, key.as_mut())
            .map_err(|e| IdentityError::Keystore(format!("argon2: {e}")))?;

        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_ref()));
        let nonce = Nonce::from_slice(&nonce_bytes);
        let pt = cipher
            .decrypt(nonce, ct.as_ref())
            .map_err(|_| IdentityError::WrongPassphrase)?;
        if pt.len() != 32 {
            return Err(IdentityError::Keystore(format!(
                "decrypted secret wrong length {}",
                pt.len()
            )));
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&pt);
        Wallet::from_bytes(&bytes)
    }

    /// Upgrade an older-schema [`KeystoreFile`] to the current schema
    /// in memory. Today this is a no-op for v1 — but the hook is
    /// here so a future v2 (or v3, …) can land without a coordinated
    /// on-disk migration. New migrations are added as `match` arms.
    ///
    /// **Unknown versions are returned unchanged** so the caller's
    /// `decrypt` path can reject them with
    /// [`IdentityError::UnsupportedKeystoreVersion`].
    pub fn migrate_to_current(file: KeystoreFile) -> KeystoreFile {
        // No migrations needed yet — v1 is the only published
        // schema. When v2 ships, prepend a `match` that rewrites v1
        // records into the v2 shape and recurses:
        //
        //     if file.v == 1 {
        //         let v2 = rewrite_v1_to_v2(&file);
        //         return Self::migrate_to_current(v2);
        //     }
        file
    }

    /// Convenience: encrypt + serialize straight to JSON bytes.
    pub fn encrypt_to_bytes(wallet: &Wallet, passphrase: &str) -> Result<Vec<u8>> {
        let file = Self::encrypt(wallet, passphrase)?;
        serde_json::to_vec_pretty(&file)
            .map_err(|e| IdentityError::Keystore(format!("serialize: {e}")))
    }

    /// Convenience: parse JSON bytes + decrypt.
    pub fn decrypt_from_bytes(bytes: &[u8], passphrase: &str) -> Result<Wallet> {
        let file: KeystoreFile = serde_json::from_slice(bytes)
            .map_err(|e| IdentityError::Keystore(format!("parse: {e}")))?;
        Self::decrypt(&file, passphrase)
    }
}

impl fmt::Debug for Keystore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keystore").finish()
    }
}

/// Derive the 32-byte KEK from the user passphrase + Argon2id salt.
fn derive_kek(passphrase: &[u8], salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let params = Params::new(ARGON2_MEM_KIB, ARGON2_TIME_COST, ARGON2_PARALLELISM, Some(32))
        .map_err(|e| IdentityError::InvalidKdf(e.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(passphrase, salt, key.as_mut())
        .map_err(|e| IdentityError::Keystore(format!("argon2: {e}")))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_round_trip() {
        let w = Wallet::generate();
        let addr_before = w.public().address();
        let file = Keystore::encrypt(&w, "correct horse battery staple").unwrap();
        let recovered = Keystore::decrypt(&file, "correct horse battery staple").unwrap();
        assert_eq!(addr_before, recovered.public().address());
    }

    #[test]
    fn wrong_passphrase_fails() {
        let w = Wallet::generate();
        let file = Keystore::encrypt(&w, "right").unwrap();
        let err = Keystore::decrypt(&file, "wrong").unwrap_err();
        assert!(matches!(err, IdentityError::WrongPassphrase));
    }

    #[test]
    fn rejects_empty_passphrase() {
        let w = Wallet::generate();
        let err = Keystore::encrypt(&w, "").unwrap_err();
        assert!(matches!(err, IdentityError::Keystore(_)));
    }

    #[test]
    fn json_round_trip() {
        let w = Wallet::generate();
        let json = Keystore::encrypt_to_bytes(&w, "p@ss").unwrap();
        let back = Keystore::decrypt_from_bytes(&json, "p@ss").unwrap();
        assert_eq!(w.public().address(), back.public().address());
    }

    #[test]
    fn rejects_unknown_version() {
        let mut file = Keystore::encrypt(&Wallet::generate(), "p").unwrap();
        file.v = 99;
        let err = Keystore::decrypt(&file, "p").unwrap_err();
        assert!(matches!(err, IdentityError::UnsupportedKeystoreVersion(99)));
    }

    #[test]
    fn migrate_to_current_is_idempotent_for_v1() {
        // Today v1 is current, so `migrate_to_current` must be a
        // no-op. When v2 lands, this test will need to be split:
        // v1 files should round-trip through migrate; v99 should be
        // returned unchanged so the caller rejects it.
        let file = Keystore::encrypt(&Wallet::generate(), "p").unwrap();
        let migrated = Keystore::migrate_to_current(file.clone());
        assert_eq!(migrated.v, file.v);
        assert_eq!(migrated.ct, file.ct);
    }

    #[test]
    fn migrate_to_current_passes_through_unknown_version() {
        // Unknown versions are returned as-is so `decrypt` can
        // reject them with `UnsupportedKeystoreVersion`. A future
        // v2 migration arm will rewrite v1 → v2, but v99 should
        // never be silently re-labelled as v1.
        let mut file = Keystore::encrypt(&Wallet::generate(), "p").unwrap();
        file.v = 99;
        let migrated = Keystore::migrate_to_current(file.clone());
        assert_eq!(migrated.v, 99);
    }

    #[test]
    fn rejects_tampered_ct() {
        let w = Wallet::generate();
        let mut file = Keystore::encrypt(&w, "p").unwrap();
        // Flip a byte of the ciphertext.
        let mut ct = hex::decode(&file.ct).unwrap();
        ct[0] ^= 0x01;
        file.ct = hex::encode(ct);
        assert!(Keystore::decrypt(&file, "p").is_err());
    }
}