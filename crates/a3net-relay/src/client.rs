//! Relay client — builds proxy URLs for the relay server.
//!
//! Mirrors `WanRelayConfig::proxy_url` from
//! `Exodus@src-backup/.../wan_relay.rs`.

use crate::config::RelayConfig;

/// Builds `/exodus-mesh/fetch?host=…&port=…&path=…` URLs.
#[derive(Debug, Clone)]
pub struct RelayClient {
    cfg: RelayConfig,
}

impl RelayClient {
    pub fn new(cfg: RelayConfig) -> Self {
        Self { cfg }
    }

    pub fn config(&self) -> &RelayConfig {
        &self.cfg
    }

    /// Build a proxy URL for forwarding a mesh-HTTP request.
    ///
    /// Returns `None` when the client is disabled or the relay base URL is
    /// empty.
    ///
    /// `mesh_path` is the path on the *remote* mesh server — it must start
    /// with `/` (typically `/blobs/{hash}/...`).
    pub fn proxy_url(&self, host: &str, port: u16, mesh_path: &str) -> Option<String> {
        if !self.cfg.enabled {
            return None;
        }
        let base = self
            .cfg
            .relay_base_url
            .as_ref()?
            .trim()
            .trim_end_matches('/');
        if base.is_empty() {
            return None;
        }
        let path = if mesh_path.starts_with('/') {
            mesh_path.to_string()
        } else {
            format!("/{mesh_path}")
        };
        Some(format!(
            "{base}/exodus-mesh/fetch?host={host}&port={port}&path={}",
            urlencoded_path(&path)
        ))
    }
}

fn urlencoded_path(path: &str) -> String {
    url::form_urlencoded::byte_serialize(path.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_proxy_url() {
        let cfg = RelayConfig {
            enabled: true,
            relay_base_url: Some("https://relay.example.com".into()),
            ..Default::default()
        };
        let client = RelayClient::new(cfg);
        let u = client
            .proxy_url("1.2.3.4", 9000, "/blobs/abc/meta")
            .unwrap();
        assert!(u.contains("relay.example.com"));
        assert!(u.contains("host=1.2.3.4"));
        assert!(u.contains("port=9000"));
        assert!(u.contains("path="));
    }

    #[test]
    fn returns_none_when_disabled() {
        let cfg = RelayConfig {
            enabled: false,
            relay_base_url: Some("https://relay.example.com".into()),
            ..Default::default()
        };
        let client = RelayClient::new(cfg);
        assert!(client.proxy_url("1.2.3.4", 9000, "/blobs/abc").is_none());
    }

    #[test]
    fn returns_none_when_no_base_url() {
        let cfg = RelayConfig::default();
        let client = RelayClient::new(cfg);
        assert!(client.proxy_url("1.2.3.4", 9000, "/blobs/abc").is_none());
    }

    #[test]
    fn adds_leading_slash_to_path() {
        let cfg = RelayConfig {
            enabled: true,
            relay_base_url: Some("https://relay.example.com".into()),
            ..Default::default()
        };
        let client = RelayClient::new(cfg);
        let u = client.proxy_url("h", 1, "blobs/x").unwrap();
        assert!(u.contains("path=%2Fblobs%2Fx"));
    }
}
