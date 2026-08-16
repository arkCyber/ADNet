//! `a3net daemon` — the first-class long-lived background process.
//!
//! [`DaemonSupervisor`] composes the subsystems that today are
//! scattered across the `a3net serve`, `a3net-ipc-adapter` example,
//! and `a3net_observability::http` modules into a single process
//! that:
//!
//! 1. Boots the [`Node`](a3net_node::Node) (blob store, gossip bus,
//!    transport).
//! 2. Starts the mesh HTTP server so other peers can pull blobs.
//! 3. Starts the JSON-RPC Unix socket that the CLI and external
//!    clients (TUI, Tauri, scripts) talk to.
//! 4. Optionally starts the Prometheus metrics HTTP server and an
//!    IPFS-compatible HTTP RPC server on TCP.
//! 5. Optionally starts the WAN relay server.
//! 6. Wires the legacy `{data_dir}/daemon.sock` shutdown protocol
//!    so `a3net shutdown` still works.
//! 7. Listens for SIGINT/SIGTERM/Ctrl-C.
//!
//! On any shutdown trigger, every subsystem is asked to drain in
//! the right order (mesh → IPC → metrics → relay → node) so that
//! no in-flight RPC returns an error mid-shutdown.
//!
//! See [`DaemonConfig`] for tunables and
//! [`DaemonSupervisor::run`] for the entry point used by
//! [`crate::Cmd::Daemon`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use a3net_ipc_adapter::NodeRpc;
use a3net_node::Node;
use a3net_observability::http::{MetricsServerConfig, serve as serve_metrics};
use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::watch;
use tracing::{info, warn};

/// Configuration knobs for the daemon.
///
/// `Default` is sensible for a single-user developer laptop. The
/// CLI builds a fully populated value from `Cmd::Daemon` flags.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub data_dir: PathBuf,
    /// Unix socket for the JSON-RPC server. If `None`, defaults to
    /// `{data_dir}/ipc.sock`.
    pub rpc_socket: Option<PathBuf>,
    /// HTTP RPC listen address (e.g. `127.0.0.1:11436`).
    /// `None` means "use the default `127.0.0.1:11436`";
    /// pass `Some("")` to disable.
    pub http_rpc_addr: Option<String>,
    /// Optional bearer token for HTTP RPC authentication.
    /// If set, clients must send `Authorization: Bearer <token>` header.
    pub http_rpc_auth_token: Option<String>,
    /// Optional metrics listen address. `None` means "use the
    /// default `127.0.0.1:9090`"; pass `Some("")` to disable.
    pub metrics_addr: Option<String>,
    /// Rooms to auto-join on boot. Useful for always-on lobby /
    /// presence servers.
    pub auto_join: Vec<String>,
    /// Emit a one-line JSON status line on the supervisor coming
    /// up. Useful for service managers.
    pub json_status: bool,
}

impl DaemonConfig {
    /// Resolved path of the JSON-RPC socket.
    pub fn rpc_socket_path(&self) -> PathBuf {
        self.rpc_socket
            .clone()
            .unwrap_or_else(|| self.data_dir.join("ipc.sock"))
    }

    /// Resolved HTTP RPC bind address (`127.0.0.1:11436` by default,
    /// `None` if explicitly disabled).
    pub fn http_rpc_bind(&self) -> Option<String> {
        self.http_rpc_addr
            .clone()
            .or_else(|| Some("127.0.0.1:11436".to_string()))
    }

    /// Resolved metrics bind address (`127.0.0.1:9090` by default,
    /// `None` if explicitly disabled).
    pub fn metrics_bind(&self) -> Option<String> {
        self.metrics_addr
            .clone()
            .or_else(|| Some("127.0.0.1:9090".to_string()))
    }
}

/// Composed handles returned by [`DaemonSupervisor::start`]. Held
/// by the supervisor for the duration of the run; dropped on
/// shutdown.
pub struct DaemonHandles {
    pub node: Arc<Node>,
    pub ipc: a3net_ipc::JsonRpcServerHandle,
    pub metrics: Option<a3net_observability::http::MetricsServer>,
    pub http_rpc: Option<a3net_ipc_adapter::http_rpc::HttpRpcServer>,
}

/// The supervisor: owns the shutdown signal and the composed
/// handles. [`DaemonSupervisor::run`] is the only thing the CLI
/// needs to call.
pub struct DaemonSupervisor {
    cfg: DaemonConfig,
    /// `true` ⇒ "please drain and exit". Set by signal handler,
    /// shutdown-socket handler, or fatal subsystem error.
    shutdown_tx: watch::Sender<bool>,
}

impl DaemonSupervisor {
    /// Bind the shutdown signal up front (cheap) and return the
    /// supervisor. Call [`DaemonSupervisor::run`] next.
    pub fn new(cfg: DaemonConfig) -> Self {
        // `watch::channel` allocates a watch whose initial value
        // is `false`. Subscribers can race on `changed()`; the
        // sender is held by `self` purely to keep the channel
        // alive — signal handlers (spawned in `wait_and_drain`)
        // clone it through the captured `shutdown_tx` field.
        let (shutdown_tx, _) = watch::channel(false);
        Self { cfg, shutdown_tx }
    }

    /// Entry point: build the node, start every subsystem, wait
    /// for a shutdown trigger, drain, return.
    pub async fn run(self) -> Result<()> {
        let cfg = self.cfg.clone();
        std::fs::create_dir_all(&cfg.data_dir)
            .with_context(|| format!("create data dir {}", cfg.data_dir.display()))?;

        let node_cfg = a3net_node::NodeConfig::load_or_create(&cfg.data_dir)
            .with_context(|| format!("load node config from {}", cfg.data_dir.display()))?;
        let node_id = node_cfg.node_id.clone();

        info!(
            node_id = %node_id.short(),
            data_dir = %cfg.data_dir.display(),
            "a3net daemon: booting node"
        );

        let node = Node::builder(node_cfg)
            .build()
            .await
            .context("build node")?;
        let node = Arc::new(node);

        // Bring up the mesh HTTP server before we accept any RPC
        // calls so the first `announce` can produce a real ticket.
        let ep = node
            .ensure_mesh()
            .await
            .context("start mesh HTTP server")?;
        info!(endpoint = %ep, "a3net daemon: mesh HTTP listening");

        // Auto-join any rooms requested on the CLI. Failures are
        // logged but do not abort startup — the user can `join`
        // them later over RPC.
        for room in &cfg.auto_join {
            let room_id: a3net_types::RoomId = room.as_str().into();
            if let Err(e) = node.join_room(&room_id).await {
                warn!(room = %room, error = %e, "auto-join failed");
            } else {
                info!(room = %room, "a3net daemon: auto-joined room");
            }
        }

        // Build the JSON-RPC adapter and start the Unix-socket
        // server. The adapter takes ownership of the `Node`; we
        // recover an `Arc<Node>` via `node_arc()` so the
        // supervisor can drive graceful shutdown without holding a
        // second, independent handle.
        let rpc_socket = cfg.rpc_socket_path();
        let handler = Arc::new(NodeRpc::new(node));
        let node = handler.node_arc();
        let ipc = a3net_ipc::JsonRpcServer::start(rpc_socket.clone(), handler.clone())
            .await
            .map_err(|e| anyhow!("bind JSON-RPC socket {}: {}", rpc_socket.display(), e))?;

        // HTTP RPC server — exposes the same JSON-RPC methods
        // over TCP as the Unix-socket adapter serves over `ipc.sock`.
        // Useful for non-Unix environments or scripted clients
        // (TUI, Tauri, CLI) that prefer HTTP to Unix sockets.
        // Defaults to `127.0.0.1:11436`.
        let http_rpc_handle = match cfg.http_rpc_bind() {
            Some(addr_str) if !addr_str.is_empty() => {
                let addr: std::net::SocketAddr = addr_str
                    .parse()
                    .with_context(|| format!("parse --http-rpc-addr {addr_str}"))?;
                let config = a3net_ipc_adapter::http_rpc::HttpRpcConfig {
                    auth_token: cfg.http_rpc_auth_token.clone(),
                    ..Default::default()
                };
                match a3net_ipc_adapter::http_rpc::serve_with_config(addr, handler.clone(), config).await {
                    Ok(h) => {
                        let auth_note = if cfg.http_rpc_auth_token.is_some() {
                            " (auth enabled)"
                        } else {
                            ""
                        };
                        info!(
                            "a3net daemon: HTTP RPC listening on http://{}/rpc{}",
                            h.bound_addr,
                            auth_note
                        );
                        Some(h)
                    }
                    Err(e) => {
                        warn!(addr = %addr, error = %e, "HTTP RPC server failed to start; continuing without it");
                        None
                    }
                }
            }
            _ => None,
        };

        // Optional metrics HTTP server. `Some("")` disables it
        // (matches the CLI flag convention).
        let metrics_handle = match cfg.metrics_bind() {
            Some(addr_str) if !addr_str.is_empty() => {
                let addr: std::net::SocketAddr = addr_str
                    .parse()
                    .with_context(|| format!("parse --metrics-addr {addr_str}"))?;
                match serve_metrics(MetricsServerConfig {
                    bind_addr: addr,
                    registry: None,
                })
                .await
                {
                    Ok(h) => {
                        info!(
                            "a3net daemon: metrics listening on http://{}/metrics",
                            h.local_addr()
                        );
                        Some(h)
                    }
                    Err(e) => {
                        warn!(addr = %addr, error = %e, "metrics server failed to start; continuing without it");
                        None
                    }
                }
            }
            _ => None,
        };

        // Wire up the room-event notifier so joined-room
        // announcements are pushed to every connected Unix-socket
        // client.
        handler.serve_with_notifier(ipc.notifier()).await;

        // Stash handles for shutdown.
        let handles = DaemonHandles {
            node: Arc::clone(&node),
            ipc,
            metrics: metrics_handle,
            http_rpc: http_rpc_handle,
        };

        // One-line status for service managers.
        if cfg.json_status {
            let payload = serde_json::json!({
                "nodeId": node_id.short().to_string(),
                "dataDir": cfg.data_dir.display().to_string(),
                "ipcSocket": rpc_socket.display().to_string(),
                "httpRpc": handles.http_rpc.as_ref().map(|h| h.bound_addr.to_string()),
                "metrics": handles.metrics.as_ref().map(|h| h.local_addr().to_string()),
            });
            println!("{}", serde_json::to_string(&payload)?);
        } else {
            println!(
                "a3net daemon ready — node {} ({}); ipc={}",
                node_id.short(),
                cfg.data_dir.display(),
                rpc_socket.display(),
            );
            if handles.http_rpc.is_none() && cfg.http_rpc_bind().is_some() {
                warn!(
                    "--http-rpc-addr was specified but the HTTP RPC server failed to bind"
                );
            }
        }

        self.wait_and_drain(handles).await
    }

    /// Race three shutdown triggers: Ctrl-C, the legacy
    /// `daemon.sock` shutdown request, and a fatal subsystem
    /// failure. Whichever fires first, drain and return.
    async fn wait_and_drain(self, handles: DaemonHandles) -> Result<()> {
        let DaemonHandles {
            node,
            ipc,
            metrics,
            http_rpc,
        } = handles;

        // Forward Ctrl-C into the same shutdown signal so the
        // legacy daemon.sock + Ctrl-C paths share one drain.
        let shutdown_tx = self.shutdown_tx.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ = shutdown_tx.send(true);
            }
        });

        // Forward a SIGTERM the same way for systemd /
        // launchd-style service managers. We register it
        // explicitly on Unix; on other platforms it's a no-op.
        #[cfg(unix)]
        {
            let shutdown_tx = self.shutdown_tx.clone();
            tokio::spawn(async move {
                use tokio::signal::unix::{SignalKind, signal};
                if let Ok(mut sig) = signal(SignalKind::terminate()) {
                    sig.recv().await;
                    let _ = shutdown_tx.send(true);
                }
            });
        }

        // Legacy shutdown protocol: `{data_dir}/daemon.sock` — the
        // pre-existing `Cmd::Shutdown` CLI talks to this. We park
        // it in its own task because `serve_daemon_control` blocks
        // on `listener.accept()` and cannot observe Ctrl-C on its
        // own.
        //
        // Critically, when this task receives a shutdown request
        // it must trigger the supervisor's shutdown signal so the
        // rest of `wait_and_drain` can proceed. Otherwise the
        // supervisor only wakes up on Ctrl-C / SIGTERM and the
        // legacy socket would block forever waiting for drain.
        let shutdown_tx = self.shutdown_tx.clone();
        let data_dir = self.cfg.data_dir.clone();
        let legacy_handle = tokio::spawn(async move {
            let drain = async move {
                // `serve_daemon_control` only awaits this drain
                // future after it has accepted a connection and
                // parsed a `ShutdownRequest` — so reaching here
                // means a real shutdown was requested over the
                // socket. Wake the supervisor before the future
                // resolves so the rest of `wait_and_drain` runs.
                let _ = shutdown_tx.send(true);
                Ok::<(), anyhow::Error>(())
            };
            crate::daemon_ctl::serve_daemon_control(data_dir, drain).await
        });

        // Wait for the first shutdown signal.
        let mut rx = self.shutdown_tx.subscribe();
        let _ = rx.changed().await;
        info!("a3net daemon: shutdown signal received, draining");

        // The legacy `daemon.sock` task, if it has accepted a
        // connection, is currently parked on its `drain` future
        // waiting for the shutdown signal. Once it sees the
        // signal it writes the ShutdownAck and removes the
        // socket file. We must NOT abort it now — that would
        // skip the ack and break `a3net shutdown`'s contract.
        //
        // We *do* want to abort it if nobody is connected (the
        // listener.accept() future has nothing to do), because
        // otherwise this task would never exit. The cleanest
        // signal is: wait for the task with a short grace period
        // via a `tokio::select!`, then abort.
        let legacy_grace = std::time::Duration::from_secs(2);
        let aborted;
        {
            // Take the handle by move into the select branch so
            // we never re-poll a completed JoinHandle (which
            // would panic in tokio 1.53+).
            let mut h = legacy_handle;
            tokio::select! {
                res = &mut h => {
                    match res {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => warn!(error = %e, "daemon control socket task ended with error"),
                        Err(e) if e.is_cancelled() => {}
                        Err(e) => warn!(error = %e, "daemon control socket task panicked"),
                    }
                    aborted = false;
                }
                _ = tokio::time::sleep(legacy_grace) => {
                    h.abort();
                    aborted = true;
                }
            }
        }
        if aborted {
            warn!(
                "daemon control socket did not exit within {:?}; aborted",
                legacy_grace,
            );
        }

        // Drain in order: stop accepting new RPC → HTTP RPC →
        // metrics → mesh → node. The order matters because RPC
        // handlers may touch the blobstore / node, so we tear it
        // down last.
        ipc.shutdown();

        if let Some(h) = http_rpc {
            h.shutdown();
        }

        if let Some(m) = metrics {
            // The metrics handle's Drop impl stops the axum task.
            drop(m);
        }

        // Tear down the mesh + relay + transport by calling
        // `Node::shutdown` (which already leaves every joined room
        // and stops every ensure_* subsystem in the right order).
        // `Node::shutdown` is idempotent in the sense that calling
        // it twice is a no-op for the daemon — but it returns an
        // `Err` if the mesh/relay handle was already taken. We
        // tolerate that as long as the daemon exits cleanly.
        if let Err(e) = node.shutdown().await {
            warn!(error = %e, "node shutdown reported an error");
        }

        // The legacy task's handle was consumed by the
        // select! above, so nothing more to do here.
        let _ = aborted; // keep variable alive for clarity

        info!("a3net daemon: drained cleanly, exiting");
        Ok(())
    }
}

/// Helper: check that the IPC socket path is not already in use
/// before we start the supervisor. We refuse rather than silently
/// steal a live daemon's socket — the operator can either pick a
/// new path or `a3net shutdown` the existing one.
pub fn preflight_socket(path: &Path) -> Result<()> {
    if path.exists() {
        bail!(
            "refusing to bind {} — already exists. Either another \
             `a3net daemon` is running, or a previous instance crashed \
             without cleaning up. Run `a3net shutdown` or remove the \
             file manually.",
            path.display()
        );
    }
    Ok(())
}

/// Convenience entry point used by the CLI dispatcher.
pub async fn run(cfg: DaemonConfig) -> Result<()> {
    if let Some(path) = &cfg.rpc_socket {
        preflight_socket(path).map_err(|e| anyhow!("preflight: {e}"))?;
    } else {
        preflight_socket(&cfg.rpc_socket_path())
            .map_err(|e| anyhow!("preflight: {e}"))?;
    }
    DaemonSupervisor::new(cfg).run().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_socket_path_defaults_under_data_dir() {
        let cfg = DaemonConfig {
            data_dir: PathBuf::from("/tmp/x"),
            rpc_socket: None,
            http_rpc_addr: None,
            http_rpc_auth_token: None,
            metrics_addr: None,
            auto_join: vec![],
            json_status: false,
        };
        assert_eq!(cfg.rpc_socket_path(), PathBuf::from("/tmp/x/ipc.sock"));
    }

    #[test]
    fn rpc_socket_path_respects_explicit_override() {
        let cfg = DaemonConfig {
            data_dir: PathBuf::from("/tmp/x"),
            rpc_socket: Some(PathBuf::from("/var/run/custom.sock")),
            http_rpc_addr: None,
            http_rpc_auth_token: None,
            metrics_addr: None,
            auto_join: vec![],
            json_status: false,
        };
        assert_eq!(
            cfg.rpc_socket_path(),
            PathBuf::from("/var/run/custom.sock")
        );
    }

    #[test]
    fn http_rpc_bind_defaults_to_localhost_11436() {
        let cfg = DaemonConfig {
            data_dir: PathBuf::from("/tmp/x"),
            rpc_socket: None,
            http_rpc_addr: None,
            http_rpc_auth_token: None,
            metrics_addr: None,
            auto_join: vec![],
            json_status: false,
        };
        assert_eq!(cfg.http_rpc_bind(), Some("127.0.0.1:11436".into()));
    }

    #[test]
    fn explicit_http_rpc_addr_wins_over_default() {
        let cfg = DaemonConfig {
            data_dir: PathBuf::from("/tmp/x"),
            rpc_socket: None,
            http_rpc_addr: Some("0.0.0.0:11437".into()),
            http_rpc_auth_token: None,
            metrics_addr: None,
            auto_join: vec![],
            json_status: false,
        };
        assert_eq!(cfg.http_rpc_bind(), Some("0.0.0.0:11437".into()));
    }

    #[test]
    fn empty_http_rpc_addr_disables_http_rpc() {
        let cfg = DaemonConfig {
            data_dir: PathBuf::from("/tmp/x"),
            rpc_socket: None,
            http_rpc_addr: Some("".into()),
            http_rpc_auth_token: None,
            metrics_addr: None,
            auto_join: vec![],
            json_status: false,
        };
        // Empty string means disabled - same pattern as metrics
        assert_eq!(cfg.http_rpc_bind(), Some("".into()));
    }

    #[test]
    fn metrics_bind_defaults_to_localhost_9090() {
        let cfg = DaemonConfig {
            data_dir: PathBuf::from("/tmp/x"),
            rpc_socket: None,
            http_rpc_addr: None,
            http_rpc_auth_token: None,
            metrics_addr: None,
            auto_join: vec![],
            json_status: false,
        };
        assert_eq!(cfg.metrics_bind(), Some("127.0.0.1:9090".into()));
    }

    #[test]
    fn explicit_metrics_addr_wins_over_default() {
        let cfg = DaemonConfig {
            data_dir: PathBuf::from("/tmp/x"),
            rpc_socket: None,
            http_rpc_addr: None,
            http_rpc_auth_token: None,
            metrics_addr: Some("0.0.0.0:9100".into()),
            auto_join: vec![],
            json_status: false,
        };
        assert_eq!(cfg.metrics_bind(), Some("0.0.0.0:9100".into()));
    }

    #[test]
    fn empty_metrics_addr_disables_metrics() {
        let cfg = DaemonConfig {
            data_dir: PathBuf::from("/tmp/x"),
            rpc_socket: None,
            http_rpc_addr: None,
            http_rpc_auth_token: None,
            metrics_addr: Some("".into()),
            auto_join: vec![],
            json_status: false,
        };
        // The empty-string sentinel is consumed by the caller, not
        // here. The supervisor treats Some("") as "disabled".
        assert_eq!(cfg.metrics_bind(), Some("".into()));
    }

    #[test]
    fn preflight_socket_rejects_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ipc.sock");
        std::fs::write(&p, b"stale").unwrap();
        let err = preflight_socket(&p).unwrap_err().to_string();
        assert!(err.contains("already exists"));
    }

    #[test]
    fn preflight_socket_accepts_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ipc.sock");
        assert!(preflight_socket(&p).is_ok());
    }

    // The end-to-end smoke test that actually exercises the
    // supervisor lives in `tests/daemon_smoke.rs` and runs the
    // built `a3net` binary as a child process. In-process coverage
    // here is restricted to configurator paths because the
    // supervisor's shutdown signal is private and a unit-test
    // join handle cannot inject Ctrl-C / SIGTERM.
}
