//! Wire protocol for fetching blobs over a [`Transport`] connection.
//!
//! Both peers speak the same JSON-encoded message envelope (UTF-8 inside a
//! [`Frame`]). The wire shape is deliberately small so it stays compatible
//! with the rest of the workspace's JSON conventions:
//!
//! ```text
//! requester -> peer:
//!   { "cmd": "Get",      "hash": "<64 hex>", "range": { ... } }      // RangeSpec
//!   { "cmd": "Close" }                                               // graceful shutdown
//!
//! peer -> requester:
//!   { "cmd": "Meta",  "sizeBytes": u64, "chunkCount": u32 }
//!   { "cmd": "Chunk", "index": u32, "offset": u64, "data": "<base64>" }
//!   { "cmd": "Done",  "blake3": "<64 hex>" }
//!   { "cmd": "Error", "message": "<text>" }
//! ```
//!
//! For [`RangeSpec::All`], the peer sends one `Chunk` per chunk and ends
//! with `Done`. For a sub-range, the peer streams the requested bytes in
//! one or more `Chunk` frames with offsets relative to the start of the
//! requested range, then sends `Done` whose `blake3` is the BLAKE3 digest
//! of the **concatenated** requested bytes (so the requester can verify
//! the slice without re-hashing the whole blob).

use std::io::Write;
use std::path::Path;

use a3net_types::{ByteRange, ContentHash, RangeSpec};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::Frame;
use crate::traits::{OutgoingConnection, TransportError, TransportResult};

/// Maximum `Chunk` payload (1 MiB). Larger blobs are split across
/// multiple `Chunk` frames so a single `Frame` stays well below the
/// transport's 4 MiB hard cap.
pub const MAX_CHUNK_PAYLOAD: usize = 1024 * 1024;

/// All messages sent over the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "cmd", rename_all = "PascalCase")]
pub enum Message {
    /// Request a blob (or sub-range). Sent requester → peer.
    Get {
        hash: String,
        #[serde(default)]
        range: RangeSpec,
    },
    /// Gracefully close the connection (no further frames expected).
    Close,
    /// Reply with the blob's metadata. Sent peer → requester, only for
    /// `RangeSpec::All`.
    Meta { size_bytes: u64, chunk_count: u32 },
    /// Reply with a chunk of bytes. `offset` is relative to the requested
    /// range start. `data` is base64.
    Chunk {
        index: u32,
        offset: u64,
        data: String,
    },
    /// Reply signalling end of stream. `blake3` is the BLAKE3 digest of
    /// the concatenated bytes the requester will have received.
    Done { blake3: String },
    /// Reply signalling a failed request.
    Error { message: String },
}

impl Message {
    /// Encode as a single transport [`Frame`].
    pub fn into_frame(self) -> TransportResult<Frame> {
        let bytes = serde_json::to_vec(&self)
            .map_err(|e| TransportError::Decode(format!("encode message: {e}")))?;
        Ok(Frame::new(bytes))
    }

    /// Decode a [`Frame`] back into a [`Message`].
    pub fn from_frame(frame: Frame) -> TransportResult<Self> {
        serde_json::from_slice(frame.as_bytes())
            .map_err(|e| TransportError::Decode(format!("decode message: {e}")))
    }

    /// True if this message is a terminal one (no further reply expected).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Message::Close | Message::Done { .. } | Message::Error { .. }
        )
    }
}

/// Read raw blob bytes from a peer by speaking the wire protocol over
/// `conn`. Used by `a3net-node::download::try_transport`.
pub async fn fetch_blob_over_transport(
    conn: &mut Box<dyn OutgoingConnection>,
    hash: &ContentHash,
    range: &RangeSpec,
    dest: &Path,
) -> TransportResult<u64> {
    let req = Message::Get {
        hash: hash.as_hex().to_string(),
        range: range.clone(),
    };
    conn.send(req.into_frame()?).await?;
    let mut total = 0u64;
    let mut expected_size: Option<u64> = None;
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::create(dest)
        .map_err(|e| TransportError::Other(format!("create {}: {e}", dest.display())))?;
    loop {
        let frame = match conn.recv().await? {
            Some(f) => f,
            None => {
                return Err(TransportError::Other(
                    "blob fetch: peer closed stream before Done".into(),
                ));
            }
        };
        match Message::from_frame(frame)? {
            Message::Meta {
                size_bytes,
                chunk_count: _,
            } => {
                expected_size = Some(size_bytes);
            }
            Message::Chunk { offset, data, .. } => {
                let bytes = BASE64
                    .decode(data.as_bytes())
                    .map_err(|e| TransportError::Decode(format!("base64 chunk: {e}")))?;
                // Defensive: drop out-of-order chunks; the protocol
                // delivers in-order so this should never happen.
                if offset != total {
                    return Err(TransportError::Other(format!(
                        "blob fetch: chunk out of order (expected offset {total}, got {offset})"
                    )));
                }
                file.write_all(&bytes)
                    .map_err(|e| TransportError::Other(format!("write {}: {e}", dest.display())))?;
                hasher.update(&bytes);
                total += bytes.len() as u64;
            }
            Message::Done { blake3 } => {
                let computed = hasher.finalize().to_hex().to_string();
                if computed != blake3 {
                    let _ = std::fs::remove_file(dest);
                    return Err(TransportError::Other(format!(
                        "blob fetch: blake3 mismatch (got {computed}, peer claimed {blake3})"
                    )));
                }
                if matches!(range, RangeSpec::All)
                    && let Some(size) = expected_size
                    && size != total
                {
                    let _ = std::fs::remove_file(dest);
                    return Err(TransportError::Other(format!(
                        "blob fetch: size mismatch (peer said {size}, received {total})"
                    )));
                }
                file.flush()
                    .map_err(|e| TransportError::Other(format!("flush {}: {e}", dest.display())))?;
                return Ok(total);
            }
            Message::Error { message } => {
                let _ = std::fs::remove_file(dest);
                return Err(TransportError::Other(format!("peer error: {message}")));
            }
            other => {
                return Err(TransportError::Other(format!(
                    "blob fetch: unexpected reply {other:?}"
                )));
            }
        }
    }
}

/// Serve a single `Get` request on the given connection. The peer should
/// be the side that holds the blob in a [`a3net_blobstore::BlobStore`].
/// This is the receiving counterpart to [`fetch_blob_over_transport`].
///
/// `store` is the local store that will be queried. The caller is
/// responsible for closing the underlying connection afterwards.
pub async fn serve_blob_request(
    conn: &mut Box<dyn OutgoingConnection>,
    store: &a3net_blobstore::BlobStore,
) -> TransportResult<()> {
    let first = conn.recv().await?;
    let req = match first {
        Some(f) => f,
        None => return Ok(()),
    };
    let req = match Message::from_frame(req)? {
        Message::Get { hash, range } => {
            let hash = ContentHash::from_hex(&hash)
                .map_err(|e| TransportError::Decode(format!("bad hash: {e}")))?;
            (hash, range)
        }
        Message::Close => return Ok(()),
        other => {
            let _ = conn
                .send(
                    Message::Error {
                        message: format!("expected Get, got {other:?}"),
                    }
                    .into_frame()?,
                )
                .await;
            return Ok(());
        }
    };
    let (hash, range) = req;
    if !store.has_complete(&hash) {
        let _ = conn
            .send(
                Message::Error {
                    message: format!("blob {} not found", hash.as_hex()),
                }
                .into_frame()?,
            )
            .await;
        return Ok(());
    }
    let (size, _count) = store
        .meta(&hash)
        .map_err(|e| TransportError::Other(format!("blob meta: {e}")))?;
    if matches!(range, RangeSpec::All) {
        conn.send(
            Message::Meta {
                size_bytes: size,
                chunk_count: 0,
            }
            .into_frame()?,
        )
        .await?;
    }
    let ranges: Vec<ByteRange> = match &range {
        RangeSpec::All => vec![
            ByteRange::new(0, size).map_err(|e| TransportError::Other(format!("range: {e}")))?,
        ],
        RangeSpec::Single(r) => vec![*r],
        RangeSpec::Multi(rs) => rs.clone(),
    };
    let mut hasher = blake3::Hasher::new();
    let mut offset = 0u64;
    for r in &ranges {
        let bytes = store
            .read_range_sync(&hash, r)
            .map_err(|e| TransportError::Other(format!("blob read: {e}")))?;
        // Split large slices into ≤ MAX_CHUNK_PAYLOAD frames so a
        // single Frame stays below the transport's hard cap.
        for chunk in bytes.chunks(MAX_CHUNK_PAYLOAD) {
            let data = BASE64.encode(chunk);
            conn.send(
                Message::Chunk {
                    index: 0,
                    offset,
                    data,
                }
                .into_frame()?,
            )
            .await?;
            hasher.update(chunk);
            offset += chunk.len() as u64;
        }
    }
    let blake3 = hasher.finalize().to_hex().to_string();
    conn.send(Message::Done { blake3 }.into_frame()?).await?;
    // Wait briefly for the client's `Close` frame so the requester
    // sees a clean end-of-stream rather than EOF. We don't block
    // forever — after a short idle window we drop the stream and let
    // the underlying QUIC endpoint close it naturally.
    let close = tokio::time::timeout(std::time::Duration::from_millis(250), conn.recv()).await;
    match close {
        Ok(Ok(Some(_))) | Ok(Ok(None)) | Ok(Err(_)) | Err(_) => {}
    }
    Ok(())
}

/// Encode an ad-hoc error frame for protocol-level failures on the
/// serve side.
pub fn error_frame(message: impl Into<String>) -> TransportResult<Frame> {
    Message::Error {
        message: message.into(),
    }
    .into_frame()
}

/// Helper that returns `true` if a frame looks like a JSON message
/// (starts with `{`). Used by tests to skip stray bytes.
pub fn looks_like_message(buf: &[u8]) -> bool {
    buf.first().copied() == Some(b'{')
}

/// Re-export the JSON value type for callers that want to construct
/// free-form frames.
pub use serde_json::Value as JsonValue;

/// Convenience constructor for a [`Message::Error`] [`serde_json::Value`]
/// (used by callers that prefer to build JSON by hand).
pub fn error_value(message: impl Into<String>) -> JsonValue {
    json!({ "cmd": "Error", "message": message.into() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Two halves of an in-memory, `tokio`-wakeable pipe used to
    /// exercise the wire protocol without a real QUIC endpoint. The
    /// implementation uses `mpsc::channel` so `recv().await` is
    /// correctly notified when the other half pushes a frame.
    fn make_pipe_pair() -> (
        Box<dyn crate::traits::OutgoingConnection>,
        Box<dyn crate::traits::OutgoingConnection>,
    ) {
        let (a_tx, a_rx) = mpsc::channel::<Frame>(64);
        let (b_tx, b_rx) = mpsc::channel::<Frame>(64);
        let a: Box<dyn crate::traits::OutgoingConnection> =
            Box::new(PipePipe { tx: a_tx, rx: b_rx });
        let b: Box<dyn crate::traits::OutgoingConnection> =
            Box::new(PipePipe { tx: b_tx, rx: a_rx });
        (a, b)
    }

    #[derive(Debug)]
    struct PipePipe {
        tx: mpsc::Sender<Frame>,
        rx: mpsc::Receiver<Frame>,
    }

    #[async_trait::async_trait]
    impl crate::traits::OutgoingConnection for PipePipe {
        async fn send(&mut self, frame: Frame) -> TransportResult<()> {
            self.tx
                .send(frame)
                .await
                .map_err(|e| TransportError::Other(format!("pipe send: {e}")))?;
            Ok(())
        }
        async fn recv(&mut self) -> TransportResult<Option<Frame>> {
            match self.rx.recv().await {
                Some(f) => Ok(Some(f)),
                None => Ok(None),
            }
        }
        async fn close(mut self: Box<Self>) -> TransportResult<()> {
            // Drop tx so the peer sees EOF.
            drop(self.tx);
            Ok(())
        }
    }

    /// Construct a `BlobStore` populated with a known blob for tests.
    fn make_store(
        dir: &std::path::Path,
        payload: &[u8],
    ) -> (a3net_blobstore::BlobStore, ContentHash) {
        let store = a3net_blobstore::BlobStore::new(dir).unwrap();
        let src = dir.join("blob.bin");
        std::fs::write(&src, payload).unwrap();
        let (hash, _size) = store.import_file_sync(&src).unwrap();
        (store, hash)
    }

    #[test]
    fn message_roundtrip() {
        let msg = Message::Get {
            hash: "a".repeat(64),
            range: RangeSpec::All,
        };
        let frame = msg.clone().into_frame().unwrap();
        let back = Message::from_frame(frame).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn message_close_is_terminal() {
        assert!(Message::Close.is_terminal());
        assert!(
            Message::Error {
                message: "x".into()
            }
            .is_terminal()
        );
        assert!(
            !Message::Get {
                hash: "a".repeat(64),
                range: RangeSpec::All,
            }
            .is_terminal()
        );
    }

    #[tokio::test]
    async fn fetch_blob_over_transport_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let payload = vec![0xABu8; 2048];
        let (store, hash) = make_store(dir.path(), &payload);
        let (mut client, mut server) = make_pipe_pair();
        let server_store = std::sync::Arc::new(store);
        let server_task = tokio::spawn(async move {
            let r = serve_blob_request(&mut server, &server_store).await;
            assert!(r.is_ok(), "server returned {r:?}");
        });
        let dest = dir.path().join("out.bin");
        let n = fetch_blob_over_transport(&mut client, &hash, &RangeSpec::All, &dest)
            .await
            .unwrap();
        assert_eq!(n, payload.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        drop(client);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn fetch_blob_range_only_returns_slice() {
        let dir = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
        let (store, hash) = make_store(dir.path(), &payload);
        let (mut client, mut server) = make_pipe_pair();
        let server_store = std::sync::Arc::new(store);
        let server_task = tokio::spawn(async move {
            let r = serve_blob_request(&mut server, &server_store).await;
            assert!(r.is_ok(), "server returned {r:?}");
        });
        let dest = dir.path().join("slice.bin");
        let range = RangeSpec::Single(ByteRange::new(100, 200).unwrap());
        let n = fetch_blob_over_transport(&mut client, &hash, &range, &dest)
            .await
            .unwrap();
        assert_eq!(n, 100);
        let got = std::fs::read(&dest).unwrap();
        assert_eq!(got, &payload[100..200]);
        drop(client);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn fetch_blob_rejects_hash_mismatch_and_removes_file() {
        // Adversarial scenario: the server replies with a Done whose
        // blake3 doesn't match what it actually streamed. The client
        // must delete the corrupt file and surface an error.
        let dir = tempfile::tempdir().unwrap();
        let payload = vec![0xCDu8; 256];
        let (store, hash) = make_store(dir.path(), &payload);
        let (mut client, mut server) = make_pipe_pair();
        // Replace the server with one that always lies about Done.
        let _server_store = std::sync::Arc::new(store);
        let server_task = tokio::spawn(async move {
            // Wait for the client's Get request.
            if let Ok(Some(_)) = server.recv().await {
                let fake_total = 256u64;
                let _ = server
                    .send(
                        Message::Meta {
                            size_bytes: fake_total,
                            chunk_count: 0,
                        }
                        .into_frame()
                        .unwrap(),
                    )
                    .await;
                let _ = server
                    .send(
                        Message::Chunk {
                            index: 0,
                            offset: 0,
                            data: BASE64.encode(&payload),
                        }
                        .into_frame()
                        .unwrap(),
                    )
                    .await;
                // Lie: claim a wrong blake3.
                let _ = server
                    .send(
                        Message::Done {
                            blake3: "deadbeef".repeat(8),
                        }
                        .into_frame()
                        .unwrap(),
                    )
                    .await;
            }
        });
        let dest = dir.path().join("bad.bin");
        let err = fetch_blob_over_transport(&mut client, &hash, &RangeSpec::All, &dest)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("blake3"), "got {err}");
        assert!(!dest.exists(), "corrupt file must be deleted");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn fetch_blob_returns_error_when_peer_sends_error_frame() {
        let dir = tempfile::tempdir().unwrap();
        let (store, hash) = make_store(dir.path(), &b"hi".repeat(8));
        let (mut client, mut server) = make_pipe_pair();
        let _server_store = std::sync::Arc::new(store);
        let server_task = tokio::spawn(async move {
            if let Ok(Some(_)) = server.recv().await {
                let _ = server
                    .send(
                        Message::Error {
                            message: "intentional".into(),
                        }
                        .into_frame()
                        .unwrap(),
                    )
                    .await;
            }
        });
        let dest = dir.path().join("err.bin");
        let err = fetch_blob_over_transport(&mut client, &hash, &RangeSpec::All, &dest)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("intentional"), "got {err}");
        assert!(!dest.exists());
        let _ = server_task.await;
    }

    /// `serve_blob_request` must reject an `Endpoint` that doesn't
    /// actually exist on disk so a malicious caller can't enumerate
    /// the store.
    #[tokio::test]
    async fn serve_blob_request_errors_on_unknown_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = a3net_blobstore::BlobStore::new(dir.path()).unwrap();
        let (mut client, mut server) = make_pipe_pair();
        let request = Message::Get {
            hash: ContentHash::from_bytes(b"missing").as_hex().to_string(),
            range: RangeSpec::All,
        };
        let _ = client.send(request.into_frame().unwrap()).await;
        // server side
        let r = serve_blob_request(&mut server, &store).await;
        assert!(
            r.is_ok(),
            "serve must not error; it replies with an Error frame"
        );
        // Client must see the Error frame.
        let frame = client.recv().await.unwrap().unwrap();
        let msg = Message::from_frame(frame).unwrap();
        match msg {
            Message::Error { message } => {
                assert!(message.contains("not found"), "got {message}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
