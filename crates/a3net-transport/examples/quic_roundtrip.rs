//! Spin up a `QuicTransport`, accept an incoming connection on one task,
//! dial it from another, and exchange framed messages in both directions.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-transport --example quic_roundtrip
//! ```

use std::net::SocketAddr;

use a3net_transport::{Frame, QuicTransportBuilder, Transport};
use a3net_types::NodeAddr;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    println!("-- a3net-transport QUIC roundtrip --");

    // --- 1. Server side ----------------------------------------------
    let server = QuicTransportBuilder::new(
        a3net_types::NodeId::random(),
        "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
    )
    .build()
    .expect("server build");

    // Force the QUIC endpoint to bind so we can learn the assigned port.
    let server_endpoint = server.get_or_init_endpoint().await.expect("server bind");
    let server_port = server_endpoint.local_addr().expect("local addr").port();
    let server_node = server.local_node_id().clone();
    println!("server id     : {}", server_node.short());
    println!("server port   : {server_port}");

    let server_handle = tokio::spawn(async move {
        println!("[server] waiting for incoming connection ...");
        let (_peer, mut incoming) = server.accept().await.expect("accept").expect("incoming");
        println!("[server] accepted connection");
        let greeting = incoming
            .recv()
            .await
            .expect("server recv greeting")
            .expect("greeting not None");
        assert_eq!(greeting, Frame::text("hello from client"));
        println!(
            "[server] received: {}",
            String::from_utf8_lossy(&greeting.0)
        );

        let reply = Frame::text("hello from server");
        incoming.send(reply).await.expect("server send");
        println!("[server] sent reply");

        // Read a second frame and echo it back, then close.
        let echoed = incoming
            .recv()
            .await
            .expect("server recv ping")
            .expect("ping not None");
        println!("[server] echoing: {}", String::from_utf8_lossy(&echoed.0));
        incoming.send(echoed).await.expect("server echo");
        // Give the client a tick to read the echo before we close.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = incoming.close().await;
        println!("[server] closed");
    });

    // --- 2. Client side ----------------------------------------------
    // Give the server a tick to start listening.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = QuicTransportBuilder::new(
        a3net_types::NodeId::random(),
        "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
    )
    .build()
    .expect("client build");
    println!("client id     : {}", client.local_node_id().short());

    let target = NodeAddr::new(server_node.clone())
        .with_direct(a3net_types::Endpoint::new("127.0.0.1", server_port));
    let mut conn = client.dial_addr(target.clone()).await.expect("client dial");
    println!("[client] connected to {}", target.node_id.short());

    conn.send(Frame::text("hello from client"))
        .await
        .expect("client send greeting");
    println!("[client] sent greeting");

    let reply = conn
        .recv()
        .await
        .expect("client recv reply")
        .expect("reply not None");
    assert_eq!(reply, Frame::text("hello from server"));
    println!("[client] received: {}", String::from_utf8_lossy(&reply.0));

    conn.send(Frame::text("ping 2"))
        .await
        .expect("client send ping");
    let echoed = conn
        .recv()
        .await
        .expect("client recv echo")
        .expect("echo not None");
    assert_eq!(echoed, Frame::text("ping 2"));
    println!(
        "[client] echo confirmed: {}",
        String::from_utf8_lossy(&echoed.0)
    );

    let _ = conn.close().await;

    // Wait for server task to finish.
    server_handle.await.expect("server task");
    println!("\nALL OK");
}
