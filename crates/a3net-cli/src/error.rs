//! `a3net-cli` error types.
//!
//! This module provides unified error handling for the CLI crate.
//! CLI errors are designed to be user-friendly and actionable.

use thiserror::Error;

/// Unified error type for CLI operations.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct CliError {
    #[from]
    inner: CliErrorKind,
}

impl CliError {
    /// Returns the exit code for the CLI process.
    pub fn exit_code(&self) -> i32 {
        match &self.inner {
            CliErrorKind::NotFound(_) => 1,
            CliErrorKind::InvalidInput(_) => 2,
            CliErrorKind::Network(_) => 3,
            CliErrorKind::PermissionDenied(_) => 4,
            CliErrorKind::Timeout(_) => 5,
            CliErrorKind::Storage(_) => 6,
            CliErrorKind::Config(_) => 7,
            CliErrorKind::Crypto(_) => 8,
            CliErrorKind::Encoding(_) => 9,
            CliErrorKind::Io(_) => 10,
        }
    }
}

/// Detailed error categories for CLI operations.
#[derive(Debug, Error)]
pub enum CliErrorKind {
    /// Resource not found (file, key, etc.).
    #[error("not found: {0}")]
    NotFound(String),

    /// Invalid user input or argument.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// Network-related error.
    #[error("network error: {0}")]
    Network(String),

    /// Permission denied.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// Operation timed out.
    #[error("timeout: {0}")]
    Timeout(String),

    /// Storage or I/O error.
    #[error("storage error: {0}")]
    Storage(String),

    /// Configuration error.
    #[error("configuration error: {0}")]
    Config(String),

    /// Cryptography error.
    #[error("cryptography error: {0}")]
    Crypto(String),

    /// Encoding/decoding error.
    #[error("encoding error: {0}")]
    Encoding(String),

    /// General I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        CliErrorKind::Encoding(e.to_string()).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_codes() {
        let not_found = CliError::from(CliErrorKind::NotFound("test".into()));
        assert_eq!(not_found.exit_code(), 1);

        let io_err = CliError::from(CliErrorKind::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "test",
        )));
        assert_eq!(io_err.exit_code(), 10);
    }

    #[test]
    fn test_error_display() {
        let err = CliError::from(CliErrorKind::NotFound("QmHash".into()));
        assert!(err.to_string().contains("not found"));
    }
}
