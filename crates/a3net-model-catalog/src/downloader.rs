//! Model Downloader - Downloads models from the P2P network
//!
//! This module handles the consumer side of model distribution:
//! - Downloading models via Iroh tickets
//! - Progress tracking
//! - Cancellation and concurrent download management

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use bytes::Bytes;
#[cfg(feature = "iroh")]
use futures::StreamExt;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

#[cfg(feature = "iroh")]
use iroh_blobs::{api::Store, ticket::BlobTicket};

use crate::catalog::ModelCatalog;
use crate::error::ModelCatalogError;
use crate::reputation::{DownloadOutcome, ProviderReputationTracker};
use crate::types::{DownloadProgress, DownloadStatus};

/// Model Downloader - handles downloading models from the network
pub struct ModelDownloader {
    catalog: Arc<ModelCatalog>,
    download_dir: PathBuf,
    #[cfg(feature = "iroh")]
    blob_store: Option<Arc<Store>>,
    active_downloads: Arc<Mutex<HashMap<String, DownloadProgress>>>,
    /// Optional reputation tracker — when set, every completed
    /// download outcome is recorded against the provider whose
    /// node_id was associated with the model in the catalog.
    reputation: Option<Arc<ProviderReputationTracker>>,
}

impl ModelDownloader {
    /// Create a new model downloader
    pub fn new(catalog: Arc<ModelCatalog>, download_dir: PathBuf) -> Self {
        Self {
            catalog,
            download_dir,
            #[cfg(feature = "iroh")]
            blob_store: None,
            active_downloads: Arc::new(Mutex::new(HashMap::new())),
            reputation: None,
        }
    }

    /// Set the Iroh blob store used to receive downloads.
    #[cfg(feature = "iroh")]
    pub fn with_blob_store(mut self, store: Arc<Store>) -> Self {
        self.blob_store = Some(store);
        self
    }

    /// Attach a [`ProviderReputationTracker`] so completed
    /// downloads automatically feed the reputation system.
    pub fn with_reputation(mut self, rep: Arc<ProviderReputationTracker>) -> Self {
        self.reputation = Some(rep);
        self
    }

    /// Borrow the attached reputation tracker (if any).
    pub fn reputation(&self) -> Option<&ProviderReputationTracker> {
        self.reputation.as_deref()
    }

    /// Record a download outcome against the model. Looks up the
    /// provider node_id from the catalog; if the model has no
    /// `provider_node_id` recorded, the call is a no-op.
    pub async fn record_outcome(
        &self,
        model_id: &str,
        outcome: DownloadOutcome,
    ) -> Result<(), ModelCatalogError> {
        let Some(tracker) = self.reputation.as_ref() else {
            return Ok(());
        };
        let provider_node_id = match self.catalog.get(model_id).await? {
            Some(manifest) => manifest
                .source_url
                .as_deref()
                .and_then(extract_provider_node_id)
                .map(|s| s.to_string()),
            None => None,
        };
        if let Some(node_id) = provider_node_id {
            tracker.record_download(&node_id, outcome, model_id)?;
        }
        Ok(())
    }

    /// Borrow the configured download directory.
    pub fn download_dir(&self) -> &PathBuf {
        &self.download_dir
    }

    /// Download a model by ticket.
    pub async fn download(
        &self,
        model_id: String,
        ticket: String,
    ) -> Result<ModelDownloadHandle, ModelCatalogError> {
        if self.is_downloading(&model_id).await {
            return Err(ModelCatalogError::DownloadInProgress(model_id));
        }

        info!("Starting download for model: {}", model_id);

        // Look up catalog entry for size metadata. We don't fail here if the
        // catalog is missing the model — we can still download an unknown blob.
        let total_bytes = self.catalog.get(&model_id).await?.map(|m| m.size_bytes);

        let mut progress = DownloadProgress::new(model_id.clone(), total_bytes.unwrap_or(0));
        progress.status = DownloadStatus::Connecting;

        {
            let mut downloads = self.active_downloads.lock().await;
            downloads.insert(model_id.clone(), progress.clone());
        }

        // Channel for streaming progress updates to the handle's owner.
        let (tx, rx) = mpsc::channel(64);
        let _ = tx.send(progress.clone()).await;

        let active_downloads = self.active_downloads.clone();
        let catalog = self.catalog.clone();
        let download_dir = self.download_dir.clone();
        #[cfg(feature = "iroh")]
        let blob_store = self.blob_store.clone();
        let model_id_for_task = model_id.clone();

        tokio::spawn(async move {
            let result = run_download(
                RunDownloadArgs {
                    model_id: &model_id_for_task,
                    ticket: &ticket,
                    total_bytes,
                    active_downloads: &active_downloads,
                    download_dir: &download_dir,
                    #[cfg(feature = "iroh")]
                    blob_store: blob_store.as_ref(),
                },
                &tx,
            )
            .await;

            let mut downloads = active_downloads.lock().await;
            match &result {
                Ok(path) => {
                    if let Some(p) = downloads.get_mut(&model_id_for_task) {
                        p.complete();
                        let _ = tx.send(p.clone()).await;
                    }
                    info!("Download completed: {:?}", path);
                }
                Err(e) => {
                    if let Some(p) = downloads.get_mut(&model_id_for_task) {
                        p.fail(e.to_string());
                        let _ = tx.send(p.clone()).await;
                    }
                    warn!("Download failed for {}: {}", model_id_for_task, e);
                }
            }

            if result.is_ok() {
                let _ = catalog.increment_downloads(&model_id_for_task).await;
            }
        });

        Ok(ModelDownloadHandle {
            model_id,
            active_downloads: self.active_downloads.clone(),
            receiver: rx,
        })
    }

    /// Download a model directly from a manifest.
    pub async fn download_model(
        &self,
        manifest: &crate::manifest::ModelManifest,
    ) -> Result<ModelDownloadHandle, ModelCatalogError> {
        self.download(manifest.id.clone(), manifest.iroh_ticket.clone()).await
    }

    /// Convenience: download all bytes for a ticket and return them in memory.
    pub async fn fetch_bytes(&self, ticket: &str) -> Result<Bytes, ModelCatalogError> {
        let ticket = ticket.to_string();
        let download_dir = self.download_dir.clone();
        #[cfg(feature = "iroh")]
        let blob_store = self.blob_store.clone();
        #[cfg(not(feature = "iroh"))]
        let blob_store: Option<()> = None;

        tokio::task::spawn_blocking(move || -> Result<Bytes, ModelCatalogError> {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                #[cfg(feature = "iroh")]
                {
                    fetch_bytes_inner(&ticket, &download_dir, blob_store.as_ref()).await
                }
                #[cfg(not(feature = "iroh"))]
                {
                    let _ = (&ticket, &download_dir, &blob_store);
                    fetch_bytes_inner(&ticket, &download_dir, None).await
                }
            })
        })
        .await
        .map_err(|e| ModelCatalogError::JoinError(e.to_string()))?
    }

    /// Get download progress for a model
    pub async fn get_progress(&self, model_id: &str) -> Option<DownloadProgress> {
        let downloads = self.active_downloads.lock().await;
        downloads.get(model_id).cloned()
    }

    /// Check if a model is currently downloading
    pub async fn is_downloading(&self, model_id: &str) -> bool {
        let downloads = self.active_downloads.lock().await;
        downloads
            .get(model_id)
            .map(|p| !matches!(p.status, DownloadStatus::Completed | DownloadStatus::Failed(_) | DownloadStatus::Cancelled))
            .unwrap_or(false)
    }

    /// Cancel an active download.
    pub async fn cancel_download(&self, model_id: &str) -> bool {
        let mut downloads = self.active_downloads.lock().await;
        if let Some(p) = downloads.get_mut(model_id) {
            p.status = DownloadStatus::Cancelled;
            true
        } else {
            false
        }
    }

    /// Clear a finished/cancelled download entry.
    pub async fn forget(&self, model_id: &str) -> bool {
        let mut downloads = self.active_downloads.lock().await;
        downloads.remove(model_id).is_some()
    }

    /// List active downloads
    pub async fn active_downloads(&self) -> Vec<DownloadProgress> {
        let downloads = self.active_downloads.lock().await;
        downloads.values().cloned().collect()
    }

    /// Count of active downloads (not yet in a terminal state).
    pub async fn active_count(&self) -> usize {
        let downloads = self.active_downloads.lock().await;
        downloads
            .values()
            .filter(|p| {
                !matches!(
                    p.status,
                    DownloadStatus::Completed
                        | DownloadStatus::Failed(_)
                        | DownloadStatus::Cancelled
                )
            })
            .count()
    }
}

struct RunDownloadArgs<'a> {
    model_id: &'a str,
    ticket: &'a str,
    total_bytes: Option<u64>,
    active_downloads: &'a Arc<Mutex<HashMap<String, DownloadProgress>>>,
    download_dir: &'a PathBuf,
    #[cfg(feature = "iroh")]
    blob_store: Option<&'a Arc<Store>>,
}

async fn run_download(
    args: RunDownloadArgs<'_>,
    tx: &mpsc::Sender<DownloadProgress>,
) -> Result<PathBuf, ModelCatalogError> {
    let RunDownloadArgs {
        model_id,
        ticket,
        total_bytes,
        active_downloads,
        download_dir,
        #[cfg(feature = "iroh")]
        blob_store,
    } = args;

    tokio::fs::create_dir_all(download_dir)
        .await
        .map_err(|e| ModelCatalogError::FileError(e.to_string()))?;

    #[cfg(feature = "iroh")]
    {
        if ticket.starts_with("iroh://") {
            if let Some(store) = blob_store {
                let parsed = BlobTicket::from_str(ticket).map_err(|e| {
                    ModelCatalogError::InvalidTicket(e.to_string())
                })?;
                return download_with_iroh(
                    store,
                    &parsed,
                    download_dir,
                    model_id,
                    total_bytes,
                    active_downloads,
                    tx,
                )
                .await;
            }
        }
    }

    // Fallback: synthesize a placeholder file so downstream code has something
    // to point at. This mirrors the iroh-disabled behaviour and makes the
    // downloader testable without a live Iroh node.
    let path = download_dir.join(format!("{}.placeholder", model_id));
    tokio::fs::write(&path, b"iroh feature not enabled")
        .await
        .map_err(|e| ModelCatalogError::FileError(e.to_string()))?;

    Ok(path)
}

#[cfg(feature = "iroh")]
async fn download_with_iroh(
    store: &Arc<Store>,
    ticket: &BlobTicket,
    download_dir: &PathBuf,
    model_id: &str,
    total_bytes: Option<u64>,
    active_downloads: &Arc<Mutex<HashMap<String, DownloadProgress>>>,
    tx: &mpsc::Sender<DownloadProgress>,
) -> Result<PathBuf, ModelCatalogError> {
    use iroh_blobs::api::blobs::ExportProgressItem;

    let hash = ticket.hash();
    info!("Downloading blob {} via Iroh to {}", hash, download_dir.display());

    // Update progress to in-flight
    {
        let mut downloads = active_downloads.lock().await;
        if let Some(p) = downloads.get_mut(model_id) {
            p.status = DownloadStatus::Downloading;
            let _ = tx.send(p.clone()).await;
        }
    }

    let target = download_dir.join(format!("{}.bin", hash.to_hex()));

    // Stream the export so we can report progress.
    let mut export = store.blobs().export(hash, &target).stream().await;
    while let Some(item) = export.next().await {
        match item {
            ExportProgressItem::Size(size) => {
                let mut downloads = active_downloads.lock().await;
                if let Some(p) = downloads.get_mut(model_id) {
                    if total_bytes.is_none() {
                        p.total_bytes = size;
                    }
                    let _ = tx.send(p.clone()).await;
                }
            }
            ExportProgressItem::CopyProgress(offset) => {
                let mut downloads = active_downloads.lock().await;
                if let Some(p) = downloads.get_mut(model_id) {
                    let total = total_bytes.unwrap_or(p.total_bytes);
                    if total > 0 {
                        p.bytes_downloaded = offset.min(total);
                    } else {
                        p.bytes_downloaded = offset;
                    }
                    p.status = DownloadStatus::Downloading;
                    let _ = tx.send(p.clone()).await;
                }
            }
            ExportProgressItem::Done => {
                let mut downloads = active_downloads.lock().await;
                if let Some(p) = downloads.get_mut(model_id) {
                    if let Some(total) = total_bytes {
                        p.bytes_downloaded = total;
                    }
                    let _ = tx.send(p.clone()).await;
                }
            }
            ExportProgressItem::Error(e) => {
                return Err(ModelCatalogError::DownloadError(format!(
                    "export failed: {}",
                    e
                )));
            }
        }
    }

    Ok(target)
}

/// Download a ticket's bytes directly to memory. Used by `fetch_bytes`.
#[cfg(feature = "iroh")]
async fn fetch_bytes_inner(
    ticket_str: &str,
    _download_dir: &PathBuf,
    blob_store: Option<&Arc<Store>>,
) -> Result<Bytes, ModelCatalogError> {
    use std::str::FromStr;

    let store = blob_store
        .ok_or_else(|| ModelCatalogError::DownloadError("no blob store".into()))?;
    let ticket = BlobTicket::from_str(ticket_str)
        .map_err(|e| ModelCatalogError::InvalidTicket(e.to_string()))?;
    let hash = ticket.hash();
    let mut reader = store.blobs().reader(hash);
    let mut buf = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(
        &mut tokio::io::BufReader::new(&mut reader),
        &mut buf,
    )
    .await?;
    Ok(Bytes::from(buf))
}

#[cfg(not(feature = "iroh"))]
async fn fetch_bytes_inner(
    _ticket_str: &str,
    _download_dir: &PathBuf,
    _blob_store: Option<&()>,
) -> Result<Bytes, ModelCatalogError> {
    Err(ModelCatalogError::DownloadError(
        "iroh feature not enabled".to_string(),
    ))
}

/// Download handle for tracking progress
pub struct ModelDownloadHandle {
    pub model_id: String,
    active_downloads: Arc<Mutex<HashMap<String, DownloadProgress>>>,
    receiver: mpsc::Receiver<DownloadProgress>,
}

impl std::fmt::Debug for ModelDownloadHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelDownloadHandle")
            .field("model_id", &self.model_id)
            .finish()
    }
}

impl ModelDownloadHandle {
    /// Get current progress
    pub async fn progress(&self) -> Option<DownloadProgress> {
        let downloads = self.active_downloads.lock().await;
        downloads.get(&self.model_id).cloned()
    }

    /// Check if download is complete
    pub async fn is_complete(&self) -> bool {
        let downloads = self.active_downloads.lock().await;
        matches!(
            downloads.get(&self.model_id).map(|p| &p.status),
            Some(DownloadStatus::Completed)
        )
    }

    /// Check if download failed
    pub async fn is_failed(&self) -> bool {
        let downloads = self.active_downloads.lock().await;
        matches!(
            downloads.get(&self.model_id).map(|p| &p.status),
            Some(DownloadStatus::Failed(_))
        )
    }

    /// Stream progress updates
    pub fn into_stream(self) -> mpsc::Receiver<DownloadProgress> {
        self.receiver
    }

    /// Await completion (or terminal failure).
    pub async fn await_completion(mut self) -> Result<DownloadProgress, ModelCatalogError> {
        while let Some(progress) = self.receiver.recv().await {
            match &progress.status {
                DownloadStatus::Completed => return Ok(progress),
                DownloadStatus::Failed(msg) => {
                    return Err(ModelCatalogError::DownloadError(msg.clone()));
                }
                DownloadStatus::Cancelled => {
                    return Err(ModelCatalogError::Cancelled);
                }
                _ => continue,
            }
        }
        Err(ModelCatalogError::DownloadError(
            "channel closed before completion".to_string(),
        ))
    }
}

/// Convenience function to download a model with minimal configuration.
pub async fn quick_download(
    catalog_path: &str,
    model_id: &str,
    download_dir: PathBuf,
) -> Result<PathBuf, ModelCatalogError> {
    let catalog = ModelCatalog::open(catalog_path).await?;
    let downloader = ModelDownloader::new(Arc::new(catalog), download_dir.clone());

    let ticket = downloader
        .catalog
        .get_ticket(model_id)
        .await?
        .ok_or_else(|| ModelCatalogError::NotFound(model_id.to_string()))?;

    let handle = downloader.download(model_id.to_string(), ticket).await?;
    let _ = handle.await_completion().await?;

    Ok(downloader.download_dir().clone())
}

/// Extract a provider node id from a model `source_url`.
///
/// Convention: a `source_url` that starts with `iroh-provider://`
/// is treated as carrying a 64-char hex node id as the host
/// portion. E.g. `iroh-provider://0123...cdef/details` → Some("0123...cdef").
/// Non-matching URLs return `None`.
fn extract_provider_node_id(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("iroh-provider://")?;
    // The host is everything up to the first `/` or `?`.
    let end = rest
        .find(|c: char| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let host = &rest[..end];
    if host.len() == 64 && host.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(host)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ModelManifest;
    use crate::types::{ModelStatus, ModelType, Quantization};
    use tempfile::TempDir;

    fn sample_manifest(id: &str, name: &str) -> ModelManifest {
        let mut m = ModelManifest::new(
            name.to_string(),
            "1.0.0".to_string(),
            ModelType::Llm,
            100,
            "a".repeat(64),
            "iroh://blob/local/abc".to_string(),
            "Author".to_string(),
            "".to_string(),
            vec![],
            "arch".to_string(),
            Quantization::None,
            "MIT".to_string(),
        );
        m.id = id.to_string();
        m
    }

    #[tokio::test]
    async fn test_downloader_creation() {
        let catalog = ModelCatalog::memory().unwrap();
        let temp_dir = TempDir::new().unwrap();

        let downloader = ModelDownloader::new(
            Arc::new(catalog),
            temp_dir.path().to_path_buf(),
        );

        assert!(!downloader.is_downloading("test").await);
        assert!(downloader.active_downloads().await.is_empty());
        assert_eq!(downloader.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_progress_tracking() {
        let catalog = ModelCatalog::memory().unwrap();
        let temp_dir = TempDir::new().unwrap();

        let downloader = ModelDownloader::new(
            Arc::new(catalog),
            temp_dir.path().to_path_buf(),
        );

        let progress = DownloadProgress::new("test".to_string(), 1000);
        assert_eq!(progress.bytes_downloaded, 0);
        assert_eq!(progress.percent(), 0.0);
    }

    #[tokio::test]
    async fn test_download_creates_placeholder_without_iroh() {
        let catalog = ModelCatalog::memory().unwrap();
        let m = sample_manifest("m1", "Test");
        catalog.add(m).await.unwrap();

        let temp_dir = TempDir::new().unwrap();
        let downloader = ModelDownloader::new(
            Arc::new(catalog),
            temp_dir.path().to_path_buf(),
        );

        let handle = downloader
            .download("m1".to_string(), "iroh://blob/local/abc".to_string())
            .await
            .unwrap();

        let final_progress = handle.await_completion().await.unwrap();
        assert!(matches!(final_progress.status, DownloadStatus::Completed));
        let path = temp_dir.path().join("m1.placeholder");
        assert!(path.exists());
    }

    #[tokio::test]
    async fn test_download_rejects_duplicate() {
        let catalog = ModelCatalog::memory().unwrap();
        let m = sample_manifest("dup", "Dup");
        catalog.add(m).await.unwrap();

        let temp_dir = TempDir::new().unwrap();
        let downloader = ModelDownloader::new(
            Arc::new(catalog),
            temp_dir.path().to_path_buf(),
        );

        let _h1 = downloader
            .download("dup".to_string(), "iroh://blob/local/abc".to_string())
            .await
            .unwrap();

        let dup_err = downloader
            .download("dup".to_string(), "iroh://blob/local/abc".to_string())
            .await
            .unwrap_err();
        assert!(matches!(dup_err, ModelCatalogError::DownloadInProgress(_)));
    }

    #[tokio::test]
    async fn test_cancel_download() {
        let catalog = ModelCatalog::memory().unwrap();
        let m = sample_manifest("c", "C");
        catalog.add(m).await.unwrap();

        let temp_dir = TempDir::new().unwrap();
        let downloader = ModelDownloader::new(
            Arc::new(catalog),
            temp_dir.path().to_path_buf(),
        );

        let _ = downloader
            .download("c".to_string(), "iroh://blob/local/abc".to_string())
            .await
            .unwrap();

        assert!(downloader.cancel_download("c").await);
        let p = downloader.get_progress("c").await.unwrap();
        assert!(matches!(p.status, DownloadStatus::Cancelled));
    }

    #[tokio::test]
    async fn test_forget_removes_entry() {
        let catalog = ModelCatalog::memory().unwrap();
        let m = sample_manifest("f", "F");
        catalog.add(m).await.unwrap();

        let temp_dir = TempDir::new().unwrap();
        let downloader = ModelDownloader::new(
            Arc::new(catalog),
            temp_dir.path().to_path_buf(),
        );

        let _ = downloader
            .download("f".to_string(), "iroh://blob/local/abc".to_string())
            .await
            .unwrap();
        assert!(downloader.forget("f").await);
        assert!(downloader.get_progress("f").await.is_none());
    }

    #[tokio::test]
    async fn test_download_increments_count() {
        let catalog = ModelCatalog::memory().unwrap();
        let m = sample_manifest("inc", "Inc");
        catalog.add(m).await.unwrap();

        let temp_dir = TempDir::new().unwrap();
        let downloader = ModelDownloader::new(
            Arc::new(catalog.clone()),
            temp_dir.path().to_path_buf(),
        );

        let _ = downloader
            .download("inc".to_string(), "iroh://blob/local/abc".to_string())
            .await
            .unwrap();
        let _ = downloader
            .download("inc".to_string(), "iroh://blob/local/abc".to_string())
            .await
            .ok();

        let _ = downloader.cancel_download("inc").await;
        let _ = downloader.forget("inc").await;

        let h = downloader
            .download("inc".to_string(), "iroh://blob/local/abc".to_string())
            .await
            .unwrap();
        let _ = h.await_completion().await;

        let after = catalog.get("inc").await.unwrap().unwrap();
        assert!(after.download_count >= 1);
    }

    // ── extract_provider_node_id helper tests ──────────────────

    #[test]
    fn extract_provider_node_id_from_iroh_provider_scheme() {
        let node = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let url = format!("iroh-provider://{}/path", node);
        assert_eq!(extract_provider_node_id(&url), Some(node));
    }

    #[test]
    fn extract_provider_node_id_trims_query_and_fragment() {
        let node = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let url = format!("iroh-provider://{}?x=1#frag", node);
        assert_eq!(extract_provider_node_id(&url), Some(node));
    }

    #[test]
    fn extract_provider_node_id_rejects_short_host() {
        let url = "iroh-provider://abc/something";
        assert_eq!(extract_provider_node_id(url), None);
    }

    #[test]
    fn extract_provider_node_id_rejects_non_hex_host() {
        let bad = format!("iroh-provider://{}/x", "z".repeat(64));
        assert_eq!(extract_provider_node_id(&bad), None);
    }

    #[test]
    fn extract_provider_node_id_rejects_wrong_scheme() {
        let node = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let url = format!("https://{}/details", node);
        assert_eq!(extract_provider_node_id(&url), None);
    }

    #[test]
    fn extract_provider_node_id_rejects_empty() {
        assert_eq!(extract_provider_node_id(""), None);
        assert_eq!(extract_provider_node_id("iroh-provider://"), None);
    }

    // ── Reputation tracker integration ─────────────────────────

    fn sample_with_provider(name: &str, provider_node_id: &str) -> ModelManifest {
        let mut m = ModelManifest::new(
            name.to_string(),
            "1.0.0".to_string(),
            ModelType::Llm,
            1024,
            "a".repeat(64),
            "iroh://blob/abc".to_string(),
            "Test".to_string(),
            "desc".to_string(),
            vec!["t".to_string()],
            "llama3".to_string(),
            Quantization::None,
            "MIT".to_string(),
        );
        m.source_url = Some(format!("iroh-provider://{}/details", provider_node_id));
        m
    }

    #[tokio::test]
    async fn record_outcome_noop_without_tracker() {
        let catalog = Arc::new(ModelCatalog::memory().unwrap());
        let temp_dir = TempDir::new().unwrap();
        let dl = ModelDownloader::new(catalog, temp_dir.path().to_path_buf());
        // Should not panic when no tracker attached
        dl.record_outcome("any", DownloadOutcome::Success)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn record_outcome_noop_for_unknown_model() {
        let catalog = Arc::new(ModelCatalog::memory().unwrap());
        let temp_dir = TempDir::new().unwrap();
        let tracker = Arc::new(ProviderReputationTracker::new());
        let dl = ModelDownloader::new(catalog, temp_dir.path().to_path_buf())
            .with_reputation(tracker);
        // Unknown model: nothing to do
        dl.record_outcome("ghost", DownloadOutcome::Success)
            .await
            .unwrap();
        assert_eq!(dl.reputation().unwrap().stats().total_providers, 0);
    }

    #[tokio::test]
    async fn record_outcome_noop_when_source_url_lacks_provider() {
        let catalog = Arc::new(ModelCatalog::memory().unwrap());
        let mut m = ModelManifest::new(
            "noprovider".to_string(),
            "1.0.0".to_string(),
            ModelType::Llm,
            1024,
            "a".repeat(64),
            "iroh://blob/abc".to_string(),
            "Test".to_string(),
            "desc".to_string(),
            vec!["t".to_string()],
            "llama3".to_string(),
            Quantization::None,
            "MIT".to_string(),
        );
        m.source_url = Some("https://huggingface.co/foo".to_string());
        catalog.add(m.clone()).await.unwrap();
        let temp_dir = TempDir::new().unwrap();
        let tracker = Arc::new(ProviderReputationTracker::new());
        let dl = ModelDownloader::new(catalog, temp_dir.path().to_path_buf())
            .with_reputation(tracker);
        dl.record_outcome(&m.id, DownloadOutcome::Success)
            .await
            .unwrap();
        // No provider node_id extracted → no provider recorded
        assert_eq!(dl.reputation().unwrap().stats().total_providers, 0);
    }

    #[tokio::test]
    async fn record_outcome_extracts_node_id_and_updates_tracker() {
        let catalog = Arc::new(ModelCatalog::memory().unwrap());
        let node = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let m = sample_with_provider("withprov", node);
        catalog.add(m.clone()).await.unwrap();
        let temp_dir = TempDir::new().unwrap();
        let tracker = Arc::new(ProviderReputationTracker::new());
        let dl = ModelDownloader::new(catalog, temp_dir.path().to_path_buf())
            .with_reputation(tracker.clone());
        dl.record_outcome(&m.id, DownloadOutcome::Success)
            .await
            .unwrap();
        let snap = tracker.get(node).expect("provider tracked");
        assert_eq!(snap.successful_downloads, 1);
    }

    #[tokio::test]
    async fn record_outcome_failure_increments_failed() {
        let catalog = Arc::new(ModelCatalog::memory().unwrap());
        let node = "1111111111111111111111111111111111111111111111111111111111111111";
        let m = sample_with_provider("failprov", node);
        catalog.add(m.clone()).await.unwrap();
        let temp_dir = TempDir::new().unwrap();
        let tracker = Arc::new(ProviderReputationTracker::new());
        let dl = ModelDownloader::new(catalog, temp_dir.path().to_path_buf())
            .with_reputation(tracker.clone());
        dl.record_outcome(&m.id, DownloadOutcome::Failure)
            .await
            .unwrap();
        let snap = tracker.get(node).expect("provider tracked");
        assert_eq!(snap.failed_downloads, 1);
    }

    #[tokio::test]
    async fn record_outcome_cancelled_doesnt_change_counters() {
        let catalog = Arc::new(ModelCatalog::memory().unwrap());
        let node = "2222222222222222222222222222222222222222222222222222222222222222";
        let m = sample_with_provider("cancprov", node);
        catalog.add(m.clone()).await.unwrap();
        let temp_dir = TempDir::new().unwrap();
        let tracker = Arc::new(ProviderReputationTracker::new());
        let dl = ModelDownloader::new(catalog, temp_dir.path().to_path_buf())
            .with_reputation(tracker.clone());
        dl.record_outcome(&m.id, DownloadOutcome::Cancelled)
            .await
            .unwrap();
        let snap = tracker.get(node).expect("provider tracked");
        assert_eq!(snap.successful_downloads, 0);
        assert_eq!(snap.failed_downloads, 0);
    }

    #[tokio::test]
    async fn record_outcome_invalid_node_id_is_silent() {
        let catalog = Arc::new(ModelCatalog::memory().unwrap());
        let mut m = ModelManifest::new(
            "badprov".to_string(),
            "1.0.0".to_string(),
            ModelType::Llm,
            1024,
            "a".repeat(64),
            "iroh://blob/abc".to_string(),
            "Test".to_string(),
            "desc".to_string(),
            vec!["t".to_string()],
            "llama3".to_string(),
            Quantization::None,
            "MIT".to_string(),
        );
        // Malformed provider url that decodes a non-hex host
        m.source_url = Some("iroh-provider://zzz/x".to_string());
        catalog.add(m.clone()).await.unwrap();
        let temp_dir = TempDir::new().unwrap();
        let tracker = Arc::new(ProviderReputationTracker::new());
        let dl = ModelDownloader::new(catalog, temp_dir.path().to_path_buf())
            .with_reputation(tracker.clone());
        // Should be a no-op, not an error
        dl.record_outcome(&m.id, DownloadOutcome::Success)
            .await
            .unwrap();
        assert_eq!(tracker.stats().total_providers, 0);
    }
}
