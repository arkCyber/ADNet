//! Errors surfaced by the WebTransport transport.

use thiserror::Error;

use crate::connect_token::ConnectTokenError;

/// Result alias for the WebTransport transport.
pub type WebTransportResult<T> = Result<T, WebTransportError>;

/// Errors that can come out of the WebTransport transport.
#[derive(Debug, Error)]
pub enum WebTransportError {
    #[error("tls: {0}")]
    Tls(String),

    #[error("bind: {0}")]
    Bind(String),

    #[error("session: {0}")]
    Session(String),

    #[error("connect-token: {0}")]
    Token(#[from] ConnectTokenError),

    #[error("noise: {0}")]
    Noise(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("backend: {0}")]
    Backend(String),
}
