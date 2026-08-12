//! Error types for the model catalog

use thiserror::Error;

/// Errors that can occur in the model catalog
#[derive(Error, Debug)]
pub enum ModelCatalogError {
    /// Database error
    #[error("Database error: {0}")]
    DatabaseError(String),

    /// Model not found
    #[error("Model not found: {0}")]
    NotFound(String),

    /// Validation error
    #[error("Validation error: {0}")]
    ValidationError(String),

    /// Iroh error
    #[error("Iroh error: {0}")]
    IrohError(String),

    /// Download error
    #[error("Download error: {0}")]
    DownloadError(String),

    /// Network error
    #[error("Network error: {0}")]
    NetworkError(String),

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// Invalid ticket format
    #[error("Invalid ticket format: {0}")]
    InvalidTicket(String),

    /// Invalid content hash
    #[error("Invalid content hash: {0}")]
    InvalidContentHash(String),

    /// File operation error
    #[error("File error: {0}")]
    FileError(String),

    /// HTTP error
    #[error("HTTP error: {0}")]
    HttpError(String),

    /// Server error
    #[error("Server error: {0}")]
    ServerError(String),

    /// Gossip error
    #[error("Gossip error: {0}")]
    GossipError(String),

    /// Provider error
    #[error("Provider error: {0}")]
    ProviderError(String),

    /// Download in progress
    #[error("Download already in progress: {0}")]
    DownloadInProgress(String),

    /// Invalid manifest
    #[error("Invalid manifest: {0}")]
    InvalidManifest(String),

    /// Concurrent download aborted (channel closed or task cancelled)
    #[error("Download cancelled")]
    Cancelled,

    /// Join error (spawn blocking)
    #[error("Join error: {0}")]
    JoinError(String),
}

impl From<rusqlite::Error> for ModelCatalogError {
    fn from(err: rusqlite::Error) -> Self {
        ModelCatalogError::DatabaseError(err.to_string())
    }
}

impl From<std::sync::PoisonError<()>> for ModelCatalogError {
    fn from(err: std::sync::PoisonError<()>) -> Self {
        ModelCatalogError::JoinError(format!("lock poisoned: {}", err))
    }
}

impl From<reqwest::Error> for ModelCatalogError {
    fn from(err: reqwest::Error) -> Self {
        ModelCatalogError::HttpError(err.to_string())
    }
}

#[cfg(feature = "iroh")]
impl From<iroh_gossip::api::ApiError> for ModelCatalogError {
    fn from(err: iroh_gossip::api::ApiError) -> Self {
        ModelCatalogError::GossipError(err.to_string())
    }
}