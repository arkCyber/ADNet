//! NAS control plane for [`crate::Node`].
//!
//! Feature-gated behind `nas` (= `dep:a3net-webdav`).

use std::net::SocketAddr;
use std::sync::Arc;

use a3net_pairing::CapabilitySet;
use a3net_webdav as webdav;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors surfaced by the NAS control plane.
#[derive(Debug, Error)]
pub enum NasError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("a NAS instance is already running on this node")]
    AlreadyRunning,
    #[error("no NAS instance is currently running")]
    NotRunning,
}

/// Configuration for the WebDAV frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NasConfig {
    pub bind: SocketAddr,
    pub server_name: String,
    #[serde(default)]
    pub initial_caps: Vec<(String, CapabilitySet)>,
}

impl Default for NasConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 8780)),
            server_name: "A3Net-WebDAV/0.1".to_string(),
            initial_caps: Vec::new(),
        }
    }
}

/// Handle to the running WebDAV server.
pub struct NasHandle {
    pub inner: webdav::WebdavServerHandle,
}

impl std::fmt::Debug for NasHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NasHandle")
            .field("local_addr", &self.inner.local_addr())
            .finish()
    }
}

impl NasHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr()
    }
    pub fn shutdown(self) {
        self.inner.shutdown();
    }
}

/// Helper: build a [`webdav::HandlerState`] from a data dir.
pub fn build_handler_state(
    data_dir: std::path::PathBuf,
    cfg: NasConfig,
) -> Result<Arc<webdav::HandlerState>, NasError> {
    use a3net_blobstore::namespace::Nas;
    let nas = Nas::open(&data_dir)
        .map_err(|e| NasError::InvalidConfig(format!("nas: {e}")))?;
    let resolver: Arc<webdav::StaticCapabilityResolver> =
        Arc::new(webdav::StaticCapabilityResolver::new());
    let now = chrono::Utc::now().timestamp_millis();
    for (id, caps) in cfg.initial_caps {
        resolver.register(
            id,
            webdav::ResolvedCapability {
                caps,
                nonce: [0u8; 32],
                expires_unix_ms: now + 86_400_000,
                revoked: false,
            },
        );
    }
    let verifier = webdav::TokenVerifier::new([0u8; 32]);
    let static_resolver: Arc<webdav::StaticCapabilityResolver> = Arc::clone(&resolver);
    let mut st = webdav::HandlerState::new(
        nas,
        resolver as Arc<dyn webdav::CapabilityResolver>,
        verifier,
    );
    st.static_resolver = Some(webdav::StaticCapabilityResolver::new());
    Ok(Arc::new(st))
}
