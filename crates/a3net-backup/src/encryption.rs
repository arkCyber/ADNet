//! Encryption support for backup files.
//!
//! DO-178C SR-7: Cryptographic protection for sensitive backup data.

use std::io::Write;
use std::path::Path;

use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng},
    ChaCha20Poly1305, Nonce,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::info;

/// Error types for encryption operations.
#[derive(Debug, Error)]
pub enum EncryptionError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("encryption failed: {0}")]
    EncryptionFailed(String),
    #[error("decryption failed: {0}")]
    DecryptionFailed(String),
    #[error("invalid key length: expected 32 bytes, got {0}")]
    InvalidKeyLength(usize),
    #[error("ciphertext too short")]
    CiphertextTooShort,
    #[error("authentication failed - data may be tampered")]
    AuthenticationFailed,
}

/// Key derivation function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyDerivation {
    /// Direct key (raw 32 bytes).
    Direct,
    /// Argon2id key derivation.
    Argon2id,
}

impl Default for KeyDerivation {
    fn default() -> Self {
        KeyDerivation::Argon2id
    }
}

/// Encryption key with metadata.
#[derive(Debug, Clone)]
pub struct EncryptionKey {
    /// Raw key bytes (32 bytes for ChaCha20Poly1305).
    key: [u8; 32],
    /// Key derivation method used.
    derivation: KeyDerivation,
    /// Salt used for derivation (if applicable).
    salt: Option<[u8; 16]>,
}

impl EncryptionKey {
    /// Generate a new random encryption key.
    ///
    /// DO-178C SR-7: Keys must be generated using CSRNG.
    pub fn generate() -> Self {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);

        Self {
            key,
            derivation: KeyDerivation::Direct,
            salt: None,
        }
    }

    /// Derive a key from a password using Argon2id.
    ///
    /// DO-178C SR-7: Password-based key derivation.
    pub fn derive_from_password(password: &str, salt: Option<[u8; 16]>) -> Self {
        use argon2::{
            password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
            Argon2,
        };

        let salt = salt.unwrap_or_else(|| {
            let mut s = [0u8; 16];
            OsRng.fill_bytes(&mut s);
            s
        });

        let salt_string = SaltString::encode_b64(&salt)
            .expect("SaltString encoding should not fail");

        let argon2 = Argon2::default();
        let hash = argon2
            .hash_password(password.as_bytes(), &salt_string)
            .expect("Argon2 hashing should not fail");

        let hash_output = hash.hash.expect("Hash output should be present");
        let hash_bytes = hash_output.as_bytes();

        let mut key = [0u8; 32];
        key.copy_from_slice(&hash_bytes[..32]);

        Self {
            key,
            derivation: KeyDerivation::Argon2id,
            salt: Some(salt),
        }
    }

    /// Get the raw key bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.key
    }

    /// Export key as hex string (for storage).
    pub fn to_hex(&self) -> String {
        hex::encode(self.key)
    }

    /// Import key from hex string.
    pub fn from_hex(hex: &str) -> Result<Self, EncryptionError> {
        let bytes = hex::decode(hex)
            .map_err(|e| EncryptionError::EncryptionFailed(format!("Invalid hex: {}", e)))?;

        if bytes.len() != 32 {
            return Err(EncryptionError::InvalidKeyLength(bytes.len()));
        }

        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);

        Ok(Self {
            key,
            derivation: KeyDerivation::Direct,
            salt: None,
        })
    }
}

/// Encrypted backup container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedHeader {
    /// Magic bytes for identification.
    magic: [u8; 8],
    /// Nonce used for encryption (12 bytes).
    nonce: [u8; 12],
    /// Key derivation method.
    derivation: KeyDerivation,
    /// Salt used (if applicable).
    salt: Option<[u8; 16]>,
    /// Original file size.
    original_size: u64,
}

impl EncryptedHeader {
    const MAGIC: [u8; 8] = *b"ADNETENC";

    /// Create a new encrypted backup header.
    fn new(original_size: u64, derivation: KeyDerivation, salt: Option<[u8; 16]>) -> Self {
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);

        Self {
            magic: Self::MAGIC,
            nonce,
            derivation,
            salt,
            original_size,
        }
    }
}

/// Encrypted backup handler.
#[derive(Debug)]
pub struct EncryptedBackup;

impl EncryptedBackup {
    /// Encrypt a file and write to output.
    ///
    /// DO-178C SR-7: Data-at-rest encryption.
    pub fn encrypt_file(
        input: &Path,
        output: &Path,
        key: &EncryptionKey,
    ) -> Result<u64, EncryptionError> {
        let plaintext = std::fs::read(input)?;
        let original_size = plaintext.len() as u64;

        let encrypted = Self::encrypt(&plaintext, key, original_size)?;

        let mut file = std::fs::File::create(output)?;
        file.write_all(&encrypted)?;

        info!(
            input = %input.display(),
            output = %output.display(),
            bytes = original_size,
            "File encrypted"
        );

        Ok(original_size)
    }

    /// Decrypt a file and write to output.
    ///
    /// DO-178C SR-7: Decryption for authorized access.
    pub fn decrypt_file(
        input: &Path,
        output: &Path,
        key: &EncryptionKey,
    ) -> Result<u64, EncryptionError> {
        let ciphertext = std::fs::read(input)?;
        let plaintext = Self::decrypt(&ciphertext, key)?;

        std::fs::write(output, &plaintext)?;

        info!(
            input = %input.display(),
            output = %output.display(),
            bytes = plaintext.len(),
            "File decrypted"
        );

        Ok(plaintext.len() as u64)
    }

    /// Encrypt data in memory.
    ///
    /// DO-178C SR-7: Cryptographic encryption with authentication.
    pub fn encrypt(data: &[u8], key: &EncryptionKey, original_size: u64) -> Result<Vec<u8>, EncryptionError> {
        let cipher = ChaCha20Poly1305::new_from_slice(key.as_bytes())
            .map_err(|e| EncryptionError::EncryptionFailed(e.to_string()))?;

        let header = EncryptedHeader::new(
            original_size,
            key.derivation,
            key.salt,
        );

        let mut nonce = Nonce::from_slice(&header.nonce);
        let ciphertext = cipher
            .encrypt(nonce, data)
            .map_err(|e| EncryptionError::EncryptionFailed(e.to_string()))?;

        // Serialize header
        let header_bytes = serde_json::to_vec(&header)
            .map_err(|e| EncryptionError::EncryptionFailed(e.to_string()))?;

        // Format: header_len (2 bytes) + header + ciphertext
        let header_len = header_bytes.len() as u16;
        let mut result = Vec::with_capacity(2 + header_bytes.len() + ciphertext.len());
        result.extend_from_slice(&header_len.to_le_bytes());
        result.extend_from_slice(&header_bytes);
        result.extend_from_slice(&ciphertext);

        Ok(result)
    }

    /// Decrypt data in memory.
    pub fn decrypt(data: &[u8], key: &EncryptionKey) -> Result<Vec<u8>, EncryptionError> {
        if data.len() < 2 {
            return Err(EncryptionError::CiphertextTooShort);
        }

        // Read header length
        let header_len = u16::from_le_bytes([data[0], data[1]]) as usize;
        if data.len() < 2 + header_len {
            return Err(EncryptionError::CiphertextTooShort);
        }

        // Parse header
        let header_bytes = &data[2..2 + header_len];
        let header: EncryptedHeader = serde_json::from_slice(header_bytes)
            .map_err(|e| EncryptionError::DecryptionFailed(e.to_string()))?;

        // Verify magic
        if header.magic != EncryptedHeader::MAGIC {
            return Err(EncryptionError::DecryptionFailed("Invalid magic bytes".to_string()));
        }

        // Extract ciphertext
        let ciphertext = &data[2 + header_len..];
        if ciphertext.len() < 16 {
            return Err(EncryptionError::CiphertextTooShort);
        }

        // Decrypt
        let cipher = ChaCha20Poly1305::new_from_slice(key.as_bytes())
            .map_err(|e| EncryptionError::DecryptionFailed(e.to_string()))?;

        let mut nonce = Nonce::from_slice(&header.nonce);
        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| EncryptionError::AuthenticationFailed)?;

        Ok(plaintext)
    }

    /// Verify a backup file is properly encrypted and can be decrypted.
    pub fn verify(backup_path: &Path, key: &EncryptionKey) -> Result<bool, EncryptionError> {
        let data = std::fs::read(backup_path)?;
        
        match Self::decrypt(&data, key) {
            Ok(_) => Ok(true),
            Err(EncryptionError::AuthenticationFailed) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_generation() {
        let key1 = EncryptionKey::generate();
        let key2 = EncryptionKey::generate();
        
        assert_ne!(key1.as_bytes(), key2.as_bytes());
    }

    #[test]
    fn test_key_derivation() {
        let key = EncryptionKey::derive_from_password("test_password", None);
        
        assert_eq!(key.derivation, KeyDerivation::Argon2id);
        assert!(key.salt.is_some());
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = EncryptionKey::generate();
        let plaintext = b"Hello, World! This is a test message.";
        
        let encrypted = EncryptedBackup::encrypt(plaintext, &key, plaintext.len() as u64).unwrap();
        let decrypted = EncryptedBackup::decrypt(&encrypted, &key).unwrap();
        
        assert_eq!(plaintext.to_vec(), decrypted);
    }

    #[test]
    fn test_wrong_key_fails() {
        let key1 = EncryptionKey::generate();
        let key2 = EncryptionKey::generate();
        let plaintext = b"Secret message";
        
        let encrypted = EncryptedBackup::encrypt(plaintext, &key1, plaintext.len() as u64).unwrap();
        let result = EncryptedBackup::decrypt(&encrypted, &key2);
        
        assert!(result.is_err());
    }
}
