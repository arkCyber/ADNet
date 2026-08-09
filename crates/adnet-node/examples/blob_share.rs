//! Blob share example — import a local file into an ADNet node and print
//! the announcement + ticket.
//!
//! Run with: `cargo run --example blob_share -p adnet-node -- /path/to/file`

use adnet_node::{Node, NodeConfig};
use adnet_types::{CdnContentKind, NodeId, RoomId};
use anyhow::Result;
use tempfile::tempdir;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let file = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: blob_share <file> [room]"))?;
    let room = args.next().unwrap_or_else(|| "lobby".to_string());

    let tmp = tempdir()?;
    let node = Node::builder(NodeConfig::new(tmp.path(), NodeId::random()))
        .build()
        .await?;

    let room_id: RoomId = room.into();
    node.join_room(&room_id).await?;
    let ann = node
        .import_and_announce(
            &room_id,
            std::path::Path::new(&file),
            file.clone(),
            CdnContentKind::GenericFile,
        )
        .await?;

    println!(
        "announced: hash={} size={} ticket={}",
        ann.content_hash,
        ann.size_bytes,
        ann.ticket.as_ref().map(|t| t.encode()).unwrap_or_default()
    );
    Ok(())
}
