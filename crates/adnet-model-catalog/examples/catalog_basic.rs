//! Minimal adnet-model-catalog example.
//!
//! Opens a SQLite catalog in a temp dir, publishes a small LLM
//! stub from raw bytes, lists it, looks it up by id, and prints
//! the resulting ticket. No iroh network is used — the ticket is
//! just a placeholder string under the `iroh` feature flag.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p adnet-model-catalog --example catalog_basic
//! ```

use adnet_model_catalog::{
    ModelCatalog, ModelFilter, ModelMetadata, ModelProvider, ModelType, Quantization,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let db_path = tmp.path().join("models.db");
    let catalog = Arc::new(ModelCatalog::open(&db_path).await?);

    let provider = ModelProvider::new(catalog.clone());

    let metadata = ModelMetadata::new("smoke-llm", ModelType::Llm)
        .with_author("test")
        .with_description("one-record catalog demo")
        .with_tags(vec!["chat".into(), "demo".into()])
        .with_architecture("llama3")
        .with_quantization(Quantization::Q4("K_M".into()))
        .with_license("MIT");

    let manifest = provider
        .publish_bytes(b"fake model bytes \x00\x01\x02".to_vec().into(), metadata)
        .await?;

    println!("== Published ==");
    println!("id           : {}", manifest.id);
    println!("name         : {}", manifest.name);
    println!("version      : {}", manifest.version);
    println!("size_bytes   : {}", manifest.size_bytes);
    println!("content_hash : {}", manifest.content_hash);
    println!("ticket       : {}", manifest.iroh_ticket);

    let fetched = catalog.get(&manifest.id).await?.expect("just inserted");
    assert_eq!(fetched.content_hash, manifest.content_hash);

    let page = catalog
        .list(ModelFilter {
            model_type: Some(ModelType::Llm),
            ..Default::default()
        })
        .await?;
    println!("\n== Listing ==");
    println!("total        : {}", page.total);
    for m in page.items {
        println!("  - {} v{} ({} bytes)", m.name, m.version, m.size_bytes);
    }

    let ticket = catalog.get_ticket(&manifest.id).await?.expect("has ticket");
    println!("\nTicket for download: {ticket}");

    let stats = catalog.stats().await?;
    println!("\nstats: total_models={} total_size={}", stats.total_models, stats.total_size_bytes);

    Ok(())
}
