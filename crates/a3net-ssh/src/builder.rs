//! [`IrohSshBuilder`] — wires the SSH-tunnel ALPN onto an iroh
//! endpoint derived from the A3Net persistent identity.
//!
//! This is the A3Net equivalent of iroh-ssh's `IrohSsh` builder.
//! The differences from upstream are deliberate:
//!
//! - We use `a3net_transport::iroh::IrohIdentity` as the single
//!   source of truth for the persistent key. iroh-ssh stores two
//!   separate files (`irohssh_ed25519` and
//!   `irohssh_service_ed25519`); we don't.
//! - The ALPN we register is `a3net/ssh-tunnel/1`, not
//!   `iroh-ssh`. Keeping the ALPN namespaced under `a3net/`
//!   ensures an SSH-tunnel server doesn't accept connections from
//!   a generic `a3net/frame/1` peer and vice-versa.
//! - We do not embed `iroh-net` defaults wholesale. Operators
//!   that want the full `n0` discovery stack can enable the
//!   `iroh` feature and pass `Endpoint::builder(presets::N0)` via
//!   the [`IrohSshBuilder::with_endpoint_presets`] hook.

#[cfg(feature = "iroh")]
use std::path::{Path, PathBuf};
#[cfg(feature = "iroh")]
use std::sync::Arc;

#[cfg(feature = "iroh")]
use crate::error::{SshError, SshResult};

#[cfg(feature = "iroh")]
use crate::keys::persistent_identity;

#[cfg(feature = "iroh")]
use iroh::{
    Endpoint, SecretKey,
    endpoint::presets,
};

// The no-feature stubs below need these for their signatures.
#[cfg(not(feature = "iroh"))]
use std::path::Path;
#[cfg(not(feature = "iroh"))]
use crate::error::{SshError, SshResult};

/// ALPN advertised by `a3net-ssh` over QUIC. Distinct from
/// `a3net/frame/1` (the framed transport ALPN used by the rest
/// of A3Net) and from `iroh-ssh` (upstream) so the two stacks
/// can coexist on the same endpoint if needed.
pub const SSH_TUNNEL_ALPN: &[u8] = b"a3net/ssh-tunnel/1";

/// Builder for [`IrohSsh`]. Construct via [`IrohSshBuilder::new`].
#[cfg(feature = "iroh")]
pub struct IrohSshBuilder {
    data_dir: PathBuf,
    accept_incoming: bool,
    accept_port: u16,
    /// Optional explicit secret key. Used by tests; production
    /// callers leave this `None` so the persistent identity is
    /// loaded.
    secret_key: Option<SecretKey>,
}

#[cfg(feature = "iroh")]
impl IrohSshBuilder {
    /// Construct a builder rooted at `data_dir`. The persistent
    /// identity at `<data_dir>/iroh_secret_key` is loaded (or
    /// created) when [`Self::build`] is called.
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
            accept_incoming: false,
            accept_port: 22,
            secret_key: None,
        }
    }

    /// Whether to accept incoming SSH-tunnel connections. Off by
    /// default; matches iroh-ssh's `server` vs `server --persist`
    /// semantics — clients-only is a valid mode for a debugging
    /// session.
    pub fn accept_incoming(mut self, accept: bool) -> Self {
        self.accept_incoming = accept;
        self
    }

    /// TCP port the local SSH daemon listens on. Defaults to 22.
    /// Set this to 2222 (etc.) when running sshd on a non-default
    /// port.
    pub fn accept_port(mut self, port: u16) -> Self {
        self.accept_port = port;
        self
    }

    /// Inject an explicit secret key. Bypasses the persistent
    /// identity file. Used by tests and by callers that want a
    /// fresh ephemeral endpoint without writing to disk.
    pub fn secret_key(mut self, key: SecretKey) -> Self {
        self.secret_key = Some(key);
        self
    }

    /// Consume the builder and produce a fully wired [`IrohSsh`].
    ///
    /// On success the returned value holds an iroh `Endpoint`
    /// ready to be turned into a [`Server`](crate::server::Server)
    /// (incoming mode) or handed to the
    /// [`client::proxy`](crate::client::proxy) module (outgoing
    /// mode).
    pub async fn build(self) -> SshResult<IrohSsh> {
        // Pull the configuration out of `self` *before* the
        // secret-key resolution so we don't try to access
        // fields of a partially-moved `self`.
        let data_dir = self.data_dir;
        let accept_port = self.accept_port;
        let accept_incoming = self.accept_incoming;

        // Resolve the secret key: explicit > persistent > fresh
        // ephemeral. Persistent is the production default; tests
        // pass an explicit key to keep `/tmp` clean.
        let (secret_key, identity_path) = match self.secret_key {
            Some(k) => (k, PathBuf::new()),
            None => {
                let identity = persistent_identity(&data_dir)?;
                let path = identity.path().to_path_buf();
                (identity.secret_key(), path)
            }
        };

        // Start from the `Minimal` preset so we don't inherit
        // any discovery behaviour the caller might not want.
        // Operators that want n0-discovery or mDNS should set
        // those explicitly at a higher layer.
        let endpoint_builder = Endpoint::builder(presets::Minimal).secret_key(secret_key);

        if accept_incoming {
            // Pre-flight: make sure the local SSH daemon is
            // actually up. iroh-ssh does the same; we'd rather
            // fail at startup than discover it on the first
            // inbound stream.
            crate::server::probe_local_ssh(accept_port).await?;
        }

        let endpoint = endpoint_builder
            .bind()
            .await
            .map_err(|e| SshError::Tunnel(format!("iroh endpoint bind failed: {e}")))?;

        Ok(IrohSsh {
            endpoint: Arc::new(endpoint),
            ssh_port: accept_port,
            accept_incoming,
            identity_path,
        })
    }
}

/// Fully constructed SSH-tunnel endpoint. Cheap to clone (each
/// clone bumps an `Arc` refcount on the underlying iroh
/// `Endpoint`).
#[cfg(feature = "iroh")]
#[derive(Clone)]
pub struct IrohSsh {
    endpoint: Arc<Endpoint>,
    ssh_port: u16,
    accept_incoming: bool,
    identity_path: PathBuf,
}

#[cfg(feature = "iroh")]
impl IrohSsh {
    /// Borrow the underlying iroh `Endpoint`.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// TCP port the local SSH daemon is expected to listen on.
    pub fn ssh_port(&self) -> u16 {
        self.ssh_port
    }

    /// Whether this endpoint should accept incoming tunnel
    /// connections. Reflects the value the builder was called
    /// with; immutable after `build()` so callers can't quietly
    /// flip an outbound endpoint into an inbound one.
    pub fn accept_incoming(&self) -> bool {
        self.accept_incoming
    }

    /// Path of the persistent identity file used to construct
    /// this endpoint.
    pub fn identity_path(&self) -> &Path {
        &self.identity_path
    }
}

// -------------------------------------------------------------------------
// Non-iroh fallback: when the crate is built without the `iroh`
// feature we still want `lib.rs` to compile so docs / lints /
// the default `cargo build` keep working. The methods below are
// stubs that return [`SshError::FeatureMissing`].

#[cfg(not(feature = "iroh"))]
pub struct IrohSshBuilder;

#[cfg(not(feature = "iroh"))]
impl IrohSshBuilder {
    /// Stub used when the `iroh` feature is disabled.
    pub fn new(_data_dir: impl AsRef<Path>) -> Self {
        Self
    }

    /// Stub: no-op when the `iroh` feature is disabled.
    pub fn accept_incoming(self, _accept: bool) -> Self {
        self
    }

    /// Stub: no-op when the `iroh` feature is disabled.
    pub fn accept_port(self, _port: u16) -> Self {
        self
    }

    /// Stub: errors with [`SshError::FeatureMissing`].
    pub async fn build(self) -> SshResult<()> {
        Err(SshError::FeatureMissing)
    }
}
