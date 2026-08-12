//! Gateway configuration for IPFS-compatible HTTP Gateway.

use std::path::PathBuf;
use std::time::Duration;

/// Gateway server configuration.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Address to bind the gateway server to.
    pub bind_addr: String,
    /// Enable CORS for cross-origin requests.
    pub cors_enabled: bool,
    /// Allowed CORS origins (empty means all).
    pub cors_allowed_origins: Vec<String>,
    /// Cache control header value.
    pub cache_control: String,
    /// Maximum response body size in bytes.
    pub max_response_size: u64,
    /// Request timeout.
    pub request_timeout: Duration,
    /// Path prefix for gateway routes.
    pub route_prefix: String,
    /// Enable verbose logging.
    pub verbose: bool,
    /// TLS configuration (None for plain HTTP).
    pub tls_config: Option<TlsConfig>,
    /// Writable gateway (enable POST/PUT/DELETE).
    pub writable: bool,
    /// Enable IPNS resolution.
    pub enable_ipns: bool,
    /// Default redirect for root path.
    pub root_redirect: Option<String>,
    /// Internal IPC socket path. When set, the gateway exposes a
    /// JSON-RPC interface on this Unix socket for in-process clients.
    /// `None` disables the IPC server.
    pub ipc_socket: Option<PathBuf>,
    /// Enable authentication.
    pub auth_enabled: bool,
    /// Admin API keys (bypass authentication).
    pub admin_api_keys: Vec<String>,
    /// Rate limit: requests per window.
    pub rate_limit: u64,
    /// Rate limit window in seconds.
    pub rate_limit_window: u64,
    /// Read-only mode (reject all write operations).
    pub read_only: bool,
    /// WebSocket server bind address (None to disable).
    pub ws_bind_addr: Option<String>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:8080".to_string(),
            cors_enabled: true,
            cors_allowed_origins: Vec::new(),
            cache_control: "public, max-age=29030400".to_string(),
            max_response_size: 100 * 1024 * 1024, // 100 MB
            request_timeout: Duration::from_secs(30),
            route_prefix: "/ipfs".to_string(),
            verbose: false,
            tls_config: None,
            writable: false,
            enable_ipns: true,
            root_redirect: None,
            ipc_socket: None,
            auth_enabled: false,
            admin_api_keys: Vec::new(),
            rate_limit: 1000,
            rate_limit_window: 60,
            read_only: false,
            ws_bind_addr: None,
        }
    }
}

/// TLS configuration for HTTPS gateway.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// Path to the certificate file (PEM).
    pub cert_path: PathBuf,
    /// Path to the private key file (PEM).
    pub key_path: PathBuf,
}

impl GatewayConfig {
    /// Create a new config with the given bind address.
    pub fn new(bind_addr: impl Into<String>) -> Self {
        Self {
            bind_addr: bind_addr.into(),
            ..Default::default()
        }
    }

    /// Set the route prefix (default: /ipfs).
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.route_prefix = prefix.into();
        self
    }

    /// Enable or disable CORS.
    pub fn with_cors(mut self, enabled: bool) -> Self {
        self.cors_enabled = enabled;
        self
    }

    /// Add allowed CORS origin.
    pub fn add_cors_origin(mut self, origin: impl Into<String>) -> Self {
        self.cors_allowed_origins.push(origin.into());
        self
    }

    /// Enable TLS with the given certificate and key paths.
    pub fn with_tls(mut self, cert: PathBuf, key: PathBuf) -> Self {
        self.tls_config = Some(TlsConfig {
            cert_path: cert,
            key_path: key,
        });
        self
    }

    /// Enable writable gateway (POST/PUT/DELETE endpoints).
    pub fn with_writable(mut self) -> Self {
        self.writable = true;
        self
    }

    /// Set the request timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Set the maximum response size.
    pub fn with_max_size(mut self, max_size: u64) -> Self {
        self.max_response_size = max_size;
        self
    }

    /// Enable verbose logging.
    pub fn with_verbose(mut self) -> Self {
        self.verbose = true;
        self
    }

    /// Set root redirect URL.
    pub fn with_root_redirect(mut self, url: impl Into<String>) -> Self {
        self.root_redirect = Some(url.into());
        self
    }

    /// Enable the internal IPC server bound to `socket_path`. The IPC
    /// surface is intentionally Unix-socket-only — it is for
    /// in-process companion services, not external clients.
    pub fn with_ipc_socket(mut self, socket_path: PathBuf) -> Self {
        self.ipc_socket = Some(socket_path);
        self
    }

    /// Check if CORS is enabled and origin is allowed.
    pub fn is_cors_allowed(&self, origin: &str) -> bool {
        if !self.cors_enabled {
            return false;
        }
        if self.cors_allowed_origins.is_empty() {
            return true;
        }
        self.cors_allowed_origins.iter().any(|o| o == origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = GatewayConfig::default();
        assert_eq!(config.bind_addr, "0.0.0.0:8080");
        assert_eq!(config.route_prefix, "/ipfs");
        assert!(config.cors_enabled);
        assert!(!config.writable);
    }

    #[test]
    fn test_config_builder() {
        let config = GatewayConfig::new("127.0.0.1:9000")
            .with_prefix("/gateway")
            .with_cors(false)
            .with_writable()
            .with_timeout(Duration::from_secs(60));

        assert_eq!(config.bind_addr, "127.0.0.1:9000");
        assert_eq!(config.route_prefix, "/gateway");
        assert!(!config.cors_enabled);
        assert!(config.writable);
        assert_eq!(config.request_timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_cors_origin_check() {
        let config = GatewayConfig::default();

        // No allowed origins means all are allowed
        assert!(config.is_cors_allowed("*"));
        assert!(config.is_cors_allowed("https://example.com"));

        let config = GatewayConfig::default()
            .add_cors_origin("https://example.com")
            .add_cors_origin("https://app.example.org");

        assert!(!config.is_cors_allowed("*"));
        assert!(config.is_cors_allowed("https://example.com"));
        assert!(config.is_cors_allowed("https://app.example.org"));
        assert!(!config.is_cors_allowed("https://evil.com"));
    }
}
