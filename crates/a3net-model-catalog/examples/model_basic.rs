//! Minimal example: open a model catalog, publish a model from raw
//! bytes, list it, then soft-delete it.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-model-catalog --example model_basic
//! ```

use a3net_model_catalog::{
    ModelCatalog, ModelMetadata, ModelProvider, ModelType, Quantization,
};
use bytes::Bytes;
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let catalog = Arc::new(ModelCatalog::open(dir.path().join("models.db")).await?);
    let provider = ModelProvider::new(catalog.clone());

    // Publish a synthetic 2 KiB "model" payload.
    let payload = Bytes::from(vec![0xABu8; 2048]);
    let metadata = ModelMetadata::new("tiny-llama", ModelType::Llm)
        .with_author("A3Net Test")
        .with_description("Tiny 2 KiB synthetic model used by the basic example.")
        .with_tags(vec!["tiny".into(), "demo".into()])
        .with_architecture("llama3")
        .with_quantization(Quantization::Q4("K_M".into()))
        .with_license("MIT")
        .with_source_url("https://example.invalid/tiny-llama");

    let manifest = provider.publish_bytes(payload, metadata).await?;
    println!(
        "published: id={} size={} hash={}…",
        manifest.id,
        manifest.size_bytes,
        &manifest.content_hash[..16.min(manifest.content_hash.len())],
    );

    // List all models.
    let page = catalog
        .list(a3net_model_catalog::ModelFilter::default())
        .await?;
    println!("catalog has {} model(s)", page.items.len());
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].name, "tiny-llama");

    // Soft-delete.
    provider.remove_model(&manifest.id).await?;
    let page2 = catalog
        .list(a3net_model_catalog::ModelFilter::default())
        .await?;
    println!("after remove: {} model(s)", page2.items.len());
    assert_eq!(page2.items.len(), 0);
    println!("ok");
    Ok(())
}