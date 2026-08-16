//! KEK derivation + cross-device encrypted bundle.
//!
//! When a user logs into a new device, they need:
//!
//! - their long-term identity key (Ed25519/X25519)
//! - their active Sender Keys for every group they're in
//! - per-device metadata (display name, avatar hash)
//!
//! The old device encrypts all of that with a KEK derived from the
//! user's chosen password + a per-bundle salt, and emits a QR code
//! or copy-pasteable base64. The new device scans + decrypts.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{CryptoError, CryptoResult};
use crate::random::random_salt_16;

/// Argon2id parameters — `t = 2, m = 64 MiB, p = 1`.
/// Same shape as `a3net-crypto::kdf::DEFAULT_ARGON2_PARAMS` but
/// explicitly pinned here so a3chat's KDF settings stay stable
/// independent of any future `a3net-crypto` tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub time_cost: u32,
    pub memory_kib: u32,
    pub parallelism: u32,
}

impl KdfParams {
    pub const DEFAULT: Self = Self {
        time_cost: 2,
        memory_kib: 64 * 1024, // 64 MiB
        parallelism: 1,
    };

    pub fn to_argon2(&self) -> CryptoResult<Argon2<'_>> {
        let params = Params::new(self.memory_kib, self.time_cost, self.parallelism, Some(32))
            .map_err(|e| CryptoError::Argon2(e.to_string()))?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }
}

impl Default for KdfParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Derive a 32-byte KEK from `password` + `salt` using Argon2id.
pub fn derive_kek(password: &[u8], salt: &[u8], params: KdfParams) -> CryptoResult<[u8; 32]> {
    if salt.len() != 16 {
        return Err(CryptoError::InvalidLength {
            field: "salt",
            expected: 16,
            actual: salt.len(),
        });
    }
    let argon = params.to_argon2()?;
    let mut kek = [0u8; 32];
    argon
        .hash_password_into(password, salt, &mut kek)
        .map_err(|e| CryptoError::Argon2(e.to_string()))?;
    Ok(kek)
}

/// Encrypted bundle — what the old device writes to the QR code or
/// file, what the new device reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedBundle {
    /// Version byte — bumped on wire-format breaks.
    pub version: u8,
    /// Hex-encoded Argon2id salt (16 bytes).
    pub salt: String,
    /// Hex-encoded random nonce (12 bytes).
    pub nonce: String,
    /// Base64-encoded ciphertext (includes 16-byte AEAD tag).
    pub ciphertext: String,
    /// KDF params, so the new device can re-derive.
    pub kdf_params: KdfParams,
    /// Device-id of the source device (for display).
    pub source_device_id: String,
    /// Unix seconds when the bundle was created.
    pub created_at: i64,
}

impl EncryptedBundle {
    pub const CURRENT_VERSION: u8 = 1;

    pub fn to_base64(&self) -> CryptoResult<String> {
        let bytes = serde_json::to_vec(self).map_err(|e| CryptoError::Internal(e.to_string()))?;
        Ok(BASE64.encode(bytes))
    }
    pub fn from_base64(s: &str) -> CryptoResult<Self> {
        let bytes = BASE64
            .decode(s)
            .map_err(|e| CryptoError::Base64Decode(e.to_string()))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| CryptoError::Internal(format!("decode bundle: {e}")))
    }
}

/// Plaintext payload that goes inside an [`EncryptedBundle`]. The
/// fields are application-specific; this type is the canonical
/// "full device export" payload used when QR-ing a new device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct BundlePayload {
    /// Hex-encoded Ed25519 secret seed (32 bytes).
    pub identity_seed: String,
    /// Hex-encoded Sender Key chain keys — one per group. The chain
    /// keys are secret material but `BundleSenderKey` does not
    /// implement `Zeroize` directly (it carries non-secret fields
    /// like the conversation id), so we mark the whole vec as
    /// skipped here and rely on its callers to zeroize individual
    /// chain keys before the bundle is built.
    #[zeroize(skip)]
    pub sender_keys: Vec<BundleSenderKey>,
    /// User display name.
    pub display_name: String,
    /// Hex-encoded BLAKE3 hash of avatar blob.
    pub avatar_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSenderKey {
    pub conversation_id: String,
    pub sender_key_id_hex: String,
    pub chain_key_hex: String,
    pub iteration: u32,
}

/// Encrypt `payload` with `password` and return the [`EncryptedBundle`].
pub fn encrypt_bundle(
    payload: &BundlePayload,
    password: &[u8],
    source_device_id: impl Into<String>,
) -> CryptoResult<EncryptedBundle> {
    let params = KdfParams::default();
    let salt = random_salt_16();
    let mut kek = derive_kek(password, &salt, params)?;
    let plaintext = serde_json::to_vec(payload)
        .map_err(|e| CryptoError::Internal(format!("encode payload: {e}")))?;
    // Build the device-id string once so we use the same value for
    // both the AEAD AAD and the bundle header.
    let device_id: String = source_device_id.into();
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&kek));
    let nonce_bytes = crate::random::random_nonce();
    let ct = cipher
        .encrypt(
            chacha20poly1305::aead::generic_array::GenericArray::from_slice(&nonce_bytes),
            Payload {
                msg: &plaintext,
                aad: device_id.as_bytes(),
            },
        )
        .map_err(|_| CryptoError::AeadTagMismatch)?;
    kek.zeroize();
    let bundle = EncryptedBundle {
        version: EncryptedBundle::CURRENT_VERSION,
        salt: hex::encode(salt),
        nonce: hex::encode(nonce_bytes),
        ciphertext: BASE64.encode(&ct),
        kdf_params: params,
        source_device_id: device_id,
        created_at: chrono::Utc::now().timestamp(),
    };
    Ok(bundle)
}

/// Decrypt a bundle. Returns the plaintext payload.
pub fn decrypt_bundle(bundle: &EncryptedBundle, password: &[u8]) -> CryptoResult<BundlePayload> {
    if bundle.version != EncryptedBundle::CURRENT_VERSION {
        return Err(CryptoError::Internal(format!(
            "unsupported bundle version {} (expected {})",
            bundle.version,
            EncryptedBundle::CURRENT_VERSION
        )));
    }
    let salt =
        hex::decode(&bundle.salt).map_err(|e| CryptoError::HexDecode(format!("salt: {e}")))?;
    let nonce =
        hex::decode(&bundle.nonce).map_err(|e| CryptoError::HexDecode(format!("nonce: {e}")))?;
    if nonce.len() != 12 {
        return Err(CryptoError::InvalidLength {
            field: "bundle.nonce",
            expected: 12,
            actual: nonce.len(),
        });
    }
    let ct = BASE64
        .decode(&bundle.ciphertext)
        .map_err(|e| CryptoError::Base64Decode(e.to_string()))?;
    let mut kek = derive_kek(password, &salt, bundle.kdf_params)?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&kek));
    let pt = cipher
        .decrypt(
            chacha20poly1305::aead::generic_array::GenericArray::from_slice(&nonce),
            Payload {
                msg: &ct,
                aad: bundle.source_device_id.as_bytes(),
            },
        )
        .map_err(|_| CryptoError::AeadTagMismatch)?;
    kek.zeroize();
    let payload: BundlePayload = serde_json::from_slice(&pt)
        .map_err(|e| CryptoError::Internal(format!("decode payload: {e}")))?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> BundlePayload {
        BundlePayload {
            identity_seed: hex::encode([7u8; 32]),
            sender_keys: vec![BundleSenderKey {
                conversation_id: "grp:abc".into(),
                sender_key_id_hex: hex::encode([1u8; 16]),
                chain_key_hex: hex::encode([2u8; 32]),
                iteration: 3,
            }],
            display_name: "Alice".into(),
            avatar_hash: Some(hex::encode([3u8; 32])),
        }
    }

    #[test]
    fn derive_kek_is_deterministic_for_same_input() {
        let salt = [1u8; 16];
        let k1 = derive_kek(b"correct horse battery staple", &salt, KdfParams::default()).unwrap();
        let k2 = derive_kek(b"correct horse battery staple", &salt, KdfParams::default()).unwrap();
        assert_eq!(k1, k2);
    }

    #[test]
    fn derive_kek_differs_with_different_salt() {
        let salt1 = [1u8; 16];
        let salt2 = [2u8; 16];
        let k1 = derive_kek(b"password", &salt1, KdfParams::default()).unwrap();
        let k2 = derive_kek(b"password", &salt2, KdfParams::default()).unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn derive_kek_rejects_wrong_salt_length() {
        let r = derive_kek(b"p", &[0u8; 8], KdfParams::default());
        assert!(matches!(r, Err(CryptoError::InvalidLength { .. })));
    }

    #[test]
    fn bundle_encrypt_decrypt_round_trip() {
        let payload = sample_payload();
        let bundle = encrypt_bundle(&payload, b"strong-password", "dev-1").unwrap();
        assert_eq!(bundle.version, EncryptedBundle::CURRENT_VERSION);
        assert_eq!(bundle.salt.len(), 32); // 16 bytes hex
        assert_eq!(bundle.nonce.len(), 24); // 12 bytes hex
        let decoded = decrypt_bundle(&bundle, b"strong-password").unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn bundle_rejects_wrong_password() {
        let payload = sample_payload();
        let bundle = encrypt_bundle(&payload, b"strong-password", "dev-1").unwrap();
        let result = decrypt_bundle(&bundle, b"wrong-password");
        assert!(matches!(result, Err(CryptoError::AeadTagMismatch)));
    }

    #[test]
    fn bundle_base64_round_trip() {
        let payload = sample_payload();
        let bundle = encrypt_bundle(&payload, b"pw", "dev-x").unwrap();
        let b64 = bundle.to_base64().unwrap();
        let restored = EncryptedBundle::from_base64(&b64).unwrap();
        assert_eq!(restored.ciphertext, bundle.ciphertext);
    }

    #[test]
    fn bundle_rejects_unsupported_version() {
        let mut bundle = encrypt_bundle(&sample_payload(), b"pw", "dev").unwrap();
        bundle.version = 99;
        let r = decrypt_bundle(&bundle, b"pw");
        assert!(matches!(r, Err(CryptoError::Internal(_))));
    }

    #[test]
    fn bundle_rejects_short_monnce() {
        let payload = sample_payload();
        let mut bundle = encrypt_bundle(&payload, b"pw", "dev").unwrap();
        // "abcd" is 4 hex chars = 2 bytes — well-formed hex but the
        // wrong length for a 12-byte nonce.
        bundle.nonce = "abcd".into();
        let r = decrypt_bundle(&bundle, b"pw");
        assert!(
            matches!(
                r,
                Err(CryptoError::HexDecode(_)) | Err(CryptoError::InvalidLength { .. })
            ),
            "expected hex-decode or length error, got {r:?}"
        );
    }

    #[test]
    fn bundle_rejects_invalid_hex_nonce() {
        let payload = sample_payload();
        let mut bundle = encrypt_bundle(&payload, b"pw", "dev").unwrap();
        // 24 chars but not all hex.
        bundle.nonce = "z".repeat(24);
        let r = decrypt_bundle(&bundle, b"pw");
        assert!(matches!(r, Err(CryptoError::HexDecode(_))));
    }
}
