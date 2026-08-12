//! Realistic example: publish two models from raw bytes, search
//! across the catalog, and demonstrate the provider-reputation
//! tracker recording a download outcome.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-model-catalog --example model_app
//! ```

use adnet_model_catalog::{
    DownloadOutcome, ModelCatalog, ModelFilter, ModelMetadata, ModelProvider, ModelType,
    ProviderReputationTracker, Quantization, ReportReason,
};
use bytes::Bytes;
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let catalog = Arc::new(ModelCatalog::open(dir.path().join("models.db")).await?);
    let provider = ModelProvider::new(catalog.clone());

    // 1. Publish two models with different tags.
    for (name, tags) in [
        ("qwen2.5-7b", vec!["chat".to_string(), "instruct".into(), "general".into()]),
        ("sdxl-turbo", vec!["diffusion".to_string(), "image".into(), "creative".into()]),
    ] {
        let meta = ModelMetadata::new(name, ModelType::Llm)
            .with_author("ADNet Demo")
            .with_description(format!("Synthetic {name}"))
            .with_tags(tags)
            .with_architecture(name)
            .with_quantization(Quantization::Q8("0".into()))
            .with_license("Apache-2.0");
        let m = provider.publish_bytes(Bytes::from(vec![0u8; 1024]), meta).await?;
        println!("published {} -> {}", m.name, m.id);
    }

    // 2. List only LLM models (filter by type).
    let page = catalog
        .list(ModelFilter {
            model_type: Some(ModelType::Llm),
            ..Default::default()
        })
        .await?;
    println!("LLM models in catalog: {}", page.items.len());
    assert!(page.items.iter().any(|m| m.name == "qwen2.5-7b"));

    // 3. Full-text search for "image" — should match the SDXL entry.
    let hits = catalog.search("image").await?;
    println!("search 'image' returned {} hit(s)", hits.len());
    assert!(hits.iter().any(|m| m.tags.contains(&"image".into())));

    // 4. Provider reputation: record a successful download.
    let rep = ProviderReputationTracker::new();
    let node_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    rep.record_download(node_id, DownloadOutcome::Success, "qwen2.5-7b")?;
    rep.record_download(node_id, DownloadOutcome::Success, "sdxl-turbo")?;
    rep.report_provider(node_id, ReportReason::MisleadingMetadata, 0, None)?;
    let snap = rep.get(node_id).expect("snapshot");
    println!(
        "reputation: node={} score={:.2} tier={:?}",
        snap.node_id, snap.score, snap.trust_tier()
    );
    assert!(snap.successful_downloads >= 2);

    // Persist snapshot back to the catalog.
    catalog.upsert_provider_reputation(&snap).await?;
    let loaded = catalog.list_provider_reputation().await?;
    println!("persisted snapshots: {}", loaded.len());
    assert_eq!(loaded.len(), 1);
    println!("ok");
    Ok(())
}