//! GraphSync over ADNet transport — IPFS-style DAG sync on top of
//! the Bitswap-style QUIC bridge.
//!
//! This module connects the synchronous DAG traversal in
//! `adnet_types::graphsync` to the async network plane. It does not
//! own a transport; it borrows the same `SharedTransport` and
//! ALPN-guarded stream pattern used by Bitswap so a node can host
//! both protocols on a single QUIC endpoint.
//!
//! ## Wire layout
//!
//! All frames are length-prefixed JSON (1-byte tag + tag-dependent
//! payload), to keep the protocol readable in Wireshark and easy to
//! extend:
//!
//! ```text
//! Request  : {"id": u64, "root": "<cid>", "selector": <bytes>, "priority": i32}
//! Block    : {"id": u64, "cid": "<cid>", "data": <bytes>}
//! Response : {"id": u64, "status": <u32>}
//! ```
//!
//! Frames are framed by `adnet_transport::Frame` (length-prefixed).
//!
//! ## Sync vs async
//!
//! The DAG traversal itself is synchronous and lock-free; the bridge
//! turns the per-block iterator into async frames. Block bodies are
//! streamed out-of-band by `Block` messages, so the responder never
//! holds a full DAG in memory.

use std::collections::HashMap;
use std::sync::Arc;

use adnet_types::graphsync::{
    BlockMessage, BlockStore, GraphSyncError, GraphSyncMessage, GraphSyncResponder, RequestMessage,
    ResponseItem, ResponseMessage, ResponseStatus,
};
use adnet_types::{Cid, NodeId};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

/// ALPN for GraphSync.
pub const GRAPHSYNC_ALPN: &[u8] = b"adnet/graphsync/1";

/// Maximum single-frame size; the protocol splits larger blocks into
/// multiple `Block` frames internally if needed (matches Bitswap).
pub const MAX_FRAME_SIZE: usize = 1024 * 1024 * 4; // 4 MB

/// Default request timeout.
pub const DEFAULT_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// GraphSync transport errors.
#[derive(Debug, Error)]
pub enum GraphSyncTransportError {
    #[error("connection: {0}")]
    Connection(String),
    #[error("peer not connected: {0}")]
    PeerNotConnected(String),
    #[error("serialization: {0}")]
    Serialization(String),
    #[error("frame too large: {size} bytes (max {max})")]
    FrameTooLarge { size: usize, max: usize },
    #[error("ALPN mismatch: expected {expected:?}, got {actual:?}")]
    AlpnMismatch { expected: Vec<u8>, actual: Vec<u8> },
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("channel closed")]
    ChannelClosed,
    #[error("invalid selector: {0}")]
    InvalidSelector(String),
    #[error("internal: {0}")]
    Internal(String),
}

/// Wire envelope — small JSON wrappers that carry either a request,
/// block, or response. Tags mirror `GraphSyncMessage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum GraphSyncWire {
    /// Request to start a sync.
    Request {
        id: u64,
        root: Cid,
        selector: Vec<u8>,
        priority: i32,
    },
    /// Block frame carrying a single block's bytes.
    Block { id: u64, cid: Cid, data: Vec<u8> },
    /// Response signalling completion / cancellation / failure.
    Response { id: u64, status: u32 },
}

impl GraphSyncWire {
    pub fn encode(&self) -> Result<Vec<u8>, GraphSyncTransportError> {
        serde_json::to_vec(self).map_err(|e| GraphSyncTransportError::Serialization(e.to_string()))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, GraphSyncTransportError> {
        serde_json::from_slice(bytes)
            .map_err(|e| GraphSyncTransportError::Serialization(e.to_string()))
    }

    pub fn request_id(&self) -> u64 {
        match self {
            GraphSyncWire::Request { id, .. }
            | GraphSyncWire::Block { id, .. }
            | GraphSyncWire::Response { id, .. } => *id,
        }
    }
}

impl From<GraphSyncMessage> for GraphSyncWire {
    fn from(msg: GraphSyncMessage) -> Self {
        match msg {
            GraphSyncMessage::Request(r) => GraphSyncWire::Request {
                id: r.id,
                root: r.root,
                selector: r.selector,
                priority: r.priority,
            },
            GraphSyncMessage::Block(b) => GraphSyncWire::Block {
                id: b.id,
                cid: b.cid,
                data: b.block,
            },
            GraphSyncMessage::Response(r) => GraphSyncWire::Response {
                id: r.id,
                status: r.status.to_u32(),
            },
        }
    }
}

impl From<GraphSyncWire> for GraphSyncMessage {
    fn from(w: GraphSyncWire) -> Self {
        match w {
            GraphSyncWire::Request {
                id,
                root,
                selector,
                priority,
            } => GraphSyncMessage::Request(RequestMessage {
                id,
                root,
                selector,
                replace: false,
                priority,
            }),
            GraphSyncWire::Block { id, cid, data } => GraphSyncMessage::Block(BlockMessage {
                id,
                cid,
                block: data,
            }),
            GraphSyncWire::Response { id, status } => {
                let s = ResponseStatus::from_u32(status).unwrap_or(ResponseStatus::Failed);
                GraphSyncMessage::Response(ResponseMessage { id, status: s })
            }
        }
    }
}

/// Adapter block-store backed by an in-memory `HashMap`. Useful for
/// `MockTransport` tests and ephemeral nodes that don't need
/// disk-backed DAGs.
#[derive(Debug, Default, Clone)]
pub struct MemDagStore {
    blocks: Arc<Mutex<HashMap<Cid, Vec<u8>>>>,
    links: Arc<Mutex<HashMap<Cid, Vec<Cid>>>>,
    /// Parallel map for named links. When populated, [`BlockStore::links_named`]
    /// surfaces the names; otherwise the default implementation derives
    /// `(None, cid)` from `links`.
    named_links: Arc<Mutex<HashMap<Cid, Vec<(Option<String>, Cid)>>>>,
}

impl MemDagStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a block and an explicit link list.
    pub fn insert(&self, cid: Cid, bytes: Vec<u8>, links: Vec<Cid>) {
        self.blocks.lock().insert(cid.clone(), bytes);
        self.links.lock().insert(cid, links);
    }

    /// Insert a block with explicit named links. Use this when the
    /// caller wants `Matcher::Links` to honor `LinkMatcher::name`.
    pub fn insert_named(&self, cid: Cid, bytes: Vec<u8>, links: Vec<(Option<String>, Cid)>) {
        let cids: Vec<Cid> = links.iter().map(|(_, c)| c.clone()).collect();
        self.blocks.lock().insert(cid.clone(), bytes);
        self.links.lock().insert(cid.clone(), cids);
        self.named_links.lock().insert(cid, links);
    }
}

impl BlockStore for MemDagStore {
    fn get(&self, cid: &Cid) -> Option<Vec<u8>> {
        self.blocks.lock().get(cid).cloned()
    }
    fn put(&self, cid: &Cid, bytes: &[u8]) {
        self.blocks.lock().insert(cid.clone(), bytes.to_vec());
    }
    fn has(&self, cid: &Cid) -> bool {
        self.blocks.lock().contains_key(cid)
    }
    fn links(&self, cid: &Cid) -> Vec<Cid> {
        self.links.lock().get(cid).cloned().unwrap_or_default()
    }
    fn links_named(&self, cid: &Cid) -> Vec<(Option<String>, Cid)> {
        if let Some(named) = self.named_links.lock().get(cid) {
            return named.clone();
        }
        self.links(cid).into_iter().map(|c| (None, c)).collect()
    }
}

/// Handle for a single in-flight GraphSync request.
///
/// Dropping the handle cancels the request on the responder side and
/// aborts the receive loop.
pub struct GraphSyncRequestHandle {
    pub id: u64,
    pub root: Cid,
    rx: mpsc::Receiver<Result<(Cid, Vec<u8>), GraphSyncTransportError>>,
    /// Per-request timeout applied by [`Self::next_block`]. `None`
    /// means "wait indefinitely" — legacy behaviour. The default
    /// value is set by [`GraphSyncClient::request`] to
    /// [`DEFAULT_REQUEST_TIMEOUT`].
    per_request_timeout: Option<std::time::Duration>,
}

impl GraphSyncRequestHandle {
    /// Override the timeout applied to subsequent [`Self::next_block`]
    /// calls. Pass `None` to disable the timeout entirely.
    pub fn set_timeout(&mut self, timeout: Option<std::time::Duration>) {
        self.per_request_timeout = timeout;
    }

    /// Receive the next block from the responder, or the final status.
    ///
    /// Honours the per-request timeout: if no frame arrives within
    /// `timeout`, returns `Some(Err(GraphSyncTransportError::Timeout))`
    /// and the pending request is left in the client's pending map
    /// (callers can drop the handle to cancel).
    pub async fn next_block(
        &mut self,
    ) -> Option<Result<(Cid, Vec<u8>), GraphSyncTransportError>> {
        match self.per_request_timeout {
            Some(t) => match tokio::time::timeout(t, self.rx.recv()).await {
                Ok(opt) => opt,
                Err(_) => Some(Err(GraphSyncTransportError::Timeout(
                    "per-request timeout".into(),
                ))),
            },
            None => self.rx.recv().await,
        }
    }
}

/// Client-side adapter for GraphSync. Holds a transport bridge, an
/// outbox of pending requests, and a counter.
#[derive(Clone)]
pub struct GraphSyncClient {
    /// Outbound transport bridge.
    transport: Arc<dyn GraphSyncTransportBridge>,
    /// In-flight request waiters, keyed by request id.
    pending:
        Arc<Mutex<HashMap<u64, mpsc::Sender<Result<(Cid, Vec<u8>), GraphSyncTransportError>>>>>,
    /// Auto-incrementing request id counter.
    next_id: Arc<Mutex<u64>>,
}

impl GraphSyncClient {
    pub fn new(transport: Arc<dyn GraphSyncTransportBridge>) -> Self {
        Self {
            transport,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(1)),
        }
    }

    /// Clone that shares the same `pending` map and `next_id` counter,
    /// so a listener task can dispatch incoming frames into the right
    /// request without owning the only `pending` map.
    pub fn clone_shared(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            pending: self.pending.clone(),
            next_id: self.next_id.clone(),
        }
    }

    /// Allocate a fresh request id (useful for retries).
    pub fn next_id(&self) -> u64 {
        let mut n = self.next_id.lock();
        let id = *n;
        *n += 1;
        id
    }

    /// Register a one-shot channel to receive blocks for `id`, and
    /// return the receive side. The sender side is stored in
    /// `pending` so the event loop can route incoming frames into it.
    pub fn allocate(
        &self,
        id: u64,
    ) -> mpsc::Receiver<Result<(Cid, Vec<u8>), GraphSyncTransportError>> {
        let (tx, rx) = mpsc::channel(64);
        self.pending.lock().insert(id, tx);
        rx
    }

    /// Hand out the receive-side of the registered channel for `id`.
    /// Used by tests that wire `register` + `take_rx` directly.
    pub fn take_rx(
        &self,
        id: u64,
    ) -> Option<mpsc::Receiver<Result<(Cid, Vec<u8>), GraphSyncTransportError>>> {
        let mut pending = self.pending.lock();
        let tx = pending.remove(&id)?;
        let (new_tx, new_rx) = mpsc::channel(64);
        pending.insert(id, new_tx);
        // Drop the old tx so its rx (if any) sees EOF.
        drop(tx);
        Some(new_rx)
    }

    /// Send a request to `peer` and return a handle that streams blocks.
    ///
    /// The default per-request timeout
    /// ([`DEFAULT_REQUEST_TIMEOUT`]) is applied to
    /// [`GraphSyncRequestHandle::next_block`]; callers can override
    /// via [`GraphSyncRequestHandle::set_timeout`] or use
    /// [`Self::request_with_timeout`] directly.
    pub async fn request(
        &self,
        peer: &NodeId,
        root: Cid,
        selector: Vec<u8>,
        priority: i32,
    ) -> Result<GraphSyncRequestHandle, GraphSyncTransportError> {
        self.request_with_timeout(peer, root, selector, priority, Some(DEFAULT_REQUEST_TIMEOUT))
            .await
    }

    /// Like [`Self::request`] but with an explicit timeout. Pass
    /// `None` to disable the per-call timeout entirely.
    pub async fn request_with_timeout(
        &self,
        peer: &NodeId,
        root: Cid,
        selector: Vec<u8>,
        priority: i32,
        timeout: Option<std::time::Duration>,
    ) -> Result<GraphSyncRequestHandle, GraphSyncTransportError> {
        let id = self.next_id();
        let wire = GraphSyncWire::Request {
            id,
            root: root.clone(),
            selector: selector.clone(),
            priority,
        };
        let bytes = wire.encode()?;
        if bytes.len() > MAX_FRAME_SIZE {
            return Err(GraphSyncTransportError::FrameTooLarge {
                size: bytes.len(),
                max: MAX_FRAME_SIZE,
            });
        }

        let rx = self.allocate(id);
        self.transport.send_to(peer, bytes).await?;
        Ok(GraphSyncRequestHandle {
            id,
            root,
            rx,
            per_request_timeout: timeout,
        })
    }

    /// Route an incoming frame to the matching pending request.
    pub fn on_frame(&self, frame: GraphSyncWire) {
        let id = frame.request_id();
        let mut pending = self.pending.lock();
        let Some(tx) = pending.get(&id) else {
            return;
        };
        match frame {
            GraphSyncWire::Block { cid, data, .. } => {
                let _ = tx.try_send(Ok((cid, data)));
            }
            GraphSyncWire::Response { status, .. } => {
                let status = ResponseStatus::from_u32(status).unwrap_or(ResponseStatus::Failed);
                match status {
                    ResponseStatus::Completed
                    | ResponseStatus::EndOfDag
                    | ResponseStatus::Partial => {
                        // Drop the sender so the handle sees EOF.
                        pending.remove(&id);
                    }
                    ResponseStatus::Cancelled => {
                        let _ =
                            tx.try_send(Err(GraphSyncTransportError::Internal("cancelled".into())));
                        pending.remove(&id);
                    }
                    ResponseStatus::Failed => {
                        let _ = tx.try_send(Err(GraphSyncTransportError::Internal(
                            "remote failed".into(),
                        )));
                        pending.remove(&id);
                    }
                    ResponseStatus::Remote => {
                        // Redirect — nothing to do at the client level
                        // yet, the bridge handles redirection.
                    }
                }
            }
            GraphSyncWire::Request { .. } => {
                // Clients don't process incoming requests.
            }
        }
    }
}

/// Responder-side adapter. Wraps a [`GraphSyncResponder`] (sync) and
/// dispatches blocks over the transport asynchronously.
pub struct GraphSyncServer {
    block_store: Arc<dyn BlockStore>,
    transport: Arc<dyn GraphSyncTransportBridge>,
    /// Active server-side requests.
    inflight: Arc<Mutex<HashMap<u64, oneshot::Sender<()>>>>,
}

impl GraphSyncServer {
    pub fn new(
        block_store: Arc<dyn BlockStore>,
        transport: Arc<dyn GraphSyncTransportBridge>,
    ) -> Self {
        Self {
            block_store,
            transport,
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Handle a single incoming wire frame. Returns true if a response
    /// was dispatched (caller should `continue` reading the next
    /// frame); returns `Err` on terminal failure.
    pub async fn on_frame(
        &self,
        peer: &NodeId,
        frame: GraphSyncWire,
    ) -> Result<(), GraphSyncTransportError> {
        match frame {
            GraphSyncWire::Request {
                id,
                root,
                selector,
                priority,
            } => {
                let req = RequestMessage {
                    id,
                    root: root.clone(),
                    selector: selector.clone(),
                    replace: false,
                    priority,
                };
                let (cancel_tx, mut cancel_rx) = oneshot::channel();
                self.inflight.lock().insert(id, cancel_tx);

                let responder = GraphSyncResponder::new(self.block_store.clone());
                let items = responder
                    .process_request_streaming(req)
                    .map_err(|e| match e {
                        GraphSyncError::InvalidSelector(s) => {
                            GraphSyncTransportError::InvalidSelector(s)
                        }
                        other => GraphSyncTransportError::Internal(other.to_string()),
                    })?;

                let transport = self.transport.clone();
                let peer = peer.clone();
                let inflight = self.inflight.clone();
                tokio::spawn(async move {
                    for item in items {
                        // Cancellation poll
                        if cancel_rx.try_recv().is_ok() {
                            break;
                        }
                        let wire = match item {
                            ResponseItem::Block { cid, data } => {
                                GraphSyncWire::Block { id, cid, data }
                            }
                            ResponseItem::Status(s) => GraphSyncWire::Response {
                                id,
                                status: s.to_u32(),
                            },
                        };
                        let bytes = match wire.encode() {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::warn!("graphsync encode failed: {}", e);
                                break;
                            }
                        };
                        if let Err(e) = transport.send_to(&peer, bytes).await {
                            tracing::warn!(%peer, "graphsync send failed: {}", e);
                            break;
                        }
                    }
                    inflight.lock().remove(&id);
                });
                Ok(())
            }
            GraphSyncWire::Block { .. } | GraphSyncWire::Response { .. } => {
                // Server doesn't act on incoming block / response frames
                // outside of the cancellation channel.
                Ok(())
            }
        }
    }

    /// Cancel an in-flight request by id. The responder stops sending
    /// blocks and emits a `Cancelled` response.
    pub fn cancel(&self, id: u64) {
        if let Some(tx) = self.inflight.lock().remove(&id) {
            let _ = tx.send(());
        }
    }
}

/// Bridge trait — every async GraphSync transport implements this so
/// [`GraphSyncClient`] / [`GraphSyncServer`] can plug into it.
#[async_trait]
pub trait GraphSyncTransportBridge: Send + Sync {
    /// Send a wire frame to `peer`. Errors surface as
    /// [`GraphSyncTransportError::Connection`].
    async fn send_to(&self, peer: &NodeId, data: Vec<u8>) -> Result<(), GraphSyncTransportError>;

    /// Local node id (used for metrics & logging).
    fn local_node_id(&self) -> &NodeId;

    /// Register a sender that the listener should hand bytes to when
    /// they arrive from `peer`. Pairs with `unregister_peer`.
    async fn register_inbound_sender(&self, peer: NodeId, tx: mpsc::Sender<Vec<u8>>);

    /// Remove the inbound sender for `peer`. Called when the peer
    /// disconnects.
    async fn unregister_peer(&self, peer: &NodeId);
}

/// In-process transport for tests / single-process demos. Each peer
/// has a `mpsc::Sender<Vec<u8>>` registered; `send_to` writes to it
/// so the peer's listener receives the frame.
pub struct MockGraphSyncTransport {
    local_node_id: NodeId,
    peers: tokio::sync::RwLock<HashMap<NodeId, mpsc::Sender<Vec<u8>>>>,
}

impl MockGraphSyncTransport {
    pub fn new(local_node_id: NodeId) -> Self {
        Self {
            local_node_id,
            peers: tokio::sync::RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl GraphSyncTransportBridge for MockGraphSyncTransport {
    async fn send_to(&self, peer: &NodeId, data: Vec<u8>) -> Result<(), GraphSyncTransportError> {
        let tx = {
            let peers = self.peers.read().await;
            peers
                .get(peer)
                .cloned()
                .ok_or_else(|| GraphSyncTransportError::PeerNotConnected(peer.to_string()))?
        };
        tx.send(data)
            .await
            .map_err(|_| GraphSyncTransportError::ChannelClosed)
    }

    fn local_node_id(&self) -> &NodeId {
        &self.local_node_id
    }

    async fn register_inbound_sender(&self, peer: NodeId, tx: mpsc::Sender<Vec<u8>>) {
        self.peers.write().await.insert(peer, tx);
    }

    async fn unregister_peer(&self, peer: &NodeId) {
        self.peers.write().await.remove(peer);
    }
}

// (GraphSync QUIC bridge and service live in `adnet-node`'s
//  `graphsync` module — `adnet-blobstore` cannot depend on
//  `adnet-transport` without a cycle.)

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::graphsync::selector;

    fn dummy_node(byte: u8) -> NodeId {
        let arr = [byte; 32];
        NodeId::from_bytes(&arr).expect("32-byte node id")
    }

    fn block(bytes: &[u8]) -> (Cid, Vec<u8>) {
        let cid = Cid::from_content_blake3(bytes);
        (cid, bytes.to_vec())
    }

    #[test]
    fn wire_request_roundtrip() {
        let cid = Cid::from_content_blake3(b"root");
        let wire = GraphSyncWire::Request {
            id: 42,
            root: cid.clone(),
            selector: selector::match_all(),
            priority: 1,
        };
        let bytes = wire.encode().unwrap();
        let parsed = GraphSyncWire::decode(&bytes).unwrap();
        match parsed {
            GraphSyncWire::Request {
                id,
                root,
                selector,
                priority,
            } => {
                assert_eq!(id, 42);
                assert_eq!(root.hash_hex(), cid.hash_hex());
                assert!(!selector.is_empty());
                assert_eq!(priority, 1);
            }
            _ => panic!("expected request"),
        }
    }

    #[test]
    fn wire_block_roundtrip() {
        let (cid, data) = block(b"hello");
        let wire = GraphSyncWire::Block {
            id: 7,
            cid,
            data: data.clone(),
        };
        let bytes = wire.encode().unwrap();
        let parsed = GraphSyncWire::decode(&bytes).unwrap();
        match parsed {
            GraphSyncWire::Block { id, data: d, .. } => {
                assert_eq!(id, 7);
                assert_eq!(d, data);
            }
            _ => panic!("expected block"),
        }
    }

    #[test]
    fn mem_dag_store_round_trip() {
        let store = MemDagStore::new();
        let (cid, data) = block(b"abc");
        store.insert(cid.clone(), data.clone(), vec![]);
        assert!(store.has(&cid));
        assert_eq!(store.get(&cid), Some(data));
        assert_eq!(store.links(&cid), Vec::<Cid>::new());
    }

    #[tokio::test]
    async fn client_request_returns_handle() {
        let transport = Arc::new(MockGraphSyncTransport::new(dummy_node(1)));
        let peer = dummy_node(2);
        let (tx, _rx) = mpsc::channel::<Vec<u8>>(8);
        transport.register_inbound_sender(peer.clone(), tx).await;

        let client = GraphSyncClient::new(transport);
        let cid = Cid::from_content_blake3(b"root");
        let handle = client
            .request(&peer, cid.clone(), selector::match_all(), 1)
            .await
            .expect("request should succeed");
        assert_eq!(handle.id, 1);
        assert_eq!(handle.root.hash_hex(), cid.hash_hex());
    }

    #[tokio::test]
    async fn server_streams_through_mock_transport() {
        let local = dummy_node(1);
        let peer = dummy_node(2);
        let transport = Arc::new(MockGraphSyncTransport::new(local.clone()));

        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(32);
        transport.register_inbound_sender(peer.clone(), tx).await;

        // Build a small DAG in the server's store
        let store = Arc::new(MemDagStore::new());
        let (cid, data) = block(b"root-bytes");
        store.insert(cid.clone(), data, vec![]);

        let server = GraphSyncServer::new(store, transport.clone());

        // Drive the server
        let req_wire = GraphSyncWire::Request {
            id: 17,
            root: cid.clone(),
            selector: selector::match_all(),
            priority: 1,
        };
        server
            .on_frame(&peer, req_wire)
            .await
            .expect("server accepts request");

        // Collect frames: 1 Block + 1 Response. Use a timeout so the test
        // doesn't hang if the server never drops the channel.
        let mut got_block = false;
        let mut got_response = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while !(got_block && got_response) {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let bytes = match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(b)) => b,
                Ok(None) => break,
                Err(_) => panic!("timed out waiting for server frames"),
            };
            let frame = GraphSyncWire::decode(&bytes).unwrap();
            match frame {
                GraphSyncWire::Block {
                    id,
                    cid: bcid,
                    data,
                } => {
                    assert_eq!(id, 17);
                    assert_eq!(bcid.hash_hex(), cid.hash_hex());
                    assert_eq!(data, b"root-bytes");
                    got_block = true;
                }
                GraphSyncWire::Response { id, status } => {
                    assert_eq!(id, 17);
                    assert_eq!(status, ResponseStatus::Completed.to_u32());
                    got_response = true;
                }
                _ => {}
            }
        }
        assert!(got_block, "expected at least one Block frame");
        assert!(got_response, "expected terminal Response frame");
    }

    #[tokio::test]
    async fn client_receives_blocks_via_transport() {
        let local = dummy_node(1);
        let peer = dummy_node(2);
        let transport = Arc::new(MockGraphSyncTransport::new(local.clone()));

        let (server_tx, mut server_rx) = mpsc::channel::<Vec<u8>>(32);
        let (client_tx, _client_rx) = mpsc::channel::<Vec<u8>>(32);
        // Register both directions
        transport
            .register_inbound_sender(peer.clone(), server_tx)
            .await;
        // The peer's bridge is separate. We simulate it by reading
        // the wire manually and feeding decoded frames into the
        // client's `on_frame`.

        let client = GraphSyncClient::new(transport.clone());
        let cid = Cid::from_content_blake3(b"abc");
        let mut handle = client
            .request(&peer, cid.clone(), selector::match_all(), 1)
            .await
            .expect("client request ok");

        // The client's request should have hit server_rx:
        let req_bytes = server_rx.recv().await.expect("server should see request");
        let req = GraphSyncWire::decode(&req_bytes).unwrap();
        assert!(matches!(req, GraphSyncWire::Request { .. }));

        // Simulate the responder pushing back a Block + Response
        client_tx
            .send(
                GraphSyncWire::Block {
                    id: req.request_id(),
                    cid: cid.clone(),
                    data: b"abc".to_vec(),
                }
                .encode()
                .unwrap(),
            )
            .await
            .unwrap();
        client.on_frame(GraphSyncWire::Block {
            id: req.request_id(),
            cid: cid.clone(),
            data: b"abc".to_vec(),
        });
        client.on_frame(GraphSyncWire::Response {
            id: req.request_id(),
            status: ResponseStatus::Completed.to_u32(),
        });

        let block = tokio::time::timeout(std::time::Duration::from_secs(2), handle.next_block())
            .await
            .expect("timed out waiting for block")
            .expect("expected a block")
            .expect("block ok");
        assert_eq!(block.0.hash_hex(), cid.hash_hex());
        assert_eq!(block.1, b"abc");
        // After terminal response, next recv returns None
        let next = tokio::time::timeout(std::time::Duration::from_secs(2), handle.next_block())
            .await
            .expect("timed out waiting for EOF");
        assert!(next.is_none());
    }

    #[tokio::test]
    async fn request_handle_times_out_when_no_frames_arrive() {
        // Build a transport where the peer never sends anything.
        // After `set_timeout(50ms)`, `next_block` should return
        // `Err(GraphSyncTransportError::Timeout)` rather than
        // hanging indefinitely.
        let local = dummy_node(1);
        let peer = dummy_node(2);
        let transport = Arc::new(MockGraphSyncTransport::new(local.clone()));
        let (tx, _rx) = mpsc::channel::<Vec<u8>>(32);
        transport.register_inbound_sender(peer.clone(), tx).await;

        let client = GraphSyncClient::new(transport);
        let cid = Cid::from_content_blake3(b"timeout-target");
        let mut handle = client
            .request_with_timeout(&peer, cid.clone(), selector::match_all(), 1, None)
            .await
            .expect("request should be sent");
        handle.set_timeout(Some(std::time::Duration::from_millis(50)));

        let res = handle.next_block().await;
        match res {
            Some(Err(GraphSyncTransportError::Timeout(_))) => {}
            other => panic!("expected Timeout error, got {:?}", other.map(|r| r.map(|_| "ok"))),
        }
    }

    #[tokio::test]
    async fn request_with_timeout_none_disables_timeout() {
        // Same setup as above but with `None` timeout — verify that
        // a slow peer doesn't trigger a phantom `Timeout` error.
        // We poll the handle for a brief window and assert the
        // channel is still open (no timeout-based return).
        let local = dummy_node(1);
        let peer = dummy_node(2);
        let transport = Arc::new(MockGraphSyncTransport::new(local.clone()));
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(32);
        transport.register_inbound_sender(peer.clone(), tx).await;

        let client = GraphSyncClient::new(transport);
        let cid = Cid::from_content_blake3(b"no-timeout");
        let handle = client
            .request_with_timeout(&peer, cid.clone(), selector::match_all(), 1, None)
            .await
            .expect("request should be sent");

        // Drain the request frame that landed on `rx` so the
        // transport doesn't keep a stale channel alive.
        let _ = rx.recv().await;

        // 30ms is far less than DEFAULT_REQUEST_TIMEOUT (60s); the
        // call must not return within this window because no
        // timeout was configured.
        let r = tokio::time::timeout(std::time::Duration::from_millis(30), async {
            // Hold the handle via a fresh mut borrow inside the
            // async block so the call goes through the timeout-free
            // path.
            let mut h = handle;
            h.next_block().await
        })
        .await;
        assert!(
            r.is_err(),
            "expected timeout (Pending) without configured timeout, got {:?}",
            r
        );
    }
}
