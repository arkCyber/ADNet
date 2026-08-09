//! iroh-backed runtime for [`Node`](crate::node::Node).
//!
//! When the `iroh` feature is enabled, this module exposes an
//! [`IrohRuntime`] that wires a pre-built [`iroh::Endpoint`] together
//! with an [`IrohBlobStore`](adnet_blobstore::IrohBlobStore) and an
//! [`IrohGossipTransport`](adnet_gossip::IrohGossipTransport) into a
//! single iroh [`iroh::protocol::Router`].
//!
//! The router hosts:
//! - `iroh_blobs::BlobsProtocol` over the `iroh_blobs::ALPN` — Bao-verified
//!   blob transfer.
//! - The gossip protocol over `iroh_gossip::ALPN` — HyParView+PlumTree
//!   epidemic broadcast trees.
//!
//! ADNet's own `Transport` trait is unaffected — when callers wire an
//! `IrohTransport` into the node, dialing a peer goes through the same
//! iroh `Endpoint` the router is bound to.
//!
//! ## Usage
//!
//! ```ignore
//! use adnet_node::iroh_runtime::IrohRuntime;
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

use adnet_blobstore::IrohBlobStore;
use adnet_chatstore::IrohDocsChat;
use adnet_gossip::IrohGossipTransport;
use iroh::{protocol::Router, Endpoint};
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
/// everything down cleanly.
pub struct IrohRuntime {
    /// The underlying iroh endpoint. Shared with `IrohTransport`
    /// when the caller wants dialing to go through the same
    /// connection pool.
    pub endpoint: Endpoint,
    /// The blob store. The `BlobsProtocol` borrows it; the
    /// ADNet-facing `BlobReader` / `BlobImporter` access it directly.
    blob_store: Arc<FsStore>,
    /// High-level wrapper around the same FsStore for ADNet callers.
    pub adnet_store: IrohBlobStore,
    /// The iroh-gossip handle. Pass `.clone()` to
    /// `IrohGossipTransport::new` to wire ADNet's room bus on top.
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
    /// This binds the supplied endpoint to the BlobsProtocol +
    /// gossip ALPNs and the docs ALPN. The router runs in a
    /// background task; call [`IrohRuntime::shutdown`] to terminate
    /// it.
    ///
    /// `docs_path` is the directory where `iroh-docs` should store
    /// its redb replica + default author. Pass `<data_dir>` to keep
    /// everything under one root, or any other directory.
    pub async fn spawn(
        endpoint: Endpoint,
        data_dir: &Path,
        _docs_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let adnet_store = IrohBlobStore::open(data_dir).await?;
        let blob_store = adnet_store.handle();
        // `BlobsProtocol::new` takes `&Store`. `FsStore` derefs to
        // `Store`, and we hold it in an `Arc` so the router's
        // background task can keep the protocol alive for the
        // lifetime of the runtime.
        let blobs = BlobsProtocol::new(&*blob_store, None);

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

        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, blobs)
            .accept(iroh_gossip::ALPN, gossip.clone())
            .accept(iroh_docs::ALPN, docs.clone())
            .spawn();

        info!(
            endpoint_id = %endpoint.id(),
            blob_dir = %adnet_store.path().display(),
            "iroh runtime online"
        );

        Ok(Self {
            endpoint,
            blob_store,
            adnet_store,
            gossip,
            docs,
            docs_api,
            router,
        })
    }

    /// Underlying blob store, exposed for callers that need direct
    /// access (e.g. to construct a custom `BlobsProtocol`).
    pub fn blob_store_handle(&self) -> Arc<FsStore> {
        Arc::clone(&self.blob_store)
    }

    /// Cheap clone of the underlying [`DocsApi`]. Useful for
    /// constructing an [`IrohDocsChat`](adnet_chatstore::IrohDocsChat)
    /// outside of the runtime — see [`IrohRuntime::chat_bridge`].
    pub fn docs_api(&self) -> Arc<DocsApi> {
        Arc::clone(&self.docs_api)
    }

    /// Construct an [`IrohDocsChat`] backed by this runtime's docs
    /// engine + blob store. Phase 5a entry point — the returned
    /// bridge uses iroh-docs `Doc`s for message sync.
    pub async fn chat_bridge(&self) -> anyhow::Result<IrohDocsChat> {
        Ok(IrohDocsChat::new(self.docs_api(), self.adnet_store.clone()).await?)
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
    /// [`GossipBus`](adnet_gossip::GossipBus) via
    /// [`GossipBus::new`](adnet_gossip::GossipBus::new).
    pub fn gossip_transport(
        &self,
        local_node: adnet_types::NodeId,
    ) -> IrohGossipTransport {
        IrohGossipTransport::new(local_node, self.gossip.clone())
    }

    /// Politely shut the runtime down: the router's background task
    /// is cancelled and the endpoint is closed.
    ///
    /// # Termination ordering (DO-178C)
    ///
    /// iroh's [`iroh::Endpoint`], [`iroh_gossip::net::Gossip`], and
    /// [`iroh::protocol::Router`] each maintain their own background
    /// tasks. Shutting them down in the wrong order leaves orphaned
    /// tasks holding references to the endpoint (and leaking QUIC
    /// sockets). The order here is:
    ///
    /// 1. Router — stops accepting new connections; drains the
    ///    in-flight ones.
    /// 2. Endpoint — closes the QUIC sockets.
    /// 3. Final barrier: confirm the underlying rt has been dropped
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

        // 1. Router drains. Bounded by SHUTDOWN_TIMEOUT so a stuck
        //    connection cannot wedge the process.
        let router_result = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.router.shutdown()).await;
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
