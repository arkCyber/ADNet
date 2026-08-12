//! Connect-side helpers for `adnet-ssh`.
//!
//! Two public surfaces:
//!
//! - [`proxy`] — parses an `user@<endpoint>` invite into an iroh
//!   connection and bridges it to the local `ssh(1)` process via
//!   SSH's `ProxyCommand` mechanism. This is what the upstream
//!   `iroh-ssh proxy` subcommand does, and it is the form mobile
//!   and CI shells want to integrate with.
//! - [`connect`] — a higher-level helper that opens a bi-stream
//!   directly. Useful for embedded callers that don't want to
//!   shell out to `ssh`.
//!
//! Both surfaces assume the iroh endpoint has already been built
//! from the same persistent identity the server side uses; the
//! builder is in [`crate::builder`].

#[cfg(feature = "iroh")]
pub mod proxy;

#[cfg(feature = "iroh")]
pub use proxy::{
    InviteToken, ParsedInvite, SftpConfig, DEFAULT_SFTP_BINARY, parse_invite, run_sftp,
    run_sftp_with,
};
#[cfg(feature = "iroh")]
use iroh::endpoint::{RecvStream, SendStream};

/// Open a single bi-directional QUIC stream to the given
/// endpoint id and return the `(send, recv)` pair ready to be
/// bridged to a `tokio::net::TcpStream` (or any other local
/// socket).
///
/// **Discovery**: `EndpointId` alone is enough for iroh to
/// resolve the peer via the configured discovery layer (DERP
/// relay, mDNS, pkarr, …). For hermetic tests where those
/// layers are unavailable, prefer [`connect_with_addr`] and
/// pass the peer's [`iroh::EndpointAddr`] directly.
#[cfg(feature = "iroh")]
pub async fn connect(
    endpoint: &iroh::Endpoint,
    target: iroh::EndpointId,
) -> Result<(SendStream, RecvStream), crate::error::SshError> {
    let addr = iroh::EndpointAddr::new(target);
    connect_with_addr(endpoint, addr).await
}

/// Open a bi-directional QUIC stream to a fully-resolved
/// endpoint address. Useful when the caller has the peer's
/// [`iroh::EndpointAddr`] (e.g. from a ticket) and wants to
/// skip discovery.
#[cfg(feature = "iroh")]
pub async fn connect_with_addr(
    endpoint: &iroh::Endpoint,
    addr: iroh::EndpointAddr,
) -> Result<(SendStream, RecvStream), crate::error::SshError> {
    let conn = endpoint
        .connect(addr, crate::builder::SSH_TUNNEL_ALPN)
        .await
        .map_err(|e| crate::error::SshError::Tunnel(format!("connect: {e}")))?;
    let (send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| crate::error::SshError::Tunnel(format!("open_bi: {e}")))?;
    Ok((send, recv))
}
