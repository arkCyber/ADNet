//! Error types for adnet-smarthome

use thiserror::Error;

pub type Result<T> = std::result::Result<T, SmartHomeError>;

#[derive(Error, Debug)]
pub enum SmartHomeError {
    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    #[error("Authentication failed: {0}")]
    Auth(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Device control error: {0}")]
    DeviceControl(String),

    #[error("Discovery failed: {0}")]
    Discovery(String),

    #[error("Signature error: {0}")]
    Signature(String),

    #[error("Not supported: {0}")]
    NotSupported(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Automation error: {0}")]
    Automation(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
