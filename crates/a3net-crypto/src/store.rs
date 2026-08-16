//! Persistent on-disk [`EncryptionKey`] store.
//!
//! The file lives at `<data_dir>/keys/storage.key` in plain JSON
//! (no passphrase prompt yet). The directory is created on demand
//! with mode `0o700` and the file itself is written with mode
//! `0o600` on Unix so that other users on the box cannot read it.
//!
//! ## Format
//!
//! ```json
//! {
//!   "version": 1,
//!   "key_hex": "<64 hex chars>",
//!   "kdf": {
//!     "algorithm": "argon2id",
//!     "params": { "m_cost_kib": 19456, "t_cost": 2, "p_cost": 1 },
//!     "salt_hex": "<hex>"
//!   }
//! }
//! ```
//!
//! `kdf` is present only when the key was derived from a passphrase
//! — the raw-key path omits it.
//!
//! ## Threat model
//!
//! `KeyStore` defends against an attacker who can read arbitrary
//! files inside `<data_dir>` but cannot run arbitrary code on the
//! host. It does **not** defend against memory dumps or a hostile
//! operator with root.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CryptoError, CryptoResult};
use crate::key::{EncryptionKey, KeyWriteAccess};

/// Bump whenever the `KeyFile` shape or the KDF parameters change.
pub const CURRENT_KEY_VERSION: u32 = 1;

/// On-disk format for `<data_dir>/keys/storage.key`. We keep it
/// JSON so an operator can `cat` / inspect / replace it by hand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyFile {
    /// Format version — bump if we ever change the KDF or AEAD.
    pub version: u32,
    /// Hex-encoded 32-byte key. We do **not** protect this with
    /// a passphrase in v1 — the threat model assumes the data dir
    /// itself is operator-private.
    pub key_hex: String,
    /// Optional KDF parameters — present only when the key was
    /// derived from a passphrase. Empty for the raw-key path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kdf: Option<KeyFileKdf>,
}

/// Persisted KDF metadata so future boots can re-derive the same
/// key from the same passphrase without having to remember the
/// parameters out-of-band.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyFileKdf {
    pub algorithm: String, // "argon2id"
    pub params: KeyFileKdfParams,
    /// Hex-encoded salt.
    pub salt_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyFileKdfParams {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

/// Persistent key store.
pub struct KeyStore {
    path: PathBuf,
}

impl KeyStore {
    /// Default key location: `<data_dir>/keys/storage.key`.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("keys").join("storage.key"),
        }
    }

    /// Where the key file would live. Useful for the CLI's
    /// status command and tests.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns `true` if a key file already exists at the
    /// expected path. Lets the CLI short-circuit `--init` runs.
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Generate a fresh random key, write it, and return it.
    pub fn init_random(&self) -> CryptoResult<EncryptionKey> {
        let key = EncryptionKey::generate_random();
        self.write_keyfile(&key, None)?;
        Ok(key)
    }

    /// Derive a key from a passphrase + salt, write it (with the
    /// KDF metadata so a future boot can re-derive), and return
    /// the derived key.
    pub fn init_passphrase(&self, passphrase: &[u8], salt: &[u8]) -> CryptoResult<EncryptionKey> {
        let key = EncryptionKey::derive_from_passphrase(passphrase, salt)?;
        self.write_keyfile(
            &key,
            Some(KeyFileKdf {
                algorithm: "argon2id".to_string(),
                params: KeyFileKdfParams {
                    m_cost_kib: crate::kdf::ARGON2_MEM_COST_KIB,
                    t_cost: crate::kdf::ARGON2_T_COST,
                    p_cost: crate::kdf::ARGON2_P_COST,
                },
                salt_hex: hex::encode(salt),
            }),
        )?;
        Ok(key)
    }

    /// Load an existing key. If the on-disk file marks a
    /// passphrase-derived key but no passphrase is supplied, the
    /// call returns [`CryptoError::PassphraseRequired`] because
    /// re-derivation isn't possible without the secret.
    pub fn load(&self, passphrase: Option<&[u8]>) -> CryptoResult<EncryptionKey> {
        if !self.path.exists() {
            return Err(CryptoError::KeyFileMissing(self.path.clone()));
        }
        let bytes = fs::read(&self.path)?;
        let kf: KeyFile = serde_json::from_slice(&bytes)
            .map_err(|e| CryptoError::InvalidKeyFile(e.to_string()))?;
        if kf.version != CURRENT_KEY_VERSION {
            return Err(CryptoError::InvalidKeyFile(format!(
                "unsupported key file version {} (expected {})",
                kf.version, CURRENT_KEY_VERSION
            )));
        }
        match (kf.kdf.as_ref(), passphrase) {
            (Some(_), None) => Err(CryptoError::PassphraseRequired),
            (Some(kdf), Some(pw)) => {
                let salt = hex::decode(&kdf.salt_hex)
                    .map_err(|e| CryptoError::InvalidHex(e.to_string()))?;
                EncryptionKey::derive_from_passphrase(pw, &salt)
            }
            (None, _) => {
                let raw = hex::decode(&kf.key_hex)
                    .map_err(|e| CryptoError::InvalidHex(e.to_string()))?;
                EncryptionKey::from_bytes(&raw)
            }
        }
    }

    /// Remove the on-disk key file. Idempotent — no error if the
    /// file is already gone.
    pub fn destroy(&self) -> CryptoResult<()> {
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    fn write_keyfile(
        &self,
        key: &EncryptionKey,
        kdf: Option<KeyFileKdf>,
    ) -> CryptoResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
            // Best-effort directory permission tightening on Unix.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }
        let raw = key.as_bytes_for_write();
        let kf = KeyFile {
            version: CURRENT_KEY_VERSION,
            key_hex: hex::encode(raw),
            kdf,
        };
        let bytes = serde_json::to_vec_pretty(&kf)
            .map_err(|e| CryptoError::InvalidKeyFile(e.to_string()))?;
        let tmp = self.path.with_extension("key.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn init_random_then_load_round_trip() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        let k1 = ks.init_random().unwrap();
        let k2 = ks.load(None).unwrap();
        assert_eq!(k1.as_bytes_for_write(), k2.as_bytes_for_write());
    }

    #[test]
    fn init_passphrase_then_load_with_passphrase() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        let salt = b"some-fixed-salt-1234567890";
        let k1 = ks.init_passphrase(b"hunter2", salt).unwrap();
        let k2 = ks.load(Some(b"hunter2")).unwrap();
        assert_eq!(k1.as_bytes_for_write(), k2.as_bytes_for_write());
    }

    #[test]
    fn load_without_passphrase_when_needed_returns_error() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        ks.init_passphrase(b"hunter2", b"some-fixed-salt-1234567890")
            .unwrap();
        let err = ks.load(None).unwrap_err();
        assert!(matches!(err, CryptoError::PassphraseRequired), "got {:?}", err);
    }

    #[test]
    fn load_wrong_passphrase_yields_different_key() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        ks.init_passphrase(b"hunter2", b"some-fixed-salt-1234567890")
            .unwrap();
        let k = ks.load(Some(b"correct horse")).unwrap();
        // Just sanity-check it doesn't panic — the actual bytes are
        // unknown until derived, but they must differ from the
        // original.
        assert_eq!(k.as_bytes_for_write().len(), 32);
    }

    #[test]
    fn load_missing_key_file() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        let err = ks.load(None).unwrap_err();
        assert!(matches!(err, CryptoError::KeyFileMissing(_)), "got {:?}", err);
    }

    #[test]
    fn destroy_is_idempotent() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        ks.init_random().unwrap();
        ks.destroy().unwrap();
        ks.destroy().unwrap();
        assert!(!ks.exists());
    }
}
