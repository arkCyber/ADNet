//! Example: feed view utilities
//!
//! Demonstrates how to:
//! - Convert room feeds to human-readable format
//! - Serialize feeds as JSON
//! - Work with RoomAsset structures
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-cli --example feed_viewer
//! ```

use adnet_cli::feed_view::feed_for_humans;
use adnet_node::RoomFeed;
use adnet_types::{
    ContentHash, CdnContentKind, NodeId, RoomAsset, RoomId,
};
use anyhow::Result;
use std::collections::HashMap;

fn main() -> Result<()> {
    println!("=== ADNet Feed Viewer Demo ===\n");

    // 1. Create a sample feed with multiple assets
    println!("1. Creating sample room feed...");
    let feed = create_sample_feed();
    println!("   Room: {}", feed.room_id.as_str());
    println!("   Assets: {}", feed.assets.len());

    // 2. Convert to human-readable format
    println!("\n2. Converting to human-readable format...");
    let human_feed = feed_for_humans(&feed);

    println!("   Room: {}", human_feed.room);
    println!("   Asset count: {}", human_feed.assets.len());

    // 3. Display each asset
    println!("\n3. Asset details:");
    for (i, asset) in human_feed.assets.iter().enumerate() {
        println!("\n   [{i}] {}", asset.title);
        println!("       Kind:  {}", asset.kind);
        println!("       Size:  {} bytes", asset.size_bytes);
        println!("       Hash:  {}...", &asset.hash[..16]);
        println!("       By:    {}...", &asset.announced_by[..16]);
        println!("       At:    {}", asset.announced_at);
    }

    // 4. Serialize to JSON
    println!("\n4. JSON serialization:");
    let json = serde_json::to_string_pretty(&human_feed)?;
    for line in json.lines().take(15) {
        println!("   {line}");
    }
    if json.lines().count() > 15 {
        println!("   ... ({} more lines)", json.lines().count() - 15);
    }

    // 5. Compact JSON for API responses
    println!("\n5. Compact JSON (for API):");
    let compact = serde_json::to_string(&human_feed)?;
    println!("   {}", &compact[..200.min(compact.len())]);
    if compact.len() > 200 {
        println!("   ... ({} more chars)", compact.len() - 200);
    }

    // 6. Filter by kind
    println!("\n6. Filtering by content kind:");
    let articles: Vec<_> = human_feed
        .assets
        .iter()
        .filter(|a| a.kind == "article")
        .collect();
    println!("   Articles: {}", articles.len());

    let models: Vec<_> = human_feed
        .assets
        .iter()
        .filter(|a| a.kind.contains("model"))
        .collect();
    println!("   AI Models: {}", models.len());

    println!("\n=== Feed Viewer Demo Complete ===");
    Ok(())
}

fn create_sample_feed() -> RoomFeed {
    let room = RoomId::new("demo-room");

    let assets = vec![
        RoomAsset {
            content_hash: ContentHash::from_bytes(b"article-1-content-here"),
            title: "Getting Started with ADNet".to_string(),
            kind: CdnContentKind::Article,
            size_bytes: 15420,
            mime_type: Some("text/markdown".to_string()),
            source_url: Some("https://example.com/articles/adnet-intro".to_string()),
            room_id: room.clone(),
            announcer_node_id: NodeId::random(),
            announced_at: chrono::Utc::now() - chrono::Duration::hours(2),
        },
        RoomAsset {
            content_hash: ContentHash::from_bytes(b"model-weights-v1-data"),
            title: "LLM Weights v1.0".to_string(),
            kind: CdnContentKind::AiModel,
            size_bytes: 7_000_000_000,
            mime_type: Some("application/octet-stream".to_string()),
            source_url: None,
            room_id: room.clone(),
            announcer_node_id: NodeId::random(),
            announced_at: chrono::Utc::now() - chrono::Duration::hours(5),
        },
        RoomAsset {
            content_hash: ContentHash::from_bytes(b"dataset-training-examples"),
            title: "Training Dataset Batch 42".to_string(),
            kind: CdnContentKind::Dataset,
            size_bytes: 2_500_000_000,
            mime_type: Some("application/x-tar".to_string()),
            source_url: None,
            room_id: room.clone(),
            announcer_node_id: NodeId::random(),
            announced_at: chrono::Utc::now() - chrono::Duration::days(1),
        },
        RoomAsset {
            content_hash: ContentHash::from_bytes(b"video-model-preview"),
            title: "Video Model Preview".to_string(),
            kind: CdnContentKind::VideoModel,
            size_bytes: 850_000_000,
            mime_type: Some("video/mp4".to_string()),
            source_url: Some("https://example.com/videos/preview.mp4".to_string()),
            room_id: room.clone(),
            announcer_node_id: NodeId::random(),
            announced_at: chrono::Utc::now() - chrono::Duration::minutes(30),
        },
        RoomAsset {
            content_hash: ContentHash::from_bytes(b"readme-file-content"),
            title: "README.md".to_string(),
            kind: CdnContentKind::GenericFile,
            size_bytes: 4200,
            mime_type: Some("text/plain".to_string()),
            source_url: None,
            room_id: room.clone(),
            announcer_node_id: NodeId::random(),
            announced_at: chrono::Utc::now(),
        },
    ];

    RoomFeed {
        room_id: room,
        assets,
        peer_map: HashMap::new(),
    }
}
