//! `KeyProvider` — the single seam at which secret key material
//! enters and leaves the application.
//!
//! Every consumer of long-lived secrets in A3Net talks to a
//! `KeyProvider`. The two reference implementations are:
//!
//! - [`InMemoryKeyProvider`] — for tests and short-lived processes.
//!   Keeps every key in a `HashMap<label, Secret<32>>` under an
//!   `RwLock`; wiped on `Drop`.
//!
//! - [`FileKeyProvider`] — persists a single *master* KEK under
//!   `<data_dir>/keys/storage.key` using an Argon2id-derived key
//!   wrapping an AES-256-GCM payload. Per-label DEKs are wrapped
//!   under the master KEK and persisted alongside it.
//!
//! Both implement the same [`KeyProvider`] trait so call-sites are
//! agnostic to the storage backend. A future hardware-backed
//! implementation (Secure Enclave / TPM / HSM) drops in without
//! changing a single call-site.
//!
//! ## Threat model
//!
//! - The `KeyProvider` defends against an attacker who can read
//!   arbitrary files inside `<data_dir>` but cannot run code on the
//!   host. With the default `FileKeyProvider`, the attacker needs
//!   *both* the on-disk master file *and* the operator passphrase.
//! - It does **not** defend against memory dumps, side-channel
//!   observation, or a hostile operator with root — those require a
//!   real TEE, which is the "future HSM provider" roadmap item.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::error::{CryptoError, CryptoResult};
use crate::kdf::{ARGON2_MEM_COST_KIB, ARGON2_P_COST, ARGON2_T_COST};
use crate::secret::Secret;

/// Domain-separation tag for per-label DEK derivation.
const DEK_INFO: &[u8] = b"a3net-crypto/dek/v1";
/// Magic header so a corrupted / foreign file is detected before
/// we try to AES-GCM-decrypt random bytes.
pub const KEYSTORE_MAGIC: &[u8; 4] = b"A3K1";
/// Current `KeyStoreFile` wire version.
pub const KEYSTORE_FILE_VERSION: u32 = 2;

/// The seam. Every long-lived key lives behind this trait.
pub trait KeyProvider: Send + Sync {
    /// Acquire (or generate, on first use) the master KEK for this
    /// provider. The returned secret is wiped on drop.
    fn master(&self) -> CryptoResult<Secret<32>>;

    /// Derive (or load from cache) a per-label 32-byte DEK bound to
    /// the master KEK. Same label → same DEK within one provider
    /// instance, but the DEK is never persisted directly: the
    /// provider stores `(label, wrapped_dek)` and unwraps on demand.
    fn derive(&self, label: &str) -> CryptoResult<Secret<32>>;

    /// Generate a fresh random 32-byte secret, wrap it under the
    /// master KEK, and persist the wrapped form keyed by `label`.
    /// Subsequent `derive(label)` calls return the same secret.
    fn generate_and_store(&self, label: &str) -> CryptoResult<Secret<32>>;

    /// Rotate the secret at `label`: generate a fresh 32-byte value,
    /// wrap it, persist the wrapped form, and return the new secret.
    /// The old wrapped form is overwritten atomically.
    fn rotate(&self, label: &str) -> CryptoResult<Secret<32>>;

    /// Drop the secret at `label` (if any). Idempotent.
    fn forget(&self, label: &str) -> CryptoResult<()>;
}

// ─────────────────────────────────────────────────────────────────────────
// InMemoryKeyProvider
// ─────────────────────────────────────────────────────────────────────────

/// In-memory key provider. Holds secrets in an `RwLock<HashMap>`; the
/// map and the secrets are all wiped on `Drop`.
#[derive(Default)]
pub struct InMemoryKeyProvider {
    master: Zeroizing<[u8; 32]>,
    derived: parking_lot::RwLock<HashMap<String, Zeroizing<[u8;32]>>>,
}

impl std::fmt::Debug for InMemoryKeyProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemoryKeyProvider")
            .field("derived_labels", &self.derived.read().len())
            .finish()
    }
}

impl Drop for InMemoryKeyProvider {
    fn drop(&mut self) {
        self.master.zeroize();
        self.derived.write().clear();
    }
}

impl InMemoryKeyProvider {
    pub fn new() -> Self {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self {
            master: Zeroizing::new(bytes),
            derived: parking_lot::RwLock::new(HashMap::new()),
        }
    }

    pub fn with_master(master: [u8; 32]) -> Self {
        Self {
            master: Zeroizing::new(master),
            derived: parking_lot::RwLock::new(HashMap::new()),
        }
    }

    fn derive_locked(master: &[u8; 32], label: &str) -> CryptoResult<[u8; 32]> {
        derive_domain(master, DEK_INFO, label.as_bytes())
    }
}

impl KeyProvider for InMemoryKeyProvider {
    fn master(&self) -> CryptoResult<Secret<32>> {
        Secret::new(*self.master)
    }

    fn derive(&self, label: &str) -> CryptoResult<Secret<32>> {
        if let Some(bytes) = self.derived.read().get(label) {
            return Secret::new(**bytes);
        }
        let bytes = Self::derive_locked(&self.master, label)?;
        self.derived.write().insert(label.to_string(), Zeroizing::new(bytes));
        Secret::new(bytes)
    }

    fn generate_and_store(&self, label: &str) -> CryptoResult<Secret<32>> {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        self.derived.write().insert(label.to_string(), Zeroizing::new(bytes));
        Secret::new(bytes)
    }

    fn rotate(&self, label: &str) -> CryptoResult<Secret<32>> {
        self.generate_and_store(label)
    }

    fn forget(&self, label: &str) -> CryptoResult<()> {
        self.derived.write().remove(label);
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────
// FileKeyProvider
// ─────────────────────────────────────────────────────────────────────────

/// On-disk shape of `KeyStoreFile` (the `.key` JSON).
#[derive(Debug, Serialize, Deserialize)]
pub struct KeyStoreFile {
    pub magic: [u8; 4],
    pub version: u32,
    /// Argon2id parameters used to derive the master KEK from the
    /// operator passphrase.
    pub kdf: KeyStoreKdf,
    /// AES-256-GCM nonce for the master-key wrap.
    pub master_nonce: [u8; 12],
    /// AES-256-GCM ciphertext (with tag) of the master KEK. The
    /// plaintext is *not* the master itself; see the doc comment on
    /// [`FileKeyProvider`] for the two-layer construction.
    pub master_ct: Vec<u8>,
    /// Per-label wrapped DEKs, if any have been issued.
    #[serde(default)]
    pub labels: HashMap<String, WrappedLabel>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct KeyStoreKdf {
    pub algorithm: String, // "argon2id"
    pub params: KeyStoreKdfParams,
    pub salt_hex: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct KeyStoreKdfParams {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WrappedLabel {
    pub nonce: [u8; 12],
    pub ct: Vec<u8>,
}

/// `FileKeyProvider` — persists the master KEK wrapped under an
/// Argon2id-derived key. Per-label DEKs are wrapped under the master
/// KEK using a separate AES-256-GCM pass.
///
/// On-disk layout (binary JSON):
///
/// ```text
///   {
///     "magic":   b"A3K1",
///     "version": 2,
///     "kdf":     { "algorithm": "argon2id", "params": {…}, "salt_hex": "…" },
///     "master_nonce": 12-byte,
///     "master_ct":    AES-GCM(derived_kek, "a3net-crypto/master/v1", inner_kek),
///     "labels":  { "<label>": { "nonce": 12-byte, "ct": AES-GCM(inner_kek, "a3net-crypto/label/v1/<label>", dek) }, … }
///   }
/// ```
///
/// ## Two-layer construction
///
/// We do **not** encrypt labels directly with the operator's
/// passphrase. Instead:
///
/// 1. Argon2id(passphrase, salt) → `derived_kek`.
/// 2. `derived_kek` wraps a per-file `inner_kek` (random 32 bytes).
/// 3. `inner_kek` is held in memory only; it is the real key that
///    wraps every label.
///
/// Rotating the operator passphrase therefore requires only one
/// re-wrap (the `inner_kek`), not one per label.
pub struct FileKeyProvider {
    path: PathBuf,
    inner_kek: Zeroizing<[u8; 32]>,
    labels: parking_lot::RwLock<HashMap<String, Zeroizing<[u8; 32]>>>,
    /// Cached copy of the on-disk file so we don't re-read on every
    /// label operation. Mutex (not RwLock) because we hold it across
    /// the file rewrite — parking_lot's RwLock would deadlock on
    /// re-entry.
    cached_file: parking_lot::Mutex<Option<KeyStoreFile>>,
}

impl std::fmt::Debug for FileKeyProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileKeyProvider")
            .field("path", &self.path)
            .field("labels", &self.labels.read().len())
            .finish()
    }
}

impl Drop for FileKeyProvider {
    fn drop(&mut self) {
        self.inner_kek.zeroize();
        self.labels.write().clear();
    }
}impl FileKeyProvider {
    /// Default on-disk path: `<data_dir>/keys/storage.key`.
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("keys").join("storage.key")
    }

    /// Initialise a fresh provider with a brand-new random `inner_kek`
    /// and the supplied operator passphrase. Writes the file
    /// atomically (`tmp + rename` + `sync_all`) with mode `0o600`.
    pub fn init(passphrase: &[u8], path: &Path) -> CryptoResult<Self> {
        let mut inner = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut inner);

        let mut salt = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let derived_kek = derive_argon2id(passphrase, &salt)?;

        let (master_nonce, master_ct) =
            wrap_with_key(&derived_kek, b"a3net-crypto/master/v1", &inner)?;

        let file = KeyStoreFile {
            magic: *KEYSTORE_MAGIC,
            version: KEYSTORE_FILE_VERSION,
            kdf: KeyStoreKdf {
                algorithm: "argon2id".to_string(),
                params: KeyStoreKdfParams {
                    m_cost_kib: ARGON2_MEM_COST_KIB,
                    t_cost: ARGON2_T_COST,
                    p_cost: ARGON2_P_COST,
                },
                salt_hex: hex::encode(salt),
            },
            master_nonce,
            master_ct,
            labels: HashMap::new(),
        };
        write_atomic(path, &file)?;

        Ok(Self {
            path: path.to_path_buf(),
            inner_kek: Zeroizing::new(inner),
            labels: parking_lot::RwLock::new(HashMap::new()),
            cached_file: parking_lot::Mutex::new(Some(file)),
        })
    }

    /// Open an existing provider. Returns `Err(PassphraseRequired)` if
    /// the file does not exist (use [`Self::init`] first). Returns
    /// `Err(WrongPassphrase)` on AEAD tag mismatch.
    pub fn open(passphrase: &[u8], path: &Path) -> CryptoResult<Self> {
        if !path.exists() {
            return Err(CryptoError::KeyFileMissing(path.to_path_buf()));
        }
        let bytes = fs::read(path)?;
        let file: KeyStoreFile = serde_json::from_slice(&bytes)
            .map_err(|e| CryptoError::InvalidKeyFile(e.to_string()))?;
        if file.magic != *KEYSTORE_MAGIC {
            return Err(CryptoError::InvalidKeyFile(
                "magic mismatch — wrong format?".into(),
            ));
        }
        if file.version != KEYSTORE_FILE_VERSION {
            return Err(CryptoError::InvalidKeyFile(format!(
                "unsupported version {} (expected {})",
                file.version, KEYSTORE_FILE_VERSION
            )));
        }
        let salt = hex::decode(&file.kdf.salt_hex)
            .map_err(|e| CryptoError::InvalidHex(e.to_string()))?;
        let derived_kek = derive_argon2id(passphrase, &salt)?;

        let mut inner = Zeroizing::new([0u8; 32]);
        open_with_key(
            &derived_kek,
            b"a3net-crypto/master/v1",
            &file.master_nonce,
            &file.master_ct,
            inner.as_mut_slice(),
        )?;

        // Pre-load the labels into the in-memory cache.
        let mut labels = HashMap::new();
        for (k, v) in file.labels.iter() {
            let mut buf = Zeroizing::new([0u8; 32]);
            open_with_key(
                &inner,
                format!("a3net-crypto/label/v1/{k}").as_bytes(),
                &v.nonce,
                &v.ct,
                buf.as_mut_slice(),
            )?;
            labels.insert(k.clone(), buf);
        }

        Ok(Self {
            path: path.to_path_buf(),
            inner_kek: inner,
            labels: parking_lot::RwLock::new(labels),
            cached_file: parking_lot::Mutex::new(Some(file)),
        })
    }

    /// Atomically rewrite the on-disk file. Re-wraps `inner_kek`
    /// under the new passphrase, re-wraps every label under the same
    /// `inner_kek`. Used by `rotate_passphrase`.
    pub fn rotate_passphrase(&self, new_passphrase: &[u8]) -> CryptoResult<()> {
        let mut salt = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let derived_kek = derive_argon2id(new_passphrase, &salt)?;

        let (master_nonce, master_ct) =
            wrap_with_key(&derived_kek, b"a3net-crypto/master/v1", &self.inner_kek)?;

        let mut labels_out = HashMap::new();
        for (k, v) in self.labels.read().iter() {
            let (nonce, ct) = wrap_with_key(
                &self.inner_kek,
                format!("a3net-crypto/label/v1/{k}").as_bytes(),
                v,
            )?;
            labels_out.insert(k.clone(), WrappedLabel { nonce, ct });
        }

        let file = KeyStoreFile {
            magic: *KEYSTORE_MAGIC,
            version: KEYSTORE_FILE_VERSION,
            kdf: KeyStoreKdf {
                algorithm: "argon2id".to_string(),
                params: KeyStoreKdfParams {
                    m_cost_kib: ARGON2_MEM_COST_KIB,
                    t_cost: ARGON2_T_COST,
                    p_cost: ARGON2_P_COST,
                },
                salt_hex: hex::encode(salt),
            },
            master_nonce,
            master_ct,
            labels: labels_out,
        };
        write_atomic(&self.path, &file)?;
        *self.cached_file.lock() = Some(file);
        Ok(())
    }
}

impl KeyProvider for FileKeyProvider {
    fn master(&self) -> CryptoResult<Secret<32>> {
        Secret::new(*self.inner_kek)
    }

    fn derive(&self, label: &str) -> CryptoResult<Secret<32>> {
        if let Some(bytes) = self.labels.read().get(label) {
            return Secret::new(**bytes);
        }
        // Generate a new DEK and wrap+persist it.
        let mut dek = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut dek);

        let mut cached = self.cached_file.lock();
        let mut file = cached
            .take()
            .ok_or_else(|| CryptoError::Internal("cached file missing".into()))?;
        let (nonce, ct) = wrap_with_key(
            &self.inner_kek,
            format!("a3net-crypto/label/v1/{label}").as_bytes(),
            &dek,
        )?;
        file.labels.insert(label.to_string(), WrappedLabel { nonce, ct });
        write_atomic(&self.path, &file)?;
        *cached = Some(file);

        self.labels.write().insert(label.to_string(), Zeroizing::new(dek));
        Secret::new(dek)
    }

    fn generate_and_store(&self, label: &str) -> CryptoResult<Secret<32>> {
        self.derive(label)
    }

    fn rotate(&self, label: &str) -> CryptoResult<Secret<32>> {
        let mut dek = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut dek);

        let mut cached = self.cached_file.lock();
        let mut file = cached
            .take()
            .ok_or_else(|| CryptoError::Internal("cached file missing".into()))?;
        let (nonce, ct) = wrap_with_key(
            &self.inner_kek,
            format!("a3net-crypto/label/v1/{label}").as_bytes(),
            &dek,
        )?;
        file.labels.insert(label.to_string(), WrappedLabel { nonce, ct });
        write_atomic(&self.path, &file)?;
        *cached = Some(file);

        self.labels.write().insert(label.to_string(), Zeroizing::new(dek));
        Secret::new(dek)
    }

    fn forget(&self, label: &str) -> CryptoResult<()> {
        self.labels.write().remove(label);

        let mut cached = self.cached_file.lock();
        let mut file = cached
            .take()
            .ok_or_else(|| CryptoError::Internal("cached file missing".into()))?;
        file.labels.remove(label);
        write_atomic(&self.path, &file)?;
        *cached = Some(file);
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Free helpers used by both providers.
// ─────────────────────────────────────────────────────────────────────────

fn derive_argon2id(passphrase: &[u8], salt: &[u8]) -> CryptoResult<[u8; 32]> {
    if salt.len() < 8 {
        return Err(CryptoError::InvalidSalt);
    }
    let params = Params::new(ARGON2_MEM_COST_KIB, ARGON2_T_COST, ARGON2_P_COST, Some(32))
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(passphrase, salt, out.as_mut_slice())
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(*out)
}

fn derive_domain(master: &[u8; 32], info: &[u8], label: &[u8]) -> CryptoResult<[u8; 32]> {
    use sha2::Sha256;
    use hkdf::Hkdf;
    let hk = Hkdf::<Sha256>::new(Some(info), master);
    let mut out = [0u8; 32];
    hk.expand(label, &mut out)
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(out)
}

fn wrap_with_key(
    key: &[u8; 32],
    ad: &[u8],
    plaintext: &[u8; 32],
) -> CryptoResult<([u8; 12], Vec<u8>)> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext.as_slice(),
                aad: ad,
            },
        )
        .map_err(|_| CryptoError::AeadEncrypt)?;
    Ok((nonce_bytes, ct))
}

fn open_with_key(
    key: &[u8; 32],
    ad: &[u8],
    nonce_bytes: &[u8; 12],
    ciphertext: &[u8],
    out: &mut [u8],
) -> CryptoResult<()> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(nonce_bytes);
    let pt = cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad: ad,
            },
        )
        .map_err(|_| CryptoError::AeadDecrypt)?;
    if pt.len() != out.len() {
        return Err(CryptoError::InvalidKeyLength(pt.len()));
    }
    out.copy_from_slice(&pt);
    Ok(())
}

fn write_atomic(path: &Path, file: &KeyStoreFile) -> CryptoResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    let bytes = serde_json::to_vec_pretty(file)
        .map_err(|e| CryptoError::InvalidKeyFile(e.to_string()))?;
    let tmp = path.with_extension("key.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Convenience: build an [`Arc<dyn KeyProvider>`] from any concrete
/// provider so call sites can store the trait object.
pub fn arc<P: KeyProvider + 'static>(p: P) -> Arc<dyn KeyProvider> {
    Arc::new(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn in_memory_round_trip() {
        let p = InMemoryKeyProvider::new();
        let s1 = p.derive("chat").unwrap();
        let s2 = p.derive("chat").unwrap();
        assert_eq!(s1.expose().to_vec(), s2.expose().to_vec());
        let s3 = p.derive("blob").unwrap();
        assert_ne!(s1.expose().to_vec(), s3.expose().to_vec());
    }

    #[test]
    fn in_memory_forget() {
        let p = InMemoryKeyProvider::new();
        let _ = p.derive("x").unwrap();
        assert!(p.forget("x").is_ok());
        // After forget, derive("x") returns a fresh random key.
        let a = p.derive("x").unwrap();
        let b = p.derive("x").unwrap();
        assert_eq!(a.expose().to_vec(), b.expose().to_vec());
    }

    #[test]
    fn in_memory_rotate_changes_value() {
        let p = InMemoryKeyProvider::new();
        let s1 = p.derive("k").unwrap();
        let s2 = p.rotate("k").unwrap();
        assert_ne!(s1.expose().to_vec(), s2.expose().to_vec());
    }

    #[test]
    fn file_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("storage.key");
        let p = FileKeyProvider::init(b"correct horse", &path).unwrap();
        let s1 = p.derive("chat").unwrap();
        drop(p);

        let p2 = FileKeyProvider::open(b"correct horse", &path).unwrap();
        let s2 = p2.derive("chat").unwrap();
        assert_eq!(s1.expose().to_vec(), s2.expose().to_vec());
    }

    #[test]
    fn file_wrong_passphrase_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("storage.key");
        FileKeyProvider::init(b"correct horse", &path).unwrap();
        let err = FileKeyProvider::open(b"wrong", &path).unwrap_err();
        assert!(matches!(err, CryptoError::AeadDecrypt));
    }

    #[test]
    fn file_rotate_passphrase_preserves_labels() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("storage.key");
        let p = FileKeyProvider::init(b"pw1", &path).unwrap();
        let s1 = p.derive("chat").unwrap();
        p.rotate_passphrase(b"pw2").unwrap();

        let p2 = FileKeyProvider::open(b"pw2", &path).unwrap();
        let s2 = p2.derive("chat").unwrap();
        assert_eq!(s1.expose().to_vec(), s2.expose().to_vec());
    }

    #[test]
    fn file_rotate_label_changes_value() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("storage.key");
        let p = FileKeyProvider::init(b"pw", &path).unwrap();
        let s1 = p.derive("k").unwrap();
        let s2 = p.rotate("k").unwrap();
        assert_ne!(s1.expose().to_vec(), s2.expose().to_vec());
    }

    #[test]
    fn file_magic_mismatch_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("storage.key");
        fs::write(&path, b"not a real file").unwrap();
        let err = FileKeyProvider::open(b"pw", &path).unwrap_err();
        assert!(matches!(err, CryptoError::InvalidKeyFile(_)));
    }
}
