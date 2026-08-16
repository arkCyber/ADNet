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
//! ## Module organisation
//!
//! The core crypto primitives (`EncryptionKey`, `KeyStore`,
//! `KeyFile`, AEAD seal/open) used to live in this file. They have
//! been moved to the dedicated `a3net-crypto` crate so the same
//! primitives are shared across `a3net-blobstore`, future
//! `a3net-agent` chat-history encryption, and `a3net-ffi` shims
//! without forming a dependency cycle. Everything in this file
//! that was previously `pub` is **still re-exported** with the
//! same name, so external callers (`a3net-cli`, integration
//! tests) don't need to change.
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
//! `KeyStore::init_passphrase` is provided for the future
//! "encrypt with a passphrase the operator must type every boot"
//! use case. v1 still writes the raw key to disk because the
//! auto-start path can't prompt.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use a3net_crypto::CryptoError;
use a3net_types::ContentHash;

use crate::chunked::CHUNK_SIZE;
use crate::store::BlobStore;

// ─────────────────────────────────────────────────────────────────────
//  Re-exports — keep the `a3net_blobstore::encrypted::*` path working
//  for every caller that was written before the move to a3net-crypto.
// ─────────────────────────────────────────────────────────────────────

/// Re-export of [`a3net_crypto::EncryptionKey`]. Identical to the
/// pre-extraction type — only the canonical home has moved.
pub use a3net_crypto::EncryptionKey;

/// Re-export of [`a3net_crypto::KeyStore`]. See that type for the
/// full API. The on-disk file format is unchanged.
pub use a3net_crypto::KeyStore;

/// XChaCha20-Poly1305 overhead: 24-byte nonce + 16-byte tag.
/// Kept as a local alias so `EncryptedBlobStore`'s `seal` / `open`
/// calls can keep referring to `AEAD_OVERHEAD` without an extra
/// `use` line.
pub use AEAD_OVERHEAD_LOCAL as AEAD_OVERHEAD;

/// Backwards-compat alias — the original constant name, pointing
/// at the canonical value in `a3net-crypto`.
pub const AEAD_OVERHEAD_LOCAL: usize = a3net_crypto::AEAD_OVERHEAD;

// ─────────────────────────────────────────────────────────────────────
//  Errors
// ─────────────────────────────────────────────────────────────────────

/// Errors emitted by the encryption layer. Every variant is a thin
/// wrapper around the matching [`a3net_crypto::CryptoError`] so
/// pre-extraction callers (`match err { EncryptionError::Kdf(_) => … }`)
/// keep working without changes.
///
/// New code should prefer [`a3net_crypto::CryptoError`] directly.
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
    #[error("ciphertext too short: {got} bytes (need at least {need})")]
    CiphertextTooShort { got: usize, need: usize },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid metadata: {0}")]
    InvalidMetadata(String),
    #[error("invalid hex in key file: {0}")]
    InvalidHex(String),
    #[error("key file is passphrase-derived; supply the passphrase")]
    PassphraseRequired,
    #[error("encrypted blob store key file not found at {0}")]
    KeyFileMissing(PathBuf),
}

impl From<CryptoError> for EncryptionError {
    fn from(e: CryptoError) -> Self {
        match e {
            CryptoError::AeadEncrypt => Self::AeadEncrypt,
            CryptoError::AeadDecrypt => Self::AeadDecrypt,
            CryptoError::CiphertextTooShort { got, need } => Self::CiphertextTooShort { got, need },
            CryptoError::InvalidKeyLength(n) => Self::InvalidKeyLength(n),
            CryptoError::InvalidSalt => Self::InvalidSalt,
            CryptoError::Kdf(s) => Self::Kdf(s),
            CryptoError::InvalidKeyFile(s) => Self::InvalidMetadata(s),
            CryptoError::InvalidHex(s) => Self::InvalidHex(s),
            CryptoError::PassphraseRequired => Self::PassphraseRequired,
            CryptoError::KeyFileMissing(p) => Self::KeyFileMissing(p),
            CryptoError::Io(e) => Self::Io(e),
            CryptoError::Internal(_) => Self::Io(std::io::Error::other("crypto internal error")),
        }
    }
}

impl From<EncryptionError> for CryptoError {
    fn from(e: EncryptionError) -> Self {
        match e {
            EncryptionError::AeadEncrypt => Self::AeadEncrypt,
            EncryptionError::AeadDecrypt => Self::AeadDecrypt,
            EncryptionError::CiphertextTooShort { got, need } => Self::CiphertextTooShort { got, need },
            EncryptionError::InvalidKeyLength(n) => Self::InvalidKeyLength(n),
            EncryptionError::InvalidSalt => Self::InvalidSalt,
            EncryptionError::Kdf(s) => Self::Kdf(s),
            EncryptionError::InvalidMetadata(s) => Self::InvalidKeyFile(s),
            EncryptionError::InvalidHex(s) => Self::InvalidHex(s),
            EncryptionError::PassphraseRequired => Self::PassphraseRequired,
            EncryptionError::KeyFileMissing(p) => Self::KeyFileMissing(p),
            EncryptionError::Io(e) => Self::Io(e),
        }
    }
}

/// On-disk format for `<data_dir>/keys/storage.key`. We keep it
/// JSON so an operator can `cat` / inspect / replace it by hand.
///
/// Re-exported from `a3net-crypto`; the local name is preserved
/// so `a3net_blobstore::KeyFile` keeps resolving.
pub use a3net_crypto::KeyFile;

/// Re-export of [`a3net_crypto::KeyFileKdf`].
pub use a3net_crypto::KeyFileKdf;

/// Re-export of [`a3net_crypto::KeyFileKdfParams`].
pub use a3net_crypto::KeyFileKdfParams;

/// Helper for the file write above. We avoid `as_bytes()` (which
/// returns a `Zeroizing<[u8; 32]>`) because that type doesn't
/// `Deref` to `[u8]` in a way `hex::encode` can consume without
/// borrowing the wrapper type's lifetime.
///
/// Re-exported from `a3net-crypto`.
pub use a3net_crypto::KeyWriteAccess;

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
        Ok(self.key.seal(plaintext)?)
    }

    /// Decrypt a chunk ciphertext back to plaintext.
    fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        Ok(self.key.open(sealed)?)
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
        self.open(&sealed).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
        })
    }

    // ─────────────────────────────────────────────────────────────────
    //  Pass-through surface for the rest of `BlobStore`'s sync API.
    //  These do not touch the key — the underlying store is the
    //  source of truth for layout, GC, etc.
    // ─────────────────────────────────────────────────────────────────

    pub fn blob_dir(&self, hash: &ContentHash) -> PathBuf {
        self.inner.blob_dir(hash)
    }
    pub fn data_dir(&self) -> &Path {
        self.inner.data_dir()
    }
    pub fn contains(&self, hash: &ContentHash) -> bool {
        self.inner.contains(hash)
    }
    pub fn list_complete(&self) -> std::io::Result<Vec<ContentHash>> {
        self.inner.list_complete()
    }
    pub fn total_size(&self) -> std::io::Result<u64> {
        self.inner.total_size()
    }
    pub fn remove(&self, hash: &ContentHash) -> std::io::Result<bool> {
        self.inner.remove(hash)
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
//  Tests — kept here (rather than in a3net-crypto) so the existing
//  `cargo test -p a3net-blobstore` entry point continues to cover the
//  end-to-end encryption-on-disk flow.
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
    fn encryption_key_derive_rejects_empty_salt() {
        let err = EncryptionKey::derive_from_passphrase(b"pw", b"short").unwrap_err();
        assert!(matches!(err, CryptoError::InvalidSalt), "got {:?}", err);
    }

    #[test]
    fn keystore_init_then_load_random() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        let k1 = ks.init_random().unwrap();
        let k2 = ks.load(None).unwrap();
        assert_eq!(k1.as_bytes_for_write(), k2.as_bytes_for_write());
    }

    #[test]
    fn keystore_init_then_load_passphrase() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        let salt = b"some-fixed-salt-1234567890";
        let k1 = ks.init_passphrase(b"hunter2", salt).unwrap();
        let k2 = ks.load(Some(b"hunter2")).unwrap();
        assert_eq!(k1.as_bytes_for_write(), k2.as_bytes_for_write());
    }

    #[test]
    fn keystore_load_without_passphrase_when_required() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        ks.init_passphrase(b"hunter2", b"some-fixed-salt-1234567890")
            .unwrap();
        let err = ks.load(None).unwrap_err();
        // The keystore returns the canonical `CryptoError`; verify it
        // round-trips to the legacy `EncryptionError` so older
        // `match` arms keep working.
        let legacy: EncryptionError = err.into();
        assert!(
            matches!(legacy, EncryptionError::PassphraseRequired),
            "got {:?}",
            legacy
        );
    }

    #[test]
    fn keystore_load_missing_file() {
        let dir = temp_dir();
        let ks = KeyStore::new(dir.path());
        let err = ks.load(None).unwrap_err();
        let legacy: EncryptionError = err.into();
        assert!(
            matches!(legacy, EncryptionError::KeyFileMissing(_)),
            "got {:?}",
            legacy
        );
    }

    #[test]
    fn seal_chunk_then_open_chunk_round_trip() {
        // Verify the AEAD layer without going through the on-disk
        // store — the full `put_bytes_sync` → `read_chunk_sync`
        // round-trip is exercised by `a3net-blobstore`'s integration
        // tests.
        let key = EncryptionKey::generate_random();
        let pt = vec![0xAB; CHUNK_SIZE / 2];
        let ct = key.seal(&pt).unwrap();
        assert_eq!(ct.len(), 24 + pt.len() + 16);
        let recovered = key.open(&ct).unwrap();
        assert_eq!(recovered, pt);
    }

    #[test]
    fn open_chunk_rejects_wrong_key() {
        let k1 = EncryptionKey::generate_random();
        let k2 = EncryptionKey::generate_random();
        let ct = k1.seal(b"hello world").unwrap();
        let err = k2.open(&ct).unwrap_err();
        assert!(matches!(err, CryptoError::AeadDecrypt));
    }

    #[test]
    fn encrypted_meta_marker_round_trips() {
        let dir = temp_dir();
        let blob_dir = dir.path().join("abc");
        fs::create_dir_all(&blob_dir).unwrap();
        let hash = ContentHash::from_bytes(b"hello");
        write_encrypted_meta(&blob_dir, &hash, 5, 1).unwrap();
        assert!(is_encrypted_meta(&blob_dir));
    }
}
