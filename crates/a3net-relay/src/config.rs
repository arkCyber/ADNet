//! Persistent relay settings.
//!
//! Mirrors `WanRelayConfig` from
//! `Exodus@src-backup/.../wan_relay.rs`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::HostPolicy;

fn default_serve_enabled() -> bool {
    true
}

fn default_serve_port() -> u16 {
    8790
}

fn default_serve_bind() -> String {
    "127.0.0.1".to_string()
}

/// Persisted WAN relay settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayConfig {
    /// Use relay URLs when fetching (client).
    pub enabled: bool,
    #[serde(default)]
    pub relay_base_url: Option<String>,
    /// Run the embedded relay HTTP server.
    #[serde(default = "default_serve_enabled")]
    pub serve_enabled: bool,
    #[serde(default = "default_serve_port")]
    pub serve_port: u16,
    #[serde(default = "default_serve_bind")]
    pub serve_bind: String,
    /// Path to a 32-byte hex secret on disk. When set *and* the
    /// `billing` feature is enabled at build time, the relay server
    /// loads this secret as the [`crate::BillingMode::from_treasury`]
    /// root wallet and exposes the `/relay/billing/v1/*` endpoints.
    /// The field is `Option` so existing `relay.json` files keep
    /// deserializing unchanged; an operator opts in by writing this
    /// field and dropping a 32-byte hex file at the path.
    #[serde(default)]
    pub billing_secret_path: Option<std::path::PathBuf>,
    /// Host policy for the relay server.
    #[serde(default)]
    pub host_policy: HostPolicy,
    /// Maximum body size in bytes.
    #[serde(default)]
    pub max_body_bytes: Option<usize>,
    /// Upstream timeout in seconds.
    #[serde(default)]
    pub upstream_timeout_secs: Option<u64>,
    /// Maximum redirects.
    #[serde(default)]
    pub max_redirects: Option<u32>,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            relay_base_url: None,
            serve_enabled: default_serve_enabled(),
            serve_port: default_serve_port(),
            serve_bind: default_serve_bind(),
            billing_secret_path: None,
            host_policy: crate::HostPolicy::default(),
            max_body_bytes: None,
            upstream_timeout_secs: None,
            max_redirects: None,
        }
    }
}

impl RelayConfig {
    /// Load from `{app_data}/relay.json` if it exists, otherwise defaults.
    pub fn load(app_data_dir: &Path) -> Self {
        let path = app_data_dir.join("relay.json");
        if !path.exists() {
            return Self::default();
        }
        fs_read_json(&path).unwrap_or_default()
    }

    /// Local relay base URL based on `serve_bind`/`serve_port`.
    pub fn local_base_url(&self) -> String {
        format!("http://{}:{}", self.serve_bind.trim(), self.serve_port)
    }

    /// When the embedded server is enabled, point `relay_base_url` at it
    /// (if empty) and ensure `enabled` is true.
    pub fn apply_local_relay_url(&mut self) {
        if self.serve_enabled {
            let base = self.local_base_url();
            if self
                .relay_base_url
                .as_ref()
                .map(|s| s.is_empty())
                .unwrap_or(true)
            {
                self.relay_base_url = Some(base);
            }
            self.enabled = true;
        }
    }

    pub fn save(&self, app_data_dir: &Path) -> Result<(), String> {
        let path = app_data_dir.join("relay.json");
        let raw = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, raw).map_err(|e| e.to_string())
    }

    /// Build a [`crate::BillingMode`] from this config. When the
    /// `billing` cargo feature is off, this always returns
    /// `BillingMode::Disabled`. When the feature is on:
    ///
    /// - If `billing_secret_path` is `None`, returns `Disabled`.
    /// - If the path is set but the file doesn't exist or is malformed,
    ///   returns `Disabled` *and* the caller will see a warning logged.
    /// - Otherwise loads the 32-byte hex secret, builds a treasury,
    ///   and returns `BillingMode::from_treasury(…)`.
    pub fn billing_mode(&self) -> crate::BillingMode {
        self.billing_mode_with_logger(|msg| tracing::warn!("{msg}"))
    }

    /// Like [`Self::billing_mode`] but lets the caller choose where the
    /// "missing / malformed secret" warning goes. Useful in tests that
    /// want to assert on the message.
    pub fn billing_mode_with_logger(&self, mut log: impl FnMut(&str)) -> crate::BillingMode {
        // The `mut` is only needed on the `billing` branch — the
        // default-features-off branch never calls `log`, so the
        // compiler warns. We avoid the warning by accepting it as
        // `mut` (allowed even when unused).
        let _ = &mut log;
        #[cfg(feature = "billing")]
        {
            let Some(path) = self.billing_secret_path.as_ref() else {
                return crate::BillingMode::Disabled;
            };
            let raw = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    log(&format!(
                        "billing_secret_path {:?} is unreadable: {e}; billing is off",
                        path
                    ));
                    return crate::BillingMode::Disabled;
                }
            };
            let trimmed = raw.trim();
            let bytes = match hex::decode(trimmed) {
                Ok(b) => b,
                Err(e) => {
                    log(&format!(
                        "billing_secret_path {:?} is not valid hex: {e}; billing is off",
                        path
                    ));
                    return crate::BillingMode::Disabled;
                }
            };
            if bytes.len() != 32 {
                log(&format!(
                    "billing_secret_path {:?} must be 32 bytes, got {}; billing is off",
                    path,
                    bytes.len()
                ));
                return crate::BillingMode::Disabled;
            }
            let mut secret = [0u8; 32];
            secret.copy_from_slice(&bytes);
            // Build a one-shot treasury around the loaded secret.
            let pub_only = a3net_identity::Wallet::from_bytes(&secret).map(|w| {
                let view = a3net_identity::TreasuryView {
                    root_public: w.public().clone(),
                    ephemeral: vec![],
                };
                a3net_identity::Treasury::from_view(view).with_root(&secret)
            });
            match pub_only {
                Ok(Ok(t)) => match crate::BillingMode::from_treasury(std::sync::Arc::new(t)) {
                    Ok(mode) => mode,
                    Err(e) => {
                        log(&format!("treasury build failed: {e}; billing is off"));
                        crate::BillingMode::Disabled
                    }
                },
                Ok(Err(e)) => {
                    log(&format!(
                        "secret is not a valid wallet: {e}; billing is off"
                    ));
                    crate::BillingMode::Disabled
                }
                Err(e) => {
                    log(&format!("treasury view build failed: {e}; billing is off"));
                    crate::BillingMode::Disabled
                }
            }
        }
        #[cfg(not(feature = "billing"))]
        {
            crate::BillingMode::Disabled
        }
    }
}

/// Status snapshot returned by the relay server (used for diagnostics).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayServerInfo {
    pub running: bool,
    pub port: u16,
    pub base_url: String,
    pub bind_host: String,
}

fn fs_read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BillingMode;

    #[test]
    fn builds_local_base_url() {
        let cfg = RelayConfig::default();
        assert_eq!(cfg.local_base_url(), "http://127.0.0.1:8790");
    }

    #[test]
    fn apply_local_relay_url_fills_empty() {
        let mut cfg = RelayConfig {
            relay_base_url: None,
            ..RelayConfig::default()
        };
        cfg.apply_local_relay_url();
        assert_eq!(cfg.relay_base_url.as_deref(), Some("http://127.0.0.1:8790"));
        assert!(cfg.enabled);
    }

    #[test]
    fn apply_local_relay_url_preserves_existing() {
        let mut cfg = RelayConfig {
            relay_base_url: Some("https://relay.example.com".into()),
            ..Default::default()
        };
        cfg.apply_local_relay_url();
        assert_eq!(
            cfg.relay_base_url.as_deref(),
            Some("https://relay.example.com")
        );
    }

    #[test]
    fn default_settings() {
        let cfg = RelayConfig::default();
        assert!(cfg.enabled);
        assert!(cfg.serve_enabled);
        assert_eq!(cfg.serve_port, 8790);
    }

    #[test]
    fn billing_mode_without_secret_path_is_disabled() {
        let cfg = RelayConfig::default();
        let mut warnings = vec![];
        let mode = cfg.billing_mode_with_logger(|m| warnings.push(m.to_string()));
        assert!(matches!(mode, BillingMode::Disabled));
        assert!(warnings.is_empty());
    }

    #[test]
    #[cfg(feature = "billing")]
    fn billing_mode_with_missing_secret_file_warns_and_disables() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = RelayConfig {
            billing_secret_path: Some(dir.path().join("does-not-exist.hex")),
            ..RelayConfig::default()
        };
        let mut warnings = vec![];
        let mode = cfg.billing_mode_with_logger(|m| warnings.push(m.to_string()));
        assert!(matches!(mode, BillingMode::Disabled));
        assert!(!warnings.is_empty());
        assert!(warnings[0].contains("unreadable"));
    }

    #[test]
    #[cfg(feature = "billing")]
    fn billing_mode_with_valid_secret_enables_billing() {
        let dir = tempfile::tempdir().unwrap();
        let secret_path = dir.path().join("root.hex");
        // Use a known-good secret (the first 32 bytes of a secp256k1
        // wallet; we don't need the wallet to load — we just need 32
        // bytes of hex).
        let secret_hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        std::fs::write(&secret_path, secret_hex).unwrap();

        let cfg = RelayConfig {
            billing_secret_path: Some(secret_path),
            ..RelayConfig::default()
        };
        let mut warnings = vec![];
        let mode = cfg.billing_mode_with_logger(|m| warnings.push(m.to_string()));
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        // Should now be `Enabled`.
        assert!(matches!(mode, BillingMode::Enabled { .. }));
    }
}
