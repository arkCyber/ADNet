//! WebTransport server / client configuration.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Default port for the WebTransport endpoint. The IANA dynamic/private
/// range starts at 49152; we pick a memorable value that's still in that
/// range to make local development easy.
pub const DEFAULT_PORT: u16 = 49152;

/// Default validity window for a connect-token.
pub const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(60);

/// Configuration for the WebTransport transport.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WebTransportConfig {
    /// Local bind address. Defaults to `0.0.0.0:49152`.
    #[serde(default = "WebTransportConfig::default_bind")]
    pub bind: SocketAddr,

    /// Path to a PEM-encoded TLS certificate. Required for any browser
    /// to connect (browsers reject self-signed by default). Self-signed
    /// is fine for `127.0.0.1` and `localhost`.
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,

    /// Path to a PEM-encoded TLS private key. Required when `tls_cert` is set.
    #[serde(default)]
    pub tls_key: Option<PathBuf>,

    /// Whether to generate an ephemeral self-signed cert on startup if no
    /// `tls_cert`/`tls_key` is provided. Useful for development only —
    /// browsers will reject this in production.
    #[serde(default = "WebTransportConfig::default_ephemeral_cert")]
    pub ephemeral_cert: bool,

    /// Connect-token TTL. Browser clients must connect within this window
    /// after receiving the token, or the server rejects the connection.
    #[serde(default = "WebTransportConfig::default_token_ttl_seconds")]
    pub token_ttl_seconds: u64,

    /// HMAC secret used to sign connect-tokens. Loaded from disk or
    /// generated at startup if absent. Keep this private — anyone with
    /// the secret can mint tokens.
    #[serde(default)]
    pub token_secret: Option<String>,
}

impl Default for WebTransportConfig {
    fn default() -> Self {
        Self {
            bind: Self::default_bind(),
            tls_cert: None,
            tls_key: None,
            ephemeral_cert: Self::default_ephemeral_cert(),
            token_ttl_seconds: Self::default_token_ttl_seconds(),
            token_secret: None,
        }
    }
}

impl WebTransportConfig {
    fn default_bind() -> SocketAddr {
        format!("0.0.0.0:{DEFAULT_PORT}").parse().expect("valid socket")
    }

    const fn default_ephemeral_cert() -> bool {
        true
    }

    const fn default_token_ttl_seconds() -> u64 {
        60
    }

    /// Token TTL as a `Duration`.
    pub fn token_ttl(&self) -> Duration {
        Duration::from_secs(self.token_ttl_seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_constants() {
        let cfg = WebTransportConfig::default();
        assert_eq!(cfg.bind.port(), DEFAULT_PORT);
        assert_eq!(cfg.token_ttl(), DEFAULT_TOKEN_TTL);
        assert!(cfg.ephemeral_cert);
    }

    #[test]
    fn roundtrip_json() {
        let cfg = WebTransportConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: WebTransportConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }
}
