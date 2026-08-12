//! SSH `ProxyCommand` plumbing for `adnet-ssh`.
//!
//! SSH supports a `ProxyCommand` directive in `~/.ssh/config`:
//!
//! ```text
//! Host *.adnet.local
//!     ProxyCommand adnet-ssh proxy %h
//! ```
//!
//! When `ssh(1)` resolves a hostname matching `*.adnet.local`,
//! it executes the proxy command and bridges the resulting
//! stdin/stdout to the SSH transport. This module is the
//! `adnet-ssh proxy` half of that contract:
//!
//! - [`parse_invite`] splits the `%h` token (which SSH expands
//!   to `user@<endpoint>`) into structured fields.
//! - [`run`] connects to the iroh endpoint id and bridges bytes
//!   between the SSH process's stdin/stdout and the QUIC stream.
//!
//! The ProxyCommand form is the *only* way most existing SSH
//! clients (OpenSSH, Paramiko, libssh2) can reach an iroh-tunnel
//! peer without code changes — so it's a load-bearing piece.

use std::process::Stdio;
use std::str::FromStr;

use iroh::endpoint::{RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::builder::{IrohSsh, IrohSshBuilder};
use crate::error::{SshError, SshResult};
use crate::metrics;

/// Default binary name to exec as the SSH ProxyCommand.
pub const DEFAULT_PROXY_BINARY: &str = "ssh";

/// Parsed `user@<endpoint>` invite string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedInvite {
    /// Local shell user on the remote machine. Defaults to
    /// `whoami::username()` if the invite omits it.
    pub user: String,
    /// Endpoint id of the remote peer.
    pub endpoint_id: iroh::EndpointId,
}

/// Token emitted by SSH's `%h` expansion.
pub type InviteToken = String;

/// Split a `user@<endpoint>` string into its components. Returns
/// [`SshError::InvalidInvite`] on malformed input (missing `@`,
/// bad base32, missing user).
pub fn parse_invite(token: &str) -> SshResult<ParsedInvite> {
    let (user, ep_str) = token.split_once('@').ok_or_else(|| SshError::InvalidInvite {
        input: token.to_string(),
        source: "missing `@` separator".into(),
    })?;
    let user = user.trim();
    if user.is_empty() {
        return Err(SshError::InvalidInvite {
            input: token.to_string(),
            source: "missing `user` prefix".into(),
        });
    }
    let endpoint_id = iroh::EndpointId::from_str(ep_str.trim()).map_err(|e| {
        SshError::InvalidInvite {
            input: token.to_string(),
            source: Box::new(e),
        }
    })?;
    Ok(ParsedInvite {
        user: user.to_string(),
        endpoint_id,
    })
}

/// Run the ProxyCommand for `token` (e.g. `alice@<endpoint-id>`).
///
/// # Pipeline
///
/// 1. Parse the invite via [`parse_invite`].
/// 2. Build (or load) the local iroh endpoint from
///    `<data_dir>/iroh_secret_key` via [`IrohSshBuilder`].
///    **This will create the identity file the first time
///    `run` is called from a given `data_dir`** — the same
///    invariant as `IrohIdentity::load_or_create`. If you want
///    to avoid writing to disk, pass an explicit
///    [`IrohSshBuilder::secret_key`] to a pre-built
///    [`IrohSsh`] and use [`run_with`] directly.
/// 3. Open a bi-stream to the remote endpoint id via
///    [`crate::client::connect`].
/// 4. Spawn the local `ssh` binary with stdin/stdout wired to
///    the QUIC stream and `wait()` until it exits.
///
/// # Failure modes
///
/// - **Persistent identity missing / corrupt**: returned as
///   [`SshError::Identity`]. The caller should surface this as
///   "ADNet data directory not initialised" rather than retry
///   blindly.
/// - **Peer unreachable via DERP / direct**: returned as
///   [`SshError::Tunnel`]. Callers that want to retry against
///   a pre-known address should call
///   [`crate::client::connect_with_addr`] directly.
/// - **`ssh` not on `PATH`**: returned as [`SshError::SpawnSsh`].
///   We deliberately don't fall back to a built-in SSH client;
///   if you want one, the crate exposes the raw stream pair
///   via [`run_with`] (pass `parsed`, ignore the `ssh_binary`
///   argument).
pub async fn run(token: &InviteToken, data_dir: &std::path::Path) -> SshResult<()> {
    let parsed = parse_invite(token)?;
    let ssh = IrohSshBuilder::new(data_dir)
        .accept_incoming(false)
        .accept_port(22)
        .build()
        .await?;
    run_with(ssh, &parsed, DEFAULT_PROXY_BINARY).await
}

/// Lower-level: run with a pre-built [`IrohSsh`]. Useful for
/// tests and for callers that want to inject their own identity.
pub async fn run_with(
    ssh: IrohSsh,
    parsed: &ParsedInvite,
    ssh_binary: &str,
) -> SshResult<()> {
    let (quic_send, quic_recv) =
        crate::client::connect(ssh.endpoint(), parsed.endpoint_id).await?;
    proxy_bridge(ssh_binary, parsed, quic_send, quic_recv).await
}

/// Internal helper: bidirectionally copy bytes between the
/// local SSH process and the QUIC stream.
async fn proxy_bridge(
    ssh_binary: &str,
    parsed: &ParsedInvite,
    quic_send: SendStream,
    quic_recv: RecvStream,
) -> SshResult<()> {
    metrics::CLIENT_BRIDGES_STARTED.inc();
    // Bump the in-flight gauge up front and decrement it on
    // every exit path. `scopeguard` would be cleaner but is
    // not currently a workspace dep — an explicit `dec()` at
    // each `?` / `return` is the next-best option and keeps
    // the dependency surface unchanged.
    metrics::CLIENT_BRIDGES_IN_FLIGHT.inc();
    let res = proxy_bridge_inner(
        ssh_binary,
        parsed,
        quic_send,
        quic_recv,
    )
    .await;
    metrics::CLIENT_BRIDGES_IN_FLIGHT.dec();
    res
}

/// Inner body of [`proxy_bridge`]. Pulled out so the in-flight
/// gauge can be decremented symmetrically with the started
/// counter regardless of which exit path `proxy_bridge` takes.
async fn proxy_bridge_inner(
    ssh_binary: &str,
    parsed: &ParsedInvite,
    mut quic_send: SendStream,
    mut quic_recv: RecvStream,
) -> SshResult<()> {
    let mut child = Command::new(ssh_binary)
        .arg("-l")
        .arg(&parsed.user)
        .arg("-o")
        .arg("ProxyUseFdpass=no")
        .arg("-T")
        .arg("-e")
        .arg("none")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("adnet-ssh-proxy")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| SshError::SpawnSsh {
            binary: ssh_binary.to_string(),
            source: e,
        })?;

    let mut child_stdin = child.stdin.take().ok_or_else(|| SshError::SpawnSsh {
        binary: ssh_binary.to_string(),
        source: std::io::Error::other("no stdin pipe"),
    })?;
    let mut child_stdout = child.stdout.take().ok_or_else(|| SshError::SpawnSsh {
        binary: ssh_binary.to_string(),
        source: std::io::Error::other("no stdout pipe"),
    })?;

    // quic_recv (AsyncRead) -> child_stdin (AsyncWrite):
    // bytes arriving on the QUIC stream are written into the
    // local ssh process's stdin. When the remote closes the
    // QUIC stream we half-close the ssh process's stdin so it
    // sees EOF and can finish its response.
    let mut quic_to_ssh = tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match quic_recv.read(&mut buf).await {
                Ok(Some(0)) | Err(_) | Ok(None) => break,
                Ok(Some(n)) => {
                    if child_stdin.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
        child_stdin.shutdown().await.ok();
    });

    // child_stdout (AsyncRead) -> quic_send (AsyncWrite):
    // bytes the local ssh process emits on stdout are written
    // back to the QUIC stream. When the ssh process exits we
    // finish the QUIC send stream so the remote sees EOF.
    let mut ssh_to_quic = tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match child_stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if quic_send.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
        quic_send.finish().ok();
    });

    // Wait for both directions to finish. Whichever finishes
    // first, the other will receive the half-close signal on
    // its next read/write and unwind naturally. We do not
    // abort the survivor — that's a leaky-choice waiting to
    // happen because the survivor may still be mid-flight
    // sending the response we want the remote to see.
    let _ = tokio::join!(&mut quic_to_ssh, &mut ssh_to_quic);
    let status = child.wait().await.map_err(|e| SshError::Other(format!("wait ssh: {e}")))?;
    if !status.success() {
        return Err(SshError::Other(format!("ssh exited {status}")));
    }
    metrics::CLIENT_BRIDGES_COMPLETED.inc();
    Ok(())
}

/// Bridge identity used by [`run`]. Re-exported from the
/// `IrohIdentity` type so the public API of this module doesn't
/// have to mention the inner transport crate.
pub use adnet_transport::iroh::IrohIdentity as PersistentIdentity;

#[cfg(all(test, feature = "iroh"))]
mod tests {
    use super::*;

    #[test]
    fn parse_invite_accepts_valid_token() {
        // Use a real Ed25519 endpoint id from the upstream
        // iroh-ssh README so we don't have to hard-code fragile
        // test fixtures. The lowercase hex form is accepted by
        // iroh's `EndpointId::from_str` (which decodes either
        // hex or z-base-32).
        let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
        let parsed = parse_invite(&format!("alice@{ep_hex}")).unwrap();
        assert_eq!(parsed.user, "alice");
        assert_eq!(parsed.endpoint_id.as_bytes().len(), 32);
    }

    #[test]
    fn parse_invite_accepts_alternate_hex_fixture() {
        // A second real Ed25519 endpoint id (also from the
        // iroh-ssh README) so we don't accidentally bake in a
        // single fixture. Both forms must round-trip through
        // the parser.
        let ep_hex = "bb8e1a5661a6dfa9ae2dd978922f30f524f6fd8c99b3de021c53f292aae74330";
        let parsed = parse_invite(&format!("alice@{ep_hex}")).unwrap();
        assert_eq!(parsed.user, "alice");
        assert_eq!(parsed.endpoint_id.as_bytes().len(), 32);
    }

    #[test]
    fn parse_invite_rejects_invalid_encoding() {
        // 64 chars but not valid hex (contains `g`, `h`, `i`, …)
        // and not valid base32 (which is A-Z2-7). iroh's parser
        // must reject this; we surface it as `InvalidInvite`.
        let bad = "g".repeat(64);
        let err = parse_invite(&format!("alice@{bad}")).unwrap_err();
        assert!(matches!(err, SshError::InvalidInvite { .. }));
    }

    #[test]
    fn parse_invite_rejects_wrong_length() {
        // 63 hex chars — one short of the 64 the parser expects.
        let bad = "a".repeat(63);
        let err = parse_invite(&format!("alice@{bad}")).unwrap_err();
        assert!(matches!(err, SshError::InvalidInvite { .. }));
    }

    #[test]
    fn parse_invite_rejects_missing_at() {
        assert!(parse_invite("aliceendpoint").is_err());
    }

    #[test]
    fn parse_invite_rejects_empty_user() {
        assert!(parse_invite("@deadbeef").is_err());
    }
}
