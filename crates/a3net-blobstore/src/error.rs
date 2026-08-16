//! `a3net-blobstore` error types.
//!
//! This module provides unified error handling for the blobstore crate.
//! All errors are categorized with stable error codes for observability.

use thiserror::Error;

/// Unified error type for blobstore operations.
///
/// Each variant includes a stable error code that can be used for
/// metric labeling and log correlation.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct BlobStoreError {
    #[from]
    inner: BlobStoreErrorKind,
}

impl BlobStoreError {
    /// Returns the error code for observability.
    pub fn code(&self) -> &'static str {
        match &self.inner {
            BlobStoreErrorKind::Io(_) => "BLOB-001",
            BlobStoreErrorKind::NotFound(_) => "BLOB-002",
            BlobStoreErrorKind::Serialization(_) => "BLOB-003",
            BlobStoreErrorKind::ChunkNotFound { .. } => "BLOB-004",
            BlobStoreErrorKind::InvalidChunkIndex(_) => "BLOB-005",
            BlobStoreErrorKind::PartialRead(_) => "BLOB-006",
            BlobStoreErrorKind::HashMismatch(_) => "BLOB-007",
            BlobStoreErrorKind::AlreadyExists(_) => "BLOB-008",
            BlobStoreErrorKind::QuotaExceeded(_) => "BLOB-009",
            BlobStoreErrorKind::CorruptedData(_) => "BLOB-010",
            BlobStoreErrorKind::Crypto(_) => "BLOB-011",
        }
    }
}

/// Detailed error categories for blobstore operations.
#[derive(Debug, Error)]
pub enum BlobStoreErrorKind {
    /// I/O error (file system, permissions, etc.)
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Blob not found in the store.
    #[error("blob not found: {0}")]
    NotFound(String),

    /// Serialization/deserialization error.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// Chunk not found at the specified index.
    #[error("chunk {index} not found for blob {hash}")]
    ChunkNotFound { index: u64, hash: String },

    /// Invalid chunk index provided.
    #[error("invalid chunk index: {0}")]
    InvalidChunkIndex(String),

    /// Partial read - expected more bytes than available.
    #[error("partial read: {0}")]
    PartialRead(String),

    /// Hash verification failed.
    #[error("hash mismatch: {0}")]
    HashMismatch(String),

    /// Blob already exists in the store.
    #[error("blob already exists: {0}")]
    AlreadyExists(String),

    /// Storage quota exceeded.
    #[error("quota exceeded: {0}")]
    QuotaExceeded(String),

    /// Data corruption detected.
    #[error("corrupted data: {0}")]
    CorruptedData(String),

    /// Cryptography error (encryption/decryption).
    #[error("crypto error: {0}")]
    Crypto(String),
}

/// Result type alias for blobstore operations.
pub type Result<T> = std::result::Result<T, BlobStoreError>;

impl From<serde_json::Error> for BlobStoreError {
    fn from(e: serde_json::Error) -> Self {
        BlobStoreErrorKind::Serialization(e.to_string()).into()
    }
}

#[cfg(feature = "car")]
impl From<crate::car::CarError> for BlobStoreError {
    fn from(e: crate::car::CarError) -> Self {
        BlobStoreErrorKind::Serialization(e.to_string()).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes() {
        let not_found = BlobStoreError::from(BlobStoreErrorKind::NotFound("test".into()));
        assert_eq!(not_found.code(), "BLOB-002");

        let io_err = BlobStoreErrorKind::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "test",
        ));
        let wrapped: BlobStoreError = io_err.into();
        assert_eq!(wrapped.code(), "BLOB-001");
    }

    #[test]
    fn test_error_display() {
        let err = BlobStoreError::from(BlobStoreErrorKind::NotFound("QmHash".into()));
        assert!(err.to_string().contains("blob not found"));
    }
}
