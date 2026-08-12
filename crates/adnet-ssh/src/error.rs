//! Typed error for `adnet-ssh`.
//!
//! Mirrors the `anyhow::Result` style of the upstream `iroh-ssh`
//! crate at the call sites, but uses a typed enum on the public
//! boundary so callers can pattern-match on the failure mode (e.g.
//! distinguish "SSH daemon isn't running" from "iroh refused to
//! bind"). Anything iroh-internal that we don't translate falls
//! through to [`SshError::Other`].

use thiserror::Error;

/// All errors produced by the SSH-over-iroh tunnel.
#[derive(Debug, Error)]
pub enum SshError {
    /// The persistent iroh identity could not be loaded or
    /// generated. The wrapped path is the file we tried to read.
    #[error("failed to load iroh identity at {path}: {source}")]
    Identity {
        /// Path of the identity file we attempted to load.
        path: String,
        /// Underlying cause (typically `std::io::Error`).
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The local SSH daemon did not accept a TCP connection within
    /// the configured timeout. iroh-ssh calls this `no ssh server
    /// available on specified port`.
    #[error("no SSH server available on port {port}; ensure sshd is installed and listening")]
    NoSshServer {
        /// TCP port we probed.
        port: u16,
    },

    /// The user-supplied `user@<endpoint>` token could not be
    /// parsed (missing `@`, bad base32, missing `user`, etc.).
    #[error("invalid `user@<endpoint>` invite: {input:?}")]
    InvalidInvite {
        /// The raw invite string the caller handed us.
        input: String,
        /// Why parsing failed.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// We couldn't spawn the local `ssh` subprocess on the
    /// `ProxyCommand` path. This usually means `ssh(1)` is missing
    /// from `PATH`.
    #[error("failed to spawn local ssh binary `{binary}`: {source}")]
    SpawnSsh {
        /// Path of the binary we tried to invoke.
        binary: String,
        #[source]
        source: std::io::Error,
    },

    /// The QUIC stream between client and server failed mid-flight.
    #[error("iroh tunnel stream failed: {0}")]
    Tunnel(String),

    /// Caller asked for an operation that requires the `iroh`
    /// cargo feature but built the crate without it.
    #[error("adnet-ssh was built without the `iroh` feature; rebuild with `-p adnet-ssh --features iroh`")]
    FeatureMissing,

    /// Catch-all for errors we deliberately do not translate.
    /// The wrapped string is a one-line description suitable for
    /// logs.
    #[error("adnet-ssh: {0}")]
    Other(String),
}

/// Crate-local convenience alias.
pub type SshResult<T> = Result<T, SshError>;

/// `?`-propagation for `writeln!` / `write!` into a `String`.
///
/// In practice `String`'s `fmt::Write` impl never returns `Err`,
/// so this conversion only fires when a caller passes a custom
/// `fmt::Write` impl (e.g. a `Vec<u8>`-backed buffer) into
/// `info::render_invite`. We keep the surface narrow: just the
/// `fmt::Error` mapping, not a blanket `From<E: Error>`.
impl From<std::fmt::Error> for SshError {
    fn from(e: std::fmt::Error) -> Self {
        SshError::Other(format!("fmt::Write failed: {e}"))
    }
}
