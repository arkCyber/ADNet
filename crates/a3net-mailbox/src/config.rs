//! Configuration for the mailbox server and client.
//!
//! Mirrors `a3net-relay`'s `RelayConfig` style: every field is optional
//! in spirit, sensible defaults are filled in by `Default`, and the
//! struct is `serde`-serializable so it can be persisted as a JSON file
//! next to the relay config.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{MailboxError, MailboxResult};

/// Default TCP port for the mailbox server. Chosen to sit one port
/// above the WAN relay (`a3net-relay` default `18790`) so the same
/// fixed-IP deployment can run both with no collision.
pub const DEFAULT_PORT: u16 = 18791;

/// Default per-message envelope byte cap (1 MiB). Phases 1–3 of a3chat
/// only need to fit a serialized `MessageEnvelope` plus a small
/// signature header; large attachments live in the blobstore and are
/// referenced by content hash from inside the envelope.
pub const DEFAULT_MAX_ENVELOPE_BYTES: usize = 1_048_576;

/// Default per-recipient message-count cap.
pub const DEFAULT_MAX_INFLIGHT_PER_USER: usize = 1_000;

/// Default per-recipient total-bytes cap (64 MiB). Roughly 64 envelopes
/// at the max size, or 64 000 text envelopes at 1 KiB each.
pub const DEFAULT_MAX_TOTAL_BYTES_PER_USER: u64 = 64 * 1024 * 1024;

/// Default message lifetime (30 days). After this point the envelope
/// is purged by the background sweeper and 永久 lost.
pub const DEFAULT_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Default dial timeout (5s). When the client cannot deliver a
/// message to the recipient's inbox over the P2P path within this
/// window, it falls back to the mailbox `enqueue`.
pub const DEFAULT_DIAL_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum allowed signature age in seconds. Even if an operator configures
/// a larger value, it is capped here to prevent accidentally disabling replay
/// protection entirely. Default: 3600 (1 hour).
pub const MAX_SIGNATURE_AGE_SECS: u64 = 3600;

/// Storage backend choice.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StorageBackend {
    /// In-memory backend — used for tests and ephemeral deployments.
    /// State is dropped on server restart.
    Memory,
    /// Single-file SQLite backend. The default production choice.
    Sqlite { path: PathBuf },
}

impl Default for StorageBackend {
    fn default() -> Self {
        StorageBackend::Sqlite {
            path: PathBuf::from("./.a3net-mailbox.db"),
        }
    }
}

/// Mailbox server / client configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MailboxConfig {
    /// Address to bind the HTTP server to. Default `"0.0.0.0"`.
    pub bind_host: String,
    /// TCP port to listen on. Default `18791`.
    pub port: u16,
    /// Storage backend choice. Default `Sqlite { path: "./.a3net-mailbox.db" }`.
    pub storage: StorageBackend,
    /// Per-envelope byte cap. Default `1_048_576` (1 MiB).
    pub max_envelope_bytes: usize,
    /// Per-recipient message-count cap. Default `1_000`.
    pub max_inflight_per_user: usize,
    /// Per-recipient total-bytes cap. Default `64 * 1024 * 1024`.
    pub max_total_bytes_per_user: u64,
    /// Default message lifetime. Default `30 days`.
    pub default_ttl: Duration,
    /// Maximum age of a sender signature (EIP-712 timestamp binding) in seconds.
    /// Signatures older than this are rejected. Capped at `MAX_SIGNATURE_AGE_SECS`
    /// (1 hour) even if configured higher. Default: 300 (5 minutes).
    pub max_signature_age_secs: u64,
    /// Whether to require sender signature on `enqueue`. Default `true`.
    pub require_sender_signature: bool,
    /// HTTP client / upstream timeout. Default `5s`.
    pub upstream_timeout: Duration,
    /// Base URL of the mailbox server, used by the
    /// [`crate::client::MailboxClient`]. E.g. `"https://mailbox.example.com"`.
    /// When `None`, the client is in "disabled" mode and all calls
    /// return `MailboxError::Config`.
    pub base_url: Option<String>,
}

impl Default for MailboxConfig {
    fn default() -> Self {
        Self {
            bind_host: "0.0.0.0".to_string(),
            port: DEFAULT_PORT,
            storage: StorageBackend::default(),
            max_envelope_bytes: DEFAULT_MAX_ENVELOPE_BYTES,
            max_inflight_per_user: DEFAULT_MAX_INFLIGHT_PER_USER,
            max_total_bytes_per_user: DEFAULT_MAX_TOTAL_BYTES_PER_USER,
            default_ttl: DEFAULT_TTL,
            max_signature_age_secs: 300,
            require_sender_signature: true,
            upstream_timeout: DEFAULT_DIAL_TIMEOUT,
            base_url: None,
        }
    }
}

impl MailboxConfig {
    /// Build a config bound to the given host:port.
    pub fn bind(host: impl Into<String>, port: u16) -> Self {
        Self {
            bind_host: host.into(),
            port,
            ..Self::default()
        }
    }

    /// Persist the config to `<dir>/mailbox.json`.
    ///
    /// Mirrors `a3net-relay::RelayConfig::save`.
    pub fn save(&self, dir: &Path) -> MailboxResult<()> {
        let path = dir.join("mailbox.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                MailboxError::Config(format!("create config dir {}: {e}", parent.display()))
            })?;
        }
        let buf = serde_json::to_vec_pretty(self)
            .map_err(|e| MailboxError::Config(format!("serialize config: {e}")))?;
        std::fs::write(&path, buf).map_err(|e| {
            MailboxError::Config(format!("write config {}: {e}", path.display()))
        })?;
        Ok(())
    }

    /// Load a config from `<dir>/mailbox.json`. Returns the default
    /// config if the file does not exist (first-run behavior).
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("mailbox.json");
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(
                    "mailbox.json at {} is malformed ({e}); falling back to defaults",
                    path.display()
                );
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }
}

/// Runtime info reported by the running server, mirrored from
/// `a3net-relay::RelayServerInfo`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MailboxServerInfo {
    pub running: bool,
    /// `true` when the in-process state is healthy (mirror of
    /// `/healthz`). Phase 1 will drive this from a real liveness check.
    pub self_check: bool,
    pub port: u16,
    pub bind_host: String,
    pub base_url: String,
}
