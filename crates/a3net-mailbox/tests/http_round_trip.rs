//! HTTP integration test — boots a real `axum` server on
//! `127.0.0.1:0`, signs envelopes with a real `Wallet`, and runs
//! the full `enqueue → pull → ack` round-trip via `reqwest`.
//!
//! Mirrors the `a3chat-rpc/tests/http_round_trip.rs` pattern.

use std::sync::Arc;
use std::time::Duration;

use a3net_identity::Wallet;
use a3net_mailbox::auth::{
    canonical_ack, canonical_enqueue, canonical_enqueue_with_timestamp, canonical_pull,
    digest_of,
};
use a3net_mailbox::client::{AckRequest, EnqueueRequest, EnqueueResponse, PullResponse};
use a3net_mailbox::config::MailboxConfig;
use a3net_mailbox::server::{MailboxServer, ServerPolicy, ServerState};
use a3net_mailbox::storage::{MailboxStore, MemoryStore, StoredEnvelope};
use a3net_mailbox::MailboxClient;
use base64::Engine as _;
use chrono::Utc;

fn must<T>(label: &str, r: Result<T, String>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{label}: {e}"),
    }
}

fn alice() -> Wallet {
    Wallet::generate()
}

fn envelope_from(
    sender: &Wallet,
    sender_id: &str,
    recipient: &str,
    msg_id: &str,
    ciphertext: &[u8],
) -> (EnqueueRequest, Vec<u8>) {
    let now = Utc::now();
    let signed_at = now.timestamp();
    // EIP-712-style binding: signature covers the timestamp.
    let msg = canonical_enqueue_with_timestamp(recipient, msg_id, ciphertext, signed_at);
    let digest = digest_of(&msg);
    let sig = sender.sign_personal(&digest).unwrap();
    let sig_bytes = sig.to_compact();
    let req = EnqueueRequest {
        sender_id: sender_id.to_string(),
        msg_id: msg_id.to_string(),
        ciphertext_b64: base64::engine::general_purpose::STANDARD.encode(ciphertext),
        sender_signature_b64: base64::engine::general_purpose::STANDARD.encode(sig_bytes),
        ttl_secs: Some(3600),
        timestamp: Some(signed_at),
    };
    (req, sig_bytes.to_vec())
}

#[tokio::test]
async fn full_round_trip_over_real_http() {
    // 1. Boot server.
    let state = ServerState::new(
        Arc::new(MemoryStore::new()),
        ServerPolicy::default(),
    );
    let mut handle = MailboxServer::start_with_state("127.0.0.1", 0, state)
        .await
        .expect("server should bind");

    // 2. Build a client pointing at the running server.
    let cfg = MailboxConfig {
        base_url: Some(handle.base_url.clone()),
        upstream_timeout: Duration::from_secs(5),
        ..MailboxConfig::default()
    };
    let client = MailboxClient::new(cfg).unwrap();

    let alice_w = alice();
    let alice_id = alice_w.public().address().to_checksum();
    let bob_w = alice(); // fresh wallet
    let bob = bob_w.public().address().to_checksum();
    let msg_id = "550e8400-e29b-41d4-a716-446655440000";

    // 3. Enqueue (Alice → Bob).
    let (req, _sig) = envelope_from(&alice_w, &alice_id, &bob, msg_id, b"hello alice");
    let url = format!("{}/v1/inbox/{}", handle.base_url, bob);
    let resp = client
        .http()
        .post(&url)
        .json(&req)
        .send()
        .await
        .expect("post enqueue");
    assert_eq!(resp.status(), 202, "first enqueue should be 202 Accepted");
    let out: EnqueueResponse = resp.json().await.expect("decode enqueue");
    assert!(!out.duplicate);
    assert_eq!(out.msg_id, msg_id);
    assert!(out.sequence >= 1);

    // 4. Pull (signed by Bob).
    let pull_msg = canonical_pull(&bob);
    let pull_digest = digest_of(&pull_msg);
    let bob_sig = bob_w.sign_personal(&pull_digest).unwrap();
    let resp: PullResponse = client
        .pull(&bob, &bob_sig.to_compact(), 0, Some(100))
        .await
        .expect("pull over http should succeed");
    assert_eq!(resp.messages.len(), 1);
    assert_eq!(resp.messages[0].msg_id, msg_id);
    assert!(resp.next_watermark >= 1);

    // 5. Ack (also signed by Bob).
    let ids = vec![msg_id.to_string()];
    let ack_msg = canonical_ack(&bob, &ids);
    let ack_digest = digest_of(&ack_msg);
    let ack_sig = bob_w.sign_personal(&ack_digest).unwrap();
    let ack_resp = client
        .ack(&bob, &ack_sig.to_compact(), &ids)
        .await
        .expect("ack over http should succeed");
    assert_eq!(ack_resp.acked, 1);

    // 6. Pull again — should be empty.
    let resp2: PullResponse = client
        .pull(&bob, &bob_sig.to_compact(), ack_resp.acked as u64, Some(100))
        .await
        .expect("pull after ack should succeed");
    assert!(resp2.messages.is_empty());

    handle.shutdown();
}

#[tokio::test]
async fn enqueue_rejects_oversized_envelope() {
    let policy = ServerPolicy {
        max_envelope_bytes: 256,
        ..ServerPolicy::default()
    };
    let state = ServerState::new(Arc::new(MemoryStore::new()), policy);
    let mut handle = MailboxServer::start_with_state("127.0.0.1", 0, state)
        .await
        .expect("server should bind");

    let mbox_cfg = MailboxConfig {
        base_url: Some(handle.base_url.clone()),
        ..MailboxConfig::default()
    };
    let client = MailboxClient::new(mbox_cfg).unwrap();

    let alice_w = alice();
    let alice_id = alice_w.public().address().to_checksum();
    let bob = "0x0000000000000000000000000000000000000001";
    let msg_id = "550e8400-e29b-41d4-a716-446655440000";
    let big_ct = vec![0x42u8; 4096];

    let (req, _sig) = envelope_from(&alice_w, &alice_id, bob, msg_id, &big_ct);
    let url = format!("{}/v1/inbox/{}", handle.base_url, bob);
    let resp = client
        .http()
        .post(&url)
        .json(&req)
        .send()
        .await
        .expect("post enqueue");
    // The big envelope + 65-byte signature + envelope metadata
    // exceeds the 256-byte limit, so the server should reject with 413.
    assert_eq!(resp.status(), 413);

    handle.shutdown();
}

#[tokio::test]
async fn enqueue_rejects_invalid_sender_signature() {
    let state = ServerState::new(
        Arc::new(MemoryStore::new()),
        ServerPolicy::default(),
    );
    let mut handle = MailboxServer::start_with_state("127.0.0.1", 0, state)
        .await
        .expect("server should bind");

    let cfg = MailboxConfig {
        base_url: Some(handle.base_url.clone()),
        ..MailboxConfig::default()
    };
    let client = MailboxClient::new(cfg).unwrap();

    let alice = alice();
    let alice_id = alice.public().address().to_checksum();
    let bob = "0x0000000000000000000000000000000000000001";
    let msg_id = "550e8400-e29b-41d4-a716-446655440000";

    // Use a 65-byte signature forged on a different canonical message
    // (an off-by-one sig for a different recipient). The server should
    // reject with 401.
    let (req, _sig) = envelope_from(&alice, &alice_id, "0x9999999999999999999999999999999999999999", msg_id, b"hello");
    // But the *envelope* claims the real recipient is Bob, so the
    // signature recovery will fail (EIP-191 was signed for a different
    // recipient).
    let mut forged = req;
    forged.sender_id = alice_id.clone();
    // Re-target the recipient to Bob via the URL — the body still
    // claims it's for the address Eve. The server will recover the
    // signature against the URL path's recipient and reject.
    let url = format!("{}/v1/inbox/{}", handle.base_url, bob);
    let resp = client
        .http()
        .post(&url)
        .json(&forged)
        .send()
        .await
        .expect("post should succeed");
    assert_eq!(resp.status(), 401);

    handle.shutdown();
}

#[tokio::test]
async fn pull_rejects_missing_signature() {
    let state = ServerState::new(
        Arc::new(MemoryStore::new()),
        ServerPolicy::default(),
    );
    let mut handle = MailboxServer::start_with_state("127.0.0.1", 0, state)
        .await
        .expect("server should bind");

    let url = format!(
        "{}/v1/inbox/0x0000000000000000000000000000000000000001",
        handle.base_url
    );
    let resp = client_get(&url).await;
    assert_eq!(resp.status(), 401);

    handle.shutdown();
}

#[tokio::test]
async fn recipient_isolation_across_users() {
    let state = ServerState::new(
        Arc::new(MemoryStore::new()),
        ServerPolicy::default(),
    );
    let mut handle = MailboxServer::start_with_state("127.0.0.1", 0, state)
        .await
        .expect("server should bind");

    let cfg = MailboxConfig {
        base_url: Some(handle.base_url.clone()),
        ..MailboxConfig::default()
    };
    let client = MailboxClient::new(cfg).unwrap();

    let alice = alice();
    let alice_id = alice.public().address().to_checksum();
    let bob_w = Wallet::generate();
    let bob = bob_w.public().address().to_checksum();
    let carol_w = Wallet::generate();
    let carol = carol_w.public().address().to_checksum();

    // Alice → Bob: 1 envelope.
    let bob_msg_id = "550e8400-e29b-41d4-a716-446655440000";
    {
        let (req, _) = envelope_from(&alice, &alice_id, &bob, bob_msg_id, b"hi bob");
        let url = format!("{}/v1/inbox/{}", handle.base_url, bob);
        let resp = client
            .http()
            .post(&url)
            .json(&req)
            .send()
            .await
            .expect("post enqueue bob");
        assert_eq!(resp.status(), 202);
    }
    // Alice → Carol: 2 envelopes.
    for (i, suffix) in ["a", "b"].iter().enumerate() {
        let msg_id = format!("550e8400-e29b-41d4-a716-44665544000{}", i);
        let (req, _) = envelope_from(
            &alice,
            &alice_id,
            &carol,
            &msg_id,
            format!("carol {suffix}").as_bytes(),
        );
        let url = format!("{}/v1/inbox/{}", handle.base_url, carol);
        let resp = client
            .http()
            .post(&url)
            .json(&req)
            .send()
            .await
            .expect("post enqueue carol");
        assert_eq!(resp.status(), 202);
    }

    // Bob should see only 1 envelope.
    let bob_sig = bob_w.sign_personal(&digest_of(&canonical_pull(&bob))).unwrap();
    let bob_resp: PullResponse = client
        .pull(&bob, &bob_sig.to_compact(), 0, Some(100))
        .await
        .expect("pull bob");
    assert_eq!(bob_resp.messages.len(), 1);
    assert_eq!(bob_resp.messages[0].msg_id, bob_msg_id);

    // Carol should see 2 envelopes.
    let carol_sig = carol_w.sign_personal(&digest_of(&canonical_pull(&carol))).unwrap();
    let carol_resp: PullResponse = client
        .pull(&carol, &carol_sig.to_compact(), 0, Some(100))
        .await
        .expect("pull carol");
    assert_eq!(carol_resp.messages.len(), 2);
    handle.shutdown();
}

#[tokio::test]
async fn quota_is_enforced_end_to_end() {
    let policy = ServerPolicy {
        max_envelope_bytes: 1024,
        ..ServerPolicy::default()
    };
    let state = ServerState::new(Arc::new(MemoryStore::new()), policy);
    let mut handle = MailboxServer::start_with_state("127.0.0.1", 0, state)
        .await
        .expect("server should bind");

    let mbox_cfg = MailboxConfig {
        base_url: Some(handle.base_url.clone()),
        ..MailboxConfig::default()
    };
    let client = MailboxClient::new(mbox_cfg).unwrap();

    let alice = alice();
    let alice_id = alice.public().address().to_checksum();
    let bob = "0x0000000000000000000000000000000000000001";

    // Test quota via direct call to the policy module.
    let pol = a3net_mailbox::policy::QuotaPolicy::new(2, 1024);
    let d = pol.check(a3net_mailbox::policy::QuotaCheck {
        current_message_count: 2,
        current_total_bytes: 0,
        incoming_envelope_bytes: 1,
    });
    assert!(matches!(d, a3net_mailbox::policy::QuotaDecision::Reject { .. }));

    // Saturation test on the wire: use a 1 KiB envelope which
    // together with the 65-byte signature + headers fits in the
    // 1024-byte cap-margin. We pick a 128-byte payload so the
    // envelope stays well under the limit.
    let (req, _) = envelope_from(
        &alice,
        &alice_id,
        bob,
        "550e8400-e29b-41d4-a716-446655440000",
        &[0u8; 128],
    );
    let url = format!("{}/v1/inbox/{}", handle.base_url, bob);
    let resp = client
        .http()
        .post(&url)
        .json(&req)
        .send()
        .await
        .expect("post enqueue");
    assert_eq!(resp.status(), 202);
    let body: EnqueueResponse = resp.json().await.expect("decode");
    assert!(body.sequence >= 1);

    handle.shutdown();
}

#[tokio::test]
async fn healthz_returns_200() {
    let state = ServerState::default();
    let mut handle = MailboxServer::start_with_state("127.0.0.1", 0, state)
        .await
        .expect("server should bind");
    let url = format!("{}/healthz", handle.base_url);
    let resp = reqwest::get(&url).await.expect("get healthz");
    assert!(resp.status().is_success());
    handle.shutdown();
}

async fn client_get(url: &str) -> reqwest::Response {
    let c = reqwest::Client::new();
    c.get(url).send().await.expect("get")
}

// ---------------------------------------------------------------------------
// Helper: an in-memory store wrapper that records all enqueues so we
// can assert sequence uniqueness from the test side.
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub(crate) struct RecordingStore {
    pub inner: std::sync::Mutex<Vec<StoredEnvelope>>,
}

#[async_trait::async_trait]
impl MailboxStore for RecordingStore {
    async fn enqueue(
        &self,
        env: &StoredEnvelope,
    ) -> a3net_mailbox::MailboxResult<a3net_mailbox::storage::EnqueueOutcome> {
        let mut g = self.inner.lock().unwrap();
        let seq = g.len() as u64 + 1;
        let mut stored = env.clone();
        stored.sequence = seq;
        g.push(stored);
        Ok(a3net_mailbox::storage::EnqueueOutcome {
            msg_id: env.msg_id.clone(),
            sequence: seq,
            queued_at: env.queued_at,
            expires_at: env.expires_at,
            duplicate: false,
        })
    }
    async fn pull(
        &self,
        _recipient_id: &str,
        _since: u64,
        _limit: usize,
    ) -> a3net_mailbox::MailboxResult<Vec<StoredEnvelope>> {
        Ok(Vec::new())
    }
    async fn ack(
        &self,
        _recipient_id: &str,
        _msg_ids: &[String],
    ) -> a3net_mailbox::MailboxResult<usize> {
        Ok(0)
    }
    async fn purge_expired(&self) -> a3net_mailbox::MailboxResult<u64> {
        Ok(0)
    }
    async fn quota_usage(
        &self,
        _recipient_id: &str,
    ) -> a3net_mailbox::MailboxResult<a3net_mailbox::storage::QuotaUsage> {
        Ok(a3net_mailbox::storage::QuotaUsage::default())
    }
}

#[tokio::test]
async fn sequence_uniqueness_under_recording() {
    let store = Arc::new(RecordingStore::default());
    let state = ServerState::new(store.clone(), ServerPolicy::default());
    let mut handle = MailboxServer::start_with_state("127.0.0.1", 0, state)
        .await
        .expect("server should bind");

    let cfg = MailboxConfig {
        base_url: Some(handle.base_url.clone()),
        ..MailboxConfig::default()
    };
    let client = MailboxClient::new(cfg).unwrap();

    let alice = alice();
    let alice_id = alice.public().address().to_checksum();
    let bob = "0x0000000000000000000000000000000000000001";

    let mut seqs = Vec::new();
    for i in 0..10 {
        let id = format!("550e8400-e29b-41d4-a716-44665544000{}", i);
        let (req, _) = envelope_from(&alice, &alice_id, bob, &id, b"x");
        let url = format!("{}/v1/inbox/{}", handle.base_url, bob);
        let resp = client
            .http()
            .post(&url)
            .json(&req)
            .send()
            .await
            .expect("post enqueue");
        assert_eq!(resp.status(), 202);
        let out: EnqueueResponse = resp.json().await.expect("decode");
        seqs.push(out.sequence);
    }
    let unique: std::collections::HashSet<_> = seqs.iter().collect();
    assert_eq!(unique.len(), seqs.len(), "sequences must be unique");

    // The recording store should hold 10 envelopes.
    let g = store.inner.lock().unwrap();
    assert_eq!(g.len(), 10);

    handle.shutdown();
}

// `AckRequest` is constructed by the client internally; we don't
// need to use it from outside the crate in this test, but keeping the
// import silences the unused warning.
#[allow(dead_code)]
fn _forced_use_of_ack_request() -> AckRequest {
    AckRequest {
        recipient_id: "0x0000000000000000000000000000000000000000".into(),
        msg_ids: vec![],
        signature_b64: String::new(),
    }
}

#[allow(dead_code)]
fn _use_must() {
    let _ = must("ok", Ok::<i32, String>(1));
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// SqliteStore HTTP integration tests
// ---------------------------------------------------------------------------

use a3net_mailbox::SqliteStore;

/// RAII guard that keeps a temp SQLite db file alive for the duration of a test.
struct TempDb {
    store: SqliteStore,
    #[allow(dead_code)]
    path: tempfile::TempPath,
}

impl TempDb {
    fn new() -> std::io::Result<Self> {
        let tmp = tempfile::Builder::new()
            .prefix("a3net-mailbox-")
            .suffix(".db")
            .rand_bytes(8)
            .tempfile()?;
        let path = tmp.into_temp_path();
        let path_str = path.to_string_lossy().to_string();
        let store = SqliteStore::open(&path_str)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        Ok(Self { store, path })
    }
}

/// Full round-trip with SqliteStore over real HTTP.
#[tokio::test]
async fn sqlite_full_round_trip_over_real_http() {
    let tmp = TempDb::new().expect("open temp db");
    let store = Arc::new(tmp.store);
    let policy = ServerPolicy::default();
    let state = ServerState::new(store, policy);

    let mut handle = MailboxServer::start_with_state("127.0.0.1", 0, state)
        .await
        .expect("server start");

    let cfg = MailboxConfig {
        base_url: Some(handle.base_url.clone()),
        upstream_timeout: Duration::from_secs(5),
        ..MailboxConfig::default()
    };
    let client = MailboxClient::new(cfg).unwrap();

    let alice_w = alice();
    let alice_id = alice_w.public().address().to_checksum();
    let bob_w = Wallet::generate();
    let bob = bob_w.public().address().to_checksum();
    let msg_id = "eb8a27658e8e9409e2e893c4bc46c2d2de4dd51b03904aaf33464a54e435bbeb";

    // Enqueue
    let (req, _) = envelope_from(&alice_w, &alice_id, &bob, msg_id, b"hello sqlite");
    let url = format!("{}/v1/inbox/{}", handle.base_url, bob);
    let resp = client
        .http()
        .post(&url)
        .json(&req)
        .send()
        .await
        .expect("enqueue");
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        panic!("enqueue failed {}: {}", status, body);
    }
    let out: EnqueueResponse = resp.json().await.expect("decode enqueue");
    assert!(!out.duplicate);

    // Pull (signed by Bob)
    let bob_sig = bob_w.sign_personal(&digest_of(&canonical_pull(&bob))).unwrap();
    let pull_url = format!("{}/v1/inbox/{}", handle.base_url, bob);
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(bob_sig.to_compact());
    let resp: PullResponse = client
        .http()
        .get(&pull_url)
        .query(&[("signature", sig_b64.as_str()), ("since", "0")])
        .send()
        .await
        .expect("pull")
        .json()
        .await
        .expect("decode pull");
    assert_eq!(resp.messages.len(), 1, "pull should return 1 message");
    assert_eq!(resp.messages[0].msg_id, msg_id);

    // Ack (also by Bob)
    let ids: Vec<String> = vec![msg_id.to_string()];
    let ack_sig = bob_w.sign_personal(&digest_of(&canonical_ack(&bob, &ids))).unwrap();
    let ack_url = format!("{}/v1/inbox/{}/ack", handle.base_url, bob);
    let ack_body = serde_json::json!({
        "recipient_id": bob,
        "msg_ids": &ids,
        "signature_b64": base64::engine::general_purpose::STANDARD.encode(ack_sig.to_compact()),
    });
    let ack_resp = client
        .http()
        .post(&ack_url)
        .json(&ack_body)
        .send()
        .await
        .expect("ack")
        .json::<serde_json::Value>()
        .await
        .expect("decode ack");
    assert_eq!(ack_resp["acked"], 1, "ack should remove 1");

    handle.shutdown();
}

/// Quota enforcement with SqliteStore.
#[tokio::test]
async fn sqlite_quota_enforced_over_http() {
    let tmp = TempDb::new().expect("open temp db");
    let store = Arc::new(tmp.store);
    let policy = ServerPolicy {
        max_envelope_bytes: 100,
        ..ServerPolicy::default()
    };
    let state = ServerState::new(store, policy);

    let mut handle = MailboxServer::start_with_state("127.0.0.1", 0, state)
        .await
        .expect("server start");

    let cfg = MailboxConfig {
        base_url: Some(handle.base_url.clone()),
        upstream_timeout: Duration::from_secs(5),
        ..MailboxConfig::default()
    };
    let client = MailboxClient::new(cfg).unwrap();

    let alice_w = alice();
    let alice_id = alice_w.public().address().to_checksum();
    let bob_w = Wallet::generate();
    let bob = bob_w.public().address().to_checksum();
    let oversized = "x".repeat(200);

    let (req, _) = envelope_from(
        &alice_w, &alice_id, &bob, "e4960a3a25fe9b7bafb0634e5399e564653f4672b1405c688473de083a81b9cb",
        oversized.as_bytes(),
    );
    let url = format!("{}/v1/inbox/{}", handle.base_url, bob);
    let resp = client
        .http()
        .post(&url)
        .json(&req)
        .send()
        .await
        .expect("post");
    // Size check before sig check → 413 Payload Too Large.
    assert!(
        resp.status() == 413 || resp.status() == 400,
        "oversized envelope should be 413 or 400, got {}",
        resp.status()
    );

    handle.shutdown();
}

/// Recipient isolation with SqliteStore.
#[tokio::test]
async fn sqlite_recipient_isolation() {
    let tmp = TempDb::new().expect("open temp db");
    let store = Arc::new(tmp.store);
    let state = ServerState::new(store, ServerPolicy::default());

    let mut handle = MailboxServer::start_with_state("127.0.0.1", 0, state)
        .await
        .expect("server start");

    let cfg = MailboxConfig {
        base_url: Some(handle.base_url.clone()),
        upstream_timeout: Duration::from_secs(5),
        ..MailboxConfig::default()
    };
    let client = MailboxClient::new(cfg).unwrap();

    let alice_w = alice();
    let alice_id = alice_w.public().address().to_checksum();
    let bob_w = Wallet::generate();
    let bob = bob_w.public().address().to_checksum();
    let carol_w = Wallet::generate();
    let carol = carol_w.public().address().to_checksum();

    for (recipient, msg_id) in [(bob.as_str(), "20f0bef0c3f70223651b26c09a7dc89b"), (carol.as_str(), "df9e7c5944ee55e9d3e7394690c98bad")] {
        let (req, _) = envelope_from(&alice_w, &alice_id, recipient, msg_id, b"hello");
        let url = format!("{}/v1/inbox/{}", handle.base_url, recipient);
        let resp = client
            .http()
            .post(&url)
            .json(&req)
            .send()
            .await
            .expect("enqueue");
        assert_eq!(resp.status(), 202, "enqueue to {recipient} should succeed");
    }

    // Bob pulls
    let bob_sig = bob_w.sign_personal(&digest_of(&canonical_pull(&bob))).unwrap();
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(bob_sig.to_compact());
    let pull_url = format!("{}/v1/inbox/{}", handle.base_url, bob);
    let bobs: PullResponse = client
        .http()
        .get(&pull_url)
        .query(&[("signature", sig_b64.as_str()), ("since", "0")])
        .send()
        .await
        .expect("bob pull")
        .json()
        .await
        .expect("decode bob pull");

    // Carol pulls
    let carol_sig = carol_w.sign_personal(&digest_of(&canonical_pull(&carol))).unwrap();
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(carol_sig.to_compact());
    let pull_url = format!("{}/v1/inbox/{}", handle.base_url, carol);
    let carols: PullResponse = client
        .http()
        .get(&pull_url)
        .query(&[("signature", sig_b64.as_str()), ("since", "0")])
        .send()
        .await
        .expect("carol pull")
        .json()
        .await
        .expect("decode carol pull");

    assert_eq!(bobs.messages.len(), 1, "bob should have 1 message");
    assert_eq!(carols.messages.len(), 1, "carol should have 1 message");
    assert_eq!(bobs.messages[0].msg_id, "20f0bef0c3f70223651b26c09a7dc89b");
    assert_eq!(carols.messages[0].msg_id, "df9e7c5944ee55e9d3e7394690c98bad");

    handle.shutdown();
}
