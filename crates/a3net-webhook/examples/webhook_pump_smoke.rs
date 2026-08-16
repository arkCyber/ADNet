//! End-to-end webhook pump smoke test.
//!
//! Constructs a `tokio::sync::broadcast::channel<Announcement>`
//! (the same shape `a3net_gossip::GossipBus::subscribe` returns),
//! spawns a `webhook::pump` task, and publishes a single
//! synthetic announcement. A local TCP listener accepts the
//! resulting HTTP POST, parses the body and headers, and
//! verifies the HMAC-SHA256 signature using the shared secret.
//!
//! This is the closest reproduction of the production hot path
//! we can run without spinning up the full gossip stack.
//!
//! Run with: `cargo run -p a3net-webhook --example webhook_pump_smoke`.

use std::sync::Arc;
use std::time::Duration;

use a3net_types::{Announcement, CdnContentKind, ContentHash, NodeId, RoomId};
use a3net_webhook::{pump, EndpointConfig, WebhookSink, sign_body};
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Receiver.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    println!("receiver listening on http://{addr}/hook");

    let received: Arc<Mutex<Option<(String, String, String)>>> = Arc::new(Mutex::new(None));
    let received_clone = Arc::clone(&received);

    let server = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = Vec::with_capacity(4096);
            stream.read_to_end(&mut buf).await.ok();
            let text = String::from_utf8_lossy(&buf).into_owned();
            let body = text
                .split_once("\r\n\r\n")
                .map(|(_, b)| b.to_string())
                .unwrap_or_default();
            let delivery = text
                .lines()
                .find_map(|l| l.strip_prefix("X-Adnet-Delivery: "))
                .unwrap_or("")
                .to_string();
            let sig = text
                .lines()
                .find_map(|l| l.strip_prefix("X-Adnet-Signature: "))
                .unwrap_or("")
                .to_string();
            *received_clone.lock() = Some((delivery, sig, body));
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await;
            let _ = stream.shutdown().await;
        }
    });

    // 2. Sink + pump.
    let secret = b"pump-smoke-secret".to_vec();
    let sink = Arc::new(WebhookSink::new(vec![EndpointConfig {
        url: format!("http://{addr}/hook"),
        secret: secret.clone(),
        room_filter: None,
        request_timeout: Duration::from_secs(2),
    }]));

    let (tx, rx) = broadcast::channel::<Announcement>(16);
    let handle = pump::run(Arc::clone(&sink), rx);

    // 3. Publish a synthetic announcement with a known
    //    `message_id`. The pump should turn it into an HTTP POST
    //    whose `X-Adnet-Delivery` matches this id and whose
    //    body contains the announcement fields verbatim.
    let announcement = Announcement {
        room_id: RoomId::from("pump-smoke-room"),
        content_hash: ContentHash::from_bytes(b"hello world"),
        node_id: NodeId::random(),
        title: "pump smoke".into(),
        kind: CdnContentKind::GenericFile,
        size_bytes: 11,
        mime_type: Some("text/plain".into()),
        source_url: None,
        ticket: None,
        timestamp: chrono::Utc::now(),
        message_id: Some("pump-smoke-delivery-1".into()),
        ttl_secs: None,
        signer: None,
        signature: None,
    };
    tx.send(announcement)?;

    // Give the pump time to drain.
    tokio::time::sleep(Duration::from_millis(150)).await;
    drop(tx);
    let delivered = handle.await??;
    println!("pump delivered {delivered} event(s)");
    let _ = server.await;

    // 4. Verify the receiver got the right payload + signature.
    let (delivery, sig, body) = received
        .lock()
        .take()
        .expect("receiver should have got the request");
    println!("delivery_id : {delivery}");
    println!("signature   : {sig}");
    println!("body        : {body}");

    let expected_sig = sign_body(&secret, body.as_bytes());
    assert_eq!(sig, expected_sig, "HMAC mismatch");
    assert_eq!(delivery, "pump-smoke-delivery-1");
    assert!(body.contains("pump-smoke-room"));
    assert!(body.contains("\"title\":\"pump smoke\""));
    println!("pump round-trip OK (HMAC + body + delivery_id verified)");
    Ok(())
}