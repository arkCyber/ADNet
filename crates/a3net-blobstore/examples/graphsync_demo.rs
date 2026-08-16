//! End-to-end GraphSync demonstration: walk a DAG from a "remote"
//! node through the in-process mock transport.
//!
//! Build & run:
//! ```bash
//! cargo run -p a3net-blobstore --example graphsync_demo
//! ```
//!
//! This shows the full client/server path:
//!   client ── Request(root, selector) ──► transport ──► server
//!   client ◄── Block(cid, data) ◄───────── transport ◄── server (traverse)
//!   client ◄── Response(Completed) ◄────── transport ◄── server

use std::sync::Arc;

use a3net_blobstore::{
    GraphSyncClient, GraphSyncServer, GraphSyncTransportBridge, GraphSyncWire, MemDagStore,
    MockGraphSyncTransport,
};
use a3net_types::graphsync::{BlockMessage, GraphSyncMessage, selector};
use a3net_types::{Cid, NodeId};
use tokio::sync::mpsc;

fn dummy_node(byte: u8) -> NodeId {
    let arr = [byte; 32];
    NodeId::from_bytes(&arr).expect("32-byte node id")
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Two virtual nodes — a "remote" server and a "local" client.
    let server_id = dummy_node(1);
    let client_id = dummy_node(2);

    // Build a small DAG in the server's store:
    //
    //   root
    //     ├── leaf_a
    //     └── leaf_b
    let server_store = Arc::new(MemDagStore::new());

    let leaf_a_cid = {
        let bytes = b"alpha block".to_vec();
        let cid = Cid::from_content_blake3(&bytes);
        server_store.insert(cid.clone(), bytes, vec![]);
        cid
    };
    let leaf_b_cid = {
        let bytes = b"beta block".to_vec();
        let cid = Cid::from_content_blake3(&bytes);
        server_store.insert(cid.clone(), bytes, vec![]);
        cid
    };
    let root_cid = {
        let bytes = b"root node payload".to_vec();
        let cid = Cid::from_content_blake3(&bytes);
        server_store.insert(
            cid.clone(),
            bytes,
            vec![leaf_a_cid.clone(), leaf_b_cid.clone()],
        );
        cid
    };

    // One shared mock transport, owned by the *server* node id.
    // Each side registers the *other* node id's inbound sender.
    let transport = Arc::new(MockGraphSyncTransport::new(server_id.clone()));
    let (server_to_client_tx, mut server_to_client_rx) = mpsc::channel::<Vec<u8>>(32);
    let (client_to_server_tx, mut client_to_server_rx) = mpsc::channel::<Vec<u8>>(32);
    transport
        .register_inbound_sender(client_id.clone(), server_to_client_tx)
        .await;
    transport
        .register_inbound_sender(server_id.clone(), client_to_server_tx)
        .await;

    let server = GraphSyncServer::new(server_store.clone(), transport.clone());
    let client = GraphSyncClient::new(transport.clone());

    // Server listener: feed each frame into `on_frame`.
    let server_id_for_task = server_id.clone();
    let server_for_task = Arc::new(server);
    let server_listener = tokio::spawn(async move {
        while let Some(bytes) = client_to_server_rx.recv().await {
            match GraphSyncWire::decode(&bytes) {
                Ok(frame) => {
                    if let Err(e) = server_for_task.on_frame(&client_id, frame).await {
                        eprintln!("server on_frame error: {}", e);
                    }
                }
                Err(e) => eprintln!("server decode error: {}", e),
            }
        }
    });

    // Client listener: feed each frame into the shared client's
    // `on_frame` so it can route into the matching pending request.
    let client_for_listener = client.clone_shared();
    let client_listener = tokio::spawn(async move {
        while let Some(bytes) = server_to_client_rx.recv().await {
            match GraphSyncWire::decode(&bytes) {
                Ok(frame) => client_for_listener.on_frame(frame),
                Err(e) => eprintln!("client decode error: {}", e),
            }
        }
    });

    // Fire off a recursive GraphSync request for `root`.
    let mut handle = client
        .request(
            &server_id_for_task,
            root_cid.clone(),
            selector::match_recursive(None),
            1,
        )
        .await
        .expect("client request ok");

    let mut blocks = 0;
    while let Some(item) = handle.next_block().await {
        match item {
            Ok((cid, data)) => {
                let label = if cid.hash_hex() == root_cid.hash_hex() {
                    "root"
                } else if cid.hash_hex() == leaf_a_cid.hash_hex() {
                    "leaf_a"
                } else if cid.hash_hex() == leaf_b_cid.hash_hex() {
                    "leaf_b"
                } else {
                    "unknown"
                };
                println!(
                    "  block {:<8}  cid={}  data={:?}",
                    label,
                    &cid.hash_hex()[..16],
                    String::from_utf8_lossy(&data)
                );
                blocks += 1;
            }
            Err(e) => eprintln!("client error: {}", e),
        }
    }
    println!("received {} blocks", blocks);

    // Sanity: a BlockMessage round-trip via the wire type.
    let bm = BlockMessage {
        id: 1,
        cid: leaf_a_cid.clone(),
        block: b"alpha block".to_vec(),
    };
    let wire = GraphSyncWire::from(GraphSyncMessage::Block(bm));
    let back = GraphSyncWire::decode(&wire.encode().unwrap()).unwrap();
    assert!(matches!(back, GraphSyncWire::Block { .. }));
    println!("wire roundtrip ok");

    server_listener.abort();
    client_listener.abort();
    println!("done");
}
