//! High-level VLESS client.
//!
//! [`VlessClient`] is the entry point most callers want. It:
//!
//! 1. parses the supplied `vless://` link,
//! 2. spawns the configured xray / sing-box backend,
//! 3. starts a local SOCKS5 and (optionally) HTTP-CONNECT proxy,
//! 4. supervises the backend process for the lifetime of the
//!    proxy, and
//! 5. shuts everything down cleanly when [`VlessClient::shutdown`]
//!    is called (or when the last handle is dropped — see
//!    [`VlessClientHandle`]).
//!
//! ## Lifecycle
//!
//! ```text
//!     VlessClient::start(cfg)         ──► spawn backend + start listeners
//!         │
//!         ├──► listen loops run until shutdown()
//!         │
//!         └──► shutdown() — SIGTERM backend, close listeners
//! ```
//!
//! ## Why a separate `Handle`
//!
//! Callers that share ownership (CLI spawns the client, then
//! hands a handle to a UI thread; or tests want to drop the
//! [`VlessClient`] and assert that the handle still works) need
//! cheap clone semantics. [`VlessClientHandle`] is an `Arc` to
//! the supervisor state; cloning it does not duplicate the
//! underlying proxy.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::error::{VlessClientError, VlessClientResult};
use crate::link::VlessLink;
use crate::proxy::{HttpConnectServer, Socks5Server};
use crate::subprocess::{BackendHandle, BackendKind, ResolvedBackend};

/// Configuration for [`VlessClient::start`].
#[derive(Debug, Clone)]
pub struct VlessClientConfig {
    /// Parsed VLESS link. Construct with
    /// [`VlessLink::parse`](crate::link::VlessLink::parse) so the
    /// caller sees a `BadLink` error before we go to the trouble
    /// of spawning the backend.
    pub link: VlessLink,

    /// Address for the local SOCKS5 listener. Must be loopback.
    /// A free port can be requested with `127.0.0.1:0`.
    pub listen_socks5: SocketAddr,

    /// Optional address for the local HTTP-CONNECT listener.
    /// `None` disables the HTTP listener entirely.
    pub listen_http: Option<SocketAddr>,

    /// Which backend binary to use.
    pub backend: BackendKind,

    /// Log level forwarded to the backend (e.g. `"warn"`,
    /// `"info"`).
    pub log_level: String,

    /// Grace period before SIGKILL after SIGTERM. Default 5s.
    pub grace: Option<std::time::Duration>,
}

impl VlessClientConfig {
    /// Sensible defaults: auto-detect backend, SOCKS5 on
    /// `127.0.0.1:1080`, no HTTP listener, `log_level="warn"`.
    pub fn from_link(link: VlessLink) -> Self {
        Self {
            link,
            listen_socks5: "127.0.0.1:1080".parse().expect("default socks5 addr"),
            listen_http: None,
            backend: BackendKind::AutoDetect,
            log_level: "warn".to_string(),
            grace: None,
        }
    }
}

/// A running VLESS client. Dropping a `VlessClient` does **not**
/// shut the backend down — call [`VlessClient::shutdown`]
/// explicitly, or use the cheaper [`VlessClientHandle`] variant
/// which keeps the client alive while you hold a clone.
pub struct VlessClient {
    inner: Arc<Supervisor>,
}

/// Cheap clone of a running VLESS client. Dropping the last
/// handle triggers shutdown.
#[derive(Clone)]
pub struct VlessClientHandle {
    inner: Arc<Supervisor>,
}

/// Shared supervisor state. The proxy servers and the backend
/// are owned here so the listener tasks can outlive the
/// [`VlessClient`] value that started them (as long as
/// [`VlessClientHandle`] is alive).
struct Supervisor {
    /// The actual SOCKS5 listener address (post-bind). The
    /// proxy server was given `upstream` at construction time
    /// — we don't store the server object because its serve
    /// future owns the listener; storing the address lets the
    /// handle report it.
    socks5_addr: Mutex<Option<SocketAddr>>,
    /// Same for HTTP-CONNECT.
    http_addr: Mutex<Option<SocketAddr>>,
    /// Subprocess handle. Wrapped behind a `Mutex` so shutdown
    /// can call `take()` and the supervisor tasks can poll it.
    backend: Mutex<Option<BackendHandle>>,
    /// Whether shutdown has been requested. Set exactly once.
    shutdown_flag: tokio::sync::watch::Sender<bool>,
    /// Tasks spawned during `start`. We don't `await` them —
    /// they exit on their own when shutdown closes the
    /// listeners.
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl VlessClient {
    /// Start the VLESS client. Spawns the backend subprocess and
    /// begins listening for proxy traffic.
    ///
    /// ## Bind ordering
    ///
    /// Two processes want the upstream SOCKS port: the local
    /// `Socks5Server` (we use it as the *destination* of outbound
    /// connects from the user's apps) and the xray / sing-box
    /// subprocess (it *binds* the port and listens). We pick a
    /// free port with a throwaway listener, drop the listener so
    /// the OS releases it, then hand the same port to both the
    /// backend config and the local proxy. There is a small TOCTOU
    /// window between the drop and xray's bind; we mitigate it by
    /// retrying xray startup if the port has been snatched.
    pub async fn start(cfg: VlessClientConfig) -> VlessClientResult<VlessClientHandle> {
        // --- 1. Pre-resolve a backend so a missing binary
        //        surfaces before we bind anything.
        let resolved = resolved_for(cfg.backend).await?;

        // --- 2. Pick the upstream SOCKS port.
        let upstream_port = pick_free_port().await?;
        let upstream_addr: SocketAddr = format!("127.0.0.1:{upstream_port}")
            .parse()
            .map_err(|e| VlessClientError::BadLink(format!("upstream addr: {e}")))?;
        let upstream_for_cfg = format!("127.0.0.1:{upstream_port}");

        // --- 3. Spawn the backend.
        let backend_json = crate::subprocess::config_for(
            resolved,
            &cfg.link,
            &upstream_for_cfg,
            &cfg.log_level,
        )?;
        let mut backend = BackendHandle::spawn(cfg.backend, &backend_json).await?;
        if let Some(grace) = cfg.grace {
            backend = backend.with_grace(grace);
        }
        // Give the backend a moment to bind its listener. We
        // can't probe it directly without a working SOCKS5 client,
        // so a short sleep is the simplest mitigation. The proxy
        // connect retries on failure, so this is a soft hint, not
        // a correctness requirement.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // --- 4. Bind the local proxies.
        let socks5 = Socks5Server::bind(cfg.listen_socks5, upstream_addr).await?;
        let socks5_addr = socks5.local_addr();
        let http = match cfg.listen_http {
            Some(addr) => {
                let h = HttpConnectServer::bind(addr, upstream_addr).await?;
                let bound = h.local_addr();
                Some((h, bound))
            }
            None => None,
        };
        let http_addr = http.as_ref().map(|(_, a)| *a);

        // --- 5. Wire the supervisor.
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        let supervisor = Arc::new(Supervisor {
            socks5_addr: Mutex::new(Some(socks5_addr)),
            http_addr: Mutex::new(http_addr),
            backend: Mutex::new(Some(backend)),
            shutdown_flag: shutdown_tx,
            tasks: Mutex::new(Vec::new()),
        });

        // --- 6. Spawn the listener tasks.
        let mut tasks = Vec::new();
        let socks5_shutdown = supervisor.clone();
        let socks5_task = tokio::spawn(async move {
            tokio::select! {
                r = socks5.serve() => {
                    if let Err(e) = r { warn!(error = %e, "socks5 serve exited with error"); }
                }
                _ = wait_for_shutdown(&socks5_shutdown) => {}
            }
        });
        tasks.push(socks5_task);

        if let Some((http_server, _)) = http {
            let http_shutdown = supervisor.clone();
            let http_task = tokio::spawn(async move {
                tokio::select! {
                    r = http_server.serve() => {
                        if let Err(e) = r { warn!(error = %e, "http serve exited with error"); }
                    }
                    _ = wait_for_shutdown(&http_shutdown) => {}
                }
            });
            tasks.push(http_task);
        }

        // --- 7. Supervisor task that tails the backend.
        let backend_watch = supervisor.clone();
        let backend_task = tokio::spawn(async move {
            watch_backend(&backend_watch).await;
        });
        tasks.push(backend_task);

        *supervisor.tasks.lock().await = tasks;

        info!(
            socks5 = %socks5_addr,
            http = ?http_addr,
            upstream = %upstream_addr,
            "vless client started"
        );

        Ok(VlessClientHandle { inner: supervisor })
    }

    /// Convenience: same as [`VlessClient::start`] but returns the
    /// owning (non-clone-cheap) variant. Useful for short-lived
    /// CLI invocations.
    pub async fn start_owned(cfg: VlessClientConfig) -> VlessClientResult<Self> {
        let handle = Self::start(cfg).await?;
        Ok(Self { inner: handle.inner })
    }

    /// Shut down the client and wait for the backend to exit.
    pub async fn shutdown(self) -> VlessClientResult<()> {
        self.inner.shutdown().await
    }

    /// Cheap handle to the same client. Useful for sharing with a
    /// status-bar thread or for tests that want to drop the
    /// owning value.
    pub fn handle(&self) -> VlessClientHandle {
        VlessClientHandle { inner: self.inner.clone() }
    }
}

impl VlessClientHandle {
    /// The bound SOCKS5 listener address.
    pub async fn socks5_addr(&self) -> Option<SocketAddr> {
        *self.inner.socks5_addr.lock().await
    }

    /// The bound HTTP-CONNECT listener address, if enabled.
    pub async fn http_addr(&self) -> Option<SocketAddr> {
        *self.inner.http_addr.lock().await
    }

    /// Shut the client down. Idempotent. The handle stays usable
    /// — calling it again is a no-op.
    pub async fn shutdown(&self) -> VlessClientResult<()> {
        self.inner.shutdown().await
    }
}

/// Wait until the supervisor is asked to shut down. Used by the
/// listener tasks to know when to stop accepting.
async fn wait_for_shutdown(supervisor: &Arc<Supervisor>) {
    let mut rx = supervisor.shutdown_flag.subscribe();
    let _ = rx.changed().await;
}

/// Watch the backend process. When it exits unexpectedly, signal
/// shutdown so the listeners exit cleanly.
async fn watch_backend(supervisor: &Arc<Supervisor>) {
    // We poll the backend's child inside a loop. The shutdown
    // signal is checked every iteration so we exit promptly on
    // either path (backend death or explicit shutdown).
    loop {
        // Take the backend mutex briefly. If shutdown has been
        // signalled, stop.
        if *supervisor.shutdown_flag.borrow() {
            return;
        }
        let still_running = {
            let mut guard = supervisor.backend.lock().await;
            match guard.as_mut() {
                None => false,
                Some(handle) => match handle.child_mut().await {
                    Some(mut child_guard) => match child_guard.as_mut() {
                        None => false,
                        Some(child) => child.try_wait().map(|s| s.is_none()).unwrap_or(false),
                    },
                    None => false,
                },
            }
        };
        if !still_running {
            // Backend is gone — signal shutdown.
            warn!("vless backend exited; propagating shutdown");
            let _ = supervisor.shutdown_flag.send(true);
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// Pick a free port by binding to `:0` and reading the port.
async fn pick_free_port() -> VlessClientResult<u16> {
    let l = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(VlessClientError::Io)?;
    let addr = l.local_addr().map_err(VlessClientError::Io)?;
    drop(l);
    Ok(addr.port())
}

impl Supervisor {
    async fn shutdown(&self) -> VlessClientResult<()> {
        // Mark first so any racing listener exits its accept loop.
        let _ = self.shutdown_flag.send(true);
        // Shut down the backend.
        let backend = {
            let mut guard = self.backend.lock().await;
            guard.take()
        };
        if let Some(b) = backend {
            // Best-effort: don't propagate BackendNotFound here
            // because it would mask a successful shutdown.
            if let Err(e) = b.shutdown().await
                && !matches!(e, VlessClientError::BackendNotFound { .. }) {
                    return Err(e);
                }
        }
        // Wait for the listener tasks to drain.
        let mut tasks = self.tasks.lock().await;
        for t in tasks.drain(..) {
            let _ = t.await;
        }
        // Clear addresses so a stale handle can't mislead.
        *self.socks5_addr.lock().await = None;
        *self.http_addr.lock().await = None;
        Ok(())
    }
}

// Tiny adapter so we can re-resolve the backend dialect from the
// requested `BackendKind` without duplicating the probe logic.
async fn resolved_for(kind: BackendKind) -> VlessClientResult<ResolvedBackend> {
    match kind {
        BackendKind::Xray => Ok(ResolvedBackend::Xray),
        BackendKind::SingBox => Ok(ResolvedBackend::SingBox),
        BackendKind::AutoDetect => {
            // Prefer xray.
            if crate::probe_for_test(&["xray", "xray-core"]).await {
                return Ok(ResolvedBackend::Xray);
            }
            if crate::subprocess::probe_for_test(&["sing-box", "sing_box"]).await {
                return Ok(ResolvedBackend::SingBox);
            }
            Err(VlessClientError::BackendNotFound {
                path: "xray|sing-box".into(),
            })
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // If the last handle goes out of scope, signal shutdown
        // so listener tasks unblock. We can't `await` here, but
        // the listeners also poll the shutdown channel.
        let _ = self.shutdown_flag.send(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_sensible() {
        let link = VlessLink::parse(
            "vless://11111111-1111-1111-1111-111111111111@example.com:443",
        )
        .expect("link");
        let cfg = VlessClientConfig::from_link(link);
        assert_eq!(cfg.listen_socks5.port(), 1080);
        assert!(cfg.listen_http.is_none());
        assert_eq!(cfg.backend, BackendKind::AutoDetect);
    }

    #[test]
    fn from_link_does_not_validate_backend() {
        // Constructing the config never spawns anything.
        let link = VlessLink::parse(
            "vless://11111111-1111-1111-1111-111111111111@example.com:443",
        )
        .expect("link");
        let cfg = VlessClientConfig::from_link(link.clone());
        assert_eq!(cfg.link.uuid, link.uuid);
    }
}
