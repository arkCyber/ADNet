//! Local mailbox demo — stand up a mailbox server on `127.0.0.1:18791`,
//! enqueue one envelope from Alice to Bob, then have Bob pull and ack.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p a3net-mailbox --example mailbox_local_demo
//! ```
//!
//! The demo uses fresh random wallets each run; no persistent state is
//! touched.

use a3net_identity::Wallet;
use a3net_mailbox::auth::{canonical_ack, canonical_enqueue, canonical_pull, digest_of};
use a3net_mailbox::client::MailboxClient;
use a3net_mailbox::config::MailboxConfig;
use a3net_mailbox::server::{MailboxServer, ServerPolicy, ServerState};
use a3net_mailbox::storage::MemoryStore;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Start the mailbox server on a random local port.
    let state = ServerState::new(Arc::new(MemoryStore::new()), ServerPolicy::default());
    let mut server = MailboxServer::start_with_state("127.0.0.1", 0, state)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("mailbox up at {}", server.base_url);

    // 2. Configure a client pointing at the running server.
    let cfg = MailboxConfig {
        base_url: Some(server.base_url.clone()),
        upstream_timeout: Duration::from_secs(5),
        ..MailboxConfig::default()
    };
    let client = MailboxClient::new(cfg)?;

    // 3. Two fresh wallets.
    let alice = Wallet::generate();
    let bob = Wallet::generate();
    let alice_id = alice.public().address().to_checksum();
    let bob_id = bob.public().address().to_checksum();
    println!("alice = {alice_id}");
    println!("bob   = {bob_id}");

    // 4. Alice enqueues an envelope to Bob.
    let msg_id = "550e8400-e29b-41d4-a716-446655440000";
    let plaintext = b"hello from alice";
    // Sign the canonical enqueue message.
    let msg = canonical_enqueue(&bob_id, msg_id, plaintext);
    let digest = digest_of(&msg);
    let sig = alice.sign_personal(&digest).unwrap();
    let sig_bytes = sig.to_compact();

    let outcome = client
        .enqueue(&bob_id, &alice_id, msg_id, plaintext, &sig_bytes, Some(3600))
        .await?;
    println!(
        "alice → bob: enqueued msg {} at sequence {} (duplicate = {})",
        outcome.msg_id, outcome.sequence, outcome.duplicate
    );

    // 5. Bob pulls his inbox.
    let pull_msg = canonical_pull(&bob_id);
    let pull_digest = digest_of(&pull_msg);
    let bob_sig = bob.sign_personal(&pull_digest).unwrap();
    let pulled = client
        .pull(&bob_id, &bob_sig.to_compact(), 0, Some(100))
        .await?;
    println!("bob pulled {} message(s), next_watermark = {}", pulled.messages.len(), pulled.next_watermark);
    for env in &pulled.messages {
        println!(
            "  msg_id={} from={} ciphertext={}B at seq={}",
            env.msg_id,
            env.sender_id,
            env.ciphertext.len(),
            env.sequence,
        );
    }

    // 6. Bob acks what he pulled.
    let ids: Vec<String> = pulled.messages.iter().map(|e| e.msg_id.clone()).collect();
    if ids.is_empty() {
        println!("bob has nothing to ack");
    } else {
        let ack_msg = canonical_ack(&bob_id, &ids);
        let ack_digest = digest_of(&ack_msg);
        let ack_sig = bob.sign_personal(&ack_digest).unwrap();
        let ack_resp = client.ack(&bob_id, &ack_sig.to_compact(), &ids).await?;
        println!("bob acked {} message(s)", ack_resp.acked);
    }

    // 7. Bob pulls again — should be empty.
    let pulled2 = client
        .pull(&bob_id, &bob_sig.to_compact(), pulled.next_watermark, Some(100))
        .await?;
    println!(
        "bob pulled {} message(s) after ack (should be 0)",
        pulled2.messages.len()
    );

    server.shutdown();
    println!("mailbox shut down cleanly");
    Ok(())
}
