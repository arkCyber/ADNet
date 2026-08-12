//! NAT traversal error types.

use thiserror::Error;

/// Result type for NAT operations.
pub type NatResult<T> = Result<T, NatError>;

/// NAT traversal error types.
#[derive(Error, Debug)]
pub enum NatError {
    #[error("STUN error: {reason}")]
    Stun { reason: String },

    #[error("TURN error: {reason}")]
    Turn { reason: String },

    #[error("UPnP error: {reason}")]
    Upnp { reason: String },

    #[error("Hole punching error: {reason}")]
    HolePunch { reason: String },

    #[error("Network error: {reason}")]
    Network { reason: String },

    #[error("Timeout waiting for {operation}")]
    Timeout { operation: String },

    #[error("NAT type detection failed: {reason}")]
    NatTypeDetection { reason: String },

    #[error("Port mapping failed: {port} - {reason}")]
    PortMappingFailed { port: u16, reason: String },

    #[error("No available NAT traversal method")]
    NoTraversalMethod,

    #[error("Peer unreachable: {peer}")]
    PeerUnreachable { peer: String },

    #[error("Configuration error: {reason}")]
    Config { reason: String },
}

impl NatError {
    /// Check if error is retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            NatError::Network { .. }
                | NatError::Timeout { .. }
                | NatError::PeerUnreachable { .. }
        )
    }

    /// Check if error indicates a fatal condition.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            NatError::Config { .. }
                | NatError::NatTypeDetection { .. }
                | NatError::NoTraversalMethod
        )
    }
}
