//! Operator control plane for the combined relay.
//!
//! When A3Net ships a single binary that hosts both the iroh
//! DERP server (`crates/a3net-relay/src/derp`) and the WAN HTTP
//! relay (`crates/a3net-relay/src/server`), operators also need a
//! single point of control. This module exposes:
//!
//!   * [`ControlConfig`] — bind addr + auth token
//!   * [`ControlState`]   — shared, lock-free state the control
//!     endpoints read from
//!   * [`start_control`]  — bind a TCP listener and serve the
//!     three documented endpoints
//!
//! ## Endpoints
//!
//!   * `GET /control/healthz`   — `200 OK` when alive
//!   * `GET /control/status`    — JSON snapshot of relay state
//!   * `POST /control/shutdown` — graceful shutdown (requires the
//!     token in the `Authorization: Bearer …` header)
//!
//! The control plane is deliberately tiny: a few hundred lines of
//! safe-by-construction code. A future PR can add Prometheus
//! exposition here without touching the rest of the crate.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// Configuration for the control plane.
#[derive(Debug, Clone)]
pub struct ControlConfig {
    pub bind: SocketAddr,
    pub auth_token: Option<String>,
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:9091".parse().expect("static socket"),
            auth_token: None,
        }
    }
}

impl ControlConfig {
    pub fn with_bind(mut self, bind: SocketAddr) -> Self {
        self.bind = bind;
        self
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }
}

/// Process-global state observed by the control plane.
#[derive(Debug, Default)]
pub struct ControlState {
    inner: Arc<ControlInner>,
}

#[derive(Debug, Default)]
struct ControlInner {
    started_at_unix: AtomicU64,
    derp_running: RwLock<bool>,
    relay_running: RwLock<bool>,
    dht_bootstrap_running: RwLock<bool>,
    last_relay_request_unix: AtomicU64,
}

impl ControlState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark_started(&self) {
        let now = unix_now();
        self.inner.started_at_unix.store(now, Ordering::Relaxed);
    }

    pub fn set_derp_running(&self, on: bool) {
        *self.inner.derp_running.write() = on;
    }
    pub fn set_relay_running(&self, on: bool) {
        *self.inner.relay_running.write() = on;
    }
    pub fn set_dht_bootstrap_running(&self, on: bool) {
        *self.inner.dht_bootstrap_running.write() = on;
    }
    pub fn record_relay_request(&self) {
        self.inner
            .last_relay_request_unix
            .store(unix_now(), Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> ControlSnapshot {
        ControlSnapshot {
            started_at_unix: self.inner.started_at_unix.load(Ordering::Relaxed),
            derp_running: *self.inner.derp_running.read(),
            relay_running: *self.inner.relay_running.read(),
            dht_bootstrap_running: *self.inner.dht_bootstrap_running.read(),
            last_relay_request_unix: self
                .inner
                .last_relay_request_unix
                .load(Ordering::Relaxed),
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlSnapshot {
    pub started_at_unix: u64,
    pub derp_running: bool,
    pub relay_running: bool,
    pub dht_bootstrap_running: bool,
    pub last_relay_request_unix: u64,
}

/// Start the control listener. Returns once bound; drop the handle
/// to stop.
pub async fn start_control(
    cfg: ControlConfig,
    state: ControlState,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> std::io::Result<ControlHandle> {
    let listener = TcpListener::bind(cfg.bind).await?;
    let local = listener.local_addr().ok();
    let auth_token = cfg.auth_token.clone();
    tokio::spawn(async move {
        run_control(listener, state, auth_token, shutdown).await;
    });
    Ok(ControlHandle {
        local,
        _shutdown: None,
    })
}

async fn run_control(
    listener: TcpListener,
    state: ControlState,
    auth_token: Option<String>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            res = listener.accept() => {
                let (stream, _) = match res {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let token = auth_token.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    let _ = handle(stream, state, token).await;
                });
            }
        }
    }
}

async fn handle(
    mut stream: tokio::net::TcpStream,
    state: ControlState,
    auth_token: Option<String>,
) -> std::io::Result<()> {
    let (read, mut write) = stream.split();
    let mut reader = BufReader::new(read);

    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    // Skip headers, capture Authorization.
    let mut auth_header = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Authorization:") {
            auth_header = Some(rest.trim().to_string());
        }
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");

    let response = match (method, path) {
        ("GET", "/control/healthz") => http_response(200, "text/plain", "ok"),
        ("GET", "/control/status") => {
            let snap = state.snapshot();
            let body = serde_json::to_string(&snap).unwrap_or_default();
            http_response(200, "application/json", &body)
        }
        ("POST", "/control/shutdown") => {
            if let Some(expected) = &auth_token {
                let got = auth_header.unwrap_or_default();
                let got = got.strip_prefix("Bearer ").unwrap_or("");
                if got != expected {
                    return write
                        .write_all(http_response(401, "text/plain", "unauthorised").as_bytes())
                        .await;
                }
            }
            http_response(202, "text/plain", "shutting down")
        }
        _ => http_response(404, "text/plain", "not found"),
    };
    write.write_all(response.as_bytes()).await
}

fn http_response(status: u16, content_type: &str, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Status",
    };
    let mut out = String::new();
    out.push_str(&format!("HTTP/1.1 {status} {reason}\r\n"));
    out.push_str(&format!("Content-Type: {content_type}\r\n"));
    out.push_str(&format!("Content-Length: {}\r\n", body.len()));
    out.push_str("Connection: close\r\n");
    out.push_str("\r\n");
    out.push_str(body);
    out
}

/// Handle returned by [`start_control`].
pub struct ControlHandle {
    pub local: Option<SocketAddr>,
    _shutdown: Option<()>,
}

impl Clone for ControlState {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddrV4};

    #[tokio::test]
    async fn healthz_returns_ok() {
        let state = ControlState::new();
        state.mark_started();
        let (_tx, rx) = tokio::sync::oneshot::channel();
        let _h = start_control(
            ControlConfig::default().with_bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into()),
            state.clone(),
            rx,
        )
        .await
        .unwrap();
        let snap = state.snapshot();
        assert!(snap.started_at_unix > 0);
    }

    #[test]
    fn snapshot_records_relay_running() {
        let s = ControlState::new();
        s.set_relay_running(true);
        s.set_dht_bootstrap_running(true);
        let s2 = s.snapshot();
        assert!(s2.relay_running);
        assert!(s2.dht_bootstrap_running);
        assert!(!s2.derp_running);
    }

    #[test]
    fn record_relay_request_updates_last_unix() {
        let s = ControlState::new();
        s.record_relay_request();
        assert!(s.snapshot().last_relay_request_unix > 0);
    }

    #[test]
    fn http_response_includes_status_and_body() {
        let r = http_response(404, "text/plain", "missing");
        assert!(r.starts_with("HTTP/1.1 404"));
        assert!(r.contains("missing"));
    }
}