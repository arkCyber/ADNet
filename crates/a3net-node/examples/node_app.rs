//! `a3net-node` 应用示例：构造一个完整的 `Node`，加入一个房间，导入一个临时文件，
//! 把它广播出去，再通过 `room_feed` 看自己的公告。模拟一次端到端的 "share to room"
//! 场景。
//!
//! 运行：`cargo run -p a3net-node --example node_app`

use std::io::Write;

use a3net_node::{Node, NodeConfig};
use a3net_types::{CdnContentKind, NodeId, RoomId};
use tempfile::tempdir;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- a3net-node app demo ---");

    let dir = tempdir()?;
    let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
        .build()
        .await?;

    let room = RoomId::new("share-room");
    node.join_room(&room).await?;

    // 1. 准备一个临时文件。
    let file = dir.path().join("hello.txt");
    {
        let mut f = std::fs::File::create(&file)?;
        writeln!(f, "hello from a3net-node app demo @ {}", chrono::Utc::now())?;
    }

    // 2. 导入 + 广播。
    let ann = node
        .import_and_announce(
            &room,
            &file,
            String::from("hello.txt"),
            CdnContentKind::GenericFile,
        )
        .await?;
    println!(
        "announced: hash={} size={} title={}",
        ann.content_hash, ann.size_bytes, ann.title
    );

    // 3. 看看自己房间的 feed。
    let feed = node.room_feed(&room).await?;
    println!("feed assets : {}", feed.assets.len());
    for asset in feed.assets.iter().take(5) {
        println!(
            "  - {} ({:?}) from {}",
            asset.title,
            asset.kind,
            asset.announcer_node_id.short()
        );
    }

    Ok(())
}