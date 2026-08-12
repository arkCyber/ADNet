//! Persistent-key resolution helpers for `adnet-ssh`.
//!
//! iroh-ssh stores its server key at `~/.ssh/irohssh_ed25519` and
//! its service-mode key at `~/.ssh/irohssh_service_ed25519`. ADNet
//! already mints a single persistent Ed25519 identity at
//! `<data-dir>/iroh_secret_key` via
//! [`adnet_transport::iroh::IrohIdentity::load_or_create`], and we
//! reuse it. The end result is identical:
//!
//! - Server endpoint id is stable across restarts.
//! - No second key file lives next to the canonical iroh identity.
//! - The same key is shared with the `adnet/frame/1` ALPN, the
//!   blob store, and the gossip layer (because the iroh endpoint
//!   is the same one the rest of the ADNet runtime is using).
//!
//! The functions in this module are intentionally tiny: they
//! either resolve to the persistent identity or to an ephemeral
//! in-memory key (matching iroh-ssh's `server` vs `server --persist`
//! distinction).

#[cfg(feature = "iroh")]
use std::path::Path;
use std::path::PathBuf;

#[cfg(feature = "iroh")]
use adnet_transport::iroh::IrohIdentity;

#[cfg(feature = "iroh")]
use crate::error::{SshError, SshResult};

/// Default subdirectory under `<data-dir>` for SSH-tunnel state.
///
/// Today this is unused (the persistent identity lives directly
/// under `<data-dir>`), but we keep it as a hook for future
/// auxiliary state — known-host entries, per-connection logs,
/// etc. — without breaking the public API.
pub const SSH_SUBDIR: &str = "ssh";

/// Filename of the persistent Ed25519 identity under
/// `<data-dir>`. Re-exported here from
/// `adnet_transport::iroh::IROH_SECRET_KEY_FILE` so callers don't
/// have to import the inner transport crate.
pub const IROH_SECRET_KEY_FILE: &str = "iroh_secret_key";

/// Load (or create) the durable identity for the SSH tunnel.
///
/// Returns the same identity that the rest of the ADNet runtime
/// uses, so the endpoint id printed by `adnet ssh info` matches
/// the endpoint id published by the iroh gossip / blob layer.
#[cfg(feature = "iroh")]
pub fn persistent_identity(data_dir: impl AsRef<Path>) -> SshResult<IrohIdentity> {
    let dir = data_dir.as_ref();
    IrohIdentity::load_or_create(dir).map_err(|e| SshError::Identity {
        path: dir.join(IROH_SECRET_KEY_FILE).display().to_string(),
        source: Box::new(e),
    })
}

/// Resolve the data directory for SSH-tunnel state.
///
/// - If the caller passed `--data-dir`, use it verbatim.
/// - Otherwise default to `./.adnet-data` (matching
///   `adnet-cli::Cli::data_dir`).
pub fn resolve_data_dir(cli_data_dir: Option<&str>) -> PathBuf {
    match cli_data_dir {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from("./.adnet-data"),
    }
}
