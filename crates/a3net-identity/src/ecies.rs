//! Low-level ECIES-style static encryption.
//!
//! One call to `encrypt` produces a fresh ephemeral X25519 keypair, performs
//! ECDH with the recipient's static X25519 public key, derives a 32-byte
//! AES-256 key via HKDF-SHA256, and seals the plaintext under AES-256-GCM.
//!
//! The wire layout is:
//!
//! ```text
//! EciesCiphertext {
//!     ephemeral_pub: [u8; 32],     // sender's one-shot X25519 public key
//!     nonce:          [u8; 12],     // AES-GCM nonce
//!     ciphertext:     Vec<u8>,      // ciphertext || 16-byte GCM tag
//! }
//! ```
//!
//! This is the standard "ephemeral ECDH + AEAD" construction used by
//! age, Signal sealed-sender, MLS, and others. It's *not* a ratchet; every
//! message uses a fresh ephemeral key, but the recipient's static key is
//! fixed. Forward secrecy *within a single message* holds (the ephemeral
//! secret is never stored), but a long-term compromise of the recipient's
//! static key breaks all past messages. That's intentional and matches the
//! "static encryption" promise of this crate.

use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::error::{IdentityError, Result};
use crate::x25519::{X25519PublicKey, X25519SecretKey};

/// HKDF info string — `b"a3net-ecies/v1"`. Versioned so we can change the
/// KDF without breaking already-deployed envelopes.
const HKDF_INFO: &[u8] = b"a3net-ecies/v1";

/// Result of a `encrypt` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EciesCiphertext {
    pub ephemeral_pub: [u8; 32],
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

/// Symmetric re-export of the recipient's public key. We keep the type
/// distinct to avoid accidents where a wallet pubkey is fed into ECIES.
pub type EciesPublicKey = X25519PublicKey;

/// Symmetric re-export of the sender's secret key.
pub type EciesSecretKey = X25519SecretKey;

/// Errors specific to ECIES. Currently we just reuse [`IdentityError`]
/// variants but this gives us a typed hook for future ECIES-only failures.
pub type EciesError = IdentityError;

/// Encrypt `plaintext` to `recipient`. Returns a fresh [`EciesCiphertext`]
/// with a fresh ephemeral key + nonce.
pub fn encrypt(recipient: &EciesPublicKey, plaintext: &[u8]) -> Result<EciesCiphertext> {
    let ephemeral = EciesSecretKey::generate();
    let ephemeral_pub = ephemeral.public_key();

    let shared = ephemeral.diffie_hellman(recipient);
    let key = derive_key(&shared)?;

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce_bytes = Aes256Gcm::generate_nonce(&mut OsRng);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| IdentityError::Aead(e.to_string()))?;

    Ok(EciesCiphertext {
        ephemeral_pub: ephemeral_pub.to_bytes(),
        nonce: nonce_bytes.into(),
        ciphertext,
    })
}

/// Decrypt a [`EciesCiphertext`] using the recipient's static secret key.
pub fn decrypt(recipient: &EciesSecretKey, ct: &EciesCiphertext) -> Result<Vec<u8>> {
    let ephemeral_pub = EciesPublicKey::from_bytes(&ct.ephemeral_pub)?;
    let shared = recipient.diffie_hellman(&ephemeral_pub);
    let key = derive_key(&shared)?;

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(&ct.nonce);
    cipher
        .decrypt(nonce, ct.ciphertext.as_ref())
        .map_err(|e| IdentityError::Aead(e.to_string()))
}

fn derive_key(shared: &[u8; 32]) -> Result<[u8; 32]> {
    let hkdf = Hkdf::<Sha256>::new(None, shared);
    let mut okm = [0u8; 32];
    hkdf.expand(HKDF_INFO, &mut okm)
        .map_err(|e| IdentityError::Hkdf(e.to_string()))?;
    Ok(okm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let recipient = EciesSecretKey::generate();
        let plaintext = b"the quick brown fox jumps over the lazy dog";
        let ct = encrypt(&recipient.public_key(), plaintext).unwrap();
        let recovered = decrypt(&recipient, &ct).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn ciphertext_is_not_deterministic() {
        let recipient = EciesSecretKey::generate();
        let a = encrypt(&recipient.public_key(), b"same plaintext").unwrap();
        let b = encrypt(&recipient.public_key(), b"same plaintext").unwrap();
        // Ephemeral keys + nonces must vary.
        assert_ne!(a.ephemeral_pub, b.ephemeral_pub);
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn wrong_recipient_fails() {
        let r1 = EciesSecretKey::generate();
        let r2 = EciesSecretKey::generate();
        let ct = encrypt(&r1.public_key(), b"secret").unwrap();
        assert!(decrypt(&r2, &ct).is_err());
    }
}
