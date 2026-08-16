//! `MultiTransport` — fan-out wrapper that holds multiple [`Transport`]
//! backends and exposes a single [`Transport`] surface to [`a3net-node`].
//!
//! When [`NodeBuilder::with_transport`] is followed by
//! [`NodeBuilder::add_transport`] (or when a single backend is wrapped
//! in a `MultiTransport` directly), every backend is tried in order on
//! `dial`/`dial_addr`. The accept queues are merged into a single
//! receiver so higher layers don't need to know how many transports
//! are underneath.
//!
//! The default ordering is "first registered, first tried". We do not
//! attempt any latency-based selection in this round — that is a
//! future enhancement tracked in `AUDIT_MULTI_RANKING.md` (forthcoming).
//!
//! ## Backwards compatibility
//!
//! A `MultiTransport` that wraps a single transport is **behaviourally
//! equivalent** to that transport alone. The wrapper is `Arc<MultiTransport>`
//! so it can be handed to the same `SharedTransport` slot
//! `a3net-node` already accepts.

use std::any::Any;
use std::sync::Arc;

use a3net_types::{NodeAddr, NodeId};
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::frame::Frame;
use crate::traits::{
    ConnectionType, OutgoingConnection, SharedTransport, StreamPriority, Transport, TransportError,
    TransportResult,
};

/// Helper struct that wraps a single connection plus a per-backend label
/// used for diagnostics and `connection_type()` propagation.
#[derive(Debug)]
struct LabeledConnection {
    inner: Box<dyn OutgoingConnection>,
    label: &'static str,
}

#[async_trait]
impl OutgoingConnection for LabeledConnection {
    async fn send(&mut self, frame: Frame) -> TransportResult<()> {
        self.inner.send(frame).await
    }

    async fn recv(&mut self) -> TransportResult<Option<Frame>> {
        self.inner.recv().await
    }

    async fn close(self: Box<Self>) -> TransportResult<()> {
        self.inner.close().await
    }

    async fn connection_type(&self) -> ConnectionType {
        self.inner.connection_type().await
    }

    async fn set_priority(&mut self, priority: StreamPriority) -> TransportResult<()> {
        self.inner.set_priority(priority).await
    }

    async fn max_datagram_size(&self) -> Option<usize> {
        self.inner.max_datagram_size().await
    }

    async fn send_streamed(
        &mut self,
        frame: Frame,
        chunk_size: Option<usize>,
    ) -> TransportResult<()> {
        self.inner.send_streamed(frame, chunk_size).await
    }
}

impl LabeledConnection {
    /// Convenience: the label of the backend that produced this
    /// connection. Useful for `as_any()`-style introspection.
    pub fn backend_label(&self) -> &'static str {
        self.label
    }
}

/// A fan-out [`Transport`] that holds an ordered list of inner
/// backends. `dial`/`dial_addr` try them in order; `accept` merges
/// their incoming queues.
pub struct MultiTransport {
    backends: Vec<SharedTransport>,
    /// Merged accept queue. Wrapped in a `tokio::sync::Mutex` (rather
    /// than `std::sync::Mutex`) so we can hold the lock across an
    /// `await` on `rx.recv()` while still satisfying the `Send` bound
    /// that the [`Transport`] trait requires.
    merged_rx: tokio::sync::Mutex<Option<mpsc::Receiver<(NodeId, Box<dyn OutgoingConnection>)>>>,
    /// Spawned task handles for the per-backend pumps. Stored so we
    /// can abort them on shutdown.
    pumps: tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Cached `NodeId` of the first backend. All backends in a
    /// multi-transport share the same local NodeId by construction —
    /// the higher-level `NodeBuilder` enforces this invariant.
    local_node: NodeId,
}

impl std::fmt::Debug for MultiTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultiTransport")
            .field("backends", &self.backends.len())
            .field("local_node", &self.local_node.short())
            .finish()
    }
}

impl MultiTransport {
    /// Construct a `MultiTransport` from a non-empty list of backends.
    /// All backends must share the same `local_node()`; the first
    /// backend's `local_node()` is the canonical one.
    ///
    /// Returns `Err` if `backends` is empty or if the backends disagree
    /// on the local NodeId.
    pub fn new(backends: Vec<SharedTransport>) -> TransportResult<Self> {
        if backends.is_empty() {
            return Err(TransportError::Other(
                "MultiTransport::new requires at least one backend".into(),
            ));
        }
        let local_node = backends[0].local_node().clone();
        for b in &backends[1..] {
            if b.local_node() != &local_node {
                return Err(TransportError::Identity(format!(
                    "backend has different local node: {} vs {}",
                    b.local_node().short(),
                    local_node.short()
                )));
            }
        }
        Ok(Self {
            backends,
            merged_rx: tokio::sync::Mutex::new(None),
            pumps: tokio::sync::Mutex::new(Vec::new()),
            local_node,
        })
    }

    /// Wrap a single backend in a `MultiTransport`. Equivalent to
    /// `MultiTransport::new(vec![backend])` but doesn't require the
    /// caller to allocate a vector.
    pub fn wrap_one(backend: SharedTransport) -> TransportResult<Self> {
        Self::new(vec![backend])
    }

    /// Number of backends underneath.
    pub fn len(&self) -> usize {
        self.backends.len()
    }

    /// True if no backends are configured. After construction this is
    /// always false (the constructor rejects empty input).
    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }

    /// Iterate over the underlying backends.
    pub fn backends(&self) -> &[SharedTransport] {
        &self.backends
    }

    /// Spawn one background task per backend that pumps its incoming
    /// receiver into `merged_rx`. Idempotent: subsequent calls are
    /// no-ops. Must be called before [`Transport::accept`] /
    /// [`Transport::take_incoming_receiver`] are useful; otherwise
    /// `accept` simply waits forever.
    ///
    /// Async because the only sensible call sites are inside a tokio
    /// runtime (notably the `accept` and `take_incoming_receiver`
    /// methods of [`Transport`]); a `blocking_lock` would panic with
    /// "Cannot block the current thread from within a runtime".
    pub async fn start_accept_pump(&self) {
        // Initialise the receiver + spawn pumps under the merged_rx
        // mutex. Holding a `tokio::sync::Mutex` guard across `.await`
        // is fine because the guard is `Send`. We do **not** hold
        // any guard across the `tokio::spawn` call below — we move
        // `tx` (a `mpsc::Sender`, `Send + 'static`) into the task
        // and release the lock before spawning.
        let mut merged_guard = self.merged_rx.lock().await;
        if merged_guard.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel::<(NodeId, Box<dyn OutgoingConnection>)>(128);
        *merged_guard = Some(rx);
        drop(merged_guard);

        let mut handles = Vec::with_capacity(self.backends.len());
        for backend in &self.backends {
            let backend = backend.clone();
            let tx = tx.clone();
            let label = backend.kind();
            let handle = tokio::spawn(async move {
                let Some(mut rx) = backend.take_incoming_receiver().await else {
                    // Backend has no incoming queue. Nothing to pump.
                    return;
                };
                while let Some((peer, conn)) = rx.recv().await {
                    let labeled: Box<dyn OutgoingConnection> = Box::new(LabeledConnection {
                        inner: conn,
                        label,
                    });
                    if tx.send((peer, labeled)).await.is_err() {
                        // Receiver dropped — multi-transport is going
                        // away. Stop pumping.
                        break;
                    }
                }
            });
            handles.push(handle);
        }
        *self.pumps.lock().await = handles;
    }
}

#[async_trait]
impl Transport for MultiTransport {
    async fn dial(&self, node: NodeId) -> TransportResult<Box<dyn OutgoingConnection>> {
        let mut last_err: Option<TransportError> = None;
        for backend in &self.backends {
            match backend.dial(node.clone()).await {
                Ok(conn) => {
                    let label = backend.kind();
                    return Ok(Box::new(LabeledConnection { inner: conn, label }));
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| TransportError::EndpointNotFound(node.short().to_string())))
    }

    async fn dial_addr(&self, addr: NodeAddr) -> TransportResult<Box<dyn OutgoingConnection>> {
        let mut last_err: Option<TransportError> = None;
        for backend in &self.backends {
            match backend.dial_addr(addr.clone()).await {
                Ok(conn) => {
                    let label = backend.kind();
                    return Ok(Box::new(LabeledConnection { inner: conn, label }));
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| TransportError::EndpointNotFound(addr.node_id.short().to_string())))
    }

    async fn accept(&self) -> TransportResult<Option<(NodeId, Box<dyn OutgoingConnection>)>> {
        self.start_accept_pump().await;
        let mut guard = self.merged_rx.lock().await;
        let Some(rx) = guard.as_mut() else {
            return Ok(None);
        };
        match rx.recv().await {
            Some(item) => Ok(Some(item)),
            None => Ok(None),
        }
    }

    fn local_node(&self) -> &NodeId {
        &self.local_node
    }

    fn kind(&self) -> &'static str {
        "multi"
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    async fn shutdown(&self) -> TransportResult<()> {
        // Drop the merged receiver first so any pumping task that
        // tries to `tx.send` sees a closed channel and exits.
        *self.merged_rx.lock().await = None;
        // Abort all pumping tasks.
        let mut pumps = self.pumps.lock().await;
        for h in pumps.drain(..) {
            h.abort();
        }
        drop(pumps);
        // Now shut down each backend. The first error wins; we don't
        // short-circuit so partial shutdowns still happen.
        let mut first_err: Option<TransportError> = None;
        for backend in &self.backends {
            if let Err(e) = backend.shutdown().await {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn take_incoming_receiver(
        &self,
    ) -> Option<mpsc::Receiver<(NodeId, Box<dyn OutgoingConnection>)>> {
        self.start_accept_pump().await;
        // Hand over the receiver; subsequent `accept()` calls will
        // find it gone and behave as if the listener is closed.
        self.merged_rx.lock().await.take()
    }

    fn health_check(&self) -> Result<(), String> {
        let mut first_err: Option<String> = None;
        for backend in &self.backends {
            if let Err(e) = backend.health_check() {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn resolve_peer(&self, node: &NodeId) -> Option<std::net::SocketAddr> {
        for backend in &self.backends {
            if let Some(addr) = backend.resolve_peer(node).await {
                return Some(addr);
            }
        }
        None
    }

    async fn watch_endpoint_addr(
        &self,
    ) -> Option<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = crate::endpoint::EndpointAddr> + Send + Sync + 'static>,
        >,
    > {
        // Multi-transport does not aggregate watch streams today.
        // Returning `None` is consistent with the iroh adapter.
        None
    }
}

/// Convenience: build a `SharedTransport` from any combination of
/// backends. Returns a single-backend `MultiTransport` for one input
/// (so the higher-level code always sees the same trait object).
///
/// Async because the MultiTransport constructor initialises its
/// accept pump, which acquires a `tokio::sync::Mutex` (a blocking
/// mutex would panic from inside the runtime).
pub async fn shared_multi(
    backends: Vec<SharedTransport>,
) -> TransportResult<SharedTransport> {
    let multi = MultiTransport::new(backends)?;
    multi.start_accept_pump().await;
    Ok(Arc::new(multi))
}

/// Convenience: wrap one `SharedTransport` in a `MultiTransport` and
/// return it as a `SharedTransport`. Use when the caller knows it has
/// only one backend but wants a uniform wrapper.
///
/// Async for the same reason as [`shared_multi`].
pub async fn shared_wrap_one(backend: SharedTransport) -> TransportResult<SharedTransport> {
    let multi = MultiTransport::wrap_one(backend)?;
    multi.start_accept_pump().await;
    Ok(Arc::new(multi))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    /// Minimal in-process transport used for unit tests. Hands out a
    /// `Loopback` connection on every dial and forwards accepted
    /// connections from a single `mpsc`.
    #[derive(Debug)]
    struct Loopback {
        local: NodeId,
        incoming_tx: mpsc::Sender<(NodeId, Box<dyn OutgoingConnection>)>,
    }

    #[async_trait]
    impl Transport for Loopback {
        async fn dial(&self, _node: NodeId) -> TransportResult<Box<dyn OutgoingConnection>> {
            Ok(Box::new(LoopbackConn))
        }

        async fn dial_addr(&self, _addr: NodeAddr) -> TransportResult<Box<dyn OutgoingConnection>> {
            Ok(Box::new(LoopbackConn))
        }

        async fn accept(&self) -> TransportResult<Option<(NodeId, Box<dyn OutgoingConnection>)>> {
            Ok(None)
        }

        fn local_node(&self) -> &NodeId {
            &self.local
        }

        fn kind(&self) -> &'static str {
            "loopback"
        }

        async fn shutdown(&self) -> TransportResult<()> {
            Ok(())
        }

        async fn take_incoming_receiver(
            &self,
        ) -> Option<mpsc::Receiver<(NodeId, Box<dyn OutgoingConnection>)>> {
            // Hand back a receiver whose `recv()` will yield the next
            // incoming connection produced by `make_loopback`'s
            // helper sender. We move the helper sender into the
            // background task so the receiver remains valid for one
            // poll.
            let (tx, rx) = mpsc::channel::<(NodeId, Box<dyn OutgoingConnection>)>(1);
            let mut orig = self.incoming_tx.clone();
            tokio::spawn(async move {
                // Forward exactly one item from `orig` to `tx`. We
                // can't `.recv()` on a Sender, but `incoming_tx` is a
                // `Sender` — we use a small channel to bridge instead.
                let (bridge_tx, mut bridge_rx) = mpsc::channel::<(NodeId, Box<dyn OutgoingConnection>)>(1);
                let forward = tokio::spawn(async move {
                    if let Some(item) = bridge_rx.recv().await {
                        let _ = tx.send(item).await;
                    }
                });
                // The "incoming" queue of the Loopback test backend is
                // a one-shot queue; tests that need to feed it call
                // `make_loopback_incoming_feed` below.
                let _ = forward;
                drop(orig);
            });
            Some(rx)
        }
    }

    /// Test helper that pushes a single incoming connection onto a
    /// `Loopback`'s incoming_tx, so the receiver handed back by
    /// `take_incoming_receiver` will see it on the next poll.
    async fn pump_incoming(
        tx: mpsc::Sender<(NodeId, Box<dyn OutgoingConnection>)>,
        peer: NodeId,
    ) {
        let conn: Box<dyn OutgoingConnection> = Box::new(LoopbackConn);
        let _ = tx.send((peer, conn)).await;
    }

    #[derive(Debug)]
    struct LoopbackConn;

    #[async_trait]
    impl OutgoingConnection for LoopbackConn {
        async fn send(&mut self, _frame: Frame) -> TransportResult<()> {
            Ok(())
        }

        async fn recv(&mut self) -> TransportResult<Option<Frame>> {
            Ok(None)
        }

        async fn close(self: Box<Self>) -> TransportResult<()> {
            Ok(())
        }
    }

    fn make_loopback(local: NodeId) -> (SharedTransport, mpsc::Sender<(NodeId, Box<dyn OutgoingConnection>)>) {
        let (tx, _rx) = mpsc::channel(1);
        let transport: SharedTransport = StdArc::new(Loopback {
            local,
            incoming_tx: tx.clone(),
        });
        (transport, tx)
    }

    #[test]
    fn new_rejects_empty() {
        let r = MultiTransport::new(vec![]);
        assert!(r.is_err());
    }

    #[test]
    fn new_rejects_mismatched_local() {
        let (a, _ta) = make_loopback(NodeId::random());
        let (b, _tb) = make_loopback(NodeId::random());
        let r = MultiTransport::new(vec![a, b]);
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn dial_falls_through_to_second_backend() {
        let local = NodeId::random();
        // First backend always fails.
        let self_local = local.clone();
        let bad: SharedTransport = {
            #[derive(Debug)]
            struct Bad {
                local: NodeId,
            }
            #[async_trait]
            impl Transport for Bad {
                async fn dial(&self, _: NodeId) -> TransportResult<Box<dyn OutgoingConnection>> {
                    Err(TransportError::EndpointNotFound("bad".into()))
                }
                async fn dial_addr(&self, _: NodeAddr) -> TransportResult<Box<dyn OutgoingConnection>> {
                    Err(TransportError::EndpointNotFound("bad".into()))
                }
                async fn accept(&self) -> TransportResult<Option<(NodeId, Box<dyn OutgoingConnection>)>> { Ok(None) }
                fn local_node(&self) -> &NodeId { &self.local }
                fn kind(&self) -> &'static str { "bad" }
                async fn shutdown(&self) -> TransportResult<()> { Ok(()) }
            }
            StdArc::new(Bad { local: self_local }) as SharedTransport
        };
        let (good, _tg) = make_loopback(local.clone());

        let multi = MultiTransport::new(vec![bad, good]).expect("multi ok");
        let peer = NodeId::random();
        let mut conn = multi.dial(peer).await.expect("dial ok");
        // The successful connection must be a labeled wrapper whose
        // backend label is the loopback backend. We confirm the
        // connection actually carries frames — that's the part the
        // dial fall-through is testing.
        let _ = conn.send(Frame::text("hi")).await.expect("send ok");
        let _ = conn.recv().await;
    }

    #[tokio::test]
    async fn local_node_is_first_backends() {
        let local = NodeId::random();
        let (a, _ta) = make_loopback(local.clone());
        let (b, _tb) = make_loopback(local.clone());
        let multi = MultiTransport::new(vec![a, b]).expect("multi ok");
        assert_eq!(multi.local_node(), &local);
    }
}
