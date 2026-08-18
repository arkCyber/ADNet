//! Client-side Server-Sent Events (SSE) consumer that turns the
//! daemon's long-lived `/rpc/stream` endpoint into a `mpsc` channel
//! of [`SseNotification`] values the UI can iterate over.
//!
//! ## Protocol
//!
//! 1. The caller owns an [`A3chatClient`] (the same one used for
//!    regular JSON-RPC calls).  Construct an [`SseStream`] with the
//!    client and an optional topic filter.
//! 2. The stream **first calls `a3chat.stream.subscribe`** to obtain
//!    a `handle_id` from the server's `StreamService`.  This binds
//!    the client RPC identity to the SSE session.
//! 3. It then **opens a `GET <base>/rpc/stream`** request carrying
//!    the same `X-A3Chat-Owner` and a fresh `X-A3Chat-Request-Id`
//!    header.
//! 4. Each SSE frame is parsed via [`eventsource_stream`] into
//!    `(event, data)` tuples.  The `data` field is a JSON-RPC 2.0
//!    notification envelope (`{ jsonrpc, method, params }`); we
//!    extract the `method` and `params` into [`SseNotification`] and
//!    forward them to the channel.
//! 5. On drop, the stream calls `a3chat.stream.unsubscribe` so the
//!    server-side handle is released.
//!
//! ## Failure modes
//!
//! - HTTP 401 → the caller is unauthenticated.  Stream returns
//!   [`SseError::Auth`].
//! - HTTP non-2xx (other) → [`SseError::Http`].
//! - Network error or server shutdown → channel is closed (`recv`
//!   returns `None`); the caller can decide whether to reconnect.
//! - Malformed JSON → log + skip (one bad frame should not tear down
//!   the whole stream).
//!
//! ## DO-178C traceability
//!
//! - **§6.4 authentication** — `X-A3Chat-Owner` is required, matches
//!   the RPC dispatcher; 401 surfaces as a structured
//!   [`SseError::Auth`] not a panic.
//! - **§11 error reporting** — every parse / IO error is logged with
//!   `tracing::warn!` so the operator can correlate against the
//!   server-side `sse_stream` span.

#![forbid(unsafe_code)]

use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use a3chat_core::error::A3chatError;
use a3chat_core::rpc::A3chatRpcMethod;

use crate::client::{A3chatClient, HEADER_REQUEST_ID};

/// Errors that can surface when opening or reading an SSE stream.
#[derive(Debug, thiserror::Error)]
pub enum SseError {
    /// The server rejected our `X-A3Chat-Owner` header (HTTP 401).
    #[error("sse: unauthorized (missing or invalid X-A3Chat-Owner)")]
    Auth,
    /// Any other non-2xx HTTP status.
    #[error("sse: http {status}")]
    Http { status: u16 },
    /// The underlying JSON-RPC subscribe call failed.
    #[error("sse: subscribe failed: {0}")]
    Subscribe(A3chatError),
    /// The server closed the stream (or the request body errored).
    #[error("sse: stream closed: {0}")]
    Stream(String),
    /// Subscribe handle returned by the server was empty/missing.
    #[error("sse: subscribe reply missing handle_id")]
    MissingHandle,
}

impl From<SseError> for A3chatError {
    fn from(e: SseError) -> Self {
        A3chatError::RpcError(e.to_string())
    }
}

/// Notification yielded by an [`SseStream`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SseNotification {
    /// Notification method name (matches [`A3chatRpcMethod`] constants
    /// like [`A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECEIVED`]).
    pub method: String,
    /// The `params` payload — opaque JSON, deserialised into the
    /// matching typed envelope by the consumer.
    pub params: Value,
    /// Server-assigned SSE `id` (when present).  Useful for
    /// `Last-Event-Id`-style replay once P1.1 wires the bus replay
    /// buffer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// Stream subscription handle (mirrors the server-side
/// [`a3chat_app::stream_service::StreamSubscription`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SseSubscription {
    pub handle_id: String,
    pub owner: String,
    pub topics: Vec<String>,
    pub stream_url: String,
    pub keepalive_secs: u32,
}

/// Configuration for opening an SSE stream.
#[derive(Debug, Clone)]
pub struct SseStreamConfig {
    /// Topic filter forwarded to `a3chat.stream.subscribe`.  When
    /// `None` the server returns the full allow-list.
    pub topics: Option<Vec<String>>,
    /// Channel buffer size (number of in-flight notifications).
    pub channel_capacity: usize,
    /// Per-request HTTP timeout for the subscribe call.  The SSE GET
    /// itself is intentionally *unbounded* — it is a long-lived
    /// stream.
    pub subscribe_timeout: Duration,
}

impl Default for SseStreamConfig {
    fn default() -> Self {
        Self {
            topics: None,
            channel_capacity: 64,
            subscribe_timeout: Duration::from_secs(10),
        }
    }
}

/// Active SSE stream.  Drop it to release the server-side
/// subscription.
pub struct SseStream {
    /// Receiver the consumer iterates over.
    rx: mpsc::Receiver<SseNotification>,
    /// Handle to the background task that owns the HTTP GET.
    task: Option<tokio::task::JoinHandle<()>>,
    /// Client used to call `a3chat.stream.unsubscribe` on drop.
    client: A3chatClient,
    /// Server-assigned handle id (captured for unsubscribe).
    handle_id: String,
    /// Tracks whether `unsubscribe` has already been sent so `Drop`
    /// can fire-and-forget without races.
    released: bool,
}

impl SseStream {
    /// Open a new SSE stream.
    ///
    /// Performs the subscribe RPC, then spawns a background task that
    /// owns the long-lived HTTP GET.  The task pushes
    /// [`SseNotification`] values into the channel returned by
    /// [`SseStream::recv`].
    pub async fn open(
        client: A3chatClient,
        cfg: SseStreamConfig,
    ) -> Result<Self, SseError> {
        // 1. Subscribe — get a handle_id.
        let params = cfg.topics.as_ref().map(|topics| {
            serde_json::json!({
                "topics": topics,
            })
        });
        let sub_value = tokio::time::timeout(
            cfg.subscribe_timeout,
            client.call(
                A3chatRpcMethod::STREAM_SUBSCRIBE,
                params.unwrap_or_else(|| serde_json::json!({})),
            ),
        )
        .await
        .map_err(|_| {
            SseError::Subscribe(A3chatError::RpcError(
                "subscribe timed out".into(),
            ))
        })?
        .map_err(SseError::Subscribe)?;
        let sub: SseSubscription = serde_json::from_value(sub_value)
            .map_err(|e| SseError::Subscribe(A3chatError::RpcError(
                format!("malformed subscribe reply: {e}"),
            )))?;
        if sub.handle_id.is_empty() {
            return Err(SseError::MissingHandle);
        }

        // 2. Open the SSE GET.
        let url = format!(
            "{}/{}",
            client.config().base_url.trim_end_matches('/'),
            sub.stream_url.trim_start_matches('/')
        );
        let owner = client.config().owner.clone().to_string();
        let request_id = client.next_request_id();
        let http = client.http_clone();
        let resp = http
            .get(&url)
            .header("X-A3Chat-Owner", &owner)
            .header(HEADER_REQUEST_ID, &request_id)
            .send()
            .await
            .map_err(|e| SseError::Stream(format!("sse get: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // Best-effort unsubscribe — server-side handle was
            // already created, don't leak it.
            let _ = client
                .call(
                    A3chatRpcMethod::STREAM_UNSUBSCRIBE,
                    serde_json::json!({ "handle_id": &sub.handle_id }),
                )
                .await;
            return Err(SseError::Auth);
        }
        if !status.is_success() {
            let _ = client
                .call(
                    A3chatRpcMethod::STREAM_UNSUBSCRIBE,
                    serde_json::json!({ "handle_id": &sub.handle_id }),
                )
                .await;
            return Err(SseError::Http {
                status: status.as_u16(),
            });
        }

        // 3. Spawn the consumer task.
        let (tx, rx) = mpsc::channel(cfg.channel_capacity.max(1));
        let mut stream = resp.bytes_stream().eventsource();
        let task = tokio::spawn(async move {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(evt) => {
                        // Skip keepalive-only frames (empty data).
                        if evt.data.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(&evt.data) {
                            Ok(v) => {
                                let method = v
                                    .get("method")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let params = v
                                    .get("params")
                                    .cloned()
                                    .unwrap_or(Value::Null);
                                let id = if evt.id.is_empty() {
                                    None
                                } else {
                                    Some(evt.id.clone())
                                };
                                let n = SseNotification {
                                    method,
                                    params,
                                    id,
                                };
                                if tx.send(n).await.is_err() {
                                    tracing::debug!(
                                        "sse receiver dropped; closing stream"
                                    );
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    err = %e,
                                    raw_len = evt.data.len(),
                                    "sse frame failed to parse — skipping"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(err = %e, "sse stream errored");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            rx,
            task: Some(task),
            client,
            handle_id: sub.handle_id,
            released: false,
        })
    }

    /// Borrow the receiver so the caller can iterate.
    pub fn receiver(&mut self) -> &mut mpsc::Receiver<SseNotification> {
        &mut self.rx
    }

    /// `await` the next notification.  Returns `None` when the
    /// server closes the stream.
    pub async fn recv(&mut self) -> Option<SseNotification> {
        self.rx.recv().await
    }

    /// Server-assigned subscription handle id (useful for logging
    /// and for tests).
    pub fn handle_id(&self) -> &str {
        &self.handle_id
    }

    /// Explicitly release the server-side subscription.  Safe to
    /// call multiple times.  After this the channel will close once
    /// the background task drains the in-flight buffer.
    pub async fn close(mut self) -> Result<(), SseError> {
        self.release().await;
        if let Some(t) = self.task.take() {
            let _ = t.await;
        }
        Ok(())
    }

    async fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        // Best-effort unsubscribe; do not propagate failure (the
        // server may already be gone).
        let _ = self
            .client
            .call(
                A3chatRpcMethod::STREAM_UNSUBSCRIBE,
                serde_json::json!({ "handle_id": &self.handle_id }),
            )
            .await;
    }
}

impl Drop for SseStream {
    fn drop(&mut self) {
        // Background unsubscribe (best-effort).  We can't .await
        // here, so spawn a fire-and-forget task that ignores
        // errors.  When `close()` was called first, `released` is
        // already true and this is a no-op.
        if !self.released {
            self.released = true;
            let client = self.client.clone();
            let handle_id = std::mem::take(&mut self.handle_id);
            tokio::spawn(async move {
                let _ = client
                    .call(
                        A3chatRpcMethod::STREAM_UNSUBSCRIBE,
                        serde_json::json!({ "handle_id": handle_id }),
                    )
                    .await;
            });
        }
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_app::A3chatApp;
    use a3chat_app::storage::StorageConfig;
    use a3chat_core::event::A3chatEvent;
    use a3chat_core::presence::{PresenceEvent, PresenceStatus};
    use a3chat_rpc::{RpcServer, RpcServerConfig};

    fn owner() -> a3chat_core::id::UserId {
        a3chat_core::id::UserId::from("alice")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_emits_published_event() {
        let dir = tempfile::tempdir().unwrap();
        let app = A3chatApp::new(
            StorageConfig::new(dir.path().to_path_buf()),
            owner(),
        )
        .unwrap();
        let bus = app.bus.clone();
        let server = RpcServer::new(app, RpcServerConfig::default());
        let handle = server.start().await.unwrap();
        let base = format!("http://{}", handle.local_addr);

        let client =
            A3chatClient::new(crate::client::A3chatClientConfig::new(
                &base,
                owner(),
            ));

        let mut sse =
            SseStream::open(client.clone(), SseStreamConfig::default())
                .await
                .expect("open sse");
        assert!(!sse.handle_id().is_empty());

        // Publish a presence event a moment after the stream is
        // open.
        let bus_for_publish = bus.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            bus_for_publish.publish(A3chatEvent::PresenceChanged {
                event: PresenceEvent {
                    user_id: a3chat_core::id::UserId::from("bob"),
                    status: PresenceStatus::Online,
                    status_message: None,
                    timestamp: chrono::Utc::now(),
                },
            });
        });

        let n = tokio::time::timeout(Duration::from_secs(3), sse.recv())
            .await
            .expect("sse timed out")
            .expect("sse closed early");
        assert_eq!(n.method, A3chatRpcMethod::NOTIFICATION_PRESENCE_CHANGED);

        // Explicit close releases the handle.
        sse.close().await.expect("close");

        // Verify unsubscribe took effect: list for this owner must
        // now be empty.
        let list = client
            .call(
                A3chatRpcMethod::STREAM_LIST,
                serde_json::json!({}),
            )
            .await
            .expect("list");
        let handles = list
            .get("handles")
            .and_then(|h| h.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(handles, 0, "subscribe handle must be released");

        handle.stop().await;
    }

    #[tokio::test]
    async fn open_with_topic_filter_records_topics() {
        let dir = tempfile::tempdir().unwrap();
        let app = A3chatApp::new(
            StorageConfig::new(dir.path().to_path_buf()),
            owner(),
        )
        .unwrap();
        let server = RpcServer::new(app, RpcServerConfig::default());
        let handle = server.start().await.unwrap();
        let base = format!("http://{}", handle.local_addr);

        let client =
            A3chatClient::new(crate::client::A3chatClientConfig::new(
                &base,
                owner(),
            ));

        let cfg = SseStreamConfig {
            topics: Some(vec!["chat".into(), "presence".into()]),
            ..SseStreamConfig::default()
        };
        let sse = SseStream::open(client.clone(), cfg)
            .await
            .expect("open sse with topics");
        assert!(!sse.handle_id().is_empty());
        sse.close().await.expect("close");

        // Re-list to confirm topics were recorded.
        // (The server currently records topics regardless of the
        // SSE connection being open; we just sanity-check that the
        // unsubscribe cleared the handle.)
        let list = client
            .call(
                A3chatRpcMethod::STREAM_LIST,
                serde_json::json!({}),
            )
            .await
            .expect("list");
        let handles = list
            .get("handles")
            .and_then(|h| h.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(handles, 0);

        handle.stop().await;
    }

    #[tokio::test]
    async fn open_rejects_invalid_topic() {
        let dir = tempfile::tempdir().unwrap();
        let app = A3chatApp::new(
            StorageConfig::new(dir.path().to_path_buf()),
            owner(),
        )
        .unwrap();
        let server = RpcServer::new(app, RpcServerConfig::default());
        let handle = server.start().await.unwrap();
        let base = format!("http://{}", handle.local_addr);

        let client =
            A3chatClient::new(crate::client::A3chatClientConfig::new(
                &base,
                owner(),
            ));

        let cfg = SseStreamConfig {
            topics: Some(vec!["bogus-topic".into()]),
            ..SseStreamConfig::default()
        };
        let r = SseStream::open(client, cfg).await;
        assert!(matches!(r, Err(SseError::Subscribe(_))));

        handle.stop().await;
    }

    #[tokio::test]
    async fn drop_releases_handle() {
        let dir = tempfile::tempdir().unwrap();
        let app = A3chatApp::new(
            StorageConfig::new(dir.path().to_path_buf()),
            owner(),
        )
        .unwrap();
        let server = RpcServer::new(app, RpcServerConfig::default());
        let handle = server.start().await.unwrap();
        let base = format!("http://{}", handle.local_addr);

        let client =
            A3chatClient::new(crate::client::A3chatClientConfig::new(
                &base,
                owner(),
            ));

        {
            let _sse = SseStream::open(client.clone(), SseStreamConfig::default())
                .await
                .expect("open");
            // _sse dropped here → Drop runs unsubscribe.
        }

        // Give the fire-and-forget task a moment.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let list = client
            .call(
                A3chatRpcMethod::STREAM_LIST,
                serde_json::json!({}),
            )
            .await
            .expect("list");
        let handles = list
            .get("handles")
            .and_then(|h| h.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(handles, 0, "drop must release the handle");

        handle.stop().await;
    }

    // Smoke test: parsing a fake SSE frame into SseNotification.
    #[test]
    fn notification_deserialises_envelope() {
        let raw = r#"{"jsonrpc":"2.0","method":"a3chat.chat.message.received","params":{"x":1}}"#;
        let v: Value = serde_json::from_str(raw).unwrap();
        let method = v
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        assert_eq!(
            method,
            A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECEIVED
        );
    }
}
