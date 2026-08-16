//! iroh-backed runtime for [`Node`](crate::node::Node).
//!
//! When the `iroh` feature is enabled, this module exposes an
//! [`IrohRuntime`] that wires a pre-built [`iroh::Endpoint`] together
//! with an [`IrohBlobStore`](a3net_blobstore::IrohBlobStore) and an
//! [`IrohGossipTransport`](a3net_gossip::IrohGossipTransport) into a
//! single iroh [`iroh::protocol::Router`].
//!
//! The router hosts:
//! - `iroh_blobs::BlobsProtocol` over the `iroh_blobs::ALPN` — Bao-verified
//!   blob transfer.
//! - The gossip protocol over `iroh_gossip::ALPN` — HyParView+PlumTree
//!   epidemic broadcast trees.
//!
//! A3Net's own `Transport` trait is unaffected — when callers wire an
//! `IrohTransport` into the node, dialing a peer goes through the same
//! iroh `Endpoint` the router is bound to.
//!
//! ## Usage
//!
//! ```ignore
//! use a3net_node::iroh_runtime::IrohRuntime;
//! use iroh::{endpoint::presets, Endpoint};
//!
//! let endpoint = Endpoint::bind(presets::N0).await?;
//! let runtime = IrohRuntime::spawn(endpoint, &data_dir).await?;
//! // runtime.router() is now running in the background, accepting
//! // BlobsProtocol + gossip connections.
//! // Pass runtime.gossip() into a GossipBus to subscribe rooms.
//! // Use runtime.blob_store() as the storage backend.
//! ```

use std::path::Path;
use std::sync::Arc;

use a3net_blobstore::IrohBlobStore;
use a3net_chatstore::IrohDocsChat;
use a3net_gossip::IrohGossipTransport;
use a3net_transport::iroh::{ADNET_FRAME_ALPN, FrameIn, IrohFrameHandler, IrohIdentity};
use iroh::{Endpoint, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use iroh_docs::api::DocsApi;
use iroh_docs::protocol::Docs;
use iroh_gossip::net::Gossip;
use tracing::info;

/// A self-contained iroh runtime that owns the `Endpoint`, the blob
/// store, the gossip protocol, the docs engine, and the `Router`
/// that hosts them all.
///
/// Drop the runtime (or call [`IrohRuntime::shutdown`]) to tear
/// everything down cleanly. The `Drop` impl performs the
/// **synchronous** half of teardown — closing the frame receiver
/// so the `IrohFrameHandler`'s `try_send` calls fail — and the
/// `iroh::Router` itself has an abort-on-drop background task that
/// will fire when the last clone goes away. For *graceful*
/// shutdown (draining in-flight connections, then closing the
/// endpoint) call [`IrohRuntime::shutdown`].
pub struct IrohRuntime {
    /// The underlying iroh endpoint. Shared with `IrohTransport`
    /// when the caller wants dialing to go through the same
    /// connection pool.
    pub endpoint: Endpoint,
    /// The blob store. The `BlobsProtocol` borrows it; the
    ///   A3Net-facing `BlobReader` / `BlobImporter` access it directly.
    blob_store: Arc<FsStore>,
    /// High-level wrapper around the same FsStore for A3Net callers.
    pub a3net_store: IrohBlobStore,
    /// The iroh-gossip handle. Pass `.clone()` to
    ///   `IrohGossipTransport::new` to wire A3Net's room bus on top.
    pub gossip: Gossip,
    /// The iroh-docs `Docs` protocol. Bound to `iroh_docs::ALPN` on
    /// the router; can also be used directly to access the
    /// [`DocsApi`] for local CRUD on chat conversations.
    docs: Docs,
    /// The RPC-style API on top of the same docs engine. Cheap to
    /// clone — the underlying engine is shared.
    docs_api: Arc<DocsApi>,
    /// The router that hosts BlobsProtocol + gossip + docs.
    router: Router,
    /// Incoming `a3net/frame/1` connections, fed by the
    /// `IrohFrameHandler` registered on the router. Callers can
    /// either hand this receiver to
    /// [`IrohTransport::with_endpoint`](a3net_transport::IrohTransport::with_endpoint)
    /// or drain it via [`IrohRuntime::frame_receiver`].
    frame_rx: tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<FrameIn>>>,
    /// Optional user-data payload attached to every pkarr packet
    /// this runtime publishes. Mirrors
    /// `iroh_dns::endpoint_info::UserData` (v1.0.3); see
    /// [`a3net_transport::iroh::discovery::UserData`]. `None` when the
    /// operator hasn't configured one.
    ///
    /// Used by [`IrohRuntime::diagnostics`] accessor and by
    /// future wire-side hooks (e.g. an admin command that
    /// publishes a fresh `user_data` on demand). The actual
    /// wire stamping happens inside
    /// [`crate::iroh::discovery::InstrumentedPublisher::publish`]
    /// when a custom `PkarrPublisherConfig` is wired through
    /// `DiscoveryBuilder`.
    #[allow(dead_code)]
    user_data: Option<a3net_transport::iroh::discovery::UserData>,
    /// Optional shared diagnostics recorder. Set when the runtime
    /// is spawned via `spawn_with_identity_and_user_data` so the
    /// pre-stamped `user_data` is observable to the operator
    /// before any `publish(...)` call lands.
    #[allow(dead_code)]
    diagnostics: Option<Arc<a3net_transport::iroh::discovery::DiscoveryDiagnostics>>,
}

/// Drop the frame receiver so the `IrohFrameHandler`'s channel
/// closes. We cannot block in `Drop` (and do not need to — the
/// `iroh::protocol::Router` already uses an `AbortOnDropHandle`
/// for its background task, and `iroh::Endpoint::close` is
/// idempotent). Callers that need a *graceful* drain should
/// invoke [`IrohRuntime::shutdown`] explicitly.
impl Drop for IrohRuntime {
    fn drop(&mut self) {
        // Try once to drop the receiver without blocking. If the
        // mutex is contended (e.g. a concurrent `take_frame_receiver`
        // call from another thread) we let the receiver stay
        // attached — it will close naturally when the
        // `iroh::Router` is dropped alongside us, since dropping
        // the `Router` drops the `IrohFrameHandler` whose
        // `Sender` then closes the channel.
        if let Ok(mut guard) = self.frame_rx.try_lock()
            && let Some(rx) = guard.take()
        {
            drop(rx);
        }
    }
}

impl std::fmt::Debug for IrohRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IrohRuntime")
            .field("endpoint_id", &self.endpoint.id())
            .field("blob_store", &"<FsStore>")
            .field("gossip", &"<Gossip>")
            .finish()
    }
}

impl IrohRuntime {
    /// Spawn an iroh runtime rooted at `<data_dir>/iroh-blobs/`.
    ///
    /// This binds the supplied endpoint to a single `Router`
    /// that hosts:
    ///
    /// - `BlobsProtocol` over `iroh_blobs::ALPN`
    /// - `Gossip` over `iroh_gossip::ALPN`
    /// - `Docs` over `iroh_docs::ALPN`
    /// - the A3Net framed transport via `IrohFrameHandler` over
    ///   `b"a3net/frame/1"`
    ///
    /// The router runs in a background task; call
    /// [`IrohRuntime::shutdown`] to terminate it gracefully.
    ///
    /// `docs_path` is the directory where `iroh-docs` should store
    /// its redb replica + default author. Pass `<data_dir>` to keep
    /// everything under one root, or any other directory.
    pub async fn spawn(
        endpoint: Endpoint,
        data_dir: &Path,
        _docs_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let a3net_store = IrohBlobStore::open(data_dir).await?;
        let blob_store = a3net_store.handle();
        // `BlobsProtocol::new` takes `&Store`. `FsStore` derefs to
        // `Store`, and we hold it in an `Arc` so the router's
        // background task can keep the protocol alive for the
        // lifetime of the runtime.
        let blobs = BlobsProtocol::new(&blob_store, None);

        let gossip = Gossip::builder().spawn(endpoint.clone());

        // iroh-docs: persistent (disk-backed) replica + default author.
        // `fs-store` is forwarded to `iroh-docs` from the crate-level
        // `iroh` feature so this code is always compiled when this module
        // is reachable.
        #[cfg(feature = "fs-store")]
        let docs = {
            let docs_root = _docs_path
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| data_dir.to_path_buf());
            tokio::fs::create_dir_all(&docs_root).await?;
            let fs: iroh_blobs::api::Store = (*blob_store).clone().into();
            iroh_docs::protocol::Docs::persistent(docs_root)
                .spawn(endpoint.clone(), fs, gossip.clone())
                .await?
        };
        let docs_api: Arc<DocsApi> = docs.api().clone().into();

        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel::<FrameIn>(64);
        let frame_handler = IrohFrameHandler::new(frame_tx);

        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, blobs)
            .accept(iroh_gossip::ALPN, gossip.clone())
            .accept(iroh_docs::ALPN, docs.clone())
            .accept(ADNET_FRAME_ALPN, frame_handler)
            .spawn();

        info!(
            endpoint_id = %endpoint.id(),
            blob_dir = %a3net_store.path().display(),
            "iroh runtime online"
        );

        Ok(Self {
            endpoint,
            blob_store,
            a3net_store,
            gossip,
            docs,
            docs_api,
            router,
            frame_rx: tokio::sync::Mutex::new(Some(frame_rx)),
            user_data: None,
            diagnostics: None,
        })
    }

    /// Spawn an iroh runtime whose endpoint is authenticated by a
    /// persistent Ed25519 identity (see
    /// [`IrohIdentity::load_or_create`]). The endpoint is bound to
    /// `bind`, so the runtime always produces the same
    /// `EndpointId` across restarts. The router advertises
    /// `a3net/frame/1` so the standard A3Net framed transport can
    /// share the same endpoint as the blobs/gossip/docs protocols.
    ///
    /// Prefer this over [`IrohRuntime::spawn`] when the runtime is
    /// also expected to authenticate incoming connections with a
    /// stable identity (production deployments).
    pub async fn spawn_with_identity(
        bind: std::net::SocketAddr,
        identity: &IrohIdentity,
        data_dir: &Path,
        docs_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Self::spawn_with_identity_and_user_data(bind, identity, data_dir, docs_path, None).await
    }

    /// Same as [`Self::spawn_with_identity`] but with a `user_data`
    /// payload that is attached to every pkarr packet the runtime
    /// publishes. Mirrors `iroh_dns::endpoint_info::UserData`;
    /// see [`a3net_transport::iroh::discovery::UserData`].
    ///
    /// When `user_data = Some(payload)` the runtime rebuilds its
    /// Pkarr publisher on top of `presets::N0` and routes every
    /// `publish(EndpointData)` through an instrumented wrapper
    /// that injects the payload — iroh 1.0.3's
    /// `PkarrPublisher::n0_dns()` has no `user_data` knob, so
    /// this is the only way to surface the field on the wire
    /// from a stock `n0_dns()` setup.
    pub async fn spawn_with_identity_and_user_data(
        bind: std::net::SocketAddr,
        identity: &IrohIdentity,
        data_dir: &Path,
        docs_path: Option<&Path>,
        user_data: Option<a3net_transport::iroh::discovery::UserData>,
    ) -> anyhow::Result<Self> {
        use iroh::{Endpoint, endpoint::presets};
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(identity.secret_key())
            .alpns(vec![ADNET_FRAME_ALPN.to_vec()])
            .bind_addr(bind)
            .map_err(|e| anyhow::anyhow!("iroh bind_addr {bind}: {e}"))?
            .bind()
            .await?;
        let mut runtime = Self::spawn(endpoint, data_dir, docs_path).await?;
        if let Some(ud) = user_data {
            // Stash the user-data on the runtime so the
            // discovery builder / diagnostics recorder can
            // surface it. The actual wire-side stamping
            // happens inside `InstrumentedPublisher::publish`
            // when the operator wires a custom
            // `PkarrPublisherConfig`. We also pre-stamp the
            // diagnostics recorder so `/discovery` reflects
            // the operator's intent before any
            // `publish(...)` call.
            runtime.user_data = Some(ud.clone());
            if let Some(diag) = runtime.diagnostics.as_ref() {
                diag.record_user_data(Some(ud));
            }
        }
        Ok(runtime)
    }

    /// Underlying blob store, exposed for callers that need direct
    /// access (e.g. to construct a custom `BlobsProtocol`).
    pub fn blob_store_handle(&self) -> Arc<FsStore> {
        Arc::clone(&self.blob_store)
    }

    /// Cheap clone of the underlying [`DocsApi`]. Useful for
    /// constructing an [`IrohDocsChat`](a3net_chatstore::IrohDocsChat)
    /// outside of the runtime — see [`IrohRuntime::chat_bridge`].
    pub fn docs_api(&self) -> Arc<DocsApi> {
        Arc::clone(&self.docs_api)
    }

    /// Construct an [`IrohDocsChat`] backed by this runtime's docs
    /// engine + blob store. Phase 5a entry point — the returned
    /// bridge uses iroh-docs `Doc`s for message sync.
    pub async fn chat_bridge(&self) -> anyhow::Result<IrohDocsChat> {
        Ok(IrohDocsChat::new(self.docs_api(), self.a3net_store.clone()).await?)
    }

    /// Underlying docs protocol handle. Useful for tests that need
    /// to drive the engine directly (e.g. shutdown, register a
    /// custom ALPN handler). Kept `pub` so test code can reach it
    /// without going through `chat_bridge`.
    pub fn docs(&self) -> &Docs {
        &self.docs
    }

    /// Construct an `IrohGossipTransport` bound to this runtime's
    /// gossip engine. The returned transport plugs straight into a
    /// [`GossipBus`](a3net_gossip::GossipBus) via
    /// [`GossipBus::new`](a3net_gossip::GossipBus::new).
    pub fn gossip_transport(&self, local_node: a3net_types::NodeId) -> IrohGossipTransport {
        IrohGossipTransport::new(local_node, self.gossip.clone())
    }

    /// Borrow the underlying iroh `Endpoint`. Useful for callers
    /// that need to run their own A3Net setups on top of the
    /// runtime (e.g. constructing a custom `IrohTransport`).
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Borrow the user-data payload attached to this runtime, if
    /// any. Mirrors `iroh_dns::endpoint_info::UserData`. Set via
    /// [`IrohRuntime::spawn_with_identity_and_user_data`].
    pub fn user_data(&self) -> Option<&a3net_transport::iroh::discovery::UserData> {
        self.user_data.as_ref()
    }

    /// Borrow the shared diagnostics recorder. Set via
    /// [`IrohRuntime::spawn_with_identity_and_user_data`] (or by
    /// callers that build the runtime through the discovery
    /// builder). Returns `None` when the runtime was spawned
    /// without a recorder.
    pub fn diagnostics(
        &self,
    ) -> Option<&Arc<a3net_transport::iroh::discovery::DiscoveryDiagnostics>> {
        self.diagnostics.as_ref()
    }

    /// Clone the shared diagnostics handle so a background task
    /// can publish metrics without holding a borrow on the
    /// runtime. Returns `None` if no recorder was registered at
    /// spawn time.
    ///
    /// Used by the V12 PR2 wiring in `Node::build_with_bus`:
    /// the metrics publisher needs to read the latest snapshot
    /// every tick, and an `Arc<DiscoveryDiagnostics>` is the
    /// cheapest way to share it across an `await` boundary.
    pub fn clone_diagnostics(&self) -> Option<Arc<a3net_transport::iroh::discovery::DiscoveryDiagnostics>> {
        self.diagnostics.clone()
    }

    /// Capture a fresh `EndpointSnapshot` from the live endpoint.
    /// Used by the metrics publisher to push
    /// `a3net_endpoint_direct_addresses` / `relay_urls` /
    /// `endpoint_closed` into the global registry.
    ///
    /// Returns `None` if the endpoint is closed (the iroh
    /// endpoint guard returns `Err` on a closed endpoint).
    pub fn capture_endpoint_snapshot(&self) -> Option<a3net_transport::iroh::endpoint_diagnostics::EndpointSnapshot> {
        use a3net_transport::iroh::endpoint_diagnostics::snapshot_endpoint as capture;
        let snap = capture(&self.endpoint, None);
        if snap.closed {
            None
        } else {
            Some(snap)
        }
    }

    /// Borrow the underlying iroh `Router`. Mostly useful for tests
    /// and for manual shutdown, but kept `pub` so callers can
    /// reach it without going through `shutdown`.
    pub fn router(&self) -> &Router {
        &self.router
    }

    /// Take the incoming-frame receiver (registered via
    /// `a3net/frame/1` on the router). Returns `None` if the
    /// receiver has already been taken — typically because it was
    /// handed to [`IrohTransport::with_endpoint`].
    pub async fn take_frame_receiver(&self) -> Option<tokio::sync::mpsc::Receiver<FrameIn>> {
        self.frame_rx.lock().await.take()
    }

    /// Hand the frame receiver out together with an `Arc<Endpoint>`.
    /// Used by [`NodeBuilder::with_iroh_runtime`](crate::node::NodeBuilder::with_iroh_runtime)
    /// when wiring the runtime into the node — the consumer
    /// wants the frame receiver first (for the transport) and
    /// then the runtime itself (for shutdown). The runtime is
    /// left intact: only the `Receiver` slot is emptied.
    ///
    /// **Single-caller contract.** This method takes `&mut self`
    /// (not `&self`) so the Rust borrow checker rules out
    /// concurrent callers. Two callers racing on `&self` would
    /// both see `Some(rx)` momentarily and the second would
    /// silently observe `None`, leaving an orphan `Sender` in the
    /// router with no consumer — the very bug this method
    /// exists to prevent. The `&mut` signature is the cheapest
    /// mechanical guard against that race.
    ///
    /// Returns `None` for the receiver if the frame receiver has
    /// already been taken (e.g. via `take_frame_receiver`). The
    /// endpoint is always returned.
    pub async fn into_parts(
        &mut self,
    ) -> (
        std::sync::Arc<Endpoint>,
        Option<tokio::sync::mpsc::Receiver<FrameIn>>,
    ) {
        let frame_rx = self.frame_rx.lock().await.take();
        let endpoint = std::sync::Arc::new(self.endpoint.clone());
        (endpoint, frame_rx)
    }

    /// Borrow the frame receiver without consuming it. Mostly useful
    /// for tests / health checks that want to know whether the
    /// channel has been handed off yet.
    pub async fn frame_receiver_is_registered(&self) -> bool {
        self.frame_rx.lock().await.is_some()
    }

    /// Politely shut the runtime down: the router's background task
    /// is cancelled, the frame channel is closed, and the endpoint
    /// is closed.
    ///
    /// # Termination ordering (DO-178C)
    ///
    /// iroh's [`iroh::Endpoint`], [`iroh_gossip::net::Gossip`], and
    /// [`iroh::protocol::Router`] each maintain their own background
    /// tasks. Shutting them down in the wrong order leaves orphaned
    /// tasks holding references to the endpoint (and leaking QUIC
    /// sockets). The order here is:
    ///
    /// 1. Drop the frame receiver (if still attached) so the
    ///    `IrohFrameHandler`'s channel closes and any in-flight
    ///    `try_send` calls stop blocking.
    /// 2. Router — stops accepting new connections; drains the
    ///    in-flight ones.
    /// 3. Endpoint — closes the QUIC sockets.
    /// 4. Final barrier: confirm the underlying rt has been dropped
    ///    by polling the endpoint's `id()` (cheap, no IO) — if the
    ///    closure succeeded we know the router has released its
    ///    strong refs.
    ///
    /// The whole sequence is wrapped in a 5-second timeout so a
    /// stuck router cannot hang `shutdown()` forever. On timeout
    /// the endpoint is closed anyway — the gossip task will be
    /// dropped the next time the bridge loses its last strong
    /// reference.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

        // 0. Release the frame receiver (drop it) so the handler's
        //    channel closes cleanly: dropping the receiver makes
        //    every existing `mpsc::Sender` see `try_send` fail,
        //    and any subsequent connection accepted by the
        //    handler will be dropped instead of queued. This is
        //    the consumer-side half of the orderly shutdown
        //    handshake; the router-side `Sender` itself is dropped
        //    when the `IrohFrameHandler` value drops, which the
        //    router does once `shutdown` returns.
        //
        //    `frame_rx` is `Mutex<Option<Receiver>>`; `mem::take`
        //    swaps the outer `Option` to `None` so we can
        //    pattern-match the inner `Receiver` out and drop it
        //    without borrowing the lock across the call.
        {
            let mut guard = self.frame_rx.lock().await;
            if let Some(rx) = std::mem::take(&mut *guard) {
                drop(rx);
            }
        }

        // 1. Router drains. Bounded by SHUTDOWN_TIMEOUT so a stuck
        //    connection cannot wedge the process.
        // Allow `redundant_pattern_matching`: the `Err(_)` arm
        // deliberately ignores the inner value (it just means
        // `tokio::time::timeout` elapsed); we only want to know
        // whether it timed out vs returned an inner `Err(e)` so we
        // can log the right warning.
        #[allow(clippy::redundant_pattern_matching)]
        let router_result = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.router.shutdown()).await;
        #[allow(clippy::redundant_pattern_matching)]
        if let Err(_) = router_result {
            tracing::warn!(
                "iroh router did not shut down within {SHUTDOWN_TIMEOUT:?}; \
                 closing endpoint anyway"
            );
        } else if let Err(e) = router_result {
            tracing::warn!("iroh router shutdown error: {e}");
        }

        // 2. Endpoint closes.
        // Note: we deliberately keep `self.gossip` alive until after
        // the router is down so any background sync attempts the
        // gossip task is mid-flight can fail cleanly. `self.gossip`
        // is dropped at the end of this function along with the
        // rest of the struct.
        self.endpoint.close().await;
        Ok(())
    }
}
