//! `a3net-node` 最小示例：构造一个最小 NodeBuilder，不导入文件、不接 transport，
//! 仅验证本地节点 ID 与 room topic 派生逻辑。
//!
//! 运行：`cargo run -p a3net-node --example node_basic`

use a3net_node::{Node, NodeConfig};
use a3net_types::{NodeId, RoomId};
use tempfile::tempdir;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let cfg = NodeConfig::new(dir.path(), NodeId::random());
    println!("node data dir : {}", dir.path().display());
    println!("local node id : {}", cfg.node_id.short());

    let node = Node::builder(cfg).build().await?;
    println!("node up");

    let room = RoomId::new("lobby");
    node.join_room(&room).await?;
    println!("joined room : {}", room.as_str());

    Ok(())
}