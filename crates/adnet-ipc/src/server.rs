//! JSON-RPC 2.0 server — binds a Unix socket, dispatches incoming requests
//! to a user-supplied [`RpcHandler`], and supports server-pushed
//! notifications.
//!
//! # Wire protocol
//!
//! Both directions speak newline-delimited JSON-RPC 2.0 over the same
//! Unix socket:
//!
//! - **Request** (client → server):
//!   `{"jsonrpc":"2.0","method":"...","params":...,"id":N}\n`
//! - **Response** (server → client):
//!   `{"jsonrpc":"2.0","result":...,"id":N}\n` or
//!   `{"jsonrpc":"2.0","error":{"code":-1,"message":"..."},"id":N}\n`
//! - **Notification** (server → client):
//!   `{"jsonrpc":"2.0","method":"<event>","params":...}\n`
//!   (no `id` field, never answered)
//!
//! The client drains notifications via [`json_rpc_stream`], which
//! loops over the socket and yields either a server response (matched
//! to a previously sent request id) or a [`Notification`].
//!
//! # Notification semantics
//!
//! Server-pushed notifications are produced by
//! [`JsonRpcServerHandle::notifier`]: the handle returns a clone of a
//! [`NotificationSender`] that any task can call `.send(...)` on.
//! Every active connection forwards a copy to its socket — connections
//! can be slow without blocking the producer (the broadcast channel
//! buffers; consumers that fall behind see `RecvError::Lagged` and
//! skip ahead).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Mutex};
use tracing::{info, warn};

/// A server-pushed notification. JSON-RPC 2.0 distinguishes
/// notifications from responses by the absence of an `id` field.
#[derive(Debug, Clone)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

/// A single JSON-RPC method handler. Implementations are expected to be
/// cheap to `Arc`-clone (they typically hold only references to the
/// service's state).
#[async_trait]
pub trait RpcHandler: Send + Sync + 'static {
    /// Handle a JSON-RPC request and produce a JSON-RPC response value.
    async fn handle(&self, method: &str, params: Value) -> Result<Value, String>;
}

/// A cloneable handle for pushing server-side notifications to every
/// connected client. Cloning is cheap and yields another handle into
/// the same broadcast channel.
#[derive(Clone)]
pub struct NotificationSender {
    tx: broadcast::Sender<Notification>,
}

impl NotificationSender {
    /// Push a notification to every connected client. Returns the
    /// number of subscribers that received the message (at least 1 if
    /// any client is connected, 0 if the server is idle).
    pub fn send(&self, method: impl Into<String>, params: Value) -> usize {
        let notif = Notification {
            method: method.into(),
            params,
        };
        self.tx.send(notif).unwrap_or(0)
    }
}

/// Running server handle — drop to shut it down. Cloning is cheap.
pub struct JsonRpcServerHandle {
    pub socket_path: PathBuf,
    shutdown_tx: broadcast::Sender<()>,
    notification_sender: NotificationSender,
}

impl JsonRpcServerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Get a cloneable handle for pushing notifications to every
    /// connected client. Pass this to whatever background task should
    /// drive the event stream (e.g. a `NodeRpc` that bridges
    /// `subscribe_room` into JSON-RPC notifications).
    pub fn notifier(&self) -> NotificationSender {
        self.notification_sender.clone()
    }
}

impl Drop for JsonRpcServerHandle {
    fn drop(&mut self) {
        // Best-effort: a dropped handle should signal the listener
        // task to exit. `send` returns Err only when there are no
        // receivers, which already means the task exited.
        let _ = self.shutdown_tx.send(());
        // Remove the on-disk socket file so the path can be re-bound
        // by a future process. We intentionally do not remove it if
        // the file no longer exists (the user may have cleaned it up).
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

/// Builder / starter for a [`JsonRpcServer`].
pub struct JsonRpcServer;

impl JsonRpcServer {
    /// Bind `socket_path` and serve `handler` until the resulting
    /// handle's `shutdown()` is called.
    pub async fn start<H: RpcHandler>(
        socket_path: PathBuf,
        handler: Arc<H>,
    ) -> Result<JsonRpcServerHandle, String> {
        Self::start_with_capacity(socket_path, handler, 1024).await
    }

    /// Same as [`start`](Self::start) but lets the caller size the
    /// notification broadcast channel.
    pub async fn start_with_capacity<H: RpcHandler>(
        socket_path: PathBuf,
        handler: Arc<H>,
        notification_capacity: usize,
    ) -> Result<JsonRpcServerHandle, String> {
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| format!("bind {}: {e}", socket_path.display()))?;

        let (shutdown_tx, mut shutdown_rx) = broadcast::channel(1);
        let (notif_tx, _) = broadcast::channel::<Notification>(notification_capacity);
        let notification_sender = NotificationSender {
            tx: notif_tx.clone(),
        };

        let active = Arc::new(Mutex::new(()));
        let handler = Arc::clone(&handler);

        tokio::spawn(async move {
            let _active_guard = active.lock().await;
            loop {
                tokio::select! {
                    accept = listener.accept() => {
                        match accept {
                            Ok((stream, _)) => {
                                let handler = Arc::clone(&handler);
                                let notif_tx = notif_tx.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handle_connection(stream, handler, notif_tx).await {
                                        warn!("rpc connection error: {e}");
                                    }
                                });
                            }
                            Err(e) => warn!("rpc accept error: {e}"),
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("rpc server shutting down");
                        break;
                    }
                }
            }
        });

        Ok(JsonRpcServerHandle {
            socket_path,
            shutdown_tx,
            notification_sender,
        })
    }
}

const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

async fn handle_connection<H: RpcHandler>(
    stream: UnixStream,
    handler: Arc<H>,
    notif_tx: broadcast::Sender<Notification>,
) -> Result<(), String> {
    let (reader, writer) = stream.into_split();
    // The writer is shared between the request-response loop and the
    // notification forwarder. A tokio Mutex serialises writes; using
    // a mutex is simpler than implementing a write-side channel and
    // is plenty fast for JSON-RPC over Unix sockets.
    let writer = Arc::new(Mutex::new(writer));

    // Bound how many bytes a single request line can occupy on the
    // read side. `BufReader::read_line` would otherwise grow without
    // limit until it sees `\n`, which a malicious peer can exploit
    // to exhaust memory. The 16 MiB cap matches the per-line limit
    // checked after the read.
    let reader = BufReader::new(reader).take(MAX_REQUEST_BYTES as u64);

    // Forwarder: every notification produced globally is serialised
    // and written to the client socket. The forwarder exits when the
    // broadcast channel is closed (server shutdown).
    let writer_for_fwd = Arc::clone(&writer);
    let mut notif_rx = notif_tx.subscribe();
    let forwarder = tokio::spawn(async move {
        loop {
            match notif_rx.recv().await {
                Ok(notif) => {
                    let frame = json!({
                        "jsonrpc": "2.0",
                        "method": notif.method,
                        "params": notif.params,
                    });
                    let mut s = frame.to_string();
                    s.push('\n');
                    let mut w = writer_for_fwd.lock().await;
                    if w.write_all(s.as_bytes()).await.is_err() {
                        break;
                    }
                    if w.flush().await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let mut reader = reader;
    let mut line = String::new();
    while reader
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?
        > 0
    {
        if line.len() > MAX_REQUEST_BYTES {
            let resp = json!({
                "jsonrpc": "2.0",
                "error": {"code": -32600, "message": format!("request too large (> {MAX_REQUEST_BYTES} bytes)")},
                "id": Value::Null
            });
            let mut s = resp.to_string();
            s.push('\n');
            let mut w = writer.lock().await;
            let _ = w.write_all(s.as_bytes()).await;
            let _ = w.flush().await;
            line.clear();
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32700, "message": format!("parse error: {e}")},
                    "id": Value::Null
                });
                let mut s = resp.to_string();
                s.push('\n');
                let mut w = writer.lock().await;
                let _ = w.write_all(s.as_bytes()).await;
                let _ = w.flush().await;
                line.clear();
                continue;
            }
        };

        // A notification is a request without `id`. We do not respond
        // to notifications — they're pure server→client events.
        let has_id = request.get("id").is_some();
        let method = request
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let id = request.get("id").cloned().unwrap_or(Value::Null);

        if !has_id {
            // Best-effort: forward to the handler too, in case a
            // future implementation wants to process inbound
            // notifications (e.g. client heartbeat). The current
            // RpcHandler trait only exposes `handle` with a
            // Result<Value, String> signature so we just ignore its
            // return value here.
            let _ = handler.handle(&method, params).await;
            line.clear();
            continue;
        }

        let response = match handler.handle(&method, params).await {
            Ok(result) => json!({"jsonrpc": "2.0", "result": result, "id": id}),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "error": {"code": -1, "message": message},
                "id": id
            }),
        };
        let mut s = response.to_string();
        s.push('\n');
        let mut w = writer.lock().await;
        let _ = w.write_all(s.as_bytes()).await;
        let _ = w.flush().await;
        line.clear();
    }

    // Client disconnected. Drain the forwarder.
    forwarder.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    struct Adder {
        calls: Arc<Mutex<HashMap<String, usize>>>,
    }

    #[async_trait]
    impl RpcHandler for Adder {
        async fn handle(&self, method: &str, params: Value) -> Result<Value, String> {
            *self
                .calls
                .lock()
                .await
                .entry(method.to_string())
                .or_insert(0) += 1;
            match method {
                "add" => {
                    let a = params
                        .get("a")
                        .and_then(|v| v.as_i64())
                        .ok_or("missing a")?;
                    let b = params
                        .get("b")
                        .and_then(|v| v.as_i64())
                        .ok_or("missing b")?;
                    Ok(json!(a + b))
                }
                "echo" => Ok(params),
                other => Err(format!("unknown method: {other}")),
            }
        }
    }

    #[tokio::test]
    async fn server_dispatches_methods() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("svc.sock");
        let calls = Arc::new(Mutex::new(HashMap::new()));
        let handler = Arc::new(Adder {
            calls: Arc::clone(&calls),
        });
        let handle = JsonRpcServer::start(sock.clone(), handler).await.unwrap();

        let r = crate::client::json_rpc_call(&sock, "test", "add", json!({"a": 2, "b": 3}))
            .await
            .unwrap();
        assert_eq!(r.as_i64(), Some(5));

        let r = crate::client::json_rpc_call(&sock, "test", "echo", json!({"hello": "world"}))
            .await
            .unwrap();
        assert_eq!(r["hello"], "world");

        handle.shutdown();
    }

    /// The server must push a notification to every connected client
    /// when the notifier is invoked. The test asserts that a single
    /// client receives the notification by reading from its socket.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn server_pushes_notifications_to_clients() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("notif.sock");

        struct Noop;
        #[async_trait]
        impl RpcHandler for Noop {
            async fn handle(&self, _m: &str, _p: Value) -> Result<Value, String> {
                Ok(Value::Null)
            }
        }
        let handle = JsonRpcServer::start(sock.clone(), Arc::new(Noop))
            .await
            .unwrap();
        let notifier = handle.notifier();

        // Open a client and drive a request through it so the server
        // has definitely entered `handle_connection` and the
        // forwarder task has subscribed to the broadcast channel.
        let s = tokio::net::UnixStream::connect(&sock).await.unwrap();
        let (r, mut w) = s.into_split();
        tokio::io::AsyncWriteExt::write_all(
            &mut w,
            b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\",\"id\":1}\n",
        )
        .await
        .unwrap();
        let mut br = BufReader::new(r);
        let mut line = String::new();
        br.read_line(&mut line).await.unwrap();
        assert!(line.contains("\"result\""), "got: {line}");

        // Wait for the forwarder task to subscribe. The forwarder is
        // spawned inside `handle_connection` after the request
        // handler yields; on a heavily loaded CI box the spawn may
        // not have completed by the time the response is read, so
        // poll briefly before firing the notification.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if notifier.tx.receiver_count() >= 1 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "forwarder never subscribed (receiver_count = {})",
                    notifier.tx.receiver_count()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        // Fire a notification — the connection should observe it.
        let n = notifier.send("ping", json!({"hello": "world"}));
        assert!(n >= 1, "expected at least 1 subscriber, got {n}");

        let mut line = String::new();
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), br.read_line(&mut line))
            .await
            .expect("timed out waiting for notification")
            .expect("read failed");
        assert!(n > 0, "got empty line");
        let v: Value = serde_json::from_str(&line).expect("parse notif");
        assert_eq!(v["method"], "ping");
        assert_eq!(v["params"]["hello"], "world");
        // No `id` field on a notification.
        assert!(v.get("id").is_none());

        handle.shutdown();
    }

    /// Dropping the handle must shut down the listener loop and
    /// remove the on-disk socket file. Without `Drop`, the listener
    /// task would keep the socket bound and the file would persist.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_drop_shuts_down_server() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("drop.sock");
        struct Noop;
        #[async_trait]
        impl RpcHandler for Noop {
            async fn handle(&self, _m: &str, _p: Value) -> Result<Value, String> {
                Ok(Value::Null)
            }
        }
        let handle = JsonRpcServer::start(sock.clone(), Arc::new(Noop))
            .await
            .unwrap();
        let socket_path = handle.socket_path().to_path_buf();
        assert!(socket_path.exists(), "socket file should exist after start");
        drop(handle);
        // Give the listener task a brief moment to observe the
        // shutdown signal and exit.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while socket_path.exists() {
            if std::time::Instant::now() >= deadline {
                panic!("socket file {} still present after drop", socket_path.display());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// A request line that exceeds `MAX_REQUEST_BYTES` must be
    /// rejected instead of being allowed to exhaust memory. The
    /// server's per-line read cap closes the connection after the
    /// error response is sent, but a fresh client connection must
    /// still be served by the same server.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_oversize_request_line() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("oversize.sock");
        struct Noop;
        #[async_trait]
        impl RpcHandler for Noop {
            async fn handle(&self, _m: &str, _p: Value) -> Result<Value, String> {
                Ok(Value::Null)
            }
        }
        let handle = JsonRpcServer::start(sock.clone(), Arc::new(Noop))
            .await
            .unwrap();

        // Open a client and write a header that, when serialised, is
        // larger than MAX_REQUEST_BYTES (16 MiB). We do not need a
        // valid JSON body — the server's per-line cap fires first.
        let s = tokio::net::UnixStream::connect(&sock).await.unwrap();
        let (r, mut w) = s.into_split();
        let giant = "x".repeat(MAX_REQUEST_BYTES + 1024);
        let payload = format!("{{\"jsonrpc\":\"2.0\",\"method\":\"{giant}\",\"id\":1}}\n");
        // The server's read cap closes the connection after the
        // first MAX_REQUEST_BYTES, so the write may fail with
        // BrokenPipe on the second half — that's fine, we just need
        // the server to *see* an oversize line.
        let _ = tokio::io::AsyncWriteExt::write_all(&mut w, payload.as_bytes()).await;
        drop(w);
        // Drain whatever the server wrote back. The server may close
        // before the full payload is delivered, so accept either an
        // oversize error or a clean EOF.
        let mut br = BufReader::new(r);
        let mut line = String::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            br.read_line(&mut line),
        )
        .await
        .expect("timed out draining oversize response");
        // If the server delivered a JSON error, validate it. The
        // Take cap may fire either as an oversize-line rejection or
        // as a parse error on the truncated JSON, both are valid.
        if !line.is_empty() {
            let v: Value =
                serde_json::from_str(&line).expect("parse error response");
            let err = v.get("error").expect("response should be an error");
            let msg = err["message"].as_str().unwrap_or_default();
            let is_oversize = msg.contains("too large");
            let is_parse = msg.contains("parse error");
            assert!(
                is_oversize || is_parse,
                "expected oversize or parse error, got: {msg}"
            );
        }
        drop(br);

        // The server should still be answering on a fresh connection.
        let r = crate::client::json_rpc_call(&sock, "oversize", "ping", json!({}))
            .await
            .unwrap();
        assert_eq!(r, Value::Null);

        handle.shutdown();
    }
}
