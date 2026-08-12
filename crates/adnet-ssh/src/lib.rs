//! `adnet-ssh` — SSH tunneled over an iroh QUIC endpoint.
//!
//! Vendored port of [`iroh-ssh`](https://github.com/rustonbsd/iroh-ssh)
//! (MIT/Apache-2.0, © Zacharias Boehler). The upstream crate exposes a
//! minimal API that lets any machine behind a NAT/firewall be reached
//! as `user@<endpoint-id>` without port forwarding, dynamic DNS, or a
//! public IP. This crate re-implements the same idea on top of the
//! ADNet stack — iroh 1.0.3, the persistent Ed25519 identity in
//! `<data-dir>/iroh_secret_key`, and the `adnet/frame/1` ALPN we
//! already use for the rest of the protocol surface.
//!
//! # What this crate does
//!
//! - Builds an iroh `Endpoint` that speaks an SSH-tunnel ALPN and
//!   proxies each incoming QUIC stream to the local SSH daemon
//!   (default port 22, configurable via `ssh_port`).
//! - Connects out to a remote endpoint id and turns the QUIC stream
//!   into a `tokio::net::TcpStream` that the system `ssh` client can
//!   consume via `ProxyCommand`.
//! - Reuses the durable Ed25519 identity minted by
//!   [`adnet_transport::iroh::IrohIdentity`] so that the SSH
//!   endpoint id is the same value as the ADNet node id and doesn't
//!   drift across restarts.
//!
//! # What this crate does *not* do
//!
//! - It does not bundle an SSH client. The connection mode expects
//!   a system `ssh(1)` to be installed; the crate only handles the
//!   `ProxyCommand` glue (see `client::proxy`).
//! - It does not install itself as a system daemon. Service-mode
//!   (`install` / `uninstall`) is intentionally out of scope — ADNet
//!   already ships `adnet-relay` and `adnet-node` as long-running
//!   services; the `adnet-ssh server` REPL command is the supported
//!   way to run a tunnel.
//!
//! # Crate layout
//!
//! - [`error`]                 — typed [`error::SshError`].
//! - [`keys`]                  — persistent key resolution helpers
//!   (mirrors iroh-ssh's `~/.ssh/irohssh_ed25519` flow but anchored
//!   at `<data-dir>/iroh_secret_key`).
//! - [`builder`]               — the [`builder::IrohSshBuilder`] used
//!   to construct an SSH-tunnel endpoint.
//! - [`server`]                — the long-running server task
//!   ([`server::Server`]) that mirrors iroh-ssh's `server_mode`.
//! - [`client`]                — the connect-side helpers, including
//!   the `ProxyCommand` parser (`client::proxy`).
//! - [`info`]                  — the `info` command, which prints the
//!   endpoint id a friend would dial.
//!
//! # Quickstart
//!
//! ```no_run
//! # #[cfg(feature = "iroh")] {
//! use adnet_ssh::{IrohSshBuilder, server};
//!
//! # async fn demo() -> anyhow::Result<()> {
//! // Server side: bind a tunnel that forwards to the local SSH
//! // daemon on port 22. The endpoint id is derived from the
//! // persistent identity in `data_dir/iroh_secret_key`.
//! let ssh = IrohSshBuilder::new("./.adnet-data")
//!     .accept_incoming(true)
//!     .accept_port(22)
//!     .build()
//!     .await?;
//! // `Server::start` is the runtime entry point: it spawns the
//! // iroh `Router`, registers the SSH-tunnel ALPN handler, and
//! // returns a handle whose `endpoint_id()` is what you share
//! // with friends as `adnet ssh connect <user>@<id>`.
//! let server = server::Server::start(ssh).await?;
//! println!("invite: adnet-ssh alice@{}", server.endpoint_id());
//! # Ok(()) }
//! # }
//! ```
//!
//! # Runtime entry points
//!
//! - [`server::Server::start`] — the long-running server; call
//!   this once at startup and `Server::shutdown` on the way out.
//! - [`client::proxy::run`] — the ProxyCommand plumbing;
//!   wired up automatically by `adnet-cli`'s `/ssh connect`
//!   slash command.
//! - [`client::connect_with_addr`] — lower-level: returns the
//!   raw QUIC stream pair for callers that want to bridge
//!   without spawning a subprocess.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod builder;
pub mod client;
pub mod error;
pub mod info;
pub mod keys;
pub mod metrics;
pub mod server;

#[cfg(feature = "iroh")]
pub use builder::{IrohSsh, IrohSshBuilder};
#[cfg(not(feature = "iroh"))]
pub use builder::IrohSshBuilder;

pub use error::SshError;
