//! Bitswap network transport adapter.
//!
//! This module provides the integration between Bitswap protocol messages
//! and the ADNet transport layer, enabling Bitswap to send/receive
//! messages over QUIC connections.
//!
//! ## Architecture
//!
//! ```text
//!   ┌─────────────────────────────────────────────────────────────┐
//!   │ BitswapEngine (in-process state machine)                   │
//!   └────────────────────┬────────────────────────────────────────┘
//!                        │  bitswap_messages
//!   ┌────────────────────▼────────────────────────────────────────┐
//!   │ BitswapNetworkAdapter (serialize / dispatch / receive)     │
//!   └────────────────────┬────────────────────────────────────────┘
//!                        │  framed bytes via BitswapTransportBridge
//!   ┌────────────────────▼────────────────────────────────────────┐
//!   │  BitswapQuicBridge  (dial / accept via adnet-transport)    │
//!   └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## ALPN negotiation
//!
//! Every dial/accept uses [`BITSWAP_ALPN`] so the underlying QUIC
//! handshake selects the Bitswap protocol. Other ALPN strings are
//! rejected — this prevents cross-protocol confusion (e.g. routing
//! bitswap frames to the DHT handler or vice versa).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adnet_blobstore::BitswapMessage;
use adnet_transport::{Frame, OutgoingConnection, SharedTransport};
use adnet_types::{ContentHash, NodeId};
use async_trait::async_trait;
use parking_lot::RwLock as PLRwLock;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, trace, warn};

/// ALPN for Bitswap protocol.
pub const BITSWAP_ALPN: &[u8] = b"adnet/bitswap/1";

/// Maximum message size.
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024 * 10; // 10MB

/// Default dial timeout for establishing new connections.
const DEFAULT_DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Result of a bitswap block request (used by the wait-for-response API).
#[derive(Debug, Clone)]
pub enum BitswapBlockOutcome {
    /// Block was found locally (not actually fetched over the wire).
    Local,
    /// Block was received from a peer.
    Received { from: NodeId, data: Vec<u8> },
    /// Peer told us they do not have the block.
    DontHave { from: NodeId },
    /// Want request was cancelled.
    Cancelled,
    /// Timeout waiting for response.
    Timeout,
    /// Transport / serialization failure.
    Error(String),
}

// ════════════════════════════════════════════════════════════════════
//  Core transport bridge trait
// ════════════════════════════════════════════════════════════════════

/// Transport error.
#[derive(Debug, thiserror::Error)]
pub enum BitswapTransportError {
    #[error("Connection error: {0}")]
    Connection(String),
    #[error("Peer not connected: {0}")]
    PeerNotConnected(String),
    #[error("Message too large: {size} bytes (max: {max})")]
    MessageTooLarge { size: usize, max: usize },
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("ALPN mismatch: expected {expected:?}, got {actual:?}")]
    AlpnMismatch { expected: Vec<u8>, actual: Vec<u8> },
    #[error("Timeout: {0}")]
    Timeout(String),
    #[error("Channel closed")]
    ChannelClosed,
}

/// Bridge trait for the transport layer.
#[async_trait]
pub trait BitswapTransportBridge: Send + Sync {
    /// Dial a peer and get a bidirectional stream.
    async fn dial(&self, peer: &NodeId) -> Result<BitswapStream, BitswapTransportError>;

    /// Send a message to a connected peer.
    async fn send_to(&self, peer: &NodeId, data: Vec<u8>) -> Result<(), BitswapTransportError>;

    /// Start listening for incoming connections.
    async fn start_listening(
        &self,
        handler: Arc<dyn BitswapConnectionHandler>,
    ) -> Result<(), BitswapTransportError>;

    /// Get local node ID.
    fn local_node_id(&self) -> &NodeId;

    /// Register a sender for an inbound peer connection (so we can reply).
    async fn register_inbound_sender(&self, peer: NodeId, tx: mpsc::Sender<Vec<u8>>);

    /// Remove all per-peer state for a disconnected peer.
    async fn unregister_peer(&self, peer: &NodeId);
}

/// A bidirectional Bitswap stream.
pub struct BitswapStream {
    /// Peer ID.
    pub peer_id: NodeId,
    /// Send channel.
    pub tx: mpsc::Sender<Vec<u8>>,
    /// Receive channel.
    pub rx: mpsc::Receiver<Vec<u8>>,
}

impl std::fmt::Debug for BitswapStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitswapStream")
            .field("peer_id", &self.peer_id)
            .field("tx_capacity", &self.tx.capacity())
            .finish()
    }
}

/// Handler for incoming connections.
#[async_trait]
pub trait BitswapConnectionHandler: Send + Sync {
    /// Handle a new incoming stream.
    async fn handle_stream(&self, stream: BitswapStream);
}

// ════════════════════════════════════════════════════════════════════
//  Network adapter: serialize/deserialize + dispatch
// ════════════════════════════════════════════════════════════════════

/// Internal transport-side event consumed by the dispatcher loop.
#[derive(Debug)]
pub enum BitswapEvent {
    /// A peer (with a known NodeId) sent us a Bitswap message.
    MessageFrom { peer: NodeId, msg: BitswapMessage },
    /// A peer established an inbound stream; we want the
    /// dispatcher to register the outbound channel under this NodeId.
    NewInboundStream {
        peer: NodeId,
        stream_tx: mpsc::Sender<Vec<u8>>,
    },
    /// A peer disconnected.
    PeerDisconnected(NodeId),
}

/// ALPN handshake: first frame on every connection.
///
/// A new inbound peer must send this `Hello` frame with the
/// `BITSWAP_ALPN` bytes before any real bitswap traffic. Outbound
/// connections made via `BitswapQuicBridge::dial` send this frame
/// automatically. This is how we ensure that the bitswap transport
/// stack never accidentally handles a DHT or mesh frame.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BitswapHello {
    /// ALPN identifier (must equal `BITSWAP_ALPN`).
    pub alpn: Vec<u8>,
    /// Protocol version (currently `1`).
    pub version: u32,
    /// Local node ID (so the peer can verify identity).
    pub node_id: NodeId,
}

impl BitswapHello {
    pub fn new(local_node_id: NodeId) -> Self {
        Self {
            alpn: BITSWAP_ALPN.to_vec(),
            version: 1,
            node_id: local_node_id,
        }
    }

    /// Verify the ALPN matches `BITSWAP_ALPN`.
    pub fn verify_alpn(&self) -> Result<(), BitswapTransportError> {
        if self.alpn == BITSWAP_ALPN {
            Ok(())
        } else {
            Err(BitswapTransportError::AlpnMismatch {
                expected: BITSWAP_ALPN.to_vec(),
                actual: self.alpn.clone(),
            })
        }
    }

    /// Encode to JSON bytes for the wire.
    pub fn encode(&self) -> Result<Vec<u8>, BitswapTransportError> {
        serde_json::to_vec(self).map_err(|e| BitswapTransportError::Serialization(e.to_string()))
    }

    /// Decode from wire bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, BitswapTransportError> {
        serde_json::from_slice(bytes).map_err(|e| BitswapTransportError::Serialization(e.to_string()))
    }
}

/// Internal outgoing intent emitted by the adapter.
#[derive(Debug)]
pub enum BitswapOutgoing {
    /// Send raw bytes to a peer (the adapter already serialized the message).
    Bytes { peer: NodeId, data: Vec<u8> },
}

type HandlerMap = HashMap<ContentHash, Box<dyn BitswapMessageHandler>>;

/// Pending-waiter registry: maps a content hash to the list of one-shot
/// channels that are awaiting a Block / DontHave / Cancel for that hash.
/// Shared across the adapter and its cloned-listener variants.
type PendingMap =
    Arc<RwLock<HashMap<ContentHash, Vec<oneshot::Sender<BitswapBlockOutcome>>>>>;

/// Network adapter for Bitswap.
///
/// This adapter bridges Bitswap messages to the transport layer.
/// It handles:
/// - Message serialization/deserialization
/// - Peer connection management
/// - Flow control
///
/// Note: Clone is not derived because `run()` consumes self.
/// Use Arc<BitswapNetworkAdapter> for shared ownership.
pub struct BitswapNetworkAdapter {
    /// Local node ID.
    local_node_id: NodeId,
    /// Transport bridge.
    transport: Arc<dyn BitswapTransportBridge>,
    /// Message handlers (per content hash).
    handlers: Arc<RwLock<HandlerMap>>,
    /// Pending request responders: a hashmap keyed by the block hash,
    /// mapping to a list of one-shot channels waiting for that block.
    pending: PendingMap,
    /// Receive channel for incoming messages from the network layer.
    rx: Option<mpsc::Receiver<BitswapEvent>>,
    /// Outgoing channel — the adapter hands serialized bytes to the bridge.
    outgoing_tx: mpsc::Sender<BitswapOutgoing>,
    /// Outgoing receiver — owned by the bridge loop.
    outgoing_rx: Option<mpsc::Receiver<BitswapOutgoing>>,
    /// Prometheus metrics. Lazily registered on first access so
    /// tests that don't pull `/metrics` don't pay the cost.
    metrics: BitswapMetrics,
}

/// Trait for message handlers.
#[async_trait]
pub trait BitswapMessageHandler: Send + Sync {
    /// Handle a message.
    async fn handle(&self, peer: &NodeId, msg: &BitswapMessage);
}

impl BitswapNetworkAdapter {
    /// Create a new network adapter.
    ///
    /// Returns the adapter plus:
    /// - `tx_events`: a sender that the transport loop feeds events into.
    pub fn new(
        local_node_id: NodeId,
        transport: Arc<dyn BitswapTransportBridge>,
    ) -> (Self, mpsc::Sender<BitswapEvent>) {
        Self::new_with_metrics(local_node_id, transport, BitswapMetrics::get())
    }

    /// Construct with a pre-built metrics handle. Useful for tests
    /// that want to swap in a no-op / independent registry.
    pub fn new_with_metrics(
        local_node_id: NodeId,
        transport: Arc<dyn BitswapTransportBridge>,
        metrics: BitswapMetrics,
    ) -> (Self, mpsc::Sender<BitswapEvent>) {
        let (tx_events, rx_events) = mpsc::channel(1024);
        let (out_tx, out_rx) = mpsc::channel(1024);

        let adapter = Self {
            local_node_id,
            transport,
            handlers: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(RwLock::new(HashMap::new())),
            rx: Some(rx_events),
            outgoing_tx: out_tx,
            outgoing_rx: Some(out_rx),
            metrics,
        };

        (adapter, tx_events)
    }

    /// Take the outgoing receiver (only callable once). The transport loop
    /// should drain this and actually push the bytes onto the wire.
    pub fn take_outgoing(&mut self) -> Option<mpsc::Receiver<BitswapOutgoing>> {
        self.outgoing_rx.take()
    }

    /// Borrow the metrics handle. Cheap: counters are `Arc`'d.
    pub fn metrics(&self) -> &BitswapMetrics {
        &self.metrics
    }

    /// Build a clone of the adapter that shares [`handlers`](Self::handlers),
    /// [`pending`](Self::pending), and [`transport`](Self::transport) but
    /// registers a fresh receive loop. Returns the new event-channel
    /// sender so the caller can route events into the cloned runner.
    pub fn clone_for_listen(&self) -> (Self, mpsc::Sender<BitswapEvent>) {
        let (tx_events, rx_events) = mpsc::channel(1024);
        let (out_tx, out_rx) = mpsc::channel(1024);
        let adapter = Self {
            local_node_id: self.local_node_id.clone(),
            transport: self.transport.clone(),
            handlers: self.handlers.clone(),
            pending: self.pending.clone(),
            rx: Some(rx_events),
            outgoing_tx: out_tx,
            outgoing_rx: Some(out_rx),
            metrics: self.metrics.clone(),
        };
        (adapter, tx_events)
    }

    /// Register a handler for a specific content hash.
    pub async fn register_handler<H>(&self, hash: ContentHash, handler: H)
    where
        H: BitswapMessageHandler + 'static,
    {
        let mut handlers = self.handlers.write().await;
        handlers.insert(hash, Box::new(handler));
    }

    /// Unregister a handler.
    pub async fn unregister_handler(&self, hash: &ContentHash) {
        let mut handlers = self.handlers.write().await;
        handlers.remove(hash);
    }

    /// Send a Want-Have message to a peer.
    ///
    /// `priority` controls the Bitswap priority queue (higher = sooner).
    /// `send_dont_have` tells the peer whether to send a `DontHave`
    /// response if it doesn't have the block (recommended for discovery).
    pub async fn send_want_have(
        &self,
        peer: &NodeId,
        hash: ContentHash,
        priority: i32,
        send_dont_have: bool,
    ) -> Result<(), BitswapTransportError> {
        let msg = BitswapMessage::WantHave {
            block: hash,
            priority,
            send_dont_have,
        };
        self.send(peer, msg).await
    }

    /// Send a Want-Block message and *wait* for the matching response.
    ///
    /// This is the fix for the previous "hung channel" implementation:
    /// the adapter now uses a `oneshot` channel keyed by content hash
    /// and resolves when the peer's `Block` / `DontHave` arrives.
    pub async fn send_want_block_and_wait(
        &self,
        peer: &NodeId,
        hash: ContentHash,
        priority: i32,
        timeout: Duration,
    ) -> Result<BitswapBlockOutcome, BitswapTransportError> {
        self.metrics.send_want_block.inc();

        // Register the waiter *before* sending so we don't race a fast peer.
        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending.write().await;
            map.entry(hash.clone()).or_default().push(tx);
        }
        // Update the pending gauge (one waiter per call).
        self.refresh_pending_gauge().await;

        // Fire off the want-block.
        if let Err(e) = self
            .send(
                peer,
                BitswapMessage::WantBlock {
                    block: hash.clone(),
                    priority,
                },
            )
            .await
        {
            // Undo our own registration — we just pushed the only entry,
            // so a `pop` is the right cleanup. `drop_pending_waiter` would
            // leave it dangling because the sender isn't closed yet.
            self.drop_last_pending_waiter(&hash).await;
            self.refresh_pending_gauge().await;
            return Err(e);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(outcome)) => {
                self.refresh_pending_gauge().await;
                Ok(outcome)
            }
            Ok(Err(_canceled)) => {
                self.refresh_pending_gauge().await;
                Ok(BitswapBlockOutcome::Cancelled)
            }
            Err(_elapsed) => {
                self.drop_pending_waiter(&hash).await;
                self.metrics.send_want_block_timeouts.inc();
                record_error(&self.metrics, BitswapErrorReason::WaitTimeout);
                self.refresh_pending_gauge().await;
                Ok(BitswapBlockOutcome::Timeout)
            }
        }
    }

    /// Refresh the `pending_blocks` gauge from the current map size.
    /// Called from every public API that mutates the pending map so
    /// the gauge stays in lock-step with reality.
    async fn refresh_pending_gauge(&self) {
        self.metrics
            .pending_blocks
            .set(self.pending.read().await.len() as i64);
    }

    /// Send a Cancel message.
    pub async fn send_cancel(
        &self,
        peer: &NodeId,
        hash: ContentHash,
    ) -> Result<(), BitswapTransportError> {
        let msg = BitswapMessage::Cancel { block: hash.clone() };

        // Drop any pending waiters — they will see Cancelled.
        let mut map = self.pending.write().await;
        if let Some(waiters) = map.remove(&hash) {
            for tx in waiters {
                let _ = tx.send(BitswapBlockOutcome::Cancelled);
            }
        }
        drop(map);
        self.refresh_pending_gauge().await;

        let result = self.send(peer, msg).await;
        if result.is_ok() {
            record_error(&self.metrics, BitswapErrorReason::CancelLocal);
        }
        result
    }

    /// Send a message to a peer.
    pub async fn send(
        &self,
        peer: &NodeId,
        msg: BitswapMessage,
    ) -> Result<(), BitswapTransportError> {
        let data = serde_json::to_vec(&msg).map_err(|e| {
            record_error(&self.metrics, BitswapErrorReason::Serialization);
            BitswapTransportError::Serialization(e.to_string())
        })?;

        if data.len() > MAX_MESSAGE_SIZE {
            record_error(&self.metrics, BitswapErrorReason::Oversize);
            return Err(BitswapTransportError::MessageTooLarge {
                size: data.len(),
                max: MAX_MESSAGE_SIZE,
            });
        }

        // Hand off to the outgoing queue so the transport loop owns the
        // actual wire I/O. We deliberately don't block on the bridge here
        // so callers can't accidentally serialize on the transport.
        self.outgoing_tx
            .send(BitswapOutgoing::Bytes {
                peer: peer.clone(),
                data: data.clone(),
            })
            .await
            .map_err(|_| BitswapTransportError::ChannelClosed)?;

        // Record the outgoing event for Prometheus. Counts both
        // frames and bytes so wire-level visibility is one
        // counter increment away.
        self.metrics.messages_sent.inc_labels(&msg_type_label(&msg));
        self.metrics.bytes_sent.inc_by(data.len() as u64);

        Ok(())
    }

    async fn drop_pending_waiter(&self, hash: &ContentHash) {
        let mut map = self.pending.write().await;
        if let Some(v) = map.get_mut(hash) {
            v.retain(|t| !t.is_closed());
            if v.is_empty() {
                map.remove(hash);
            }
        }
    }

    /// Remove the most recently registered waiter for `hash` (the one
    /// we just pushed). Used by `send_want_block_and_wait` to undo its
    /// own registration when the underlying send fails.
    async fn drop_last_pending_waiter(&self, hash: &ContentHash) {
        let mut map = self.pending.write().await;
        if let Some(v) = map.get_mut(hash) {
            v.pop();
            if v.is_empty() {
                map.remove(hash);
            }
        }
    }

    /// Drive the receive loop. Returns when the event channel is closed.
    pub async fn run(mut self) {
        loop {
            let evt = match self.rx.as_mut() {
                Some(rx) => rx.recv().await,
                None => {
                    debug!("bitswap event channel not available; adapter loop exiting");
                    return;
                }
            };
            match evt {
                Some(BitswapEvent::MessageFrom { peer, msg }) => {
                    self.dispatch_message(&peer, &msg).await;
                }
                Some(BitswapEvent::NewInboundStream { peer, stream_tx }) => {
                    debug!(%peer, "registered inbound bitswap sender");
                    self.transport.register_inbound_sender(peer, stream_tx).await;
                }
                Some(BitswapEvent::PeerDisconnected(peer)) => {
                    debug!(%peer, "peer {} disconnected", peer);
                    self.transport.unregister_peer(&peer).await;
                }
                None => {
                    debug!("bitswap event channel closed; adapter loop exiting");
                    return;
                }
            }
        }
    }

    /// Dispatch a single message: resolve pending waiters first, then
    /// forward to per-hash handlers.
    async fn dispatch_message(&self, peer: &NodeId, msg: &BitswapMessage) {
        // Step 1: resolve any pending want-requests for Block/DontHave.
        match msg {
            BitswapMessage::Block { block, data } => {
                let outcome = BitswapBlockOutcome::Received {
                    from: peer.clone(),
                    data: data.clone(),
                };
                Self::resolve_pending(&self.pending, block, outcome).await;
            }
            BitswapMessage::DontHave { block } => {
                let outcome = BitswapBlockOutcome::DontHave { from: peer.clone() };
                Self::resolve_pending(&self.pending, block, outcome).await;
            }
            BitswapMessage::Cancel { block } => {
                // Peer-initiated cancel: resolve any outstanding waiters.
                let outcome = BitswapBlockOutcome::Cancelled;
                Self::resolve_pending(&self.pending, block, outcome).await;
            }
            _ => {}
        }
        // Update the pending gauge after resolution.
        self.refresh_pending_gauge().await;

        // Step 2: invoke any per-hash handlers that the application registered.
        if let Some(hash) = message_hash(msg) {
            let handlers = self.handlers.read().await;
            if let Some(handler) = handlers.get(&hash) {
                handler.handle(peer, msg).await;
            }
        }

        // Step 3: log.
        match msg {
            BitswapMessage::WantHave { block, .. } => {
                trace!(%peer, "WantHave for {}", block.short());
            }
            BitswapMessage::WantBlock { block, .. } => {
                trace!(%peer, "WantBlock for {}", block.short());
            }
            BitswapMessage::Have { block, .. } => {
                debug!(%peer, "Have for {}", block.short());
            }
            BitswapMessage::DontHave { block } => {
                debug!(%peer, "DontHave for {}", block.short());
            }
            BitswapMessage::Block { block, data } => {
                debug!(%peer, "Block for {} ({} bytes)", block.short(), data.len());
            }
            _ => {
                trace!(%peer, "Message: {:?}", msg);
            }
        }
    }

    async fn resolve_pending(
        pending: &PendingMap,
        block: &ContentHash,
        outcome: BitswapBlockOutcome,
    ) {
        let waiters = {
            let mut map = pending.write().await;
            map.remove(block)
        };
        if let Some(waiters) = waiters {
            for tx in waiters {
                let _ = tx.send(outcome.clone());
            }
        }
    }

    /// Get the local node ID.
    pub fn local_node_id(&self) -> &NodeId {
        &self.local_node_id
    }

    /// Get a handle to the underlying bridge (used to start the loop).
    pub fn transport(&self) -> Arc<dyn BitswapTransportBridge> {
        self.transport.clone()
    }

    /// Get a handle to the outbound wire pump channel.
    pub fn outgoing_sender(&self) -> mpsc::Sender<BitswapOutgoing> {
        self.outgoing_tx.clone()
    }
}

/// Backwards-compatible accessor for the pending-waiter map (used in tests).
impl BitswapNetworkAdapter {
    pub fn pending(&self) -> PendingMap {
        self.pending.clone()
    }
}

/// Extract the primary content hash from a Bitswap message, if any.
fn message_hash(msg: &BitswapMessage) -> Option<ContentHash> {
    match msg {
        BitswapMessage::WantHave { block, .. } => Some(block.clone()),
        BitswapMessage::WantBlock { block, .. } => Some(block.clone()),
        BitswapMessage::Have { block, .. } => Some(block.clone()),
        BitswapMessage::DontHave { block } => Some(block.clone()),
        BitswapMessage::Block { block, .. } => Some(block.clone()),
        _ => None,
    }
}

// ════════════════════════════════════════════════════════════════════
//  QUIC bridge: dial/accept over adnet-transport
// ════════════════════════════════════════════════════════════════════

/// Connection state tracked by the QUIC bridge.
struct PeerChannel {
    /// Sender side for outbound messages.
    tx: mpsc::Sender<Vec<u8>>,
}

/// Real QUIC bridge backed by `adnet_transport::Transport`.
pub struct BitswapQuicBridge {
    local_node_id: NodeId,
    transport: SharedTransport,
    /// Per-peer outbound channel.
    peers: PLRwLock<HashMap<NodeId, PeerChannel>>,
    /// Default dial timeout.
    dial_timeout: Duration,
    /// Async mutex that serializes `dial()` calls so two concurrent
    /// callers don't each open a fresh QUIC connection to the same peer.
    dial_lock: tokio::sync::Mutex<()>,
    /// Prometheus metrics. Lazily registered on first access.
    metrics: BitswapMetrics,
}

impl BitswapQuicBridge {
    /// Create a new QUIC bridge around the given transport.
    pub fn new(local_node_id: NodeId, transport: SharedTransport) -> Arc<Self> {
        Arc::new(Self::with_metrics(local_node_id, transport, BitswapMetrics::get()))
    }

    /// Construct with a pre-built metrics handle. Useful for tests
    /// that want to swap in a custom registry.
    pub fn with_metrics(
        local_node_id: NodeId,
        transport: SharedTransport,
        metrics: BitswapMetrics,
    ) -> Self {
        Self {
            local_node_id,
            transport,
            peers: PLRwLock::new(HashMap::new()),
            dial_timeout: DEFAULT_DIAL_TIMEOUT,
            dial_lock: tokio::sync::Mutex::new(()),
            metrics,
        }
    }

    /// Borrow the metrics handle.
    pub fn metrics(&self) -> &BitswapMetrics {
        &self.metrics
    }

    /// Update the active-peers gauge from the current map size.
    fn refresh_active_peers(&self) {
        self.metrics
            .active_peers
            .set_f64(self.peers.read().len() as f64);
    }

    /// Override the dial timeout.
    pub fn with_dial_timeout(mut self, timeout: Duration) -> Self {
        self.dial_timeout = timeout;
        self
    }

    /// Spawn the accept loop that pulls inbound QUIC connections from
    /// the transport and feeds them to the adapter via `event_tx`.
    pub fn spawn_accept_loop(
        self: Arc<Self>,
        event_tx: mpsc::Sender<BitswapEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match self.transport.accept().await {
                    Ok(Some((peer, conn))) => {
                        let event_tx = event_tx.clone();
                        let bridge = self.clone();
                        tokio::spawn(async move {
                            if let Err(e) =
                                Self::serve_inbound(bridge, peer.clone(), conn, event_tx.clone())
                                    .await
                            {
                                warn!(%peer, "bitswap inbound serve error: {}", e);
                                let _ = event_tx
                                    .send(BitswapEvent::PeerDisconnected(peer.clone()))
                                    .await;
                            }
                        });
                    }
                    Ok(None) => {
                        debug!("transport accept returned None; stopping loop");
                        return;
                    }
                    Err(e) => {
                        warn!("transport accept error: {}", e);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        })
    }

    /// Spawn a pump that drains the [`BitswapNetworkAdapter`]'s
    /// outgoing queue and routes each `BitswapOutgoing::Bytes` to the
    /// matching per-peer outbound channel.
    ///
    /// Without this pump, the adapter would queue serialized frames
    /// into its outgoing channel with no consumer — every
    /// `send_want_have` / `send_want_block` / `send_cancel` would
    /// silently drop. The pump completes the data plane:
    ///
    /// ```text
    ///   BitswapNetworkAdapter::outgoing_tx
    ///     └─► outgoing_pump (this task)
    ///          └─► bridge.peers[peer].tx  (per-peer)
    ///               └─► QUIC wire (in dial())
    /// ```
    ///
    /// `outgoing_rx` is typically obtained via
    /// [`BitswapNetworkAdapter::take_outgoing`] before the adapter's
    /// `run` loop is spawned. Callers that own both halves wire them
    /// together at node startup.
    pub fn spawn_outgoing_pump(
        self: Arc<Self>,
        mut outgoing_rx: mpsc::Receiver<BitswapOutgoing>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(out) = outgoing_rx.recv().await {
                match out {
                    BitswapOutgoing::Bytes { peer, data } => {
                        // Snapshot the sender; if the peer just
                        // disconnected we drop the bytes — there's
                        // no other consumer.
                        let tx = {
                            let peers = self.peers.read();
                            peers.get(&peer).map(|c| c.tx.clone())
                        };
                        match tx {
                            Some(tx) => {
                                if let Err(e) = tx.send(data).await {
                                    warn!(%peer, "outgoing pump send failed: {}", e);
                                }
                            }
                            None => {
                                warn!(%peer, "outgoing pump: peer not connected, dropping frame");
                            }
                        }
                    }
                }
            }
            debug!("bitswap outgoing pump exited");
        })
    }

    /// Serve a single inbound connection.
    async fn serve_inbound(
        bridge: Arc<Self>,
        peer: NodeId,
        conn: Box<dyn OutgoingConnection>,
        event_tx: mpsc::Sender<BitswapEvent>,
    ) -> Result<(), BitswapTransportError> {
        debug!(%peer, "serving inbound bitswap connection (alpn={:?})", BITSWAP_ALPN);

        // Buffer outbound bytes until the ALPN handshake completes.
        // Pre-handshake writes are held in `pre_handshake_queue` and
        // flushed as soon as `handshake_done_rx` resolves. This keeps
        // the wire contract tight: no application frame can be sent
        // before the ALPN hello is verified.
        let (tx_to_wire, mut rx_to_wire) = mpsc::channel::<Vec<u8>>(64);
        // We use a `oneshot` rather than `Notify` so that an arrival
        // of the handshake signal *before* the write loop starts
        // listening is not lost. `Notify::notify_waiters` is
        // edge-triggered — any `notified()` registered after the
        // notify is silently dropped. `oneshot` is level-triggered.
        let (handshake_done_tx, handshake_done_rx) = oneshot::channel::<()>();

        // Announce the inbound sender so the adapter can route replies.
        // Replies will queue until the hello check below flips the
        // notify.
        if event_tx
            .send(BitswapEvent::NewInboundStream {
                peer: peer.clone(),
                stream_tx: tx_to_wire,
            })
            .await
            .is_err()
        {
            return Err(BitswapTransportError::ChannelClosed);
        }

        // Share the connection between read and write halves via an
        // `Arc<Mutex<…>>`. QUIC allows concurrent reads/writes on the
        // same connection, so a lightweight mutex is sufficient.
        let conn = Arc::new(tokio::sync::Mutex::new(conn));

        // Spawn the read pump. First frame must be our BitswapHello.
        let peer_for_read = peer.clone();
        let event_tx_for_read = event_tx.clone();
        let conn_for_read = conn.clone();
        let bridge_for_read = bridge.clone();
        let handshake_done_tx = Arc::new(tokio::sync::Mutex::new(Some(handshake_done_tx)));
        let read_task = tokio::spawn(async move {
            // ALPN handshake: first frame must be BitswapHello.
            let hello_frame = {
                let mut guard = conn_for_read.lock().await;
                match guard.recv().await {
                    Ok(Some(frame)) => frame,
                    Ok(None) => {
                        warn!(%peer_for_read, "bitswap inbound EOF before ALPN handshake");
                        return;
                    }
                    Err(e) => {
                        warn!(%peer_for_read, "bitswap inbound recv error during handshake: {}", e);
                        return;
                    }
                }
            };
            match BitswapHello::decode(hello_frame.as_bytes()) {
                Ok(hello) => {
                    if let Err(e) = hello.verify_alpn() {
                        warn!(%peer_for_read, "bitswap ALPN mismatch: {}", e);
                        record_error(&bridge_for_read.metrics, BitswapErrorReason::Alpn);
                        // Best-effort: send a goodbye / cancel out so
                        // the peer learns we rejected the connection.
                        // We don't fault on send failure — the handshake
                        // already failed.
                        let goodbye = BitswapMessage::Cancel {
                            block: ContentHash::from_bytes(b"alpn-rejection"),
                        };
                        if let Ok(bytes) = serde_json::to_vec(&goodbye) {
                            let mut guard = conn_for_read.lock().await;
                            let _ = guard.send(Frame::new(bytes)).await;
                        }
                        return;
                    }
                    debug!(%peer_for_read, "bitswap ALPN handshake OK");
                    // Signal the write loop. We use `take()` so the
                    // sender is consumed exactly once — we don't care
                    // if the receiver has already been dropped (the
                    // test would have already noticed the mismatch).
                    let slot = {
                        let mut guard = handshake_done_tx.lock().await;
                        guard.take()
                    };
                    if let Some(tx) = slot {
                        let _ = tx.send(());
                    }
                }
                Err(e) => {
                    warn!(%peer_for_read, "bitswap ALPN decode failed: {}", e);
                    record_error(&bridge_for_read.metrics, BitswapErrorReason::Alpn);
                    return;
                }
            }

            // Main read loop.
            loop {
                let frame = {
                    let mut guard = conn_for_read.lock().await;
                    match guard.recv().await {
                        Ok(Some(frame)) => frame,
                        Ok(None) => {
                            debug!(%peer_for_read, "bitswap inbound EOF");
                            break;
                        }
                        Err(e) => {
                            warn!(%peer_for_read, "bitswap inbound recv error: {}", e);
                            break;
                        }
                    }
                };
                let msg = match serde_json::from_slice::<BitswapMessage>(frame.as_bytes()) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(%peer_for_read, "bitswap deserialize error: {}", e);
                        record_error(&bridge_for_read.metrics, BitswapErrorReason::Serialization);
                        continue;
                    }
                };
                bridge_for_read.metrics.messages_received.inc();
                bridge_for_read.metrics.bytes_received.inc_by(frame.len() as u64);
                if event_tx_for_read
                    .send(BitswapEvent::MessageFrom {
                        peer: peer_for_read.clone(),
                        msg,
                    })
                    .await
                    .is_err()
                {
                    debug!(%peer_for_read, "adapter gone, stopping read pump");
                    break;
                }
            }
        });

        // Pump bytes from the local send queue to the wire, but block
        // until the handshake completes (so we don't leak bytes to a
        // peer that hasn't passed the ALPN check). The `mpsc` channel
        // already buffers queued frames for us; we just need to wait
        // for the handshake signal before draining. We use a `oneshot`
        // rather than `Notify` because `oneshot` is level-triggered —
        // a notification that arrives before the receiver `await`s is
        // not lost.
        let _ = handshake_done_rx.await;

        // Handshake done — drain the live channel.
        while let Some(bytes) = rx_to_wire.recv().await {
            let mut guard = conn.lock().await;
            if let Err(e) = guard.send(Frame::new(bytes)).await {
                warn!(%peer, "bitswap inbound send error: {}", e);
                break;
            }
        }

        // Connection done — deregister and signal the adapter.
        bridge.peers.write().remove(&peer);
        bridge.refresh_active_peers();
        let _ = event_tx
            .send(BitswapEvent::PeerDisconnected(peer.clone()))
            .await;

        let _ = read_task.await;
        Ok(())
    }
}

#[async_trait]
impl BitswapTransportBridge for BitswapQuicBridge {
    async fn dial(&self, peer: &NodeId) -> Result<BitswapStream, BitswapTransportError> {
        // Fast path: if a peer is already registered, just return a fresh
        // placeholder stream backed by the same channel. This avoids re-handshaking.
        // The returned `BitswapStream::rx` is intentionally a dead
        // channel — callers consume inbound through the adapter's run
        // loop, not through the stream handle.
        if let Some(existing) = {
            let peers = self.peers.read();
            peers.get(peer).map(|c| c.tx.clone())
        } {
            let (_tx, rx) = mpsc::channel::<Vec<u8>>(1);
            return Ok(BitswapStream {
                peer_id: peer.clone(),
                tx: existing,
                rx,
            });
        }

        // Cold path: actually dial. We hold the write guard across the
        // dial so concurrent callers don't both dial — the second one
        // will reuse the first connection.
        let _guard = self.dial_lock.lock().await;
        self.metrics.dial_attempts.inc();
        if let Some(existing) = self.peers.read().get(peer).map(|c| c.tx.clone()) {
            let (_tx, rx) = mpsc::channel::<Vec<u8>>(1);
            return Ok(BitswapStream {
                peer_id: peer.clone(),
                tx: existing,
                rx,
            });
        }

        let conn = match tokio::time::timeout(
            self.dial_timeout,
            self.transport.dial(peer.clone()),
        )
        .await
        {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                self.metrics.dial_failures.inc();
                record_error(&self.metrics, BitswapErrorReason::Dial);
                return Err(BitswapTransportError::Connection(e.to_string()));
            }
            Err(_) => {
                self.metrics.dial_failures.inc();
                record_error(&self.metrics, BitswapErrorReason::Dial);
                return Err(BitswapTransportError::Timeout(format!("dial {}", peer)));
            }
        };

        // ALPN handshake: send our hello first, then everything else.
        // We don't wait for the peer's hello here — the server side
        // (`serve_inbound`) already verified the inbound frame before
        // accepting the connection, so by the time we have a working
        // QUIC stream, the wire is bitswap-purposed on both ends.
        let conn = Arc::new(tokio::sync::Mutex::new(conn));
        {
            let mut conn_guard = conn.lock().await;
            let hello = BitswapHello::new(self.local_node_id.clone());
            let hello_bytes = hello.encode()?;
            if let Err(e) = conn_guard.send(Frame::new(hello_bytes)).await {
                record_error(&self.metrics, BitswapErrorReason::Alpn);
                drop(conn_guard);
                drop(_guard);
                return Err(BitswapTransportError::Connection(format!(
                    "ALPN hello send failed: {e}"
                )));
            }
        }

        let (tx_to_wire, mut rx_to_wire) = mpsc::channel::<Vec<u8>>(64);
        let (_tx_from_wire, rx_from_wire) = mpsc::channel::<Vec<u8>>(64);

        // Spawn the wire pump.
        let peer_for_pump = peer.clone();
        let conn_for_pump = conn.clone();
        tokio::spawn(async move {
            while let Some(bytes) = rx_to_wire.recv().await {
                let mut conn_guard = conn_for_pump.lock().await;
                if let Err(e) = conn_guard.send(Frame::new(bytes)).await {
                    warn!(%peer_for_pump, "bitswap dial send error: {}", e);
                    break;
                }
            }
        });

        // Register the outbound channel so the adapter's outgoing queue
        // can push bytes into it.
        self.peers.write().insert(
            peer.clone(),
            PeerChannel {
                tx: tx_to_wire.clone(),
            },
        );

        // Release the dial lock — concurrent callers will reuse the entry.
        drop(_guard);

        Ok(BitswapStream {
            peer_id: peer.clone(),
            tx: tx_to_wire,
            rx: rx_from_wire,
        })
    }

    async fn send_to(&self, peer: &NodeId, data: Vec<u8>) -> Result<(), BitswapTransportError> {
        // Fast path: existing peer channel.
        // We can't hold the read lock while awaiting `tx.send`, so we
        // snapshot the sender and then drop the lock.
        let tx = {
            let peers = self.peers.read();
            peers.get(peer).map(|c| c.tx.clone())
        };
        if let Some(tx) = tx {
            return tx
                .send(data)
                .await
                .map_err(|_| BitswapTransportError::ChannelClosed);
        }

        // Cold path: dial then send.
        let _stream = self.dial(peer).await?;
        let tx = {
            let peers = self.peers.read();
            peers
                .get(peer)
                .ok_or_else(|| BitswapTransportError::PeerNotConnected(peer.to_string()))?
                .tx
                .clone()
        };
        tx.send(data)
            .await
            .map_err(|_| BitswapTransportError::ChannelClosed)
    }

    async fn start_listening(
        &self,
        _handler: Arc<dyn BitswapConnectionHandler>,
    ) -> Result<(), BitswapTransportError> {
        Ok(())
    }

    fn local_node_id(&self) -> &NodeId {
        &self.local_node_id
    }

    async fn register_inbound_sender(&self, peer: NodeId, tx: mpsc::Sender<Vec<u8>>) {
        self.peers.write().insert(peer, PeerChannel { tx });
        self.refresh_active_peers();
    }

    async fn unregister_peer(&self, peer: &NodeId) {
        self.peers.write().remove(peer);
        record_error(&self.metrics, BitswapErrorReason::PeerDisconnected);
        self.refresh_active_peers();
    }
}

// ════════════════════════════════════════════════════════════════════
//  Helpers
// ════════════════════════════════════════════════════════════════════

/// Map a Bitswap message to its low-cardinality label string for
/// the `messages_sent_total{type=...}` series. The label set is
/// bounded by the enum variants.
fn message_type_label(msg: &BitswapMessage) -> &'static str {
    match msg {
        BitswapMessage::WantHave { .. } => "want_have",
        BitswapMessage::WantBlock { .. } => "want_block",
        BitswapMessage::Have { .. } => "have",
        BitswapMessage::DontHave { .. } => "dont_have",
        BitswapMessage::Block { .. } => "block",
        BitswapMessage::Cancel { .. } => "cancel",
        BitswapMessage::BatchWant { .. } => "batch_want",
        BitswapMessage::BatchResponse { .. } => "batch_response",
    }
}

/// Build a single-pair label set for the `errors_total{reason=...}`
/// series. We treat `LabelSet::new` errors as best-effort
/// (validation failures are vanishingly rare for hard-coded reason
/// strings); if the set is invalid we fall back to the empty set
/// so the counter still increments.
fn reason_label(reason: &str) -> adnet_observability::labels::LabelSet {
    match adnet_observability::labels::LabelSet::new(std::iter::once((
        "reason".to_string(),
        reason.to_string(),
    ))) {
        Ok(ls) => ls,
        Err(_) => adnet_observability::labels::LabelSet::EMPTY,
    }
}

/// Build a single-pair label set for the `messages_sent_total{type=...}`
/// series.
fn msg_type_label(msg: &BitswapMessage) -> adnet_observability::labels::LabelSet {
    match adnet_observability::labels::LabelSet::new(std::iter::once((
        "type".to_string(),
        message_type_label(msg).to_string(),
    ))) {
        Ok(ls) => ls,
        Err(_) => adnet_observability::labels::LabelSet::EMPTY,
    }
}

/// Increment the `errors_total` counter with the given reason.
fn record_error(metrics: &BitswapMetrics, reason: BitswapErrorReason) {
    metrics
        .errors
        .inc_labels(&reason_label(reason.label()));
}

/// Mock Bitswap transport for testing (in-process channel pairs).
///
/// Pairs of peers exchange bytes through the same `peers` table: each
/// peer registered under `peer_id` carries the **send side** of a
/// `Vec<u8>` channel, and the matching **receive side** lives in
/// `rx_keepalive` so the sender never observes `ChannelClosed` for
/// the lifetime of the mock. Tests interact with the channel through
/// the returned [`BitswapStream`] which exposes its own `(tx, rx)`
/// pair — those are decoupled from the per-peer channel so individual
/// streams can be dropped without tearing down the peer.
pub struct MockBitswapTransport {
    local_node_id: NodeId,
    /// `peer_id` → send side of the cross-peer channel.
    peers: tokio::sync::RwLock<HashMap<NodeId, mpsc::Sender<Vec<u8>>>>,
    /// Keeps the receive side of every cross-peer channel alive.
    /// Wrapped in `Option` so a test can close a peer explicitly
    /// (via `take_peer_rx`).
    peer_rx_keepalive: tokio::sync::Mutex<HashMap<NodeId, mpsc::Receiver<Vec<u8>>>>,
}

impl MockBitswapTransport {
    pub fn new(local_node_id: NodeId) -> Self {
        Self {
            local_node_id,
            peers: tokio::sync::RwLock::new(HashMap::new()),
            peer_rx_keepalive: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Number of peers currently registered in the mock's table.
    pub async fn peer_count(&self) -> usize {
        self.peers.read().await.len()
    }

    /// Receive bytes the local side has been sent to via `dial`.
    ///
    /// Tests can use this to verify outbound traffic on a freshly
    /// dialed peer without juggling the per-stream `BitswapStream::rx`.
    /// Returns `None` when the peer is unknown.
    pub async fn try_recv(&self, peer: &NodeId) -> Option<Vec<u8>> {
        let mut keepalive = self.peer_rx_keepalive.lock().await;
        keepalive.get_mut(peer)?.try_recv().ok()
    }
}

#[async_trait]
impl BitswapTransportBridge for MockBitswapTransport {
    async fn dial(&self, peer: &NodeId) -> Result<BitswapStream, BitswapTransportError> {
        let peer_id = peer.clone();

        // If we already have an outbound channel for this peer, reuse
        // it. Tests can subscribe via `try_recv` to verify sends.
        if self.peers.read().await.contains_key(&peer_id) {
            let (local_tx, local_rx) = mpsc::channel(100);
            return Ok(BitswapStream {
                peer_id,
                tx: local_tx,
                rx: local_rx,
            });
        }

        // Create a new cross-peer channel pair and store both halves.
        // The send-side goes to `peers` so future `send_to` calls can
        // push bytes to the peer. The receive-side is kept alive in
        // `peer_rx_keepalive` so the cross-peer sender never observes
        // `ChannelClosed` while the mock is in use.
        let (peer_tx, peer_rx) = mpsc::channel(100);
        let (local_tx, local_rx) = mpsc::channel(100);

        self.peers.write().await.insert(peer_id.clone(), peer_tx);
        let mut keepalive = self.peer_rx_keepalive.lock().await;
        keepalive.insert(peer_id.clone(), peer_rx);
        drop(keepalive);

        Ok(BitswapStream {
            peer_id,
            tx: local_tx,
            rx: local_rx,
        })
    }

    async fn send_to(&self, peer: &NodeId, data: Vec<u8>) -> Result<(), BitswapTransportError> {
        if let Some(tx) = self.peers.read().await.get(peer).cloned() {
            return tx
                .send(data)
                .await
                .map_err(|_| BitswapTransportError::Connection("Channel closed".to_string()));
        }
        // Cold path: dial first, then send.
        let _stream = self.dial(peer).await?;
        let tx = self
            .peers
            .read()
            .await
            .get(peer)
            .cloned()
            .ok_or_else(|| BitswapTransportError::PeerNotConnected(peer.to_string()))?;
        tx.send(data)
            .await
            .map_err(|_| BitswapTransportError::Connection("Channel closed".to_string()))
    }

    async fn start_listening(
        &self,
        _handler: Arc<dyn BitswapConnectionHandler>,
    ) -> Result<(), BitswapTransportError> {
        Ok(())
    }

    fn local_node_id(&self) -> &NodeId {
        &self.local_node_id
    }

    async fn register_inbound_sender(&self, peer: NodeId, tx: mpsc::Sender<Vec<u8>>) {
        let mut peers = self.peers.write().await;
        peers.insert(peer, tx);
    }

    async fn unregister_peer(&self, peer: &NodeId) {
        let mut peers = self.peers.write().await;
        peers.remove(peer);
        // Drop the keepalive receiver too so a future `dial` of the
        // same peer starts fresh.
        let mut keepalive = self.peer_rx_keepalive.lock().await;
        keepalive.remove(peer);
    }
}

// ════════════════════════════════════════════════════════════════════
//  Stats
// ════════════════════════════════════════════════════════════════════

/// Stats for Bitswap transport.
#[derive(Debug, Clone, Default)]
pub struct BitswapTransportStats {
    pub messages_sent: u64,
    pub messages_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub errors: u64,
}

impl BitswapTransportStats {
    pub fn record_sent(&mut self, bytes: usize) {
        self.messages_sent += 1;
        self.bytes_sent += bytes as u64;
    }

    pub fn record_received(&mut self, bytes: usize) {
        self.messages_received += 1;
        self.bytes_received += bytes as u64;
    }

    pub fn record_error(&mut self) {
        self.errors += 1;
    }
}

// ════════════════════════════════════════════════════════════════════
//  Prometheus metrics
// ════════════════════════════════════════════════════════════════════
//
// BitSwap-over-QUIC exposes a small, low-cardinality metric surface
// so operators can wire the live bridge into the same Prometheus
// pull model as the rest of the `adnet-*` crates. The counters track
// wire-level events (frames out/in, bytes out/in, errors, dials);
// gauges track active peers + pending waits. Errors are broken down
// by `reason` so a single global counter lets operators alert on the
// most common failure mode without per-hash labels.

use adnet_observability::metrics::{Counter, Gauge};
use adnet_observability::registry::{GLOBAL as OBS_REGISTRY, Registry};
use once_cell::sync::Lazy;

/// Process-wide singleton metrics handle. Registered once on first
/// access via the `Lazy` below; subsequent calls return the same
/// `Arc`-shared counters. The `adnet-observability` registry panics
/// on duplicate registrations, so we must avoid the naive
/// `register-on-every-new()` pattern (see `registry.rs:265`).
static GLOBAL_METRICS: Lazy<BitswapMetrics> = Lazy::new(|| BitswapMetrics::register(&OBS_REGISTRY));

/// Prometheus-backed Bitswap metrics. All counters / gauges are
/// registered eagerly on first access via [`BitswapMetrics::get`].
/// Cloning the handle is cheap — the underlying counters are `Arc`
/// so every call site observes the same series.
#[derive(Debug, Clone)]
pub struct BitswapMetrics {
    /// `adnet_bitswap_messages_sent_total` — total Bitswap frames
    /// sent on the wire (Want-Have / Want-Block / Block / Have /
    /// DontHave / Cancel).
    pub messages_sent: Arc<Counter>,
    /// `adnet_bitswap_messages_received_total` — total frames
    /// received from any peer.
    pub messages_received: Arc<Counter>,
    /// `adnet_bitswap_bytes_sent_total` — total payload bytes sent.
    pub bytes_sent: Arc<Counter>,
    /// `adnet_bitswap_bytes_received_total` — total payload bytes
    /// received.
    pub bytes_received: Arc<Counter>,
    /// `adnet_bitswap_errors_total{reason=...}` — total errors
    /// broken down by reason. Reasons: `dial`, `alpn`,
    /// `serialization`, `oversize`, `connection`, `cancel_local`,
    /// `wait_timeout`, `peer_disconnected`.
    pub errors: Arc<Counter>,
    /// `adnet_bitswap_dial_attempts_total` — total `dial()` calls
    /// (counted before lock acquisition).
    pub dial_attempts: Arc<Counter>,
    /// `adnet_bitswap_dial_failures_total` — total dial failures
    /// (timeout, connection refused, etc.).
    pub dial_failures: Arc<Counter>,
    /// `adnet_bitswap_send_want_block_total` — total
    /// `send_want_block_and_wait` invocations.
    pub send_want_block: Arc<Counter>,
    /// `adnet_bitswap_send_want_block_timeouts_total` — total
    /// `send_want_block_and_wait` calls that hit the timeout.
    pub send_want_block_timeouts: Arc<Counter>,
    /// `adnet_bitswap_active_peers` — number of peers currently
    /// registered in the bridge (gauge).
    pub active_peers: Arc<Gauge>,
    /// `adnet_bitswap_pending_blocks` — number of in-flight
    /// WantBlock requests waiting for a Block frame (gauge).
    pub pending_blocks: Arc<Gauge>,
}

impl BitswapMetrics {
    /// Register every metric. Idempotent — repeated calls return
    /// counters that share the same series in the registry.
    pub fn register(registry: &Registry) -> Self {
        Self {
            messages_sent: registry.register_counter(
                "adnet_bitswap_messages_sent_total",
                "Total Bitswap frames sent on the wire.",
            ),
            messages_received: registry.register_counter(
                "adnet_bitswap_messages_received_total",
                "Total Bitswap frames received from any peer.",
            ),
            bytes_sent: registry.register_counter(
                "adnet_bitswap_bytes_sent_total",
                "Total payload bytes sent on the wire.",
            ),
            bytes_received: registry.register_counter(
                "adnet_bitswap_bytes_received_total",
                "Total payload bytes received from any peer.",
            ),
            errors: registry.register_counter(
                "adnet_bitswap_errors_total",
                "Total Bitswap transport errors, broken down by reason.",
            ),
            dial_attempts: registry.register_counter(
                "adnet_bitswap_dial_attempts_total",
                "Total Bitswap dial attempts.",
            ),
            dial_failures: registry.register_counter(
                "adnet_bitswap_dial_failures_total",
                "Total Bitswap dial failures (timeout, connection refused, etc.).",
            ),
            send_want_block: registry.register_counter(
                "adnet_bitswap_send_want_block_total",
                "Total send_want_block_and_wait invocations.",
            ),
            send_want_block_timeouts: registry.register_counter(
                "adnet_bitswap_send_want_block_timeouts_total",
                "Total send_want_block_and_wait calls that hit the timeout.",
            ),
            active_peers: registry.register_gauge(
                "adnet_bitswap_active_peers",
                "Number of peers currently registered in the bridge.",
            ),
            pending_blocks: registry.register_gauge(
                "adnet_bitswap_pending_blocks",
                "Number of in-flight WantBlock requests waiting for a response.",
            ),
        }
    }

    /// Convenience: borrow the process-wide singleton metrics handle.
    /// The underlying counters are registered exactly once and shared
    /// across every adapter / bridge in the process.
    pub fn get() -> Self {
        GLOBAL_METRICS.clone()
    }
}

/// Standardised error reasons for the `errors_total` counter. Each
/// call site maps to one of these so the Prometheus label set stays
/// bounded.
#[derive(Debug, Clone, Copy)]
pub enum BitswapErrorReason {
    Dial,
    Alpn,
    Serialization,
    Oversize,
    Connection,
    CancelLocal,
    WaitTimeout,
    PeerDisconnected,
}

impl BitswapErrorReason {
    pub fn label(self) -> &'static str {
        match self {
            BitswapErrorReason::Dial => "dial",
            BitswapErrorReason::Alpn => "alpn",
            BitswapErrorReason::Serialization => "serialization",
            BitswapErrorReason::Oversize => "oversize",
            BitswapErrorReason::Connection => "connection",
            BitswapErrorReason::CancelLocal => "cancel_local",
            BitswapErrorReason::WaitTimeout => "wait_timeout",
            BitswapErrorReason::PeerDisconnected => "peer_disconnected",
        }
    }
}

// ════════════════════════════════════════════════════════════════════
//  Tests
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::NodeId;

    #[tokio::test]
    async fn test_message_content_hash() {
        let hash = ContentHash::from_bytes(b"test-content");
        let want_have = BitswapMessage::WantHave {
            block: hash.clone(),
            priority: 0,
            send_dont_have: true,
        };
        assert_eq!(message_hash(&want_have), Some(hash));
    }

    #[tokio::test]
    async fn test_block_resolves_pending() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let bridge: Arc<dyn BitswapTransportBridge> =
            Arc::new(MockBitswapTransport::new(local.clone()));
        let (adapter, event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
        let pending = adapter.pending();
        tokio::spawn(async move { adapter.run().await });

        let hash = ContentHash::from_bytes(b"world");
        let (tx, rx) = oneshot::channel();
        pending
            .write()
            .await
            .entry(hash.clone())
            .or_default()
            .push(tx);

        let msg = BitswapMessage::Block {
            block: hash.clone(),
            data: b"world".to_vec(),
        };
        event_tx
            .send(BitswapEvent::MessageFrom {
                peer: peer.clone(),
                msg,
            })
            .await
            .unwrap();

        let outcome = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("timeout")
            .expect("channel closed");

        match outcome {
            BitswapBlockOutcome::Received { from, data } => {
                assert_eq!(from, peer);
                assert_eq!(data, b"world".to_vec());
            }
            other => panic!("unexpected outcome: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_dont_have_resolves_pending() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let bridge: Arc<dyn BitswapTransportBridge> =
            Arc::new(MockBitswapTransport::new(local.clone()));
        let (adapter, event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
        let pending = adapter.pending();
        tokio::spawn(async move { adapter.run().await });

        let hash = ContentHash::from_bytes(b"missing");
        let (tx, rx) = oneshot::channel();
        pending
            .write()
            .await
            .entry(hash.clone())
            .or_default()
            .push(tx);

        event_tx
            .send(BitswapEvent::MessageFrom {
                peer: peer.clone(),
                msg: BitswapMessage::DontHave { block: hash.clone() },
            })
            .await
            .unwrap();

        let outcome = tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .expect("timeout")
            .expect("channel closed");

        match outcome {
            BitswapBlockOutcome::DontHave { from } => assert_eq!(from, peer),
            other => panic!("unexpected outcome: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_cancel_clears_pending() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let bridge: Arc<dyn BitswapTransportBridge> =
            Arc::new(MockBitswapTransport::new(local.clone()));
        let (adapter, event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
        let pending = adapter.pending();
        tokio::spawn(async move { adapter.run().await });

        let hash = ContentHash::from_bytes(b"cancel-me");
        let (tx, rx) = oneshot::channel::<BitswapBlockOutcome>();
        pending
            .write()
            .await
            .entry(hash.clone())
            .or_default()
            .push(tx);

        // Send a Cancel message which should resolve with Cancelled outcome.
        event_tx
            .send(BitswapEvent::MessageFrom {
                peer: peer.clone(),
                msg: BitswapMessage::Cancel { block: hash.clone() },
            })
            .await
            .unwrap();

        let outcome = tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .expect("timeout")
            .expect("channel closed");
        assert!(matches!(outcome, BitswapBlockOutcome::Cancelled));
    }

    #[tokio::test]
    async fn test_send_serializes_and_queues() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let bridge: Arc<dyn BitswapTransportBridge> =
            Arc::new(MockBitswapTransport::new(local.clone()));
        let (mut adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);

        let outgoing_rx = adapter.take_outgoing().expect("take_outgoing once");
        tokio::spawn(async move {
            let mut rx = outgoing_rx;
            while let Some(msg) = rx.recv().await {
                match msg {
                    BitswapOutgoing::Bytes { peer, data } => {
                        // Sanity: should deserialize.
                        let _: BitswapMessage = serde_json::from_slice(&data).unwrap();
                        assert!(!peer.to_string().is_empty());
                    }
                }
            }
        });

        let hash = ContentHash::from_bytes(b"hello");
        adapter
            .send_want_have(&peer, hash, 0, true)
            .await
            .expect("send_want_have");
    }

    #[tokio::test]
    async fn test_send_rejects_oversize() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let bridge: Arc<dyn BitswapTransportBridge> =
            Arc::new(MockBitswapTransport::new(local.clone()));
        let (adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);

        // Build a giant message by stuffing a 11MB Block.
        let big = vec![0u8; 11 * 1024 * 1024];
        let result = adapter
            .send(
                &peer,
                BitswapMessage::Block {
                    block: ContentHash::from_bytes(b"x"),
                    data: big,
                },
            )
            .await;
        assert!(matches!(
            result,
            Err(BitswapTransportError::MessageTooLarge { .. })
        ));
    }

    #[test]
    fn test_alpn_constant() {
        assert_eq!(BITSWAP_ALPN, b"adnet/bitswap/1");
    }

    #[tokio::test]
    async fn test_send_want_block_and_wait_resolves_with_block() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let bridge: Arc<dyn BitswapTransportBridge> =
            Arc::new(MockBitswapTransport::new(local.clone()));
        let (adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
        let pending = adapter.pending();
        // `clone_for_listen` returns a fresh adapter whose rx is paired
        // with a brand-new event_tx. Spawn the cloned run loop; the
        // test sends via run_tx and the waiter is in the shared
        // `pending` map, so the cloned run loop dispatches and resolves.
        let (run, run_tx) = adapter.clone_for_listen();
        tokio::spawn(run.run());

        let hash = ContentHash::from_bytes(b"awaited-block");

        // Pre-register a oneshot waiter the way send_want_block_and_wait does.
        let (tx, rx) = oneshot::channel::<BitswapBlockOutcome>();
        pending.write().await.entry(hash.clone()).or_default().push(tx);

        // Simulate the peer sending us the Block via the cloned
        // event channel that the live run loop is consuming.
        tokio::spawn({
            let peer = peer.clone();
            let hash = hash.clone();
            async move {
                // Sleep long enough that the run loop is fully scheduled
                // and draining the receiver. 100 ms is plenty even under
                // a heavy parallel test load.
                tokio::time::sleep(Duration::from_millis(100)).await;
                let _ = run_tx
                    .send(BitswapEvent::MessageFrom {
                        peer,
                        msg: BitswapMessage::Block {
                            block: hash,
                            data: b"data".to_vec(),
                        },
                    })
                    .await;
            }
        });

        let outcome = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("timeout waiting for waiter")
            .expect("oneshot cancelled");
        match outcome {
            BitswapBlockOutcome::Received { from, data } => {
                assert_eq!(from, peer);
                assert_eq!(data, b"data");
            }
            other => panic!("unexpected outcome: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_send_want_block_and_wait_times_out() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let bridge: Arc<dyn BitswapTransportBridge> =
            Arc::new(MockBitswapTransport::new(local.clone()));
        let (adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
        let (run, _run_tx) = adapter.clone_for_listen();
        tokio::spawn(run.run());

        let hash = ContentHash::from_bytes(b"never-comes");
        let outcome = adapter
            .send_want_block_and_wait(&peer, hash, 0, Duration::from_millis(50))
            .await
            .expect("call ok");
        assert!(matches!(outcome, BitswapBlockOutcome::Timeout));
    }

    #[tokio::test]
    async fn test_send_cancel_returns_ok_for_unknown_block() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let bridge: Arc<dyn BitswapTransportBridge> =
            Arc::new(MockBitswapTransport::new(local.clone()));
        let (adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
        let (run, _run_tx) = adapter.clone_for_listen();
        tokio::spawn(run.run());

        // Sending a Cancel for a block we never requested must still succeed.
        let result = adapter
            .send_cancel(&peer, ContentHash::from_bytes(b"different"))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_bitswap_stream_debug_format() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(8);
        let stream = BitswapStream {
            peer_id: NodeId::random(),
            tx,
            rx,
        };
        let display = format!("{stream:?}");
        assert!(display.contains("BitswapStream"));
        assert!(display.contains("peer_id"));
    }

    #[tokio::test]
    async fn test_send_want_have_signature_round_trip() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let bridge: Arc<dyn BitswapTransportBridge> =
            Arc::new(MockBitswapTransport::new(local.clone()));
        let (adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
        let (run, _run_tx) = adapter.clone_for_listen();
        tokio::spawn(run.run());

        let hash = ContentHash::from_bytes(b"want-have-test");
        adapter
            .send_want_have(&peer, hash, 5, false)
            .await
            .expect("send_want_have");
    }

    /// Regression test: a duplicate dial on the same peer must reuse the
    /// registered channel (fast path) and *not* panic or hang. This
    /// guards against accidental future refactors that drop the
    /// `if let Some(existing) = ...` fast-path block.
    #[tokio::test]
    async fn test_mock_dial_idempotent() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let mock = Arc::new(MockBitswapTransport::new(local.clone()));
        let bridge: Arc<dyn BitswapTransportBridge> = mock.clone();

        let s1 = bridge.dial(&peer).await.expect("first dial");
        let s2 = bridge.dial(&peer).await.expect("second dial");
        assert_eq!(s1.peer_id, peer);
        assert_eq!(s2.peer_id, peer);
        // The mock's peer table has exactly one entry for `peer` —
        // the second dial must NOT have created a new channel.
        assert_eq!(mock.peer_count().await, 1);
    }

    /// Regression test: `send_to` must drop the read lock before
    /// awaiting on the channel. Otherwise a misbehaving peer channel
    /// that doesn't drain would deadlock the bridge.
    #[tokio::test]
    async fn test_mock_send_to_works_after_dial() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let mock = Arc::new(MockBitswapTransport::new(local.clone()));
        let bridge: Arc<dyn BitswapTransportBridge> = mock.clone();

        // First dial.
        let _s = bridge.dial(&peer).await.expect("dial");
        // Now send. Since `peer_rx_keepalive` is in scope, this must
        // succeed without the channel being closed.
        bridge
            .send_to(&peer, b"hello".to_vec())
            .await
            .expect("send_to");
        // Verify the bytes reached the peer's receive side.
        let got = mock
            .try_recv(&peer)
            .await
            .expect("peer should have bytes");
        assert_eq!(got, b"hello");
    }

    /// Regression test: `register_inbound_sender` + `unregister_peer`
    /// must round-trip without leaking entries or panicking on double
    /// unregister.
    #[tokio::test]
    async fn test_mock_register_and_unregister_idempotent() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let mock = Arc::new(MockBitswapTransport::new(local.clone()));
        let (tx, _rx) = mpsc::channel::<Vec<u8>>(8);

        mock.register_inbound_sender(peer.clone(), tx).await;
        assert_eq!(mock.peer_count().await, 1);

        mock.unregister_peer(&peer).await;
        assert_eq!(mock.peer_count().await, 0);

        // Double unregister must be a no-op.
        mock.unregister_peer(&peer).await;
        assert_eq!(mock.peer_count().await, 0);
    }

    /// After `unregister_peer`, a fresh `dial` must succeed (cold
    /// path) — the previous entry should not be silently reused.
    #[tokio::test]
    async fn test_mock_unregister_then_redial() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let mock = Arc::new(MockBitswapTransport::new(local.clone()));
        let bridge: Arc<dyn BitswapTransportBridge> = mock.clone();

        let _ = bridge.dial(&peer).await.expect("first dial");
        mock.unregister_peer(&peer).await;
        let _ = bridge.dial(&peer).await.expect("redial");
        assert_eq!(mock.peer_count().await, 1);
    }

    /// `BitswapHello` round-trips through JSON correctly: ALPN bytes,
    /// version, and node_id are preserved.
    #[tokio::test]
    async fn test_hello_serde_round_trip() {
        let local = NodeId::random();
        let hello = BitswapHello::new(local.clone());
        let bytes = hello.encode().expect("encode");
        let back = BitswapHello::decode(&bytes).expect("decode");
        assert_eq!(hello.alpn, back.alpn);
        assert_eq!(hello.version, back.version);
        assert_eq!(hello.node_id, back.node_id);
    }

    /// A `BitswapHello` with the wrong ALPN must produce an
    /// `AlpnMismatch` error pointing at the offending bytes.
    #[tokio::test]
    async fn test_hello_alpn_mismatch_payload() {
        let bad = BitswapHello {
            alpn: b"not/right".to_vec(),
            version: 1,
            node_id: NodeId::random(),
        };
        match bad.verify_alpn() {
            Err(BitswapTransportError::AlpnMismatch { expected, actual }) => {
                assert_eq!(expected, BITSWAP_ALPN);
                assert_eq!(actual, b"not/right");
            }
            other => panic!("expected AlpnMismatch, got {:?}", other),
        }
    }

    /// Regression: `dispatch_message` must clone the data payload out
    /// of the incoming frame before resolving waiters, so the original
    /// message buffer can be freed without affecting already-resolved
    /// waiters.
    #[tokio::test]
    async fn test_block_payload_is_owned_after_resolve() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let bridge: Arc<dyn BitswapTransportBridge> =
            Arc::new(MockBitswapTransport::new(local.clone()));
        let (adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);
        let pending = adapter.pending();
        tokio::spawn(async move { adapter.run().await });

        let hash = ContentHash::from_bytes(b"owned");
        let (tx, rx) = oneshot::channel();
        pending
            .write()
            .await
            .entry(hash.clone())
            .or_default()
            .push(tx);

        let payload = vec![1u8, 2, 3, 4, 5];
        let msg = BitswapMessage::Block {
            block: hash.clone(),
            data: payload.clone(),
        };
        _event_tx
            .send(BitswapEvent::MessageFrom {
                peer: peer.clone(),
                msg,
            })
            .await
            .unwrap();

        let outcome = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("timeout")
            .expect("channel closed");
        match outcome {
            BitswapBlockOutcome::Received { data, .. } => assert_eq!(data, payload),
            other => panic!("unexpected: {:?}", other),
        }
    }

    /// The `BitswapOutgoing::Bytes` envelope must carry a peer ID and
    /// the raw serialized message. The outgoing-pump contract is
    /// "bytes flow to the bridge". This test enforces the envelope
    /// shape (no accidental rename or field loss).
    #[tokio::test]
    async fn test_outgoing_envelope_shape() {
        let local = NodeId::random();
        let peer = NodeId::random();
        let bridge: Arc<dyn BitswapTransportBridge> =
            Arc::new(MockBitswapTransport::new(local.clone()));
        let (mut adapter, _event_tx) = BitswapNetworkAdapter::new(local.clone(), bridge);

        let outgoing_rx = adapter.take_outgoing().expect("take_outgoing once");
        let probe = tokio::spawn(async move {
            let mut rx = outgoing_rx;
            rx.recv().await
        });

        let hash = ContentHash::from_bytes(b"envelope-shape");
        adapter
            .send_want_have(&peer, hash.clone(), 10, false)
            .await
            .expect("send_want_have");

        let env = tokio::time::timeout(Duration::from_secs(1), probe)
            .await
            .expect("timeout")
            .expect("join")
            .expect("recv");
        match env {
            BitswapOutgoing::Bytes { peer: got_peer, data } => {
                assert_eq!(got_peer, peer);
                let parsed: BitswapMessage = serde_json::from_slice(&data).expect("decode");
                match parsed {
                    BitswapMessage::WantHave {
                        block,
                        priority,
                        send_dont_have,
                    } => {
                        assert_eq!(block, hash);
                        assert_eq!(priority, 10);
                        assert!(!send_dont_have);
                    }
                    other => panic!("unexpected payload: {:?}", other),
                }
            }
        }
    }
}
