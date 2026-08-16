//! X25519 static keys. Used *only* for the [`crate::ecies`] envelope —
//! **not** for signing. Keep these distinct from the wallet's secp256k1 key.

use rand::RngCore;
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret as X25519StaticSecret};
use zeroize::{Zeroize, Zeroizing};

use crate::error::{IdentityError, Result};

/// 32-byte X25519 secret key. Wrapped in `Zeroizing<[u8;32]>` so that
/// every `Drop` (including panics) wipes the underlying bytes.
#[derive(Clone)]
pub struct X25519SecretKey(Zeroizing<[u8; 32]>);

impl X25519SecretKey {
    /// Generate a fresh key from the OS RNG.
    pub fn generate() -> Self {
        let mut bytes = Zeroizing::new([0u8; 32]);
        rand::thread_rng().fill_bytes(bytes.as_mut_slice());
        Self(bytes)
    }

    /// Load from 32 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 32 {
            return Err(IdentityError::InvalidX25519Key(format!(
                "expected 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut out = Zeroizing::new([0u8; 32]);
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        *self.0
    }

    /// Derive the matching public key.
    pub fn public_key(&self) -> X25519PublicKey {
        let secret = X25519StaticSecret::from(*self.0);
        X25519PublicKey(X25519Public::from(&secret))
    }

    /// Compute the shared secret with the given peer public key.
    pub fn diffie_hellman(&self, peer: &X25519PublicKey) -> [u8; 32] {
        let secret = X25519StaticSecret::from(*self.0);
        *secret.diffie_hellman(&peer.0).as_bytes()
    }
}

impl Zeroize for X25519SecretKey {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl Drop for X25519SecretKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for X25519SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X25519SecretKey")
            .field("public", &self.public_key().to_hex())
            .finish()
    }
}

/// 32-byte X25519 public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct X25519PublicKey(#[serde(with = "x25519_serde")] pub(crate) X25519Public);

impl X25519PublicKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 32 {
            return Err(IdentityError::InvalidX25519Key(format!(
                "expected 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        Ok(Self(X25519Public::from(arr)))
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0.to_bytes())
    }
}

impl From<[u8; 32]> for X25519PublicKey {
    fn from(b: [u8; 32]) -> Self {
        Self(X25519Public::from(b))
    }
}

mod x25519_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use x25519_dalek::PublicKey;

    pub fn serialize<S: Serializer>(pk: &PublicKey, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(pk.as_bytes())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<PublicKey, D::Error> {
        let bytes: [u8; 32] = <[u8; 32]>::deserialize(d)?;
        Ok(PublicKey::from(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dh_is_symmetric() {
        let a = X25519SecretKey::generate();
        let b = X25519SecretKey::generate();
        let a_pub = a.public_key();
        let b_pub = b.public_key();
        assert_eq!(a.diffie_hellman(&b_pub), b.diffie_hellman(&a_pub));
    }

    #[test]
    fn keys_have_consistent_encoding() {
        let s = X25519SecretKey::generate();
        let p = s.public_key();
        assert_eq!(X25519PublicKey::from_bytes(&p.to_bytes()).unwrap(), p);
    }
}
