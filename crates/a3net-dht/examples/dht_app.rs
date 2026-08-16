//! `a3net-dht` 应用示例：模拟一个 A3Net 节点在本地通告自己持有的资源，
//! 然后构建另一个节点的 K-bucket 路由表（加入前一个节点作为对端），
//! 演示 `find_providers` 命中。
//!
//! 本例不真实启动 QUIC / iroh，只演示 DHT 层 API 的常见组合：
//!   - `add_peer` 填充路由表
//!   - `announce_content` 把 (key, addr) 注册为 provider
//!   - `find_providers` 跨节点查询
//!
//! 运行：`cargo run -p a3net-dht --example dht_app`

use std::net::SocketAddr;

use a3net_dht::{DhtConfig, DhtKey, DhtNode};
use a3net_types::NodeId;

#[tokio::main]
async fn main() {
    let alice = NodeId::random();
    let bob = NodeId::random();

    let alice_node = DhtNode::new(DhtConfig {
        local_id: alice.clone(),
        ..Default::default()
    });
    alice_node.set_local_addr("/ip4/127.0.0.1/tcp/9001".into());

    let bob_node = DhtNode::new(DhtConfig {
        local_id: bob.clone(),
        ..Default::default()
    });
    bob_node.set_local_addr("/ip4/127.0.0.1/tcp/9002".into());

    // Bob 知道 Alice 在 127.0.0.1:9001，把她加入自己的路由表。
    let alice_addr: SocketAddr = "127.0.0.1:9001".parse().unwrap();
    bob_node.add_peer(alice.clone(), alice_addr).await;

    // Alice 公告自己持有 "movie-2025.mkv"
    let key = DhtKey::from_bytes(b"movie-2025.mkv".to_vec());
    alice_node.announce_content(&key).await;

    println!("alice : {}", alice.short());
    println!("bob   : {}", bob.short());
    println!("alice routing table size = {}", alice_node.num_peers().await);
    println!("bob   routing table size = {}", bob_node.num_peers().await);

    // 不接 sender 的本地查询：两个节点都看不到对方。
    let only_local = bob_node.find_providers(&key).await;
    println!(
        "bob find_providers (no sender) -> {} providers",
        only_local.len()
    );
    println!(
        "  hint: enable `a3net-transport` to wire a `DhtNetworkSender` and trigger real GetProviders"
    );
}