//! Standalone A3Net daemon example.
//!
//! Boots a full `a3net-node::Node`, binds a JSON-RPC Unix socket, and
//! runs forever serving external clients (TUI, Tauri, web frontend,
//! or any process that can speak JSON-RPC over a Unix socket).
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-ipc-adapter --example daemon -- /tmp/a3net.sock
//! ```
//!
//! Then in another terminal:
//! ```bash
//! a3net ipc-call /tmp/a3net.sock info '{}'
//! # or use the bundled tui_smoke example:
//! cargo run -p a3net-ipc-adapter --example tui_smoke -- /tmp/a3net.sock
//! ```
//!
//! The daemon joins the `lobby` room on startup so the bundled TUI
//! smoke test has something to render immediately. Press Ctrl-C to
//! shut down.

use std::path::PathBuf;

use a3net_ipc_adapter::start_daemon;
use a3net_node::{Node, NodeConfig};
use a3net_types::NodeId;
use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let socket_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/a3net-daemon.sock"));

    let data_dir = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./.a3net-daemon-data"));

    std::fs::create_dir_all(&data_dir)?;
    let cfg = NodeConfig::load_or_create(&data_dir)?;
    let node_id: NodeId = cfg.node_id.clone();
    let node: Node = Node::builder(cfg).build().await?;

    // Auto-join the lobby so the TUI has something to render.
    let lobby: a3net_types::RoomId = "lobby".into();
    if let Err(e) = node.join_room(&lobby).await {
        info!(error = %e, "could not auto-join lobby (continuing)");
    }

    let handle = start_daemon(socket_path.clone(), node).await?;
    info!(
        "a3net daemon ready: node_id={}, data={}, socket={}",
        node_id.short(),
        data_dir.display(),
        socket_path.display()
    );

    // Block until Ctrl-C, then shut the daemon down.
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("received Ctrl-C, shutting daemon down");
        }
        _ = std::future::pending::<()>() => {}
    }
    handle.shutdown();
    Ok(())
}
