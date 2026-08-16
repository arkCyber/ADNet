//! `a3net-vless-client` — VLESS/V2Ray-style VPN client for A3Net.
//!
//! ## What this crate is
//!
//! A **client-side** VPN client. It lets a process on the host open a
//! local SOCKS5 (and HTTP-CONNECT) proxy whose outbound traffic is
//! tunnelled through a remote VLESS endpoint — the same protocol
//! spoken by [`v2ray-core`](https://github.com/v2fly/v2ray-core),
//! [`Xray-core`](https://github.com/XTLS/Xray-core), and
//! [`sing-box`](https://sing-box.sagernet.org/).
//!
//! ## What this crate is NOT
//!
//! - It does **not** speak VLESS frames itself. The VLESS protocol is
//!   framed on top of TLS and brings with it a long tail of optional
//!   transport features (XTLS-Vision, REALITY, WebSocket, gRPC, ...).
//!   Re-implementing all of that in Rust would duplicate the work that
//!   the Xray / v2ray / sing-box projects already maintain.
//! - It does **not** embed the Go runtime. Cross-language FFI between
//!   Rust and Go's runtime (goroutines, GC, stack growth) is fragile
//!   and the upstream projects do not support it.
//!
//! Instead, this crate **spawns an external xray / sing-box subprocess**
//! and configures it programmatically. The subprocess terminates VLESS
//! on the wire; the Rust side owns the lifecycle, configuration, and
//! the local proxy the user's apps connect to.
//!
//! ## Layering
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────────┐
//! │  User apps (curl, browser, …)                                     │
//! └───────────────────────────────────────┬───────────────────────────┘
//!                                         │  SOCKS5 / HTTP CONNECT
//! ┌───────────────────────────────────────▼───────────────────────────┐
//! │  a3net-vless-client (this crate)                                  │
//! │   ├─ link::parse       — vless://… URI parser                     │
//! │   ├─ proxy::Socks5Server — local SOCKS5/HTTP listener              │
//! │   ├─ proxy::HttpServer   — HTTP-CONNECT listener (for curl -x)    │
//! │   └─ subprocess::Backend — wraps xray / sing-box over stdio       │
//! └───────────────────────────────────────┬───────────────────────────┘
//!                                         │  JSON config (stdin)
//!                                         │  xray API gRPC (optional)
//! ┌───────────────────────────────────────▼───────────────────────────┐
//! │  xray-core / sing-box subprocess                                  │
//! │   └─ speaks VLESS over TCP+TLS to the remote endpoint             │
//! └───────────────────────────────────────┬───────────────────────────┘
//!                                         │  VLESS over TLS
//! ┌───────────────────────────────────────▼───────────────────────────┐
//! │  Remote v2ray / Xray / sing-box server                             │
//! └───────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Why a subprocess, not FFI
//!
//! - The VLESS wire format is stable, but the **feature surface** of an
//!   Xray/sing-box config (TLS fingerprints, mux, transport plugins,
//!   routing rules, REALITY) is moving fast and is implemented in Go.
//!   Re-implementing it would be a permanent maintenance burden.
//! - Xray and sing-box already ship first-class JSON / stdin
//!   configuration and a gRPC API for runtime control. Wrapping them
//!   as subprocesses is the officially supported integration model.
//! - Go's runtime (goroutines, GC) does not play well with C-ABI
//!   callers. There are no known successful long-running Rust ↔
//!   Go FFI integrations for xray-core.
//!
//! ## Scope of this initial version
//!
//! - **Client only.** This crate never *terminates* VLESS itself; it
//!   only configures and supervises a subprocess that does.
//! - **All transports**: TCP, WebSocket, HTTP/2, gRPC, KCP/mKCP.
//! - **All security layers**: plain, TLS, XTLS-Vision, REALITY.
//! - **Local SOCKS5 and HTTP-CONNECT** outbound proxy.
//! - **Standalone link parsing** — `link::parse("vless://…")` returns a
//!   [`link::VlessLink`] without ever spawning anything, so the CLI can
//!   display the parsed view to the user before committing to a
//!   connection.
//!
//! ## Example
//!
//! ```rust,no_run
//! use a3net_vless_client::{VlessLink, VlessClient, VlessClientConfig};
//!
//! # async fn demo() -> anyhow::Result<()> {
//! let link = VlessLink::parse(
//!     "vless://11111111-1111-1111-1111-111111111111@example.com:443\
//!     ?security=tls&sni=example.com&type=tcp#mynode",
//! )?;
//! let cfg = VlessClientConfig {
//!     link,
//!     listen_socks5: "127.0.0.1:1080".parse()?,
//!     listen_http:   None,
//!     backend:       a3net_vless_client::subprocess::BackendKind::AutoDetect,
//!     log_level:     "warn".into(),
//!     grace:         None,
//! };
//! let client = VlessClient::start(cfg).await?;
//! // … use 127.0.0.1:1080 as a SOCKS5 proxy …
//! client.shutdown().await?;
//! # Ok(()) }
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod error;
pub mod link;
pub mod subprocess;
pub mod proxy;
pub mod client;

pub use client::{VlessClient, VlessClientConfig, VlessClientHandle};
pub use error::{VlessClientError, VlessClientResult};
pub use link::{VlessLink, VlessTransport, VlessTls, VlessFlow};
pub use subprocess::{BackendKind, BackendHandle, probe_for_test};
