//! `a3net-dht` 最小示例：构造一个 `DhtNode`、构造一个 `DhtKey`、
//! 在本地 `announce_content` + `find_providers` 完成一次回环验证。
//!
//! 运行：`cargo run -p a3net-dht --example dht_basic`

use a3net_dht::{DhtConfig, DhtKey, DhtNode};
use a3net_types::NodeId;

#[tokio::main]
async fn main() {
    let local_id = NodeId::random();
    let dht = DhtNode::new(DhtConfig {
        local_id: local_id.clone(),
        ..Default::default()
    });
    // 没有真实网络 sender，本节点只能看到自己公告的内容。
    dht.set_local_addr("/ip4/127.0.0.1/tcp/9000".into());

    let payload = b"hello dht world";
    let hash_hex = blake3::hash(payload).to_hex();
    let key = DhtKey::from_content_hash_hex(&hash_hex);

    println!("local id   : {}", local_id.short());
    println!("content    : blake3({:?})", std::str::from_utf8(payload).unwrap());
    println!("key hex    : {}", key.as_hex());

    dht.announce_content(&key).await;

    let providers = dht.find_providers(&key).await;
    println!("providers  : {}", providers.len());
    for p in providers {
        println!("  - {} @ {}", p.provider_id.short(), p.provider_addr);
    }

    // 不相关的 key 找不到
    let other = DhtKey::from_bytes([0xaa; 32].to_vec());
    let empty = dht.find_providers(&other).await;
    println!("unrelated  : {} providers (expected 0)", empty.len());
}