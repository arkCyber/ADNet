//! Example: batch operations with room feeds
//!
//! Demonstrates how to:
//! - Work with multiple room feeds in batch
//! - Filter and aggregate feed data
//! - Generate reports from feed data
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-cli --example batch_feeds
//! ```

use a3net_cli::feed_view::feed_for_humans;
use a3net_node::RoomFeed;
use a3net_types::{ContentHash, CdnContentKind, NodeId, RoomAsset, RoomId};
use anyhow::Result;
use std::collections::HashMap;

fn main() -> Result<()> {
    println!("=== A3Net Batch Feeds Demo ===\n");

    // Simulate feeds from multiple rooms
    let feeds = vec![
        ("lobby", create_feed("lobby", 5)),
        ("research", create_feed("research", 3)),
        ("models", create_feed("models", 4)),
    ];

    // 1. Aggregate all feeds
    println!("1. Aggregating feeds from {} rooms...", feeds.len());
    let mut total_assets = 0;
    let mut total_size: u64 = 0;
    let mut kind_counts: HashMap<String, usize> = HashMap::new();

    for (_room_name, feed) in &feeds {
        let human = feed_for_humans(feed);
        total_assets += human.assets.len();
        for asset in &human.assets {
            total_size += asset.size_bytes;
            *kind_counts.entry(asset.kind.to_string()).or_insert(0) += 1;
        }
    }
    println!("   Total assets: {}", total_assets);
    println!("   Total size: {} bytes ({:.2} GB)", total_size, total_size as f64 / 1e9);

    // 2. Generate summary report
    println!("\n2. Summary Report:");
    let avg_size = if total_assets == 0 { 0 } else { total_size / total_assets as u64 };
    println!("   Average asset size: {} bytes", avg_size);
    println!("   Content kinds:");
    for (kind, count) in &kind_counts {
        println!("     - {kind}: {count}");
    }

    // 3. Find largest assets
    println!("\n3. Top 5 Largest Assets:");
    let mut all_assets_flat: Vec<(String, String, u64)> = Vec::new();
    
    for (room_name, feed) in &feeds {
        let human = feed_for_humans(feed);
        for asset in &human.assets {
            all_assets_flat.push((
                room_name.to_string(),
                asset.title.to_string(),
                asset.size_bytes,
            ));
        }
    }
    
    all_assets_flat.sort_by(|a, b| b.2.cmp(&a.2));
    for (i, (room, title, size)) in all_assets_flat.iter().take(5).enumerate() {
        println!("   {}. [{}] {} - {} bytes", i + 1, room, title, size);
    }

    // 4. Room summaries
    println!("\n4. Room Summaries:");
    for (room_name, feed) in &feeds {
        let human = feed_for_humans(feed);
        let room_size: u64 = human.assets.iter().map(|a| a.size_bytes).sum();
        println!("   {room_name}: {} assets, {} bytes", human.assets.len(), room_size);
    }

    // 5. Export to JSON
    println!("\n5. Export Feeds to JSON:");
    let mut export_rooms = Vec::new();
    for (name, feed) in &feeds {
        let human = feed_for_humans(feed);
        export_rooms.push(serde_json::json!({
            "room": name,
            "asset_count": human.assets.len(),
            "total_size": human.assets.iter().map(|a| a.size_bytes).sum::<u64>(),
        }));
    }

    let export = serde_json::json!({
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "room_count": feeds.len(),
        "total_assets": total_assets,
        "total_size_bytes": total_size,
        "kind_counts": kind_counts,
        "rooms": export_rooms,
    });

    let export_json = serde_json::to_string_pretty(&export)?;
    println!("   JSON size: {} bytes", export_json.len());
    println!("   Content:");
    for line in export_json.lines().take(12) {
        println!("   {line}");
    }
    if export_json.lines().count() > 12 {
        println!("   ... ({} more lines)", export_json.lines().count() - 12);
    }

    println!("\n=== Batch Feeds Demo Complete ===");
    Ok(())
}

fn create_feed(room_name: &str, count: usize) -> RoomFeed {
    let room = RoomId::new(room_name);
    let kinds = vec![
        CdnContentKind::Article,
        CdnContentKind::AiModel,
        CdnContentKind::Dataset,
        CdnContentKind::VideoModel,
        CdnContentKind::GenericFile,
    ];

    let assets: Vec<_> = (0..count)
        .map(|i| {
            let kind = kinds[i % kinds.len()].clone();
            let size: u64 = match kind {
                CdnContentKind::Article => 10_000 + (i as u64 * 1000),
                CdnContentKind::AiModel => 5_000_000_000 + (i as u64 * 1_000_000_000),
                CdnContentKind::Dataset => 1_000_000_000 + (i as u64 * 500_000_000),
                CdnContentKind::VideoModel => 500_000_000 + (i as u64 * 100_000_000),
                CdnContentKind::GenericFile => 1000 + (i as u64 * 500),
                CdnContentKind::Profile => 100 + (i as u64 * 10),
            };
            let title = format!("{} - Item {}", room_name, i + 1);

            RoomAsset {
                content_hash: ContentHash::from_bytes(
                    format!("{}-{}-content", room_name, i).as_bytes(),
                ),
                title,
                kind,
                size_bytes: size,
                mime_type: None,
                source_url: None,
                room_id: room.clone(),
                announcer_node_id: NodeId::random(),
                announced_at: chrono::Utc::now() - chrono::Duration::hours(i as i64),
            }
        })
        .collect();

    RoomFeed {
        room_id: room,
        assets,
        peer_map: HashMap::new(),
    }
}
