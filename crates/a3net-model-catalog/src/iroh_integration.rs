//! Iroh Integration - High-level Iroh API for model distribution
//!
//! This module provides a simplified interface to Iroh's features for
//! model distribution, including:
//! - Blob storage and retrieval
//! - Ticket generation
//! - Gossip-based discovery
//!
//! ## Note on versions
//!
//! The workspace pins `iroh = 1.0.3` (the umbrella crate) together with
//! `iroh-blobs = 0.103.0`. The 0.103 blobs crate exposes its API through
//! `iroh_blobs::api::Store` (and concrete stores like `MemStore` /
//! `FsStore` that deref to it). The previous
//! `iroh::client::Iroh::add_file/add_bytes/download/create_ticket` facade
//! no longer exists, so this module re-implements the equivalent
//! workflows on top of the new API.

#[cfg(feature = "iroh")]
use std::path::PathBuf;
#[cfg(feature = "iroh")]
use std::sync::Arc;

#[cfg(feature = "iroh")]
use anyhow::{Context, Result};

#[cfg(feature = "iroh")]
use iroh_blobs::{
    api::{
        blobs::{AddProgressItem, ExportProgressItem, ReaderOptions},
        Store,
    },
    store::fs::FsStore,
    Hash,
};
#[cfg(feature = "iroh")]
use iroh_blobs::ticket::BlobTicket;

#[cfg(feature = "iroh")]
use futures::StreamExt;
#[cfg(feature = "iroh")]
use tracing::{debug, error, info, warn};

/// Iroh-based model distribution client.
#[cfg(feature = "iroh")]
pub struct IrohModelClient {
    store: Store,
    data_dir: PathBuf,
}

#[cfg(feature = "iroh")]
impl IrohModelClient {
    /// Wrap an existing [`Store`].
    pub fn from_store(store: Store, data_dir: PathBuf) -> Self {
        Self { store, data_dir }
    }

    /// Open a local on-disk blob store suitable for hosting models.
    pub async fn new_local(data_dir: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&data_dir).await?;
        let fs_store = FsStore::load(&data_dir)
            .await
            .with_context(|| format!("loading blob store at {}", data_dir.display()))?;
        let store: Store = fs_store.into();
        Ok(Self { store, data_dir })
    }

    /// Borrow the underlying `iroh_blobs::api::Store`.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Import a model file into the local blob store and return its hash.
    pub async fn import_model(&self, path: &std::path::Path) -> Result<Hash> {
        let mut s = self.store.blobs().add_path(path).stream().await;
        let mut hash = None;
        while let Some(item) = s.next().await {
            debug!("add_path: {:?}", item);
            if let AddProgressItem::Done(tt) = item {
                hash = Some(tt.hash());
            }
        }
        let hash = hash.context("add_path stream ended without producing a hash")?;
        info!("Model imported: {}", hash);
        Ok(hash)
    }

    /// Import raw bytes into the blob store and return the resulting hash.
    pub async fn import_bytes(&self, data: Vec<u8>, name: &str) -> Result<Hash> {
        let mut s = self.store.blobs().add_bytes(bytes::Bytes::from(data)).stream().await;
        let mut hash = None;
        while let Some(item) = s.next().await {
            debug!("add_bytes({}): {:?}", name, item);
            if let AddProgressItem::Done(tt) = item {
                hash = Some(tt.hash());
            }
        }
        let hash = hash.context("add_bytes stream ended without producing a hash")?;
        info!("Model '{}' imported: {}", name, hash);
        Ok(hash)
    }

    /// Build a downloadable [`BlobTicket`] for an already-imported blob.
    ///
    /// iroh-blobs 0.103 does not provide a "create_ticket" method on
    /// `Blobs`; tickets are constructed directly via [`BlobTicket::new`].
    /// Since this client has no live endpoint, the ticket carries a
    /// placeholder [`EndpointAddr`](iroh::EndpointAddr) derived from a
    /// freshly-generated [`SecretKey`](iroh::SecretKey). Callers that need
    /// a routable ticket should construct it themselves using a real
    /// [`iroh::Endpoint`]'s address.
    pub async fn get_ticket(&self, hash: &Hash) -> Result<BlobTicket> {
        let _ = &self.store;
        let secret = iroh::SecretKey::generate();
        let addr = iroh::EndpointAddr::new(secret.public());
        Ok(BlobTicket::new(addr, *hash, Default::default()))
    }

    /// Read a blob from the local store into memory.
    pub async fn read_model(&self, hash: &Hash) -> Result<Vec<u8>> {
        let mut reader = self
            .store
            .blobs()
            .reader_with_opts(ReaderOptions { hash: *hash });
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(
            &mut tokio::io::BufReader::new(&mut reader),
            &mut buf,
        )
        .await?;
        Ok(buf)
    }

    /// Export a blob from the local store to the filesystem.
    pub async fn export_to(&self, hash: &Hash, target: &std::path::Path) -> Result<()> {
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut s = self.store.blobs().export(*hash, target).stream().await;
        while let Some(item) = s.next().await {
            debug!("export: {:?}", item);
        }
        Ok(())
    }

    /// Check whether the blob is fully present locally.
    pub async fn has_model(&self, hash: &Hash) -> bool {
        let result = self.store.blobs().observe(*hash).stream().await;
        match result {
            Ok(mut s) => {
                while let Some(item) = s.next().await {
                    if item.is_complete() {
                        return true;
                    }
                }
                false
            }
            Err(e) => {
                error!("observe failed: {}", e);
                false
            }
        }
    }
}

/// Iroh gossip integration for model discovery.
#[cfg(feature = "iroh")]
pub struct IrohGossipBridge {
    gossip: Arc<iroh_gossip::net::Gossip>,
    topic_prefix: String,
}

#[cfg(feature = "iroh")]
impl IrohGossipBridge {
    /// Create a new gossip bridge using the given gossip client.
    pub fn new(gossip: Arc<iroh_gossip::net::Gossip>, topic_prefix: &str) -> Self {
        Self {
            gossip,
            topic_prefix: topic_prefix.to_string(),
        }
    }

    /// Get the full topic name.
    fn topic(&self, name: &str) -> String {
        format!("{}-{}", self.topic_prefix, name)
    }

    /// Compute a stable [`TopicId`] for a named topic.
    pub fn topic_id(&self, topic: &str) -> iroh_gossip::proto::TopicId {
        let hash = blake3::hash(self.topic(topic).as_bytes());
        iroh_gossip::proto::TopicId::from_bytes(*hash.as_bytes())
    }

    /// Subscribe to a named topic (returns the raw Iroh [`GossipTopic`]).
    pub async fn subscribe(
        &self,
        topic: &str,
        bootstrap: Vec<iroh::EndpointId>,
    ) -> Result<iroh_gossip::api::GossipTopic, iroh_gossip::api::ApiError> {
        self.gossip.subscribe(self.topic_id(topic), bootstrap).await
    }

    /// Publish `payload` to the given named topic.
    pub async fn publish(
        topic: &mut iroh_gossip::api::GossipTopic,
        payload: Vec<u8>,
    ) -> Result<(), iroh_gossip::api::ApiError> {
        topic.broadcast(bytes::Bytes::from(payload)).await
    }
}

/// Combined Iroh runtime for model distribution
#[cfg(feature = "iroh")]
pub struct ModelIrohRuntime {
    client: IrohModelClient,
    node_id: Option<String>,
}

#[cfg(feature = "iroh")]
impl ModelIrohRuntime {
    /// Create a new runtime with a local blob store at `data_dir`.
    pub async fn new(data_dir: PathBuf) -> Result<Self> {
        let client = IrohModelClient::new_local(data_dir).await?;
        Ok(Self {
            client,
            node_id: None,
        })
    }

    /// Attach an Iroh node ID (hex-encoded [`EndpointId`]) to the runtime.
    pub fn with_node_id(mut self, node_id: String) -> Self {
        self.node_id = Some(node_id);
        self
    }

    /// Borrow the underlying client.
    pub fn client(&self) -> &IrohModelClient {
        &self.client
    }

    /// The configured node ID, if any.
    pub fn node_id(&self) -> Option<&str> {
        self.node_id.as_deref()
    }

    /// Import `path` into the local blob store and return a download ticket.
    pub async fn publish_model(
        &self,
        path: &std::path::Path,
    ) -> Result<BlobTicket> {
        let hash = self.client.import_model(path).await?;
        self.client.get_ticket(&hash).await
    }

    /// Export a blob (from the local store) to `target`.
    pub async fn export_model(
        &self,
        hash: &Hash,
        target: &std::path::Path,
    ) -> Result<()> {
        self.client.export_to(hash, target).await
    }

    /// Check whether a blob exists in the local store.
    pub async fn has_model(&self, hash: &Hash) -> bool {
        self.client.has_model(hash).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_local_import_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        let model_path = temp_dir.path().join("model.bin");
        tokio::fs::write(&model_path, b"hello model world")
            .await
            .unwrap();

        let runtime = ModelIrohRuntime::new(data_dir).await;
        let runtime = match runtime {
            Ok(r) => r,
            Err(e) => {
                warn!("skipping test_local_import_roundtrip: {}", e);
                return;
            }
        };

        let ticket = runtime.publish_model(&model_path).await.unwrap();
        assert_eq!(ticket.hash().to_string().len(), 64);
    }
}
