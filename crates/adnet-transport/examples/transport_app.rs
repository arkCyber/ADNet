//! `adnet-transport` 应用示例：用 QUIC 端点模拟一个 ADNet 节点间的 "echo +
//! 帧分发" 通道，演示 `Frame::text` / `Frame::from_json` / `Frame::new` 三种载荷。
//!
//! 运行：`cargo run -p adnet-transport --example transport_app`

use std::net::SocketAddr;

use adnet_transport::{Frame, QuicTransportBuilder, Transport};
use adnet_types::{Endpoint, NodeAddr};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let server = QuicTransportBuilder::new(
        adnet_types::NodeId::random(),
        "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
    )
    .build()
    .expect("server build");

    // 触发真实 bind 以拿到端口。
    let endpoint = server.get_or_init_endpoint().await.expect("server bind");
    let port = endpoint.local_addr().expect("addr").port();
    let server_node = server.local_node_id().clone();
    println!("[server] id={} port={port}", server_node.short());

    // 服务端：收一帧，回两帧。
    let server_task = tokio::spawn(async move {
        if let Ok(Some((peer, mut conn))) = server.accept().await {
            println!("[server] accepted from {}", peer.short());
            if let Ok(Some(req)) = conn.recv().await {
                println!("[server] recv frame len={}", req.as_bytes().len());
                let payload = serde_json::json!({ "echo": true, "from": peer.short() });
                let _ = conn.send(Frame::from_json(&payload).unwrap()).await;
                let _ = conn.send(Frame::new(b"goodbye".to_vec())).await;
            }
        }
    });

    // 给 server 一瞬间。
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = QuicTransportBuilder::new(
        adnet_types::NodeId::random(),
        "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
    )
    .build()
    .expect("client build");

    let target = NodeAddr::new(server_node).with_direct(Endpoint::new("127.0.0.1", port));
    let mut conn = client.dial_addr(target).await.expect("client dial");
    conn.send(Frame::text("ping")).await.expect("send ping");

    // 等待两条响应。
    for _ in 0..2 {
        match conn.recv().await {
            Ok(Some(f)) => println!("[client] recv frame len={}", f.as_bytes().len()),
            Ok(None) => break,
            Err(e) => println!("[client] err: {e}"),
        }
    }
    let _ = conn.close().await;
    let _ = server_task.await;
    println!("DONE");
}