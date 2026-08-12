//! Optional AEAD-encrypted [`BlobStore`] wrapper.
//!
//! When the CLI operator opts into encryption
//! (`adbnet config set storage.encrypt.enabled true`), the
//! [`crate::scope::StorageTopology`] swaps its **private** scope
//! `BlobStore` for an [`EncryptedBlobStore`]. Writes go through
//! the sync `import_file_sync` / `put_bytes_sync` paths which
//! encrypt each chunk with `XChaCha20-Poly1305`; reads decrypt the
//! chunk bytes back into the same BLAKE3 content hash (so the
//! on-disk CID is still derivable from plaintext, but the chunk
//! blobs themselves are unintelligible without the key).
//!
//! ## Threat model
//!
//! * **At rest** — without the on-disk `keys/storage.key` (or the
//!   passphrase that derives it) the chunk blobs are
//!   indistinguishable from random. An attacker who copies
//!   `<data_dir>/private/` sees only `[nonce | ciphertext | tag]`
//!   payloads.
//! * **In memory** — the AEAD key is wiped with
//!   [`zeroize::Zeroize`] on drop; chunk plaintexts are short-
//!   lived and dropped at the end of each read.
//! * **Not in scope** — side-channel resistance (cache timing),
//!   key compromise via memory dumps, malicious operators who
//!   own the host. We don't pretend to defend against those
//!   here.
//!
//! ## Layout on disk
//!
//! ```text
//! <data_dir>/private/
//!   <hex-hash>/
//!     meta.json      {"hash": ..., "sizeBytes": ..., "chunkCount": ..., "encrypted": true}
//!     complete
//!     chunks/
//!       000000       XChaCha20-Poly1305 ciphertext of plaintext chunk 0
//!       000001       ...
//!       ...
//! ```
//!
//! Note: `meta.json` only carries the *plaintext* size / chunk
//! count, not the ciphertext expansion. The ciphertext is
//! exactly `CHUNK_SIZE + 16` bytes for full chunks (last chunk
//! may be shorter, with a correspondingly smaller ciphertext).
//!
//! ## Key lifecycle
//!
//! 1. The operator runs `adbnet storage encrypt-init` (one-shot)
//!    or sets `storage.encrypt.enabled = true` in `app.toml`. The
//!    CLI generates a random 32-byte master key and writes it to
//!    `<data_dir>/keys/storage.key` (mode 0600).
//! 2. On every CLI start, [`KeyStore::load`] reads that file
//!    and hands the bytes to [`EncryptionKey::from_bytes`].
//! 3. On `adbnet storage encrypt-disable`, the operator may
//!    either wipe the on-disk blobs (no automatic re-decrypt
//!    in v1 — we'd need the key at GC time, which we don't have)
//!    or simply delete the key file (rendering all prior
//!    ciphertexts unreadable).
//!
//! ## Passphrase-derived keys
//!
//! `KeyStore::derive_from_passphrase` is provided for the future
//! "encrypt with a passphrase the operator must type every boot"
//! use case. v1 still writes the raw key to disk because the
//! auto-start path can't prompt.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use adnet_types::ContentHash;
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::chunked::CHUNK_SIZE;
use crate::store::BlobStore;

/// XChaCha20-Poly1305 overhead: 24-byte nonce + 16-byte tag.
pub const AEAD_OVERHEAD: usize = 24 + 16;

/// 32-byte master key used with XChaCha20-Poly1305.
#[derive(Clone)]
pub struct EncryptionKey {
    bytes: [u8; 32],
}

impl EncryptionKey {
    /// Random 32-byte key. Uses the OS RNG via `rand::rngs::OsRng`.
    pub fn generate_random() -> Self {
        use rand::RngCore;
        let mut rng = rand::rngs::OsRng;
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Wrap 32 raw bytes. Returns `Err` if the slice is the
    /// wrong size — never panics on a malformed input.
    pub fn from_bytes(b: &[u8]) -> Result<Self, EncryptionError> {
        if b.len() != 32 {
            return Err(EncryptionError::InvalidKeyLength(b.len()));
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(b);
        Ok(Self { bytes })
    }

    /// Derive a 32-byte key from a passphrase using Argon2id with
    /// the parameters recommended by the OWASP Password Storage
    /// Cheat Sheet (m=19 MiB, t=2, p=1).
    pub fn derive_from_passphrase(
        passphrase: &[u8],
        salt: &[u8],
    ) -> Result<Self, EncryptionError> {
        if salt.is_empty() {
            return Err(EncryptionError::InvalidSalt);
        }
        let params = Params::new(19 * 1024, 2, 1, Some(32))
            .map_err(|e| EncryptionError::Kdf(e.to_string()))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut out = [0u8; 32];
        argon
            .hash_password_into(passphrase, salt, &mut out)
            .map_err(|e| EncryptionError::Kdf(e.to_string()))?;
        Ok(Self { bytes: out })
    }

    fn aead(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(Key::from_slice(&self.bytes))
    }
}

impl Drop for EncryptionKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl std::fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material in logs / panic messages.
        f.debug_struct("EncryptionKey").finish()
    }
}

/// Errors emitted by the encryption layer. All variants carry
/// enough context for the CLI to surface a useful message without
/// leaking any key material.
#[derive(Debug, thiserror::Error)]
pub enum EncryptionError {
    #[error("invalid key length: expected 32 bytes, got {0}")]
    InvalidKeyLength(usize),
    #[error("salt must not be empty")]
    InvalidSalt,
    #[error("key derivation failed: {0}")]
    Kdf(String),
    #[error("AEAD encryption failed")]
    AeadEncrypt,
    #[error("AEAD decryption failed (wrong key or corrupted ciphertext)")]
    AeadDecrypt,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid metadata: {0}")]
    InvalidMetadata(String),
    #[error("encrypted blob store key file not found at {0}")]
    KeyFileMissing(PathBuf),
}

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

const CURRENT_KEY_VERSION: u32 = 1;

/// Persistent key store. Reads/writes `<data_dir>/keys/storage.key`
/// in plain text (no passphrase prompt yet). The directory is
/// created on demand.
pub struct KeyStore {
    path: PathBuf,
}

impl KeyStore {
    /// Default key location: `<data_dir>/keys/storage.key`. The
    /// `data_dir` is the root of the operator's node.
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
    pub fn init_random(&self) -> Result<EncryptionKey, EncryptionError> {
        let key = EncryptionKey::generate_random();
        self.write_keyfile(&key, None)?;
        Ok(key)
    }

    /// Derive a key from a passphrase + salt, write it (with the
    /// KDF metadata so a future boot can re-derive), and return
    /// the derived key.
    pub fn init_passphrase(
        &self,
        passphrase: &[u8],
        salt: &[u8],
    ) -> Result<EncryptionKey, EncryptionError> {
        let key = EncryptionKey::derive_from_passphrase(passphrase, salt)?;
        self.write_keyfile(
            &key,
            Some(KeyFileKdf {
                algorithm: "argon2id".to_string(),
                params: KeyFileKdfParams {
                    m_cost_kib: 19 * 1024,
                    t_cost: 2,
                    p_cost: 1,
                },
                salt_hex: hex::encode(salt),
            }),
        )?;
        Ok(key)
    }

    /// Load an existing key. If the on-disk file marks a
    /// passphrase-derived key but no passphrase is supplied, the
    /// call returns `Kdf(...)` because re-derivation isn't
    /// possible without the secret.
    pub fn load(
        &self,
        passphrase: Option<&[u8]>,
    ) -> Result<EncryptionKey, EncryptionError> {
        if !self.path.exists() {
            return Err(EncryptionError::KeyFileMissing(self.path.clone()));
        }
        let bytes = fs::read(&self.path)?;
        let kf: KeyFile = serde_json::from_slice(&bytes)
            .map_err(|e| EncryptionError::InvalidMetadata(e.to_string()))?;
        if kf.version != CURRENT_KEY_VERSION {
            return Err(EncryptionError::InvalidMetadata(format!(
                "unsupported key file version {} (expected {})",
                kf.version, CURRENT_KEY_VERSION
            )));
        }
        match (kf.kdf.as_ref(), passphrase) {
            (Some(kdf), Some(pw)) => {
                let salt = hex::decode(&kdf.salt_hex)
                    .map_err(|e| EncryptionError::InvalidMetadata(e.to_string()))?;
                EncryptionKey::derive_from_passphrase(pw, &salt)
            }
            (Some(_), None) => Err(EncryptionError::Kdf(
                "key file is passphrase-derived; supply the passphrase".to_string(),
            )),
            (None, _) => {
                let raw = hex::decode(&kf.key_hex)
                    .map_err(|e| EncryptionError::InvalidMetadata(e.to_string()))?;
                EncryptionKey::from_bytes(&raw)
            }
        }
    }

    /// Remove the on-disk key file. Idempotent — no error if the
    /// file is already gone.
    pub fn destroy(&self) -> Result<(), EncryptionError> {
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    fn write_keyfile(
        &self,
        key: &EncryptionKey,
        kdf: Option<KeyFileKdf>,
    ) -> Result<(), EncryptionError> {
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
            .map_err(|e| EncryptionError::InvalidMetadata(e.to_string()))?;
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

/// Helper for the file write above. We avoid `as_bytes()` (which
/// returns a `Zeroizing<[u8; 32]>`) because that type doesn't
/// `Deref` to `[u8]` in a way `hex::encode` can consume without
/// borrowing the wrapper type's lifetime.
pub trait KeyWriteAccess {
    fn as_bytes_for_write(&self) -> [u8; 32];
    /// Same as [`KeyWriteAccess::as_bytes_for_write`] but the
    /// public name matches what other crates (CLI, integration
    /// tests) import. Returns a copy so the caller can't keep a
    /// borrow on the key — it's safe to log / compare without
    /// leaking via `Drop`.
    fn as_bytes_for_test(&self) -> [u8; 32] {
        self.as_bytes_for_write()
    }
}
impl KeyWriteAccess for EncryptionKey {
    fn as_bytes_for_write(&self) -> [u8; 32] {
        self.bytes
    }
}

// ─────────────────────────────────────────────────────────────────────
//  EncryptedBlobStore — wrapper that encrypts every chunk before it
//  hits disk and decrypts every chunk on the way back.
// ─────────────────────────────────────────────────────────────────────

/// Marker we drop into `meta.json` so a future open of the same
/// data dir knows it must decrypt.
pub const META_ENCRYPTED_FIELD: &str = "encrypted";

/// Chunk-level AEAD-encrypted view over an existing
/// [`BlobStore`]. The wrapped store remains the source of truth
/// for layout (`<hash>/chunks/<index>`); we just transform the
/// bytes that hit `write` / come out of `read`.
pub struct EncryptedBlobStore {
    inner: BlobStore,
    key: EncryptionKey,
}

impl EncryptedBlobStore {
    /// Wrap an existing [`BlobStore`] for encryption / decryption.
    pub fn new(inner: BlobStore, key: EncryptionKey) -> Self {
        Self { inner, key }
    }

    /// Borrow the underlying unencrypted store. Used by tests
    /// to assert on-disk state directly.
    pub fn inner(&self) -> &BlobStore {
        &self.inner
    }

    /// Borrow the key. Mainly for tests / diagnostics.
    pub fn key(&self) -> &EncryptionKey {
        &self.key
    }

    /// Encrypt a single chunk of plaintext.
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        use rand::RngCore;
        let mut rng = rand::rngs::OsRng;
        let mut nonce_bytes = [0u8; 24];
        rng.fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);
        let aead = self.key.aead();
        let ct = aead
            .encrypt(nonce, plaintext)
            .map_err(|_| EncryptionError::AeadEncrypt)?;
        let mut out = Vec::with_capacity(AEAD_OVERHEAD + ct.len());
        out.extend_from_slice(nonce.as_slice());
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypt a chunk ciphertext back to plaintext.
    fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        if sealed.len() < AEAD_OVERHEAD {
            return Err(EncryptionError::AeadDecrypt);
        }
        let (nonce_bytes, ciphertext) = sealed.split_at(24);
        let nonce = XNonce::from_slice(nonce_bytes);
        let aead = self.key.aead();
        let pt = aead
            .decrypt(nonce, ciphertext)
            .map_err(|_| EncryptionError::AeadDecrypt)?;
        Ok(pt)
    }

    /// Encrypt a buffer (any size). Returns the on-disk form
    /// (nonce + ct + tag).
    pub fn seal_chunk(&self, chunk: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        self.seal(chunk)
    }

    /// Decrypt an on-disk chunk back to plaintext.
    pub fn open_chunk(&self, sealed: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        self.open(sealed)
    }

    // ─────────────────────────────────────────────────────────────────
    //  Sync import / read surface — mirrors `BlobStore`'s sync API.
    // ─────────────────────────────────────────────────────────────────

    /// Import a file, encrypting each chunk on the way in.
    /// Returns the plaintext BLAKE3 content hash.
    pub fn import_file_sync(&self, source: &Path) -> std::io::Result<(ContentHash, u64)> {
        use std::io::BufReader;
        let (hash, size) = self.inner.hash_file(source)?;
        let dest_dir = self.inner.blob_dir(&hash);
        if dest_dir.join("complete").exists() {
            return Ok((hash, size));
        }
        let staging = self
            .inner
            .data_dir()
            .join(format!(".importing-{}", hash.as_hex()));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(staging.join("chunks"))?;
        let f = fs::File::open(source)?;
        let mut reader = BufReader::new(f);
        let mut buf = vec![0u8; CHUNK_SIZE];
        let mut idx = 0u32;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let sealed = self.seal(&buf[..n]).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            })?;
            fs::write(
                staging.join("chunks").join(format!("{:06}", idx)),
                &sealed,
            )?;
            idx += 1;
        }
        write_encrypted_meta(&staging, &hash, size, idx)?;
        fs::write(staging.join("complete"), b"1")?;
        fs::rename(&staging, &dest_dir)?;
        Ok((hash, size))
    }

    /// Store raw bytes as a chunked, encrypted blob.
    pub fn put_bytes_sync(&self, data: &[u8]) -> std::io::Result<(ContentHash, u64)> {
        let hash = ContentHash::from_bytes(data);
        let total = data.len() as u64;
        let final_dir = self.inner.blob_dir(&hash);
        if final_dir.join("complete").exists() {
            return Ok((hash, total));
        }
        let staging = self
            .inner
            .data_dir()
            .join(format!(".importing-{}", hash.as_hex()));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(staging.join("chunks"))?;
        let mut idx = 0u32;
        for chunk in data.chunks(CHUNK_SIZE) {
            let sealed = self.seal(chunk).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            })?;
            fs::write(
                staging.join("chunks").join(format!("{:06}", idx)),
                &sealed,
            )?;
            idx += 1;
        }
        write_encrypted_meta(&staging, &hash, total, idx)?;
        fs::write(staging.join("complete"), b"1")?;
        fs::rename(&staging, &final_dir)?;
        Ok((hash, total))
    }

    /// Read a single chunk, decrypting it.
    pub fn read_chunk_sync(
        &self,
        hash: &ContentHash,
        index: u32,
    ) -> std::io::Result<Vec<u8>> {
        let sealed = self.inner.read_chunk_sync(hash, index)?;
        self.open(&sealed)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    }

    /// Read the entire blob, decrypting every chunk.
    pub fn get_sync(&self, hash: &ContentHash) -> Option<Vec<u8>> {
        if !self.has_complete(hash) {
            return None;
        }
        let (size, count) = match self.inner.meta(hash) {
            Ok(v) => v,
            Err(_) => return None,
        };
        let mut out = Vec::with_capacity(size as usize);
        for i in 0..count {
            let chunk = match self.read_chunk_sync(hash, i) {
                Ok(c) => c,
                Err(_) => return None,
            };
            out.extend_from_slice(&chunk);
        }
        Some(out)
    }

    /// True when the wrapped store has the blob AND its
    /// `meta.json` claims encryption (sanity check).
    pub fn has_complete(&self, hash: &ContentHash) -> bool {
        self.inner.has_complete(hash) && is_encrypted_meta(&self.inner.blob_dir(hash))
    }

    /// Mirror `BlobStore::meta` — read `(size_bytes, chunk_count)`
    /// from the on-disk `meta.json`.
    pub fn meta(&self, hash: &ContentHash) -> Result<(u64, u32), crate::chunked::ChunkError> {
        self.inner.meta(hash)
    }

    /// Read a byte range, decrypting all chunks it touches.
    pub fn read_range_sync(
        &self,
        hash: &ContentHash,
        offset: u32,
        len: u32,
    ) -> std::io::Result<Vec<u8>> {
        let blob = self
            .get_sync(hash)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "blob not found"))?;
        let end = (offset as usize + len as usize).min(blob.len());
        Ok(blob[offset as usize..end].to_vec())
    }

    /// List all complete hashes — pass-through to the inner
    /// store. GC iterates this.
    pub fn list_complete(&self) -> std::io::Result<Vec<ContentHash>> {
        self.inner.list_complete()
    }

    /// Remove a blob. Pass-through; the wrapper doesn't encrypt
    /// `remove`.
    pub fn remove(&self, hash: &ContentHash) -> std::io::Result<bool> {
        self.inner.remove(hash)
    }

    /// `gc_orphans` pass-through.
    pub fn gc_orphans(&self, pins: &crate::pin_set::PinSet) -> std::io::Result<Vec<ContentHash>> {
        self.inner.gc_orphans(pins)
    }

    /// `gc_unpinned` pass-through.
    pub fn gc_unpinned<I, S>(&self, pinned: I) -> std::io::Result<Vec<ContentHash>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.inner.gc_unpinned(pinned)
    }

    /// `gc_all` pass-through.
    pub fn gc_all(&self) -> std::io::Result<Vec<ContentHash>> {
        self.inner.gc_all()
    }
}

/// Helper: write a `meta.json` that includes the `encrypted: true`
/// marker so a future open can pick the right reader.
pub fn write_encrypted_meta(
    blob_dir: &Path,
    hash: &ContentHash,
    size_bytes: u64,
    chunk_count: u32,
) -> std::io::Result<()> {
    let meta = serde_json::json!({
        "hash": hash.as_hex(),
        "sizeBytes": size_bytes,
        "chunkCount": chunk_count,
        META_ENCRYPTED_FIELD: true,
    });
    fs::write(blob_dir.join("meta.json"), serde_json::to_vec(&meta)?)
}

/// Return `true` if the on-disk `meta.json` declares
/// `encrypted = true`.
pub fn is_encrypted_meta(blob_dir: &Path) -> bool {
    let path = blob_dir.join("meta.json");
    let Ok(bytes) = fs::read(&path) else {
        return false;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    v.get(META_ENCRYPTED_FIELD)
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

// ─────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn encryption_key_round_trips_bytes() {
        let k = EncryptionKey::generate_random();
        let raw = k.as_bytes_for_write();
        let restored = EncryptionKey::from_bytes(&raw).unwrap();
        assert_eq!(restored.as_bytes_for_write().as_slice(), raw.as_slice());
    }

    #[test]
    fn encryption_key_from_bytes_rejects_wrong_size() {
        assert!(EncryptionKey::from_bytes(&[]).is_err());
        assert!(EncryptionKey::from_bytes(&[0u8; 16]).is_err());
        assert!(EncryptionKey::from_bytes(&[0u8; 64]).is_err());
        assert!(EncryptionKey::from_bytes(&[0u8; 32]).is_ok());
    }

    #[test]
    fn encryption_key_derive_is_deterministic_with_same_salt() {
        let salt = b"some-fixed-salt-1234567890";
        let k1 = EncryptionKey::derive_from_passphrase(b"hunter2", salt).unwrap();
        let k2 = EncryptionKey::derive_from_passphrase(b"hunter2", salt).unwrap();
        assert_eq!(k1.as_bytes_for_write().as_slice(), k2.as_bytes_for_write().as_slice());
    }

    #[test]
    fn encryption_key_derive_changes_with_salt() {
        // Argon2 requires salts of at least 8 bytes; we use
        // 16 to leave headroom for the canonical RFC 9106
        // recommendation.
        let k1 = EncryptionKey::derive_from_passphrase(b"hunter2", b"salt-aaaaaaaa-aaa").unwrap();
        let k2 = EncryptionKey::derive_from_passphrase(b"hunter2", b"salt-bbbbbbbb-bbb").unwrap();
        assert_ne!(k1.as_bytes_for_write().as_slice(), k2.as_bytes_for_write().as_slice());
    }

    #[test]
    fn encryption_key_derive_changes_with_passphrase() {
        let salt = b"salt-aaaaaaaaaaaaaa";
        let k1 = EncryptionKey::derive_from_passphrase(b"hunter2", salt).unwrap();
        let k2 = EncryptionKey::derive_from_passphrase(b"correct horse", salt).unwrap();
        assert_ne!(k1.as_bytes_for_write().as_slice(), k2.as_bytes_for_write().as_slice());
    }

    #[test]
    fn derive_rejects_short_salt() {
        // Argon2 enforces a minimum salt length (8 bytes by
        // default). Operators must supply enough entropy or
        // the call fails fast — there's no silent fallback to
        // a derived "weak" key.
        let err = EncryptionKey::derive_from_passphrase(b"pw", b"short").unwrap_err();
        assert!(matches!(err, EncryptionError::Kdf(_)), "got {:?}", err);
    }

    #[test]
    fn keystore_init_and_load_round_trip() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        assert!(!ks.exists());
        let k = ks.init_random().unwrap();
        assert!(ks.exists());
        let loaded = ks.load(None).unwrap();
        assert_eq!(k.as_bytes_for_write().as_slice(), loaded.as_bytes_for_write().as_slice());
    }

    #[test]
    fn keystore_init_passphrase_and_load_with_pw() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        let k1 = ks.init_passphrase(b"correct horse", b"salt-abc").unwrap();
        let loaded = ks.load(Some(b"correct horse")).unwrap();
        assert_eq!(k1.as_bytes_for_write().as_slice(), loaded.as_bytes_for_write().as_slice());
    }

    #[test]
    fn keystore_load_passphrase_derived_without_pw_errors() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        ks.init_passphrase(b"correct horse", b"salt-abc").unwrap();
        let err = ks.load(None).unwrap_err();
        assert!(matches!(err, EncryptionError::Kdf(_)), "got {:?}", err);
    }

    #[test]
    fn keystore_load_wrong_passphrase_yields_different_key() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        let original = ks.init_passphrase(b"correct horse", b"salt-abc").unwrap();
        let wrong = ks.load(Some(b"WRONG")).unwrap();
        assert_ne!(original.as_bytes_for_write().as_slice(), wrong.as_bytes_for_write().as_slice());
    }

    #[test]
    fn keystore_destroy_is_idempotent() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        ks.init_random().unwrap();
        ks.destroy().unwrap();
        ks.destroy().unwrap();
        assert!(!ks.exists());
    }

    #[test]
    fn keystore_load_missing_file_errors() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        let err = ks.load(None).unwrap_err();
        assert!(matches!(err, EncryptionError::KeyFileMissing(_)), "got {:?}", err);
    }

    #[test]
    fn encrypted_chunk_round_trip() {
        let key = EncryptionKey::generate_random();
        let inner = BlobStore::new(&temp_dir().path()).unwrap();
        let enc = EncryptedBlobStore::new(inner, key);
        let plaintext = vec![0xab; CHUNK_SIZE];
        let sealed = enc.seal_chunk(&plaintext).unwrap();
        assert_ne!(sealed, plaintext);
        let opened = enc.open_chunk(&sealed).unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn seal_changes_with_nonce() {
        let key = EncryptionKey::generate_random();
        let inner = BlobStore::new(&temp_dir().path()).unwrap();
        let enc = EncryptedBlobStore::new(inner, key);
        let plaintext = b"hello world";
        let s1 = enc.seal_chunk(plaintext).unwrap();
        let s2 = enc.seal_chunk(plaintext).unwrap();
        assert_ne!(&s1[..24], &s2[..24]);
        assert_ne!(s1, s2);
    }

    #[test]
    fn open_rejects_truncated_ciphertext() {
        let key = EncryptionKey::generate_random();
        let inner = BlobStore::new(&temp_dir().path()).unwrap();
        let enc = EncryptedBlobStore::new(inner, key);
        let err = enc.open_chunk(&[0u8; 10]).unwrap_err();
        assert!(matches!(err, EncryptionError::AeadDecrypt));
    }

    #[test]
    fn open_with_wrong_key_fails() {
        let dir = temp_dir();
        let enc_a = EncryptedBlobStore::new(
            BlobStore::new(&dir.path().join("a")).unwrap(),
            EncryptionKey::generate_random(),
        );
        let plaintext = b"secret message";
        let sealed = enc_a.seal_chunk(plaintext).unwrap();

        let enc_b = EncryptedBlobStore::new(
            BlobStore::new(&dir.path().join("b")).unwrap(),
            EncryptionKey::generate_random(),
        );
        let err = enc_b.open_chunk(&sealed).unwrap_err();
        assert!(matches!(err, EncryptionError::AeadDecrypt));
    }

    #[test]
    fn put_bytes_round_trips_through_inner_blobstore() {
        let dir = temp_dir();
        let inner = BlobStore::new(dir.path()).unwrap();
        let enc = EncryptedBlobStore::new(
            BlobStore::new(dir.path()).unwrap(),
            EncryptionKey::generate_random(),
        );
        let payload = b"hello encrypted blobstore";
        let (hash, size) = enc.put_bytes_sync(payload).unwrap();
        assert_eq!(size, payload.len() as u64);
        assert!(inner.has_complete(&hash));
        let back = enc.get_sync(&hash).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn put_bytes_writes_encrypted_marker_in_meta() {
        let dir = temp_dir();
        let enc = EncryptedBlobStore::new(
            BlobStore::new(dir.path()).unwrap(),
            EncryptionKey::generate_random(),
        );
        let payload = b"with-encrypted-marker";
        let (hash, _) = enc.put_bytes_sync(payload).unwrap();
        let blob_dir = enc.inner().blob_dir(&hash);
        assert!(is_encrypted_meta(&blob_dir));
    }

    #[test]
    fn on_disk_chunks_are_actually_ciphertext() {
        let dir = temp_dir();
        let enc = EncryptedBlobStore::new(
            BlobStore::new(dir.path()).unwrap(),
            EncryptionKey::generate_random(),
        );
        // All-zero plaintext — the smoking-gun test that we
        // really encrypt: ciphertext must contain *non-zero*
        // bytes (a real AEAD output of zero plaintext is still
        // pseudorandom).
        let payload = vec![0u8; CHUNK_SIZE];
        let (hash, _) = enc.put_bytes_sync(&payload).unwrap();
        let chunk0 = enc.inner().read_chunk_sync(&hash, 0).unwrap();
        // The sealed chunk is nonce(24) + ct+tag(CHUNK_SIZE+16).
        assert_eq!(chunk0.len(), CHUNK_SIZE + AEAD_OVERHEAD);
        assert!(chunk0.iter().any(|b| *b != 0), "ciphertext should not be all-zero");
    }

    #[test]
    fn import_file_round_trips_via_disk() {
        let dir = temp_dir();
        let src = dir.path().join("source.bin");
        let payload: Vec<u8> = (0..(CHUNK_SIZE * 2 + 17)).map(|i| (i % 251) as u8).collect();
        fs::write(&src, &payload).unwrap();

        let enc = EncryptedBlobStore::new(
            BlobStore::new(&dir.path().join("encrypted")).unwrap(),
            EncryptionKey::generate_random(),
        );
        let (hash, size) = enc.import_file_sync(&src).unwrap();
        assert_eq!(size, payload.len() as u64);
        let back = enc.get_sync(&hash).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn read_range_returns_decrypted_subrange() {
        let dir = temp_dir();
        let enc = EncryptedBlobStore::new(
            BlobStore::new(dir.path()).unwrap(),
            EncryptionKey::generate_random(),
        );
        let payload: Vec<u8> = (0..(CHUNK_SIZE * 3)).map(|i| (i % 200) as u8).collect();
        let (hash, _) = enc.put_bytes_sync(&payload).unwrap();
        let slice = enc.read_range_sync(&hash, CHUNK_SIZE as u32, 64).unwrap();
        assert_eq!(slice, &payload[CHUNK_SIZE..CHUNK_SIZE + 64]);
    }

    #[test]
    fn gc_orphans_passthrough_removes_unpinned() {
        let dir = temp_dir();
        let enc = EncryptedBlobStore::new(
            BlobStore::new(dir.path()).unwrap(),
            EncryptionKey::generate_random(),
        );
        let (a, _) = enc.put_bytes_sync(b"a").unwrap();
        let (b, _) = enc.put_bytes_sync(b"b").unwrap();
        let mut pins = crate::pin_set::PinSet::new();
        pins.add(&a, false, std::collections::BTreeSet::new(), 1);
        let removed = enc.gc_orphans(&pins).unwrap();
        assert_eq!(removed, vec![b.clone()]);
        assert!(enc.has_complete(&a));
        assert!(!enc.has_complete(&b));
    }
}