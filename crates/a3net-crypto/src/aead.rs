//! Thin wrapper around [`EncryptionKey::seal`] /
//! [`EncryptionKey::open`] so callers can import a single name for
//! "encrypt this chunk of bytes / decrypt that on-disk chunk".
//!
//! The real work lives on [`EncryptionKey`]; this module exists so
//! that `a3net-blobstore::EncryptedBlobStore` can keep its
//! `seal_chunk` / `open_chunk` API by re-exporting these functions
//! (one less rename to chase in the eventual migration).

pub use crate::key::EncryptionKey;

/// XChaCha20-Poly1305 nonce + tag overhead (re-exported here for
/// callers that want to compute on-disk sizes without pulling in
/// `key`).
pub use crate::key::AEAD_OVERHEAD;
