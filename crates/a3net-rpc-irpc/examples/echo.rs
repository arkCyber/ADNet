//! `examples/echo.rs` — minimal irpc demo over a *local* channel.
//!
//! Run with:
//!
//! ```sh
//! cd crates/a3net-rpc-irpc
//! cargo run --no-default-features --features local --example echo
//! ```
//!
//! What this demonstrates
//! ----------------------
//! The `echo` protocol contains exactly one of each channel pattern
//! from `service::Protocol` collapsed into a tiny example:
//!
//! | Variant      | Pattern            | Outcome                   |
//! |--------------|--------------------| ------------------------- |
//! | `Ping`       | oneshot            | `Pong { n }`              |
//! | `Reverse`    | rx-streaming        | server folds all chunks, returns reversed joined string |
//! | `Countdown`  | tx-streaming        | server emits 3..0         |
//! | `Chat`       | bidi-streaming      | client says "hi!" twice, server echoes "hi!" twice |
//!
//! It does **not** open a QUIC connection: the irpc client is built
//! in-process over a `tokio::sync::mpsc` channel. This is the
//! canonical "local" mode that irpc exposes, and it lets us verify
//! the derive macro expansion runs cleanly without a network.

use irpc::{
    Client, WithChannels,
    channel::{oneshot, mpsc},
    rpc_requests,
};
use serde::{Deserialize, Serialize};

#[rpc_requests(message = EchoMessage)]
#[derive(Debug, Serialize, Deserialize)]
enum EchoProtocol {
    /// Plain unary call.
    #[rpc(tx = oneshot::Sender<Pong>)]
    #[wrap(Ping)]
    Ping { n: u32 },

    /// Server folds an rx-stream of strings into one reversed reply.
    #[rpc(rx = mpsc::Receiver<String>, tx = oneshot::Sender<String>)]
    #[wrap(Reverse)]
    Reverse { sep: String },

    /// Server emits a tx-stream of decreasing numbers.
    #[rpc(tx = mpsc::Sender<u32>)]
    #[wrap(Countdown)]
    Countdown { from: u32 },

    /// Bidi: client streams greetings, server streams back uppercase.
    #[rpc(tx = mpsc::Sender<String>, rx = mpsc::Receiver<String>)]
    #[wrap(Chat)]
    Chat,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
struct Pong {
    n: u32,
}

fn spawn_server() -> Client<EchoProtocol> {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    tokio::spawn(server_actor(rx));
    irpc::Client::local(tx)
}

async fn server_actor(mut rx: tokio::sync::mpsc::Receiver<EchoMessage>) {
    while let Some(msg) = rx.recv().await {
        match msg {
            EchoMessage::Ping(msg) => {
                let WithChannels { inner, tx, .. } = msg;
                tx.send(Pong { n: inner.n * 2 }).await.ok();
            }
            EchoMessage::Reverse(msg) => {
                let WithChannels {
                    inner,
                    tx,
                    mut rx: stream,
                    ..
                } = msg;
                let mut pieces = Vec::new();
                while let Some(s) = stream.recv().await {
                    pieces.push(s);
                }
                pieces.reverse();
                tx.send(pieces.join(&inner.sep)).await.ok();
            }
            EchoMessage::Countdown(msg) => {
                let WithChannels { inner, tx, .. } = msg;
                let mut n = inner.from;
                while n > 0 {
                    if tx.send(n).await.is_err() {
                        break;
                    }
                    n -= 1;
                }
            }
            EchoMessage::Chat(msg) => {
                let WithChannels { tx, mut rx, .. } = msg;
                tokio::spawn(async move {
                    while let Some(s) = rx.recv().await {
                        let upper = s.to_uppercase();
                        if tx.send(upper).await.is_err() {
                            break;
                        }
                    }
                });
            }
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = spawn_server();

    // 1) oneshot
    let pong = client.rpc(Ping { n: 21 }).await?;
    assert_eq!(pong, Pong { n: 42 });

    // 2) rx-streaming (client feeds server a stream)
    let (mut tx_stream, reply) = client.rpc(Reverse { sep: "-".into() }).await?;
    tx_stream.send("alpha".into()).await?;
    tx_stream.send("beta".into()).await?;
    tx_stream.send("gamma".into()).await?;
    drop(tx_stream);
    let joined = reply.await?;
    assert_eq!(joined, "gamma-beta-alpha");

    // 3) tx-streaming (server feeds client)
    let mut countdown = client.streaming(Countdown { from: 3 }).await?;
    let mut seen = Vec::new();
    while let Some(n) = countdown.recv().await {
        seen.push(n);
    }
    assert_eq!(seen, vec![3, 2, 1]);

    // 4) bidi
    let (mut greeting_tx, mut greeting_rx) = client
        .bidi_streaming(Chat, /* tx_buf = */ 4, /* rx_buf = */ 4)
        .await?;
    greeting_tx.send("hi!".into()).await?;
    greeting_tx.send("there!".into()).await?;
    drop(greeting_tx);
    assert_eq!(greeting_rx.recv().await, Some("HI!".to_string()));
    assert_eq!(greeting_rx.recv().await, Some("THERE!".to_string()));
    assert_eq!(greeting_rx.recv().await, None);

    println!("echo example: all four interaction patterns OK ✓");
    Ok(())
}
