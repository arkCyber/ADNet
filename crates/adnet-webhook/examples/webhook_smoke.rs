//! End-to-end webhook smoke test.
//!
//! Spawns a `tokio::net::TcpListener` that accepts one HTTP POST,
//! verifies the HMAC-SHA256 signature in the `X-Adnet-Signature`
//! header, and then 200s. The `WebhookSink` is then asked to
//! deliver an `AdnetEvent::Announcement` to that listener.
//!
//! Run with: `cargo run -p adnet-webhook --example webhook_smoke`.

use std::sync::Arc;
use std::time::Duration;

use adnet_webhook::{
    AdnetEvent, EndpointConfig, EventSink, WebhookSink, sign_body,
};
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    println!("receiver listening on http://{addr}/hook");

    let received: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let received_clone = Arc::clone(&received);

    let server = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = Vec::with_capacity(4096);
            stream.read_to_end(&mut buf).await.ok();
            let text = String::from_utf8_lossy(&buf).into_owned();
            // Extract body (everything after `\r\n\r\n`).
            let body = text
                .split_once("\r\n\r\n")
                .map(|(_, b)| b.to_string())
                .unwrap_or_default();
            let sig = text
                .lines()
                .find_map(|l| l.strip_prefix("X-Adnet-Signature: "))
                .unwrap_or("")
                .to_string();
            *received_clone.lock() = Some((sig, body));
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await;
            let _ = stream.shutdown().await;
        }
    });

    let secret = b"topsecret".to_vec();
    let sink = WebhookSink::new(vec![EndpointConfig {
        url: format!("http://{addr}/hook"),
        secret: secret.clone(),
        room_filter: None,
        request_timeout: Duration::from_secs(2),
    }]);

    let payload = serde_json::json!({
        "room_id": "lobby",
        "node_id": "abc123",
        "title": "hello world",
    });
    let event = AdnetEvent::Announcement { payload };
    sink.deliver(&event, "delivery-1").await?;

    // Give the receiver task a moment to finish.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = server.await;

    let received = received.lock().take();
    let (sig, body) = received.expect("receiver should have got the request");
    println!("received body: {body}");
    println!("received sig : {sig}");
    let expected = sign_body(&secret, body.as_bytes());
    assert_eq!(sig, expected, "HMAC mismatch");
    println!("HMAC verified OK");
    Ok(())
}