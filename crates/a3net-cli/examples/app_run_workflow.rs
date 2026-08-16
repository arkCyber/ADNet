//! Example: realistic "add to room, watch the feed" workflow.
//!
//! Demonstrates the canonical first-run user journey — ingest a local
//! file, announce it into a room, then read back the feed that would
//! be served to remote peers.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-cli --example app_run_workflow
//! ```

use a3net_cli::{feed_view::feed_for_humans, Cli, Cmd};
use a3net_node::RoomFeed;
use a3net_types::{BlobTicket, ContentHash, NodeAddr, NodeId, RoomAsset, RoomId};
use anyhow::Result;
use clap::Parser;
use std::collections::HashMap;
use std::path::PathBuf;

fn main() -> Result<()> {
    println!("=== A3Net CLI: end-to-end ingest + announce + feed ===\n");

    // ── 1. Parse a realistic invocation ─────────────────────────────────
    let args = vec![
        "a3net",
        "--data-dir",
        "./.a3net-demo",
        "announce",
        "--room",
        "home-photos",
        "--file",
        "./photos/holiday.jpg",
        "--title",
        "Holiday 2026",
        "--kind",
        "image",
    ];
    let cli = Cli::try_parse_from(args)?;
    println!("[1] Parsed announce command:");
    println!("    data dir : {}", cli.data_dir);
    if let Cmd::Announce {
        room,
        file,
        title,
        kind,
    } = cli.cmd
    {
        println!("    room     : {room}");
        println!("    file     : {file}");
        println!("    title    : {title}");
        println!("    kind     : {kind}");
    }

    // ── 2. Ensure data dir exists ───────────────────────────────────────
    let data_dir = PathBuf::from(&cli.data_dir);
    std::fs::create_dir_all(&data_dir)?;
    println!("\n[2] Data dir ready at {}", data_dir.display());

    // ── 3. Build a representative blob ticket ───────────────────────────
    let me = NodeId::random();
    let addr = NodeAddr::new(me.clone());
    let hash = ContentHash::from_bytes(b"demo-blob-holiday2026");
    let ticket = BlobTicket::whole(&me, &addr, &hash);
    let raw_ticket = ticket.encode();
    println!("\n[3] Built blob ticket for the announced asset:");
    println!("    {raw_ticket}");

    // ── 4. Round-trip the ticket to prove the encoding is stable ────────
    let parsed = BlobTicket::parse(&raw_ticket)?;
    assert_eq!(parsed.node_id, me);
    assert_eq!(parsed.content_hash, hash);
    println!("    ✓ round-trip parse OK");

    // ── 5. Render the room feed as a human would see it ─────────────────
    let feed = build_demo_feed();
    let human = feed_for_humans(&feed);
    let json = serde_json::to_string_pretty(&human)?;
    println!("\n[4] Feed for the room (humans):\n{json}");

    // ── 6. Sequence the next commands the user would type ───────────────
    println!("[5] Next commands the user might run:");
    let sequence = [
        ("a3net status", "snapshot of node + mesh"),
        ("a3net feed --room home-photos", "re-read the feed"),
        ("a3net get <cid> -o ./holiday.jpg", "download the asset"),
        ("a3net pin add <cid>", "pin it locally"),
        ("a3net diagnostics --json", "gather diagnostics"),
    ];
    for (cmd, desc) in sequence {
        println!("    {cmd:40}  → {desc}");
    }

    println!("\nALL OK");
    Ok(())
}

fn build_demo_feed() -> RoomFeed {
    let room = RoomId::from("home-photos");
    let assets = vec![RoomAsset {
        content_hash: ContentHash::from_bytes(b"demo-blob-holiday2026"),
        title: "Holiday 2026".to_string(),
        kind: a3net_types::CdnContentKind::GenericFile,
        size_bytes: 4_500_000,
        mime_type: Some("image/jpeg".to_string()),
        source_url: None,
        room_id: room.clone(),
        announcer_node_id: NodeId::random(),
        announced_at: chrono::Utc::now(),
    }];
    RoomFeed {
        room_id: room,
        assets,
        peer_map: HashMap::new(),
    }
}
