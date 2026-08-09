//! `adnet` CLI entry point.

use std::path::PathBuf;

use adnet_cli::feed_view::feed_for_humans;
use adnet_cli::{Cli, Cmd};
use adnet_node::{Node, NodeConfig};
use adnet_types::{CdnContentKind, ContentHash, RoomId};
use anyhow::Result;
use clap::Parser;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let data_dir = PathBuf::from(&cli.data_dir);
    std::fs::create_dir_all(&data_dir)?;
    // Persist node_id across restarts so tickets and gossip addresses
    // remain stable — mirrors iroh's per-process `SecretKey` persistence.
    let cfg = NodeConfig::load_or_create(&data_dir)?;
    let node_id = cfg.node_id.clone();
    let node = Node::builder(cfg).build().await?;

    info!(
        "adnet node {} (data: {})",
        node_id.short(),
        data_dir.display()
    );

    match cli.cmd {
        Cmd::Init => {
            println!("node_id  = {}", node_id);
            println!("short    = adnet-{}", node_id.short());
            println!("data_dir = {}", data_dir.display());
        }

        Cmd::Serve => {
            let ep = node.ensure_mesh().await?;
            println!("mesh listening on http://{}/blobs/<hash>", ep);
            // Graceful shutdown on SIGINT / SIGTERM — the prior
            // implementation blocked forever with
            // `std::future::pending()` which prevented Ctrl-C from
            // tearing the server down cleanly.
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("received Ctrl-C, shutting down");
                }
                _ = std::future::pending::<()>() => {}
            }
            node.shutdown().await?;
        }

        Cmd::Announce {
            room,
            file,
            title,
            kind,
        } => {
            let room: RoomId = room.into();
            node.join_room(&room).await?;
            let kind = CdnContentKind::from_str_loose(&kind)
                .ok_or_else(|| anyhow::anyhow!("unknown kind: {kind}"))?;
            let path = std::path::PathBuf::from(&file);
            let ann = node.import_and_announce(&room, &path, title, kind).await?;
            let ticket = ann.ticket.as_ref().map(|t| t.encode()).unwrap_or_default();
            println!(
                "{}",
                serde_json::json!({
                    "room": room.as_str(),
                    "hash": ann.content_hash.as_hex(),
                    "sizeBytes": ann.size_bytes,
                    "ticket": ticket,
                })
            );
        }

        Cmd::Feed { room } => {
            let room: RoomId = room.into();
            node.join_room(&room).await?;
            let feed = node.room_feed(&room).await?;
            let json = serde_json::to_string_pretty(&feed_for_humans(&feed))?;
            println!("{json}");
        }

        Cmd::Echo { room } => {
            let room: RoomId = room.into();
            node.join_room(&room).await?;
            let hash = ContentHash::from_bytes(format!("echo:{room}").as_bytes());
            let ann = adnet_types::Announcement {
                room_id: room.clone(),
                content_hash: hash,
                node_id: node_id.clone(),
                title: format!("echo into {room}"),
                kind: CdnContentKind::GenericFile,
                size_bytes: 0,
                mime_type: None,
                source_url: None,
                ticket: None,
                timestamp: chrono::Utc::now(),
                signer: None,
                signature: None,
            };
            node.announce(&room, &ann).await?;
            println!("echoed into {room}");
        }

        Cmd::Run => {
            // Start the mesh server up front so the REPL can talk
            // about `/mesh`, `/announce`, etc. without each command
            // having to lazily trigger `ensure_mesh` on first use.
            if let Err(e) = node.ensure_mesh().await {
                info!(error = %e, "mesh not started (continuing without it)");
            }
            // Hand the node over to the REPL. The REPL is responsible
            // for tearing it down on `/quit` / EOF.
            let repl_result = adnet_cli::run_repl(data_dir.clone(), node).await;
            info!("REPL ended, exiting");
            repl_result?;
        }
    }

    Ok(())
}
