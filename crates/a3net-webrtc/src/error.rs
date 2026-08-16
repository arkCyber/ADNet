//! Errors surfaced by the WebRTC transport.

use thiserror::Error;

/// Result alias for the WebRTC transport.
pub type WebRtcResult<T> = Result<T, WebRtcError>;

/// Errors that can come out of the WebRTC transport.
#[derive(Debug, Error)]
pub enum WebRtcError {
    /// The ICE connection failed to establish within the configured timeout.
    #[error("ice establish timeout after {0:?}")]
    IceEstablishTimeout(std::time::Duration),

    /// SDP exchange failed.
    #[error("sdp: {0}")]
    Sdp(String),

    /// Signaling channel error (publishing or fetching SDP / candidates).
    #[error("signaling: {0}")]
    Signaling(String),

    /// Noise handshake failed.
    #[error("noise: {0}")]
    Noise(String),

    /// DataChannel-level error.
    #[error("data channel: {0}")]
    DataChannel(String),

    /// Frame codec error.
    #[error("frame: {0}")]
    Frame(String),

    /// Peer identity could not be derived or did not match expectations.
    #[error("identity: {0}")]
    Identity(String),

    /// The local identity is missing or invalid.
    #[error("local identity missing")]
    LocalIdentityMissing,

    /// The remote peer closed the channel.
    #[error("peer closed")]
    PeerClosed,

    /// An I/O error happened on the underlying socket.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// A catch-all for unexpected backend failures.
    #[error("webrtc backend: {0}")]
    Backend(String),
}

impl WebRtcError {
    /// True if the error means the peer is gone and we should give up.
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::PeerClosed | Self::IceEstablishTimeout(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_classification() {
        assert!(WebRtcError::PeerClosed.is_fatal());
        assert!(WebRtcError::IceEstablishTimeout(std::time::Duration::from_secs(1)).is_fatal());
        assert!(!WebRtcError::Signaling("timeout".into()).is_fatal());
    }
}
