//! [`EncryptionKey`] — a 32-byte AEAD key with `Zeroize` on drop.
//!
//! This is the canonical key type across A3Net. Every higher-level
//! crypto surface (`EncryptedBlobStore`, `KeyStore`, future
//! `KeyProvider` implementations) is built on top of this type.
//!
//! ## Why a single byte array?
//!
//! * One allocation, one Drop, one wipe.
//! * `Zeroize` on `Drop` is enforced by the compiler and visible in
//!   the type's `Debug` impl (which intentionally omits the bytes).
//! * We never need public access to the raw bytes except via the
//!   [`KeyWriteAccess`] trait (kept narrow so it can be audited).

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use zeroize::Zeroize;

use crate::error::{CryptoError, CryptoResult};
use crate::kdf::derive_argon2id;

/// XChaCha20-Poly1305 nonce + tag overhead.
///
/// `24` bytes of nonce + `16` bytes of Poly1305 tag = 40 bytes are
/// prepended / appended to every chunk on disk.
pub const AEAD_OVERHEAD: usize = 24 + 16;

/// 32-byte master key used with XChaCha20-Poly1305.
///
/// `Clone` is implemented so that the `EncryptedBlobStore` wrapper
/// (which holds the key alongside the wrapped `BlobStore`) can hand
/// the key to per-chunk seal/open calls without going through a
/// reference. The cloned key is wiped on its own `Drop`.
#[derive(Clone)]
pub struct EncryptionKey {
    bytes: [u8; 32],
}

impl EncryptionKey {
    /// Random 32-byte key from the OS RNG.
    pub fn generate_random() -> Self {
        use rand::RngCore;
        let mut rng = rand::rngs::OsRng;
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Wrap 32 raw bytes. Returns `Err` if the slice is the wrong
    /// size — never panics on malformed input.
    pub fn from_bytes(b: &[u8]) -> CryptoResult<Self> {
        if b.len() != 32 {
            return Err(CryptoError::InvalidKeyLength(b.len()));
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(b);
        Ok(Self { bytes })
    }

    /// Derive a 32-byte key from a passphrase + salt using Argon2id.
    /// See [`crate::kdf`] for the parameter choice.
    pub fn derive_from_passphrase(passphrase: &[u8], salt: &[u8]) -> CryptoResult<Self> {
        let bytes = derive_argon2id(passphrase, salt)?;
        Ok(Self { bytes })
    }

    /// Build a fresh XChaCha20-Poly1305 AEAD instance bound to this
    /// key. Cheap — `XChaCha20Poly1305::new` is just a key schedule.
    pub(crate) fn aead(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(Key::from_slice(&self.bytes))
    }

    /// Seal an arbitrary plaintext with a random 24-byte nonce.
    ///
    /// Returns `nonce(24) || ciphertext || tag(16)` suitable for
    /// writing to disk as a self-contained chunk.
    pub fn seal(&self, plaintext: &[u8]) -> CryptoResult<Vec<u8>> {
        use rand::RngCore;
        let mut rng = rand::rngs::OsRng;
        let mut nonce_bytes = [0u8; 24];
        rng.fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);
        let ct = self
            .aead()
            .encrypt(nonce, plaintext)
            .map_err(|_| CryptoError::AeadEncrypt)?;
        let mut out = Vec::with_capacity(AEAD_OVERHEAD + ct.len());
        out.extend_from_slice(nonce.as_slice());
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Open a sealed chunk (`nonce(24) || ciphertext || tag(16)`).
    /// Returns [`CryptoError::CiphertextTooShort`] or
    /// [`CryptoError::AeadDecrypt`] on bad input.
    pub fn open(&self, sealed: &[u8]) -> CryptoResult<Vec<u8>> {
        if sealed.len() < AEAD_OVERHEAD {
            return Err(CryptoError::CiphertextTooShort {
                got: sealed.len(),
                need: AEAD_OVERHEAD,
            });
        }
        let (nonce_bytes, ciphertext) = sealed.split_at(24);
        let nonce = XNonce::from_slice(nonce_bytes);
        let pt = self
            .aead()
            .decrypt(nonce, ciphertext)
            .map_err(|_| CryptoError::AeadDecrypt)?;
        Ok(pt)
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

/// Narrow accessor used by [`crate::store::KeyStore`] when
/// serialising the key to the JSON `KeyFile`. The trait is
/// intentionally minimal so that "give me the raw bytes" is only
/// possible at the persistence boundary — not on every `Debug`.
pub trait KeyWriteAccess {
    fn as_bytes_for_write(&self) -> [u8; 32];

    /// Public name used by tests / CLI status code.
    fn as_bytes_for_test(&self) -> [u8; 32] {
        self.as_bytes_for_write()
    }
}

impl KeyWriteAccess for EncryptionKey {
    fn as_bytes_for_write(&self) -> [u8; 32] {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_bytes() {
        let k = EncryptionKey::generate_random();
        let raw = k.as_bytes_for_write();
        let restored = EncryptionKey::from_bytes(&raw).unwrap();
        assert_eq!(restored.as_bytes_for_write().as_slice(), raw.as_slice());
    }

    #[test]
    fn from_bytes_rejects_wrong_size() {
        assert!(EncryptionKey::from_bytes(&[]).is_err());
        assert!(EncryptionKey::from_bytes(&[0u8; 16]).is_err());
        assert!(EncryptionKey::from_bytes(&[0u8; 64]).is_err());
        assert!(EncryptionKey::from_bytes(&[0u8; 32]).is_ok());
    }

    #[test]
    fn seal_open_roundtrip() {
        let k = EncryptionKey::generate_random();
        let ct = k.seal(b"hello world").unwrap();
        // Header + plaintext + tag.
        assert_eq!(ct.len(), 24 + b"hello world".len() + 16);
        let pt = k.open(&ct).unwrap();
        assert_eq!(pt, b"hello world");
    }

    #[test]
    fn open_rejects_truncated() {
        let k = EncryptionKey::generate_random();
        let err = k.open(&[0u8; 10]).unwrap_err();
        assert!(matches!(err, CryptoError::CiphertextTooShort { .. }));
    }

    #[test]
    fn open_rejects_wrong_key() {
        let k1 = EncryptionKey::generate_random();
        let k2 = EncryptionKey::generate_random();
        let ct = k1.seal(b"hello").unwrap();
        let err = k2.open(&ct).unwrap_err();
        assert!(matches!(err, CryptoError::AeadDecrypt));
    }

    #[test]
    fn debug_does_not_leak_key_material() {
        let k = EncryptionKey::generate_random();
        let s = format!("{:?}", k);
        assert!(!s.contains(&hex::encode(k.as_bytes_for_write())));
    }
}
