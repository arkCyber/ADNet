//! secp256k1 wallet — EVM-compatible signing key.
//!
//! A wallet is just a 32-byte secret key. We deliberately do *not* wrap it in
//! any HD-derivation type; load your key from raw bytes (a file, an env var,
//! a memory-mapped HSM blob, etc.) and use [`Wallet::sign_personal`] to
//! produce an EIP-191 signature.

use std::fmt;

use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use tiny_keccak::Hasher as _;
use zeroize::{Zeroize, Zeroizing};

use crate::address::Address;
use crate::error::{IdentityError, Result};
use crate::signing::{EIP191_PREFIX, PERSONAL_SIGN_VERSION, PersonalSignature};

/// A signing-capable wallet (holds the secret key).
///
/// Internally the wallet stores the **raw 32-byte scalar** inside a
/// `Zeroizing<[u8;32]>`. Every signing operation reconstructs a
/// `secp256k1::SecretKey` on the stack (cheap), uses it once, and
/// drops it. The wallet itself is wiped on `Drop`.
///
/// We deliberately avoid keeping a long-lived `SecretKey` because
/// `SecretKey` is not `Zeroize` — the only way to wipe it is to
/// not keep it.
#[derive(Clone)]
pub struct Wallet {
    secret: Zeroizing<[u8; 32]>,
    public: WalletPublic,
}

impl Wallet {
    /// Generate a new random wallet using the OS RNG.
    pub fn generate() -> Self {
        let secp = Secp256k1::new();
        let (sk, _) = secp.generate_keypair(&mut rand::thread_rng());
        let public = WalletPublic::from_secret(&sk)
            .expect("valid secret just generated");
        let secret = Zeroizing::new(sk.secret_bytes());
        Self { secret, public }
    }

    /// Load from a 32-byte secret. Returns an error if the value is zero or
    /// out of range.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 32 {
            return Err(IdentityError::InvalidSecretKey(format!(
                "expected 32 bytes, got {}",
                bytes.len()
            )));
        }
        // Validate via `SecretKey::from_slice` so we never accept a
        // scalar outside the curve order.
        let sk = SecretKey::from_slice(bytes)
            .map_err(|e| IdentityError::InvalidSecretKey(e.to_string()))?;
        let public = WalletPublic::from_secret(&sk)?;
        let mut buf = [0u8; 32];
        buf.copy_from_slice(bytes);
        Ok(Self {
            secret: Zeroizing::new(buf),
            public,
        })
    }

    /// Borrow the 32-byte big-endian secret key.
    pub fn secret_bytes(&self) -> [u8; 32] {
        *self.secret
    }

    /// Borrow the public half.
    pub fn public(&self) -> &WalletPublic {
        &self.public
    }

    /// EIP-191 `personal_sign` of the given 32-byte digest.
    ///
    /// We **pre-hash** outside this crate and sign the resulting 32 bytes
    /// inside the EIP-191 wrapper. Callers are responsible for choosing the
    /// digest (sha256, blake3, keccak256, …); A3Net's standard policy is
    /// "EIP-191 over whatever the caller hashed" so the prefix is fixed and
    /// `recover_personal` recovers the same signer regardless of hash choice.
    pub fn sign_personal(&self, digest_32: &[u8; 32]) -> Result<PersonalSignature> {
        if digest_32.len() != 32 {
            return Err(IdentityError::InvalidSignature(
                "personal_sign expects a 32-byte digest".into(),
            ));
        }
        // Reconstruct a `SecretKey` on the stack from the wiped-on-drop
        // scalar buffer; the `SecretKey` itself is dropped at fn-end.
        let sk = SecretKey::from_slice(self.secret.as_ref())
            .map_err(|e| IdentityError::InvalidSecretKey(e.to_string()))?;

        let mut prefixed = Vec::with_capacity(EIP191_PREFIX.len() + 26 + 32);
        prefixed.extend_from_slice(EIP191_PREFIX);
        // Length encoded as decimal ASCII. EIP-191 spec: `\x19Ethereum Signed Message:\n<len><data>`.
        let len_buf = small_uint_to_decimal(32);
        prefixed.extend_from_slice(len_buf.as_bytes());
        prefixed.extend_from_slice(digest_32);

        // keccak256(prefixed) → sign.
        let mut keccak = tiny_keccak::Keccak::v256();
        keccak.update(&prefixed);
        let mut hash = [0u8; 32];
        keccak.finalize(&mut hash);

        let msg = Message::from_digest(hash);
        let secp = Secp256k1::signing_only();
        let recoverable = secp.sign_ecdsa_recoverable(&msg, &sk);
        let (recovery_id, compact) = recoverable.serialize_compact();
        // Copy `r` and `s` out of the compact bytes *before* wiping
        // them. The compact array is a 64-byte stack copy that we
        // own and can zero out before the function returns.
        let r: [u8; 32] = compact[..32].try_into().unwrap();
        let s: [u8; 32] = compact[32..].try_into().unwrap();
        let mut compact = compact;
        compact.zeroize();
        Ok(PersonalSignature {
            r,
            s,
            v: PERSONAL_SIGN_VERSION + recovery_id.to_i32() as u8,
        })
    }
}

impl fmt::Debug for Wallet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Wallet")
            .field("address", &self.public.address.to_checksum())
            .field("pubkey_hex", &self.public.public_key_hex())
            .finish()
    }
}

impl Drop for Wallet {
    fn drop(&mut self) {
        // `Zeroizing<[u8;32]>` already wipes on `Drop`, but the
        // explicit zero here makes the intent visible in source.
        self.secret.zeroize();
    }
}

/// Public half of a wallet — shareable, used for verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletPublic {
    pubkey: PublicKey,
    address: Address,
}

impl WalletPublic {
    pub(crate) fn from_secret(secret: &SecretKey) -> Result<Self> {
        let secp = Secp256k1::new();
        let pubkey = PublicKey::from_secret_key(&secp, secret);
        let address = derive_address(&pubkey);
        Ok(Self { pubkey, address })
    }

    /// Reconstruct from a 33-byte compressed SEC pubkey.
    pub fn from_compressed(bytes: &[u8]) -> Result<Self> {
        let pubkey = PublicKey::from_slice(bytes)
            .map_err(|e| IdentityError::InvalidPublicKey(e.to_string()))?;
        let address = derive_address(&pubkey);
        Ok(Self { pubkey, address })
    }

    /// Recover an A3Net wallet public key from a personal_sign signature.
    ///
    /// Returns [`IdentityError::InvalidSignature`] if no valid signer exists
    /// for the given inputs.
    pub fn recover_personal(digest_32: &[u8; 32], sig: &PersonalSignature) -> Result<Self> {
        let mut prefixed = Vec::with_capacity(EIP191_PREFIX.len() + 26 + 32);
        prefixed.extend_from_slice(EIP191_PREFIX);
        let len_buf = small_uint_to_decimal(32);
        prefixed.extend_from_slice(len_buf.as_bytes());
        prefixed.extend_from_slice(digest_32);

        let mut keccak = tiny_keccak::Keccak::v256();
        keccak.update(&prefixed);
        let mut hash = [0u8; 32];
        keccak.finalize(&mut hash);

        let mut sig_bytes = [0u8; 64];
        sig_bytes[..32].copy_from_slice(&sig.r);
        sig_bytes[32..].copy_from_slice(&sig.s);
        // EIP-191 v = 27 + raw_recovery_id. We accept any of 27 or 28 from
        // external signers but our own `sign_personal` always emits 27 or 28.
        let raw_id = if sig.v == 27 {
            0
        } else if sig.v == 28 {
            1
        } else {
            return Err(IdentityError::InvalidSignature(format!(
                "v must be 27 or 28, got {}",
                sig.v
            )));
        };
        let recoverable = secp256k1::ecdsa::RecoverableSignature::from_compact(
            &sig_bytes,
            secp256k1::ecdsa::RecoveryId::from_i32(raw_id)
                .map_err(|e| IdentityError::InvalidSignature(e.to_string()))?,
        )
        .map_err(|e| IdentityError::InvalidSignature(e.to_string()))?;
        let msg = Message::from_digest(hash);
        let secp = Secp256k1::new();
        let pubkey = secp
            .recover_ecdsa(&msg, &recoverable)
            .map_err(|e| IdentityError::InvalidSignature(e.to_string()))?;
        let address = derive_address(&pubkey);
        Ok(Self { pubkey, address })
    }

    pub fn address(&self) -> Address {
        self.address
    }

    /// 33-byte compressed SEC encoding.
    pub fn public_key_bytes(&self) -> [u8; 33] {
        self.pubkey.serialize()
    }

    /// 33-byte compressed SEC as lowercase hex.
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.pubkey.serialize())
    }

    /// 64-byte uncompressed X||Y (no 0x04 prefix).
    pub fn public_key_xy(&self) -> [u8; 64] {
        let uncompressed = self.pubkey.serialize_uncompressed();
        let mut out = [0u8; 64];
        out.copy_from_slice(&uncompressed[1..]);
        out
    }
}

/// Address derivation: `keccak256(uncompressed_pubkey[1..])[12..32]`.
fn derive_address(pubkey: &PublicKey) -> Address {
    let uncompressed = pubkey.serialize_uncompressed();
    let mut hasher = tiny_keccak::Keccak::v256();
    hasher.update(&uncompressed[1..]);
    let mut hash = [0u8; 32];
    hasher.finalize(&mut hash);
    Address::from_bytes(hash[12..32].try_into().expect("sha3 output is 32 bytes"))
}

/// ASCII decimal of a small unsigned integer (we only need it for `32`).
fn small_uint_to_decimal(n: usize) -> String {
    // For n < 1000 (we only call this with 32) this is overkill, but it keeps
    // the implementation self-contained and obviously correct.
    let mut out = String::new();
    let mut v = n;
    if v == 0 {
        return "0".to_string();
    }
    while v > 0 {
        out.insert(0, (b'0' + (v % 10) as u8) as char);
        v /= 10;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_recover_round_trip() {
        let w = Wallet::generate();
        let digest = blake3_test_digest(b"hello");
        let sig = w.sign_personal(&digest).unwrap();
        let recovered = WalletPublic::recover_personal(&digest, &sig).unwrap();
        assert_eq!(w.public().address(), recovered.address());
    }

    #[test]
    fn compressed_round_trip() {
        let w = Wallet::generate();
        let pk = w.public().public_key_bytes();
        let decoded = WalletPublic::from_compressed(&pk).unwrap();
        assert_eq!(w.public().address(), decoded.address());
    }

    fn blake3_test_digest(data: &[u8]) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(data);
        let out = h.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(out.as_bytes());
        bytes
    }
}
