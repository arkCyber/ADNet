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
use adnet_gossip::IrohGossipTransport;
use iroh::{protocol::Router, Endpoint};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use iroh_gossip::net::Gossip;
use tracing::info;

/// A self-contained iroh runtime that owns the `Endpoint`, the blob
/// store, the gossip protocol, and the `Router` that hosts them.
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
    /// The router that hosts BlobsProtocol + gossip.
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
    /// gossip ALPNs. The router runs in a background task; call
    /// [`IrohRuntime::shutdown`] to terminate it.
    pub async fn spawn(endpoint: Endpoint, data_dir: &Path) -> anyhow::Result<Self> {
        let adnet_store = IrohBlobStore::open(data_dir).await?;
        let blob_store = adnet_store.handle();
        // `BlobsProtocol::new` takes `&Store`. `FsStore` derefs to
        // `Store`, and we hold it in an `Arc` so the router's
        // background task can keep the protocol alive for the
        // lifetime of the runtime.
        let blobs = BlobsProtocol::new(&*blob_store, None);

        let gossip = Gossip::builder().spawn(endpoint.clone());

        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, blobs)
            .accept(iroh_gossip::ALPN, gossip.clone())
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
            router,
        })
    }

    /// Underlying blob store, exposed for callers that need direct
    /// access (e.g. to construct a custom `BlobsProtocol`).
    pub fn blob_store_handle(&self) -> Arc<FsStore> {
        Arc::clone(&self.blob_store)
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
    pub async fn shutdown(self) -> anyhow::Result<()> {
        // Router::shutdown() drains in-flight connections.
        self.router.shutdown().await?;
        self.endpoint.close().await;
        Ok(())
    }
}
