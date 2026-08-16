//! WebTransport client.
//!
//! Round-1 scaffold. Round-2 will:
//! 1. Connect with `wtransport::Client::new(...)`.
//! 2. Send the connect-token in an HTTP header.
//! 3. Open a bidirectional stream, run the Noise handshake, and return a
//!    [`NoiseSession`] ready to encrypt A3Net frames.

use a3net_types::NodeId;

use crate::config::WebTransportConfig;
use crate::error::{WebTransportError, WebTransportResult};

/// A WebTransport client.
pub struct WtClient {
    config: WebTransportConfig,
    local_node_id: NodeId,
}

impl WtClient {
    pub fn new(config: WebTransportConfig, local_node_id: NodeId) -> Self {
        Self {
            config,
            local_node_id,
        }
    }

    /// Connect to a remote server. Round-1 scaffold.
    pub async fn connect(&self, _url: &str, _token: &str) -> WebTransportResult<()> {
        Err(WebTransportError::Session(
            "WtClient::connect is not yet wired to wtransport (Round-2)".into(),
        ))
    }

    pub fn local_node_id(&self) -> &NodeId {
        &self.local_node_id
    }
}
