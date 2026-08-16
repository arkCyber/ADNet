//! Minimal TUI smoke client for the `a3net-ipc-adapter` daemon.
//!
//! Connects to a running daemon via JSON-RPC, subscribes to the
//! `announcement` notification stream, and renders a simple
//! text-mode "TUI" to stdout that updates in real time as new
//! announcements arrive. The polling thread re-fetches `feed`
//! every 2 seconds so the view is also fresh on idle.
//!
//! This is intentionally NOT a real TUI library (no ratatui, no
//! crossterm) — the point is to demonstrate the wire protocol
//! and prove that `json_rpc_stream` works end-to-end. A real TUI
//! front-end would re-render via `crossterm` on every notification
//! and use the same socket for both reads and writes.
//!
//! Run with:
//! ```bash
//! # Terminal 1
//! cargo run -p a3net-ipc-adapter --example daemon -- /tmp/a3net.sock
//!
//! # Terminal 2
//! cargo run -p a3net-ipc-adapter --example tui_smoke -- /tmp/a3net.sock lobby
//! ```
//!
//! Press Ctrl-C to exit. The rendered output is plain text, one
//! line per asset, refreshed in place.

use std::path::PathBuf;
use std::time::Duration;

use a3net_ipc::{StreamItem, json_rpc_call, json_rpc_stream};
use anyhow::Result;
use futures::StreamExt;
use serde_json::json;
use tokio::time::interval;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    let socket = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/a3net-daemon.sock"));
    let room = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "lobby".to_string());

    println!(
        "[tui_smoke] connecting to {} (room={})",
        socket.display(),
        room
    );
    println!("[tui_smoke] press Ctrl-C to exit");

    // 1. One-shot info call so we know the node is reachable.
    let info = json_rpc_call(&socket, "tui", "info", json!({})).await?;
    println!(
        "[tui_smoke] connected to node: {}",
        info["nodeId"].as_str().unwrap_or("?")
    );

    // 2. Initial feed render.
    let feed = json_rpc_call(&socket, "tui", "feed", json!({"room": room})).await?;
    render_feed(&feed, 0);

    // 3. Open the streaming channel for notifications and a
    //    periodic poll for the feed (belt and braces: even if a
    //    notification is dropped because of Lagged, the periodic
    //    poll refreshes the view).
    let mut stream = json_rpc_stream(&socket, "tui").await?;
    let mut tick = interval(Duration::from_secs(2));
    let mut poll_count: u64 = 0;
    let mut notif_count: u64 = 0;

    loop {
        tokio::select! {
            // A frame arrived from the daemon.
            item = stream.next() => {
                let Some(item) = item else { break; };
                let item = item?;
                match item {
                    StreamItem::Response { id, value } => {
                        // The streaming channel only sees
                        // unsolicited frames; the response to
                        // our `info` / `feed` calls has
                        // already been drained by
                        // `json_rpc_call`. If we see one
                        // here we just count it for
                        // diagnostics.
                        let _ = (id, value);
                    }
                    StreamItem::Notification(n) => {
                        notif_count += 1;
                        println!("\n[tui_smoke] notification #{}: method={}",
                            notif_count, n.method);
                        if n.method == "announcement" {
                            println!("[tui_smoke]   room: {}", n.params["room"]);
                            println!("[tui_smoke]   hash: {}", n.params["hash"]);
                            println!("[tui_smoke]   title: {}", n.params["title"]);
                            println!("[tui_smoke]   kind: {}", n.params["kind"]);
                            println!("[tui_smoke]   size: {} bytes", n.params["sizeBytes"]);
                        }
                    }
                }
            }
            // Periodic feed poll.
            _ = tick.tick() => {
                poll_count += 1;
                let feed = json_rpc_call(&socket, "tui", "feed", json!({"room": room})).await?;
                render_feed(&feed, poll_count);
            }
            // Ctrl-C.
            _ = tokio::signal::ctrl_c() => {
                println!("\n[tui_smoke] Ctrl-C received, exiting");
                break;
            }
        }
    }
    Ok(())
}

fn render_feed(feed: &serde_json::Value, poll_count: u64) {
    let assets = feed["assets"].as_array().map(|a| a.len()).unwrap_or(0);
    let peer_count: usize = feed["peerMap"]
        .as_object()
        .map(|m| {
            m.values()
                .filter_map(|v| v.as_array())
                .map(|a| a.len())
                .sum()
        })
        .unwrap_or(0);
    println!(
        "[tui_smoke] poll #{poll_count}: {assets} assets, {peer_count} peer tickets in room {}",
        feed["room"].as_str().unwrap_or("?")
    );
    if let Some(arr) = feed["assets"].as_array() {
        for a in arr.iter().take(5) {
            println!(
                "  - {} ({} bytes, kind={}, hash={})",
                a["title"].as_str().unwrap_or("?"),
                a["sizeBytes"].as_u64().unwrap_or(0),
                a["kind"].as_str().unwrap_or("?"),
                a["hash"].as_str().unwrap_or("?")
            );
        }
        if arr.len() > 5 {
            println!("  … {} more", arr.len() - 5);
        }
    }
}
