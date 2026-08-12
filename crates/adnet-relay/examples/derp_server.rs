//! `adnet-relay` self-hosted DERP entrypoint.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p adnet-relay --example derp_server --features derp
//! ```
//!
//! This binary is the **self-hosted DERP server** counterpart to
//! the `iroh-relay` example. Operators deploy it on a public host
//! (a VPS, a Kubernetes pod, an EC2 instance) and point their
//! ADNet nodes at it via `relay_base_url`. The server speaks the
//! upstream `iroh-relay` wire protocol, so any iroh / ADNet
//! client can connect to it.
//!
//! ## What this example does
//!
//! - Loads `DerpConfig` from `--config <path>` if provided, else
//!   falls back to a development default that binds on
//!   `127.0.0.1:7842` with no TLS. **Do not** run the dev default
//!   in production — it's plaintext and won't authenticate clients.
//! - Spawns the embedded `DerpServer` via
//!   `adnet_relay::derp::DerpServer::spawn(cfg)`.
//! - Blocks on `tokio::signal::ctrl_c()` for graceful shutdown.
//! - On shutdown, calls `DerpServer::shutdown()` which flushes the
//!   upstream `IrohRelayServer`'s supervisor task and returns.
//!
//! ## Production hardening checklist
//!
//! 1. Set `tls = LetsEncrypt { contact, dir }` or `tls = Manual {
//!    cert, key }` — see `adnet_relay::derp::DerpTlsConfig`.
//! 2. Pin the bind address to a public interface (not `127.0.0.1`).
//! 3. Put it behind a reverse proxy (Caddy, nginx) if you want
//!    path-level filtering, rate limits, or geo-routing.
//! 4. Wire the relay logs into your observability stack — the
//!    upstream `iroh-relay` server already emits `tracing` events.
//! 5. Set `access` to an `Allow { ips }` list if you want to
//!    restrict which clients can dial the relay.
//!
//! ## Why an example and not a separate crate
//!
//! The `iroh-relay` upstream project distributes its server as a
//! binary in the `iroh-relay` crate. Mirroring that pattern,
//! `adnet-relay` exposes the server as a public example rather
//! than a separate crate — operators who want a "fat binary"
//! self-host daemon can copy this file into their own
//! `adnet-selfhost` crate and add HTTP admin endpoints, ACME
//! renewal jobs, etc. on top.

use std::path::PathBuf;

use adnet_relay::derp::{DerpConfig, DerpServer, DerpServerHandle};
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialise tracing. Operators can override the filter via
    // the standard `RUST_LOG` env var. We deliberately do **not**
    // initialise a JSON formatter — the upstream `iroh-relay`
    // events look better in the default compact form.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,iroh_relay=info")),
        )
        .init();

    // Load the config. Operators can pass `--config /etc/adnet/derp.json`
    // to override; otherwise we fall back to a development default
    // that binds on `[::]:80` with no TLS. The default is
    // intentionally non-production — see the doc comment at the
    // top of this file for the hardening checklist.
    let cfg = match parse_args() {
        Some(path) => {
            info!(config = %path.display(), "loading DERP config from disk");
            load_config_from_file(&path).unwrap_or_else(|e| {
                panic!("load DerpConfig from {}: {e}", path.display());
            })
        }
        None => {
            info!("no --config supplied; using dev default ([::]:80 plaintext)");
            DerpConfig::default()
        }
    };

    let bind = cfg.http_bind_addr;
    let server = DerpServer::spawn(cfg)
        .await
        .unwrap_or_else(|e| panic!("spawn DERP server on {bind}: {e:?}"));

    let info = server.handle().info();
    info!(
        http_addr   = ?info.http_addr,
        https_addr  = ?info.https_addr,
        quic_addr   = ?info.quic_addr,
        primary_url = ?server.handle().primary_url(),
        "DERP server up; press Ctrl-C to stop"
    );

    // Block on Ctrl-C, then gracefully shut down. The upstream
    // `iroh-relay` server has its own supervisor task that needs
    // a clean handoff; just dropping the `DerpServer` would
    // detach the task and leave the socket in TIME_WAIT for ~60s.
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Ctrl-C received, shutting down DERP server");
        }
        res = wait_for_shutdown_signal() => {
            if let Err(e) = res {
                error!(error = %e, "shutdown signal handler failed");
            }
        }
    }

    if let Err(e) = server.shutdown().await {
        error!(error = ?e, "DERP shutdown error");
    }
    info!("DERP server stopped");
    Ok(())
}

/// Parse `argv` for `--config <path>`. We avoid pulling in `clap`
/// to keep the example footprint small — operators who want
/// richer CLI ergonomics can copy this file and add clap.
fn parse_args() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => return args.next().map(PathBuf::from),
            "-c" => return args.next().map(PathBuf::from),
            _ => {}
        }
    }
    None
}

/// Helper: future that completes on either Ctrl-C or SIGTERM. The
/// `tokio::signal::unix::signal(SIGTERM)` path is gated on Unix;
/// on Windows we only listen for Ctrl-C.
async fn wait_for_shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate())?;
        sigterm.recv().await;
        info!("SIGTERM received");
    }
    #[cfg(not(unix))]
    {
        // On non-Unix the Ctrl-C handler is already wired by
        // `tokio::signal::ctrl_c`; this future never resolves
        // and the outer `select!` falls through to the Ctrl-C
        // branch.
        std::future::pending::<()>().await;
    }
    Ok(())
}

#[allow(dead_code)]
fn _typecheck_handle(handle: DerpServerHandle) -> DerpServerHandle {
    handle
}

/// Read a [`DerpConfig`] from a JSON file. The file is expected
/// to contain a top-level object with the same camelCase fields
/// the `serde` rename produces — e.g.:
/// ```json
/// {
///   "httpBindAddr": "0.0.0.0:443",
///   "tls": { "manual": { "cert": "/etc/adnet/fullchain.pem", "key": "/etc/adnet/privkey.pem" } },
///   "access": { "allow": [{ "endpointId": "..." }] }
/// }
/// ```
fn load_config_from_file(path: &std::path::Path) -> Result<DerpConfig, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("read config file {}: {e}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|e| format!("parse DerpConfig JSON at {}: {e}", path.display()))
}
