//! WebTransport server (HTTP/3).
//!
//! Round-1 scaffold. Round-2 will:
//! 1. Wire [`wtransport::Server`] with the configured TLS cert.
//! 2. Verify the [`ConnectToken`] on the initial HTTP request.
//! 3. Open the first bidirectional stream of the accepted session.
//! 4. Run the Noise handshake on that stream (using
//!    [`a3net_webrtc::noise_dc`]).
//! 5. Hand off to the regular A3Net [`Frame`](a3net_types) codec.

use std::net::SocketAddr;

use a3net_types::NodeId;
use a3net_webrtc::noise_dc;

use crate::config::WebTransportConfig;
use crate::connect_token::{ConnectToken, ConnectTokenError, TokenClaim};
use crate::error::{WebTransportError, WebTransportResult};

/// A running WebTransport server. Round-1 stub.
pub struct WtServer {
    pub config: WebTransportConfig,
}

/// Handle returned by [`WtServer::bind`]. Round-1 stub.
pub struct WtServerHandle {
    pub local_addr: SocketAddr,
    pub token_secret: Vec<u8>,
    /// Signalling: shutdown signal sent from [`WtServerHandle::shutdown`].
    pub shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl WtServer {
    /// Generate a fresh 32-byte HMAC secret for connect-token signing.
    pub fn generate_secret() -> Vec<u8> {
        use rand::RngCore;
        let mut buf = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut buf);
        buf.to_vec()
    }

    /// Bind the configured socket and start serving.
    ///
    /// Round-1 stub: returns a placeholder handle. Round-2 will:
    /// 1. Generate or load the TLS cert.
    /// 2. Construct a `wtransport::Server`.
    /// 3. Spawn the accept loop.
    pub async fn bind(
        config: WebTransportConfig,
        _local_node_id: NodeId,
    ) -> WebTransportResult<WtServerHandle> {
        let token_secret = config
            .token_secret
            .as_deref()
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_else(Self::generate_secret);
        let _ = noise_dc::NOISE_PATTERN; // confirm we depend on it
        let local_addr = config.bind;
        Ok(WtServerHandle {
            local_addr,
            token_secret,
            shutdown_tx: None,
        })
    }
}

/// Verify an incoming connect-token. Exposed so the HTTP layer (Round-2)
/// can call this directly.
pub fn verify_connect_token(
    token_b64: &str,
    secret: &[u8],
    now: u64,
) -> Result<TokenClaim, ConnectTokenError> {
    ConnectToken::from_string(token_b64).and_then(|t| t.verify(secret, now))
}

/// Mint a new connect-token.
pub fn mint_token(
    node_id: NodeId,
    ttl_seconds: u64,
    secret: &[u8],
) -> WebTransportResult<ConnectToken> {
    let claim = TokenClaim::new(node_id, ttl_seconds);
    ConnectToken::sign(&claim, secret)
}

impl WtServerHandle {
    pub async fn shutdown(self) -> WebTransportResult<()> {
        if let Some(tx) = self.shutdown_tx {
            let _ = tx.send(());
        }
        Ok(())
    }
}
