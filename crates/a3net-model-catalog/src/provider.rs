//! Model Provider - Publishes models to the P2P network
//!
//! This module handles the provider side of model distribution:
//! - Importing model files to the local blob store
//! - Generating Iroh tickets
//! - Announcing models via the gossip bus

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

#[cfg(feature = "iroh")]
use iroh::{EndpointAddr, EndpointId};
#[cfg(feature = "iroh")]
use iroh_blobs::{
    api::{
        blobs::{AddProgressItem, ExportProgressItem},
        Store,
    },
    ticket::BlobTicket,
    Hash,
};
#[cfg(feature = "iroh")]
use iroh_gossip::{api::GossipTopic, net::Gossip};
#[cfg(feature = "iroh")]
use futures::StreamExt;
use crate::catalog::ModelCatalog;
use crate::error::ModelCatalogError;
use crate::manifest::{compute_blake3_hash, ModelManifest};
use crate::types::{ModelStatus, ModelType, Quantization};

/// Gossip topic for model announcements
const MODEL_ANNOUNCEMENT_TOPIC: &str = "a3net-model-announcements-v1";

/// Parse a hex-encoded BLAKE3 hash into an Iroh [`Hash`].
///
/// iroh-blobs 0.103 does not re-export its `HexOrBase32ParseError`, so we
/// decode the hex ourselves and call [`Hash::from_bytes`].
#[cfg(feature = "iroh")]
fn parse_hash(hex_str: &str) -> Result<Hash, ModelCatalogError> {
    if hex_str.len() != 64 {
        return Err(ModelCatalogError::InvalidContentHash(format!(
            "expected 64 hex chars, got {}",
            hex_str.len()
        )));
    }
    let bytes = hex::decode(hex_str)
        .map_err(|e| ModelCatalogError::InvalidContentHash(e.to_string()))?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
        ModelCatalogError::InvalidContentHash("decoded hash wrong length".to_string())
    })?;
    Ok(Hash::from_bytes(bytes))
}

/// Derive a stable `TopicId` from a string by hashing it with BLAKE3.
#[cfg(feature = "iroh")]
fn topic_id(name: &str) -> iroh_gossip::proto::TopicId {
    let hash = blake3::hash(name.as_bytes());
    iroh_gossip::proto::TopicId::from_bytes(*hash.as_bytes())
}

/// All metadata fields required to publish a model.
///
/// Used by both [`ModelProvider::publish_model`] and [`ModelProvider::publish_bytes`]
/// so callers can build a single [`ModelMetadata`] struct and reuse it.
#[derive(Debug, Clone)]
pub struct ModelMetadata {
    pub name: String,
    pub version: String,
    pub model_type: ModelType,
    pub author: String,
    pub description: String,
    pub tags: Vec<String>,
    pub architecture: String,
    pub quantization: Quantization,
    pub license: String,
    pub source_url: Option<String>,
}

impl ModelMetadata {
    /// Create a minimal metadata struct with sensible defaults.
    pub fn new(name: impl Into<String>, model_type: ModelType) -> Self {
        Self {
            name: name.into(),
            version: "1.0.0".to_string(),
            model_type,
            author: "anonymous".to_string(),
            description: String::new(),
            tags: Vec::new(),
            architecture: "unknown".to_string(),
            quantization: Quantization::None,
            license: "UNKNOWN".to_string(),
            source_url: None,
        }
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = author.into();
        self
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    pub fn with_architecture(mut self, arch: impl Into<String>) -> Self {
        self.architecture = arch.into();
        self
    }

    pub fn with_quantization(mut self, q: Quantization) -> Self {
        self.quantization = q;
        self
    }

    pub fn with_license(mut self, license: impl Into<String>) -> Self {
        self.license = license.into();
        self
    }

    pub fn with_source_url(mut self, url: impl Into<String>) -> Self {
        self.source_url = Some(url.into());
        self
    }
}

/// Model Provider - handles publishing models to the network
pub struct ModelProvider {
    catalog: Arc<ModelCatalog>,
    node_id: Option<String>,
    #[cfg(feature = "iroh")]
    blob_store: Option<Arc<Store>>,
    #[cfg(feature = "iroh")]
    gossip: Option<Arc<Gossip>>,
    /// Active topic handles, one per gossip subscription. Storing them keeps
    /// the gossip subscription alive.
    #[cfg(feature = "iroh")]
    gossip_topics: Arc<tokio::sync::Mutex<Vec<GossipTopic>>>,
}

impl ModelProvider {
    /// Create a new model provider
    pub fn new(catalog: Arc<ModelCatalog>) -> Self {
        Self {
            catalog,
            node_id: None,
            #[cfg(feature = "iroh")]
            blob_store: None,
            #[cfg(feature = "iroh")]
            gossip: None,
            #[cfg(feature = "iroh")]
            gossip_topics: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    /// Set the node ID for this provider
    pub fn with_node_id(mut self, node_id: String) -> Self {
        self.node_id = Some(node_id);
        self
    }

    /// Set the Iroh blob store.
    #[cfg(feature = "iroh")]
    pub fn with_blob_store(mut self, store: Arc<Store>) -> Self {
        self.blob_store = Some(store);
        self
    }

    /// Set the gossip instance for announcements
    #[cfg(feature = "iroh")]
    pub fn with_gossip(mut self, gossip: Arc<Gossip>) -> Self {
        self.gossip = Some(gossip);
        self
    }

    /// Read-only access to the underlying catalog.
    pub fn catalog(&self) -> &Arc<ModelCatalog> {
        &self.catalog
    }

    /// Current node ID, if one has been configured.
    pub fn node_id(&self) -> Option<&str> {
        self.node_id.as_deref()
    }

    /// Publish a model file to the P2P network.
    pub async fn publish_model(
        &self,
        path: impl AsRef<Path>,
        metadata: ModelMetadata,
    ) -> Result<ModelManifest, ModelCatalogError> {
        let path = path.as_ref();
        info!("Publishing model: {} from {:?}", metadata.name, path);

        let file_data = tokio::fs::read(path).await.map_err(|e| {
            ModelCatalogError::FileError(format!("Failed to read file: {}", e))
        })?;

        self.publish_bytes_inner(Bytes::from(file_data), metadata).await
    }

    /// Publish a model from raw bytes.
    pub async fn publish_bytes(
        &self,
        data: Bytes,
        metadata: ModelMetadata,
    ) -> Result<ModelManifest, ModelCatalogError> {
        self.publish_bytes_inner(data, metadata).await
    }

    async fn publish_bytes_inner(
        &self,
        data: Bytes,
        metadata: ModelMetadata,
    ) -> Result<ModelManifest, ModelCatalogError> {
        let size_bytes = data.len() as u64;
        let content_hash = compute_blake3_hash(&data);
        info!(
            "Publishing '{}' ({} bytes, hash {}…)",
            metadata.name,
            size_bytes,
            &content_hash[..16]
        );

        let iroh_ticket = self.generate_ticket(&content_hash, data.clone()).await?;

        let mut manifest = ModelManifest::new(
            metadata.name.clone(),
            metadata.version,
            metadata.model_type,
            size_bytes,
            content_hash,
            iroh_ticket,
            metadata.author,
            metadata.description,
            metadata.tags,
            metadata.architecture,
            metadata.quantization,
            metadata.license,
        );
        manifest.source_url = metadata.source_url;

        manifest.validate()?;
        self.catalog.add(manifest.clone()).await?;

        info!(
            "Model published: {} (ID: {})",
            manifest.name, manifest.id
        );

        #[cfg(feature = "iroh")]
        if let Some(ref gossip) = self.gossip {
            if let Err(e) = self.announce_model(&manifest, gossip).await {
                // Announcement is best-effort - don't fail the publish
                warn!("Failed to announce model via gossip: {}", e);
            }
        }

        Ok(manifest)
    }

    /// Generate an Iroh ticket for a blob, importing the bytes if a blob store is
    /// configured.
    async fn generate_ticket(
        &self,
        content_hash: &str,
        data: Bytes,
    ) -> Result<String, ModelCatalogError> {
        #[cfg(feature = "iroh")]
        {
            if let Some(ref store) = self.blob_store {
                let expected_hash = parse_hash(content_hash)?;

                let imported_hash = self.import_blob(store, data).await?;

                // Sanity-check that the imported hash matches the hash we computed.
                // If not, our hashing path disagrees with the blob store - refuse
                // to issue a ticket for the wrong content.
                if imported_hash != expected_hash {
                    return Err(ModelCatalogError::IrohError(format!(
                        "hash mismatch: computed {} but blob store imported {}",
                        expected_hash, imported_hash
                    )));
                }

                let node_id = self.node_id.as_ref().ok_or_else(|| {
                    ModelCatalogError::ProviderError("Node ID not set".to_string())
                })?;
                let parsed_endpoint = EndpointId::from_str(node_id).map_err(|e| {
                    ModelCatalogError::ProviderError(e.to_string())
                })?;
                let addr = EndpointAddr::new(parsed_endpoint);

                let ticket = BlobTicket::new(addr, imported_hash, Default::default());
                return Ok(ticket.to_string());
            }
        }

        // Fallback: synthesize a placeholder ticket when no Iroh store is configured.
        Ok(format!("iroh://blob/local/{}", content_hash))
    }

    /// Import the bytes into the blob store and return the resulting hash.
    #[cfg(feature = "iroh")]
    async fn import_blob(&self, store: &Store, data: Bytes) -> Result<Hash, ModelCatalogError> {
        let mut s = store.blobs().add_bytes(data).stream().await;
        let mut imported = None;
        while let Some(item) = s.next().await {
            match item {
                AddProgressItem::Done(tt) => {
                    imported = Some(tt.hash());
                }
                AddProgressItem::Error(e) => {
                    return Err(ModelCatalogError::IrohError(format!(
                        "import failed: {}",
                        e
                    )));
                }
                _ => {}
            }
        }
        imported.ok_or_else(|| {
            ModelCatalogError::IrohError("import stream ended without hash".to_string())
        })
    }

    /// Announce a model via the gossip bus
    #[cfg(feature = "iroh")]
    async fn announce_model(
        &self,
        manifest: &ModelManifest,
        gossip: &Arc<Gossip>,
    ) -> Result<(), ModelCatalogError> {
        let announcement = ModelAnnouncement {
            model_id: manifest.id.clone(),
            content_hash: manifest.content_hash.clone(),
            model_type: manifest.model_type.clone(),
            size_bytes: manifest.size_bytes,
            name: manifest.name.clone(),
            author: manifest.author.clone(),
            timestamp: Utc::now(),
            provider_name: None,
        };

        let payload = serde_json::to_vec(&announcement)?;

        // Try to subscribe if we haven't already; reuse existing handle otherwise.
        let mut topics = self.gossip_topics.lock().await;
        if topics.is_empty() {
            let topic = gossip
                .subscribe(topic_id(MODEL_ANNOUNCEMENT_TOPIC), Vec::new())
                .await
                .map_err(|e| ModelCatalogError::GossipError(e.to_string()))?;
            topics.push(topic);
        }
        let topic: &mut GossipTopic = &mut topics[0];
        topic
            .broadcast(payload.into())
            .await
            .map_err(|e| ModelCatalogError::GossipError(e.to_string()))?;
        info!("Model announced via gossip: {}", manifest.id);
        Ok(())
    }

    /// Update an existing model's status.
    pub async fn update_status(
        &self,
        model_id: &str,
        status: ModelStatus,
    ) -> Result<(), ModelCatalogError> {
        self.catalog.update_status(model_id, status).await
    }

    /// Remove a model from the catalog (soft delete).
    ///
    /// This marks the model as "Removed" so it no longer appears in listings,
    /// but the blob data remains in the Iroh store. Use [`delete_model`](Self::delete_model)
    /// for a permanent deletion that also removes the blob.
    pub async fn remove_model(&self, model_id: &str) -> Result<(), ModelCatalogError> {
        self.catalog.remove(model_id).await
    }

    /// Permanently delete a model: soft-delete in the catalog AND (if a blob
    /// store is configured) attempt to remove the blob.
    ///
    /// Note: Iroh's blob store is content-addressed and immutable — blobs are
    /// deduplicated and shared across models. Calling `delete` on the store is
    /// best-effort; the blob is only truly removed when no remaining reference
    /// holds it. The catalog entry is always soft-deleted regardless.
    #[cfg(not(feature = "iroh"))]
    pub async fn delete_model(&self, model_id: &str) -> Result<(), ModelCatalogError> {
        self.catalog.remove(model_id).await
    }

    /// Permanently delete a model: soft-delete in the catalog AND (if a blob
    /// store is configured) attempt to remove the blob.
    ///
    /// Note: Iroh's blob store is content-addressed and immutable — blobs are
    /// deduplicated and shared across models. Calling `delete` on the store is
    /// best-effort; the blob is only truly removed when no remaining reference
    /// holds it. The catalog entry is always soft-deleted regardless.
    #[cfg(feature = "iroh")]
    pub async fn delete_model(&self, model_id: &str) -> Result<(), ModelCatalogError> {
        let manifest = self
            .catalog
            .get(model_id)
            .await?
            .ok_or_else(|| ModelCatalogError::NotFound(model_id.to_string()))?;

        // 1. Soft-delete in catalog
        self.catalog.remove(model_id).await?;

        // 2. Best-effort blob deletion (if configured)
        if let Some(ref store) = self.blob_store {
            let hash = parse_hash(&manifest.content_hash)?;
            match store.blobs().delete(hash).await {
                Ok(_) => {
                    info!(
                        "Deleted blob {} for model {}",
                        manifest.content_hash, manifest.id
                    );
                }
                Err(e) => {
                    warn!(
                        "Could not delete blob {} from store (may still be referenced): {}",
                        manifest.content_hash, e
                    );
                }
            }
        }

        info!(
            "Model {} permanently deleted (catalog entry removed)",
            manifest.id
        );
        Ok(())
    }

    /// List all models owned by this provider (all models in the catalog).
    pub async fn list_models(&self) -> Result<Vec<ModelManifest>, ModelCatalogError> {
        let page = self.catalog.list(Default::default()).await?;
        Ok(page.items)
    }

    /// Update a model's metadata in place (no re-hash, no re-import).
    pub async fn update_metadata(
        &self,
        model_id: &str,
        updater: impl FnOnce(&mut ModelManifest),
    ) -> Result<ModelManifest, ModelCatalogError> {
        let mut manifest = self
            .catalog
            .get(model_id)
            .await?
            .ok_or_else(|| ModelCatalogError::NotFound(model_id.to_string()))?;
        updater(&mut manifest);
        manifest.updated_at = Utc::now();
        manifest.validate()?;
        self.catalog.add(manifest.clone()).await?;
        Ok(manifest)
    }

    /// Re-publish the byte payload into the local blob store without touching
    /// the catalog.
    #[cfg(feature = "iroh")]
    pub async fn reimport(
        &self,
        data: Bytes,
    ) -> Result<Hash, ModelCatalogError> {
        let store = self.blob_store.as_ref().ok_or_else(|| {
            ModelCatalogError::ProviderError("no blob store configured".to_string())
        })?;
        self.import_blob(store, data).await
    }
}

/// Model announcement message for gossip
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAnnouncement {
    pub model_id: String,
    pub content_hash: String,
    pub model_type: ModelType,
    pub size_bytes: u64,
    pub name: String,
    pub author: String,
    pub timestamp: chrono::DateTime<Utc>,
    pub provider_name: Option<String>,
}

/// Convenience function to publish a model with minimal configuration.
pub async fn quick_publish<P: AsRef<Path>>(
    catalog_path: P,
    model_path: P,
    name: String,
    model_type: ModelType,
    author: String,
) -> Result<ModelManifest, ModelCatalogError> {
    let catalog = ModelCatalog::open(catalog_path).await?;
    let provider = ModelProvider::new(Arc::new(catalog));

    let metadata = ModelMetadata::new(name, model_type)
        .with_author(author)
        .with_description("Auto-published model");

    provider.publish_model(model_path, metadata).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn metadata(name: &str, model_type: ModelType) -> ModelMetadata {
        ModelMetadata::new(name, model_type)
            .with_author("Test Author")
            .with_description("test description")
            .with_tags(vec!["test".to_string()])
            .with_architecture("test")
            .with_quantization(Quantization::Q4("K_M".to_string()))
            .with_license("MIT")
    }

    #[tokio::test]
    async fn test_publish_bytes() {
        let catalog = ModelCatalog::memory().unwrap();
        let provider = ModelProvider::new(Arc::new(catalog));

        let data = Bytes::from(vec![1u8; 1024]);

        let manifest = provider
            .publish_bytes(data, metadata("TestModel", ModelType::Llm))
            .await
            .unwrap();

        assert_eq!(manifest.name, "TestModel");
        assert_eq!(manifest.size_bytes, 1024);
        assert_eq!(manifest.content_hash.len(), 64);
        assert!(manifest.iroh_ticket.starts_with("iroh://"));
    }

    #[tokio::test]
    async fn test_publish_file() {
        let catalog = ModelCatalog::memory().unwrap();
        let provider = ModelProvider::new(Arc::new(catalog));

        let temp_file = NamedTempFile::new().unwrap();
        tokio::fs::write(temp_file.path(), b"test model data")
            .await
            .unwrap();

        let manifest = provider
            .publish_model(temp_file.path(), metadata("FileModel", ModelType::Lora))
            .await
            .unwrap();

        assert_eq!(manifest.name, "FileModel");
        assert_eq!(manifest.size_bytes, 15);
    }

    #[tokio::test]
    async fn test_publish_file_not_found() {
        let catalog = ModelCatalog::memory().unwrap();
        let provider = ModelProvider::new(Arc::new(catalog));

        let res = provider
            .publish_model(
                "/no/such/path.bin",
                metadata("No", ModelType::Lora),
            )
            .await;
        assert!(matches!(res, Err(ModelCatalogError::FileError(_))));
    }

    #[tokio::test]
    async fn test_update_status() {
        let catalog = ModelCatalog::memory().unwrap();
        let provider = ModelProvider::new(Arc::new(catalog));

        let manifest = provider
            .publish_bytes(
                Bytes::from(vec![0u8; 16]),
                metadata("S", ModelType::Llm),
            )
            .await
            .unwrap();

        provider
            .update_status(&manifest.id, ModelStatus::Unavailable)
            .await
            .unwrap();

        let after = provider
            .catalog
            .get(&manifest.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.status, ModelStatus::Unavailable);
    }

    #[tokio::test]
    async fn test_update_metadata() {
        let catalog = ModelCatalog::memory().unwrap();
        let provider = ModelProvider::new(Arc::new(catalog));

        let manifest = provider
            .publish_bytes(
                Bytes::from(vec![0u8; 16]),
                metadata("Renamable", ModelType::Llm),
            )
            .await
            .unwrap();

        let updated = provider
            .update_metadata(&manifest.id, |m| {
                m.description = "updated".to_string();
            })
            .await
            .unwrap();

        assert_eq!(updated.description, "updated");
    }

    #[tokio::test]
    async fn test_remove_model_is_soft_delete() {
        let catalog = ModelCatalog::memory().unwrap();
        let provider = ModelProvider::new(Arc::new(catalog));

        let manifest = provider
            .publish_bytes(
                Bytes::from(vec![0u8; 16]),
                metadata("ToRemove", ModelType::Llm),
            )
            .await
            .unwrap();

        provider.remove_model(&manifest.id).await.unwrap();

        // Soft delete: `get` still returns the model (it has no status filter)
        let fetched = provider
            .catalog
            .get(&manifest.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.status, ModelStatus::Removed);

        // But `list` filters it out because status != 'Removed'
        let listed = provider
            .list_models()
            .await
            .unwrap();
        assert!(listed.iter().all(|m| m.id != manifest.id));
    }

    #[tokio::test]
    async fn test_delete_model_removes_from_catalog() {
        let catalog = ModelCatalog::memory().unwrap();
        let provider = ModelProvider::new(Arc::new(catalog));

        let manifest = provider
            .publish_bytes(
                Bytes::from(vec![0u8; 16]),
                metadata("ToDelete", ModelType::Llm),
            )
            .await
            .unwrap();

        // delete_model is only available with iroh feature; without it falls back to soft delete.
        #[cfg(not(feature = "iroh"))]
        {
            provider.delete_model(&manifest.id).await.unwrap();
            let listed = provider.list_models().await.unwrap();
            assert!(listed.iter().all(|m| m.id != manifest.id));
        }

        #[cfg(feature = "iroh")]
        {
            // Without a blob store, delete_model just soft-deletes
            provider.delete_model(&manifest.id).await.unwrap();
            let listed = provider.list_models().await.unwrap();
            assert!(listed.iter().all(|m| m.id != manifest.id));
        }
    }

    #[tokio::test]
    async fn test_validation_rejects_invalid() {
        let catalog = ModelCatalog::memory().unwrap();
        let provider = ModelProvider::new(Arc::new(catalog));

        let mut bad = metadata("OK", ModelType::Llm);
        bad.name = "".into();
        let err = provider
            .publish_bytes(Bytes::from(vec![1u8; 4]), bad)
            .await
            .unwrap_err();
        assert!(matches!(err, ModelCatalogError::ValidationError(_)));
    }
}
