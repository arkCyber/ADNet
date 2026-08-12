//! App-level adnet-model-catalog example: a small "community model
//! registry" flow with publish, search, tag filtering, status
//! transitions, and reputation tracking. All happens in a temp
//! SQLite database — no Iroh network is touched.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p adnet-model-catalog --example catalog_app
//! ```

use adnet_model_catalog::{
    DownloadOutcome, ModelCatalog, ModelMetadata, ModelProvider, ModelStatus, ModelType,
    ProviderReputationTracker, Quantization, ReportReason,
};
use adnet_types::node::NodeId;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let db_path = tmp.path().join("registry.db");
    let catalog = Arc::new(ModelCatalog::open(&db_path).await?);
    let provider = ModelProvider::new(catalog.clone());

    // Two providers, three models total — a chat-tuned LLM, a
    // cyberpunk LoRA, and an unreleased embedding.
    let alice = NodeId::from_bytes(&[0xA1u8; 32])?;
    let bob = NodeId::from_bytes(&[0xB2u8; 32])?;
    let alice_provider = ModelProvider::new(catalog.clone()).with_node_id(alice.to_string());
    let bob_provider = ModelProvider::new(catalog.clone()).with_node_id(bob.to_string());

    let llm = alice_provider
        .publish_bytes(
            b"meta-llama-3 chat weights placeholder".to_vec().into(),
            ModelMetadata::new("llama3-chat-8b", ModelType::Llm)
                .with_author("Meta")
                .with_description("Llama 3 8B chat model")
                .with_tags(vec!["chat".into(), "instruct".into()])
                .with_architecture("llama3")
                .with_quantization(Quantization::Q4("K_M".into()))
                .with_license("LLAMA-3"),
        )
        .await?;

    let lora = bob_provider
        .publish_bytes(
            b"sd-cyberpunk-lora weights placeholder".to_vec().into(),
            ModelMetadata::new("cyberpunk-lora", ModelType::Lora)
                .with_author("Bob")
                .with_description("Cyberpunk style LoRA for stable-diffusion")
                .with_tags(vec!["cyberpunk".into(), "sci-fi".into(), "art".into()])
                .with_architecture("sdxl")
                .with_quantization(Quantization::FP16)
                .with_license("CC-BY-4"),
        )
        .await?;

    let embedding = alice_provider
        .publish_bytes(
            b"bge-small embedding weights placeholder".to_vec().into(),
            ModelMetadata::new("bge-small", ModelType::Embedding)
                .with_author("BAAI")
                .with_description("BGE small embedding")
                .with_tags(vec!["embedding".into(), "retrieval".into()])
                .with_architecture("bge")
                .with_quantization(Quantization::FP16),
        )
        .await?;

    println!("== Search ==");
    for m in catalog.search("cyberpunk").await? {
        println!("  hit: {} (tags: {:?})", m.name, m.tags);
    }
    for m in catalog.search("llama").await? {
        println!("  hit: {} ({} bytes)", m.name, m.size_bytes);
    }

    println!("\n== Status transitions ==");
    println!("marking embedding as Unavailable");
    provider.update_status(&embedding.id, ModelStatus::Unavailable).await?;
    println!("marking llm as Removed (soft delete)");
    provider.remove_model(&llm.id).await?;

    let remaining = catalog.search("bge").await?;
    println!("search for 'bge' now returns {} model(s)", remaining.len());

    println!("\n== Reputation ==");
    let rep = ProviderReputationTracker::new();
    if let Ok(snapshots) = catalog.list_provider_reputation().await {
        rep.hydrate(snapshots);
    }

    rep.record_download(&alice.to_string(), DownloadOutcome::Success, &llm.id)?;
    rep.record_download(&alice.to_string(), DownloadOutcome::Success, &llm.id)?;
    rep.record_download(&bob.to_string(), DownloadOutcome::Success, &lora.id)?;
    rep.report_provider(&bob.to_string(), ReportReason::MisleadingMetadata, 0, None)?;

    if let Some(snap) = rep.get(&alice.to_string()) {
        catalog.upsert_provider_reputation(&snap).await?;
        println!("alice trust tier        : {:?}", snap.trust_tier());
        println!("alice successful d/l    : {}", snap.successful_downloads);
    }
    if let Some(snap) = rep.get(&bob.to_string()) {
        catalog.upsert_provider_reputation(&snap).await?;
        println!("bob trust tier          : {:?}", snap.trust_tier());
        println!("bob reports             : {}", snap.reports_count);
    }

    println!("\n== Stats ==");
    let stats = catalog.stats().await?;
    println!("total_models : {}", stats.total_models);
    println!("total_bytes  : {}", stats.total_size_bytes);
    for (kind, count) in &stats.models_by_type {
        println!("  {kind}: {count}");
    }

    let tags = catalog.get_all_tags().await?;
    println!("\n== All tags ==");
    for (tag, n) in tags {
        println!("  {tag}: {n}");
    }

    println!("\n== Hard delete ==");
    provider.delete_model(&llm.id).await?;
    println!("llm should now be gone from the catalog");

    Ok(())
}
