//! `adnet-gossip` 应用示例：模拟一个 ADNet 房间里的节点在导入资源后把
//! `Announcement` 广播出去，订阅节点把内容组织成一个简易的 "room feed"，
//! 这是 `ray room feed` 命令后端的最小复刻。
//!
//! 运行：`cargo run -p adnet-gossip --example gossip_app`

use std::sync::Arc;

use adnet_gossip::{GossipBus, InProcessGossip};
use adnet_types::{
    Announcement, CdnContentKind, ContentHash, NodeId, RoomId,
};
use chrono::Utc;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    // 三个节点共享同一个 in-process transport。
    let transport: Arc<InProcessGossip> = Arc::new(InProcessGossip::default());

    let alice = GossipBus::new(NodeId::random(), transport.clone());
    let bob = GossipBus::new(NodeId::random(), transport.clone());
    let carol = GossipBus::new(NodeId::random(), transport.clone());

    let room = RoomId::new("ai-models");
    for node in [&alice, &bob, &carol] {
        node.join_room(&room).await.expect("join");
    }

    // bob / carol 先订阅，alice 后发布。
    let mut bob_rx = bob.subscribe(&room);
    let mut carol_rx = carol.subscribe(&room);

    // 等订阅注册。
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Alice 连续发布 3 个不同大小的 "AI 模型"。
    for (i, (title, size)) in [
        ("Llama-3-8B-Instruct-Q4", 4_500_000_000u64),
        ("Mistral-7B-OpenOrca-Q5", 5_100_000_000),
        ("Phi-3-Mini-4K-Q8", 3_900_000_000),
    ]
    .iter()
    .enumerate()
    {
        let payload = format!("{title}-{i}").into_bytes();
        let ann = Announcement {
            room_id: room.clone(),
            content_hash: ContentHash::from_bytes(&payload),
            node_id: alice.local_node().clone(),
            title: (*title).into(),
            kind: CdnContentKind::AiModel,
            size_bytes: *size,
            mime_type: Some("application/octet-stream".into()),
            source_url: Some(format!("https://example.com/{title}.gguf")),
            ticket: None,
            timestamp: Utc::now(),
            message_id: None,
            ttl_secs: None,
            signer: None,
            signature: None,
        };
        alice.publish(&room, &ann).await.expect("publish");
    }

    // 收集 bob 和 carol 收到的 feed。
    let timeout = std::time::Duration::from_millis(500);
    let mut feed: Vec<Announcement> = Vec::new();
    loop {
        match tokio::time::timeout(timeout, bob_rx.recv()).await {
            Ok(Ok(a)) => feed.push(a),
            _ => break,
        }
        if feed.len() == 3 {
            break;
        }
    }
    // 防止 carol 的 receiver 因为 len==3 而使示例提前退出。
    let _ = tokio::time::timeout(timeout, carol_rx.recv()).await;

    println!("room : {}", room.as_str());
    println!("feed : {} announcement(s)", feed.len());
    for a in feed {
        println!(
            "  - {} [{}] {:.2} GiB from {}",
            a.title,
            format!("{:?}", a.kind),
            a.size_bytes as f64 / 1_073_741_824.0,
            a.node_id.short()
        );
    }
}