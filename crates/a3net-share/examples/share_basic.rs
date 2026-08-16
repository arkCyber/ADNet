//! `share_basic` — 创建一个 share ticket + ticket 编解码
//!
//! 演示 `walk_import` 扫描目录 → 生成 ticket → 解析 ticket 验证 round-trip。
//!
//! 运行:`cargo run -p a3net-share --example share_basic`

use std::sync::Arc;

use a3net_share::{ShareTicket, WalkOptions, walk_import};
use a3net_types::{ContentHash, NodeAddr, NodeId};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("hello.txt"), b"hello world")?;

    // 1. 扫描目录。
    let put = Arc::new(|b: &[u8]| Ok(ContentHash::from_bytes(b)));
    let (manifest, mh, _stats) =
        walk_import(dir.path(), put, WalkOptions::default()).await?;
    println!("walked {} entries, root multihash = {mh}", manifest.iter().count());

    // 2. 生成 ticket。
    let me = NodeId::random();
    let endpoint = NodeAddr::new(me.clone());
    let ticket = ShareTicket::new(&me, &endpoint, &mh, &manifest, 0)?;
    let encoded = ticket.encode();
    println!("ticket len = {} chars", encoded.len());

    // 3. 反向解析。
    let parsed = ShareTicket::parse(&encoded)?;
    assert_eq!(parsed.node_id, me);
    println!("parsed ✔ node={}", parsed.node_id);

    Ok(())
}