//! # A3Net Model Catalog
//!
//! AI Model Distribution Network - A decentralized P2P system for distributing
//! AI models using A3Net's Iroh-based infrastructure.
//!
//! ## Features
//!
//! - **Model Metadata Catalog**: SQLite-based storage for model information
//! - **P2P Distribution**: Use Iroh's Bao-verified blob transfer
//! - **Model Discovery**: Gossip-based announcements for model availability
//! - **Provider Reputation**: Per-provider scoring, trust tiers, user reports
//! - **Web Interface**: Built-in HTTP server with model browsing UI
//! - **Search & Filter**: Full-text search with tag-based filtering
//!
//! ## Model Lifecycle
//!
//! ```text
//! add ──► list/search ──► download ──► remove (soft)
//!                  │                      │
//!                  └──► update ──────────┴──► delete (hard)
//! ```
//!
//! | Operation | Method | Effect |
//! |-----------|--------|--------|
//! | Add | `ModelProvider::publish_model` | Imports blob, generates ticket, inserts catalog entry |
//! | List | `ModelCatalog::list` | Paginated listing, filtered by type/tag/arch/search |
//! | Search | `ModelCatalog::search` | Full-text search across name/description/author/tags |
//! | Update | `ModelProvider::update_metadata` | Updates description/tags/version/license/source_url |
//! | Remove | `ModelProvider::remove_model` | Soft delete — marks "Removed", hidden from listings |
//! | Delete | `ModelProvider::delete_model` | Hard delete — removes catalog entry + best-effort blob removal |
//!
//! ## Provider Reputation
//!
//! Each peer that publishes models gets a `ProviderReputation` snapshot,
//! updated from three signal sources:
//!
//! - **Download outcome** (success/failure/cancelled) — fed via
//!   [`reputation::ProviderReputationTracker::record_download`].
//! - **Manifest integrity** — currently a manual gate; future versions
//!   will validate hashes against `ModelManifest` and call
//!   `report_provider(..., ReportReason::MisleadingMetadata, ...)` on
//!   mismatch.
//! - **User reports** — see [`reputation::ReportReason`].
//!
//! Reputation snapshots are persisted via
//! [`ModelCatalog::upsert_provider_reputation`] so they survive restarts.
//! The server exposes them at `/api/providers`, and the CLI shows them
//! via `a3net-model-catalog providers`.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use a3net_model_catalog::{
//!     ModelCatalog, ModelProvider, ModelType,
//!     ProviderReputationTracker, ReportReason,
//! };
//! use a3net_model_catalog::provider::ModelMetadata;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Open catalog
//! let catalog = Arc::new(ModelCatalog::open("models.db").await?);
//!
//! // ── Set up reputation ──
//! let rep = Arc::new(ProviderReputationTracker::new());
//! // Hydrate from on-disk snapshots
//! if let Ok(snapshots) = catalog.list_provider_reputation().await {
//!     rep.hydrate(snapshots);
//! }
//!
//! // ── Add a model ──
//! let provider = ModelProvider::new(catalog.clone());
//! let metadata = ModelMetadata::new("llama3-8b", ModelType::Llm)
//!     .with_author("Meta")
//!     .with_description("Llama 3 8B instruct model")
//!     .with_tags(vec!["chat".into(), "instruct".into()])
//!     .with_architecture("llama3")
//!     .with_quantization(a3net_model_catalog::Quantization::Q4("K_M".into()))
//!     .with_source_url("https://huggingface.co/meta/llama3-8b");
//!
//! let manifest = provider.publish_model("/path/to/model.bin", metadata).await?;
//!
//! // ── Record a download outcome ──
//! rep.record_download(&provider_node_id, DownloadOutcome::Success, &manifest.id)?;
//!
//! // ── File a user report ──
//! rep.report_provider(
//!     &provider_node_id,
//!     ReportReason::MisleadingMetadata,
//!     0,
//!     Some("ticket doesn't match hash".into()),
//! )?;
//!
//! // ── Persist snapshot back to disk ──
//! if let Some(snap) = rep.get(&provider_node_id) {
//!     catalog.upsert_provider_reputation(&snap).await?;
//! }
//! # Ok(())
//! # }
//! ```

pub mod manifest;
pub mod catalog;
pub mod provider;
pub mod downloader;
pub mod discovery;
pub mod reputation;
pub mod error;
pub mod types;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "iroh")]
pub mod iroh_integration;

pub use manifest::ModelManifest;
pub use types::{ModelType, Quantization};
pub use catalog::ModelCatalog;
pub use provider::{ModelProvider, ModelMetadata};
pub use downloader::{ModelDownloader, ModelDownloadHandle};
pub use discovery::{ModelDiscovery, ProviderInfo, DiscoveryEvent};
pub use reputation::{
    DownloadOutcome, ProviderReputation, ProviderReputationTracker, ReportReason, ReputationStats,
    TrustFlag, TrustTier,
};
pub use error::ModelCatalogError;
pub use types::*;
