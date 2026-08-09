//! JSON-RPC 2.0 client over a Unix socket.
//!
//! Lifted from `Exodus@src-backup/.../microservice/gossip_client.rs` and
//! `group_chat_client.rs`. Both clients are byte-identical except for a
//! trivial error-message string — we keep the differences behind a service
//! label parameter.
//!
//! Two complementary surfaces:
//!
//! - [`json_rpc_call`] — one-shot request/response on a fresh socket.
//! - [`json_rpc_stream`] — long-lived socket that interleaves
//!   responses (matched by `id`) and server-pushed notifications (no
//!   `id` field). Use this when the server is expected to push
//!   events.

use std::io::ErrorKind;
use std::path::Path;

use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::server::Notification;

const DEFAULT_BUFFER: usize = 16 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Errors produced by [`json_rpc_call`] and [`json_rpc_stream`].
#[derive(Debug, Error)]
pub enum JsonRpcError {
    #[error("failed to connect to {service}: {source}")]
    Connect {
        service: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to send request: {0}")]
    Send(#[from] std::io::Error),
    #[error("failed to parse response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("{service} error: {message}")]
    Server { service: String, message: String },
    #[error("missing `result` in response")]
    NoResult,
    #[error("connection closed: {0}")]
    ConnectionClosed(String),
    #[error("server response with unknown id `{0}` (no outstanding request)")]
    UnknownResponseId(i64),
}

/// One frame received from the server. Either a response to a
/// previously-sent request (matched by `id`) or a server-pushed
/// notification.
#[derive(Debug, Clone)]
pub enum StreamItem {
    Response {
        id: i64,
        value: Result<Value, String>,
    },
    Notification(Notification),
}

/// Send a JSON-RPC 2.0 request over a Unix socket and decode the response.
///
/// `service` is a human-readable label used in error messages
/// (e.g. `"P2P gossip"`, `"Group Chat"`).
pub async fn json_rpc_call(
    socket_path: &Path,
    service: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let request = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1,
    });
    let request_str = serde_json::to_string(&request)?;

    let stream =
        UnixStream::connect(socket_path)
            .await
            .map_err(|source| JsonRpcError::Connect {
                service: service.to_string(),
                source,
            })?;
    let (reader, mut writer) = stream.into_split();

    writer
        .write_all(request_str.as_bytes())
        .await
        .map_err(JsonRpcError::Send)?;
    writer.write_all(b"\n").await.map_err(JsonRpcError::Send)?;

    let mut reader = BufReader::with_capacity(DEFAULT_BUFFER, reader);
    let mut response_line = Vec::new();
    reader
        .read_until(b'\n', &mut response_line)
        .await
        .map_err(JsonRpcError::Send)?;
    if response_line.is_empty() {
        return Err(JsonRpcError::Send(std::io::Error::new(
            ErrorKind::UnexpectedEof,
            "server closed connection without a response",
        )));
    }
    if response_line.len() > MAX_RESPONSE_BYTES {
        return Err(JsonRpcError::Send(std::io::Error::new(
            ErrorKind::InvalidData,
            "JSON-RPC response exceeds size limit",
        )));
    }

    let response: serde_json::Value = serde_json::from_slice(&response_line)?;
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or(&error.to_string())
            .to_string();
        return Err(JsonRpcError::Server {
            service: service.to_string(),
            message,
        });
    }
    response
        .get("result")
        .cloned()
        .ok_or(JsonRpcError::NoResult)
}

/// Open a long-lived connection to the server and decode incoming
/// frames. Each yielded `StreamItem` is either a `Response` (matched
/// by `id`) or a `Notification` (no `id` field).
///
/// The stream terminates when the server closes the socket (yields
/// `None`).
pub async fn json_rpc_stream(
    socket_path: &Path,
    service: &str,
) -> Result<futures::stream::BoxStream<'static, Result<StreamItem, JsonRpcError>>, JsonRpcError> {
    use futures::stream::unfold;
    let stream =
        UnixStream::connect(socket_path)
            .await
            .map_err(|source| JsonRpcError::Connect {
                service: service.to_string(),
                source,
            })?;
    let (reader, _writer) = stream.into_split();
    let reader = BufReader::with_capacity(DEFAULT_BUFFER, reader);
    let service = service.to_string();

    let s = unfold((reader, service), move |(mut reader, service)| async move {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) => None,
            Ok(_) => {
                if line.len() > MAX_RESPONSE_BYTES {
                    return Some((
                        Err(JsonRpcError::Send(std::io::Error::new(
                            ErrorKind::InvalidData,
                            "JSON-RPC frame exceeds size limit",
                        ))),
                        (reader, service),
                    ));
                }
                let v: Value = match serde_json::from_slice(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        return Some((Err(JsonRpcError::Parse(e)), (reader, service)));
                    }
                };

                let item = if v.get("id").is_some() {
                    let id = v.get("id").and_then(|i| i.as_i64()).unwrap_or(0);
                    let value = if let Some(err) = v.get("error") {
                        let msg = err
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or(&err.to_string())
                            .to_string();
                        Err(format!("{service}: {msg}"))
                    } else {
                        Ok(v.get("result").cloned().unwrap_or(Value::Null))
                    };
                    StreamItem::Response { id, value }
                } else {
                    let method = v
                        .get("method")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();
                    let params = v.get("params").cloned().unwrap_or(Value::Null);
                    StreamItem::Notification(Notification { method, params })
                };
                Some((Ok(item), (reader, service)))
            }
            Err(e) => Some((Err(JsonRpcError::Send(e)), (reader, service))),
        }
    });
    Ok(Box::pin(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn roundtrip_with_dummy_server() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("rpc.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["method"], "ping");
            let resp = json!({"jsonrpc":"2.0","result":{"ok":true},"id":req["id"]});
            writer.write_all(resp.to_string().as_bytes()).await.unwrap();
            writer.write_all(b"\n").await.unwrap();
        });

        let v = json_rpc_call(&sock, "Test", "ping", json!({}))
            .await
            .unwrap();
        assert_eq!(v["ok"], true);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn handles_server_error() {
        let tmp = tempfile::tempdir().unwrap();
        let sock: PathBuf = tmp.path().join("err.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            let resp = json!({
                "jsonrpc":"2.0",
                "error":{"code":-1,"message":"unknown method"},
                "id":req["id"]
            });
            writer.write_all(resp.to_string().as_bytes()).await.unwrap();
            writer.write_all(b"\n").await.unwrap();
        });

        let err = json_rpc_call(&sock, "Test", "nope", json!({}))
            .await
            .unwrap_err();
        match err {
            JsonRpcError::Server { service, message } => {
                assert_eq!(service, "Test");
                assert!(message.contains("unknown"));
            }
            e => panic!("unexpected error: {e:?}"),
        }
        server.await.unwrap();
    }

    /// `json_rpc_stream` should yield server-pushed notifications as
    /// `StreamItem::Notification` when the server writes a frame
    /// without an `id` field.
    #[tokio::test]
    async fn stream_yields_notifications() {
        use futures::StreamExt;
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("stream.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (_reader, mut writer) = stream.into_split();
            // No inbound request — just push a notification.
            let notif = json!({
                "jsonrpc": "2.0",
                "method": "event",
                "params": { "value": 42 }
            });
            let mut s = notif.to_string();
            s.push('\n');
            writer.write_all(s.as_bytes()).await.unwrap();
            // Hold the socket open briefly so the client can read.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let mut stream = json_rpc_stream(&sock, "stream-test").await.unwrap();
        let item = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("timed out")
            .expect("stream ended")
            .expect("frame error");
        match item {
            StreamItem::Notification(n) => {
                assert_eq!(n.method, "event");
                assert_eq!(n.params["value"], 42);
            }
            other => panic!("expected Notification, got {other:?}"),
        }
        server.await.unwrap();
    }
}
