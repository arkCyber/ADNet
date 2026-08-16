// SPDX-License-Identifier: MIT OR Apache-2.0
//
// `chaos.rs` — Chaos / fault-injection tests for `a3net-dht`.
//
// Aerospace note (DO-178C §6.4.4 — robustness under stress):
// these tests inject failures (timeouts, decode errors,
// dropped frames, exhausted capacity, malformed inputs) into
// every public surface and verify the module never panics,
// never deadlocks, and never silently swallows a failure that
// the caller should observe.
//
// Run with: `cargo test -p a3net-dht --lib chaos`

#![cfg(test)]

use std::sync::Arc;
use std::time::Duration;

use a3net_types::NodeId;
use async_trait::async_trait;
use tokio::sync::{mpsc, RwLock};

use crate::bucket::{Contact, RoutingTable};
use crate::handler::{DhtProtocolHandler, PendingRequestError};
use crate::network::{DhtNetworkSender, TransportDhtSender};
use crate::protocol::{DhtCodec, DhtMessageBuilder, DhtWireMessage, RequestId};
use crate::query::{DhtMessageSender, QueryError};
use crate::record::DhtKey;
use crate::store::new_in_memory_store;

// ─────────────────────────────────────────────────────────────────
// Chaos harness — flaky transport that fails on demand
// ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct ChaosTransport {
    behavior: Arc<RwLock<ChaosBehavior>>,
}

#[derive(Clone, Debug)]
enum ChaosBehavior {
    Ok,
    AlwaysFailNetwork(String),
    AlwaysTimeout,
    RandomFail { fail_rate: u8 }, // 0..=100
    Hang,
}

impl ChaosTransport {
    fn new(behavior: ChaosBehavior) -> Self {
        Self {
            behavior: Arc::new(RwLock::new(behavior)),
        }
    }
    async fn set(&self, b: ChaosBehavior) {
        *self.behavior.write().await = b;
    }
}

#[async_trait]
impl TransportDhtSender for ChaosTransport {
    async fn send_to(&self, peer: &NodeId, _data: &[u8]) -> Result<(), QueryError> {
        let b = self.behavior.read().await.clone();
        match b {
            ChaosBehavior::Ok => Ok(()),
            ChaosBehavior::AlwaysFailNetwork(msg) => {
                Err(QueryError::Network(format!("chaos: {msg}")))
            }
            ChaosBehavior::AlwaysTimeout => Err(QueryError::Timeout),
            ChaosBehavior::RandomFail { fail_rate } => {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos();
                if (nanos as u8) < fail_rate {
                    Err(QueryError::Network("chaos random".into()))
                } else {
                    Ok(())
                }
            }
            ChaosBehavior::Hang => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(())
            }
        }
    }
    async fn get_peer_addr(&self, _peer: &NodeId) -> Option<String> {
        Some("127.0.0.1:0".to_string())
    }
}

// ─────────────────────────────────────────────────────────────────
// §6.4.4 — Transport failure modes do not panic
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn find_node_returns_timeout_on_chaos_transport() {
    let local = NodeId::random();
    let peer = NodeId::random();
    let transport = Arc::new(ChaosTransport::new(ChaosBehavior::AlwaysTimeout));
    let rt = Arc::new(RwLock::new(RoutingTable::new(local.clone())));
    let sender = Arc::new(DhtNetworkSender::new(local, transport, rt));
    let key = DhtKey::from_bytes(vec![1u8; 32]);
    let result = sender.find_node(&peer, &key).await;
    assert!(matches!(result, Err(QueryError::Timeout)));
}

#[tokio::test]
async fn find_node_returns_network_error_on_chaos_transport() {
    let local = NodeId::random();
    let peer = NodeId::random();
    let transport = Arc::new(ChaosTransport::new(ChaosBehavior::AlwaysFailNetwork(
        "kaboom".into(),
    )));
    let rt = Arc::new(RwLock::new(RoutingTable::new(local.clone())));
    let sender = Arc::new(DhtNetworkSender::new(local, transport, rt));
    let key = DhtKey::from_bytes(vec![1u8; 32]);
    let result = sender.find_node(&peer, &key).await;
    assert!(matches!(result, Err(QueryError::Network(_))));
}

#[tokio::test]
async fn find_node_handles_random_failures_without_panic() {
    // Aerospace note (DO-178C §6.4.4): randomly injecting
    // 50% transport failures for 100 queries must never panic.
    let local = NodeId::random();
    let peer = NodeId::random();
    let transport = Arc::new(ChaosTransport::new(ChaosBehavior::RandomFail {
        fail_rate: 50,
    }));
    let rt = Arc::new(RwLock::new(RoutingTable::new(local.clone())));
    let sender = Arc::new(DhtNetworkSender::new(local, transport, rt));
    let key = DhtKey::from_bytes(vec![1u8; 32]);
    for _ in 0..100 {
        let _ = sender.find_node(&peer, &key).await;
    }
}

#[tokio::test]
async fn find_node_handles_hang_within_timeout() {
    // A peer that hangs forever must not stall the caller past
    // the configured per-request timeout.
    let local = NodeId::random();
    let peer = NodeId::random();
    let transport = Arc::new(ChaosTransport::new(ChaosBehavior::Hang));
    let rt = Arc::new(RwLock::new(RoutingTable::new(local.clone())));
    let sender = Arc::new(DhtNetworkSender::new(local, transport, rt));
    let key = DhtKey::from_bytes(vec![1u8; 32]);
    // The internal wait_for_response has its own
    // DEFAULT_TIMEOUT=5s; we wrap the call in a 10s budget so
    // even on a slow CI we surface a real regression rather
    // than timing out the test.
    let result =
        tokio::time::timeout(Duration::from_secs(10), sender.find_node(&peer, &key)).await;
    match result {
        Ok(Err(QueryError::Timeout)) => {} // expected
        Ok(Err(e)) => panic!("expected Timeout, got {e:?}"),
        Ok(Ok(_)) => panic!("Hang transport unexpectedly succeeded"),
        Err(_) => panic!("find_node took longer than 10s"),
    }
}

// ─────────────────────────────────────────────────────────────────
// §6.4.2 — Pending-request table saturation reports Err
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn pending_table_saturation_is_observable() {
    // Aerospace note (DO-178C §6.4.2): an attacker that can
    // fill the pending-request table must not be able to hide
    // the resulting saturation. The new API returns
    // `Err(PendingRequestError::TableFull)` so the caller can
    // back off; the old API silently dropped requests.
    let local = NodeId::random();
    let rt = Arc::new(RwLock::new(RoutingTable::new(local.clone())));
    let store = new_in_memory_store();
    let (handler, _rx) = DhtProtocolHandler::new(local, rt, store);

    // Saturate with garbage requests; eventually one must be
    // rejected.
    let mut first_rejection: Option<usize> = None;
    for i in 0..1_000 {
        let (tx, _rx) = mpsc::channel(1);
        let req_id = RequestId(format!("chaos-req-{i}"));
        match handler.register_request(req_id, tx).await {
            Ok(()) => {}
            Err(PendingRequestError::TableFull(_)) => {
                first_rejection = Some(i);
                break;
            }
        }
    }
    assert!(
        first_rejection.is_some(),
        "TableFull must surface before the 1000th request"
    );
}

// ─────────────────────────────────────────────────────────────────
// §6.4.4 — Codec rejects garbage without panic
// ─────────────────────────────────────────────────────────────────

#[test]
fn codec_handles_random_garbage_without_panic() {
    use proptest::prelude::*;
    let _ = proptest! {
        #[test]
        fn prop_decode_garbage(bytes in proptest::collection::vec(any::<u8>(), 0..=4096)) {
            // The decoder MUST return Err for any non-conforming
            // payload, NEVER panic.
            let _ = DhtCodec::decode(&bytes);
        }
    };
}

#[tokio::test]
async fn handler_handles_garbage_frames_without_panic() {
    let local = NodeId::random();
    let rt = Arc::new(RwLock::new(RoutingTable::new(local.clone())));
    let store = new_in_memory_store();
    let (mut handler, _rx) = DhtProtocolHandler::new(local, rt, store);
    // Feed a fistful of garbage frames; the handler must not
    // panic and must return None (no response for invalid
    // input).
    for len in [0, 1, 2, 7, 64, 1024, 1 << 14] {
        let garbage = vec![0xAAu8; len];
        let result = handler.handle_frame(&garbage).await;
        assert!(result.is_none(), "garbage frame len={len} must not produce a response");
    }
}

// ─────────────────────────────────────────────────────────────────
// §6.4.4 — DhtMessageSender trait impl: every code path under chaos
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn dht_message_sender_impl_handles_chaos() {
    let local = NodeId::random();
    let peer_id = NodeId::random();
    let transport = Arc::new(ChaosTransport::new(ChaosBehavior::AlwaysFailNetwork(
        "msg-sender-chaos".into(),
    )));
    let rt = Arc::new(RwLock::new(RoutingTable::new(local.clone())));
    let sender = Arc::new(DhtNetworkSender::new(local.clone(), transport, rt));

    let contact = Contact::new(peer_id, "127.0.0.1:8080".parse().unwrap());
    let key = DhtKey::from_bytes(vec![1u8; 32]);

    // send_find_node must surface the network error rather
    // than panicking.
    let res = sender.send_find_node(&contact, &key, "req-1").await;
    assert!(matches!(res, Err(QueryError::Network(_))));
    // send_get_providers — same expectation.
    let res = sender.send_get_providers(&contact, &key, "req-2").await;
    assert!(matches!(res, Err(QueryError::Network(_))));
    // send_add_provider — must return Err on transport failure.
    let rec = crate::record::ProviderRecord::new(
        key.clone(),
        local.clone(),
        "127.0.0.1:0".into(),
    );
    let res = sender.send_add_provider(&contact, &key, &rec).await;
    assert!(matches!(res, Err(QueryError::Network(_))));
    // send_put_value — same.
    let value = crate::record::DhtValue {
        data: vec![1, 2, 3],
        timestamp: 1,
        ttl_secs: 60,
    };
    let res = sender.send_put_value(&contact, &key, &value, "req-3").await;
    assert!(matches!(res, Err(QueryError::Network(_))));
}

// ─────────────────────────────────────────────────────────────────
// §6.4.4 — Behaviour transitions (Ok ↔ Fail) under load
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn transport_behavior_transitions_under_load() {
    // Toggle the transport between Ok and Fail in mid-stream.
    // The sender must observe the transition and react
    // accordingly.
    let local = NodeId::random();
    let peer = NodeId::random();
    let transport = Arc::new(ChaosTransport::new(ChaosBehavior::Ok));
    let rt = Arc::new(RwLock::new(RoutingTable::new(local.clone())));
    let sender = Arc::new(DhtNetworkSender::new(local, transport.clone(), rt));
    let key = DhtKey::from_bytes(vec![1u8; 32]);
    // First call should be Ok (the transport is in Ok mode but
    // find_node still needs a response, so it times out).
    let _ = sender.find_node(&peer, &key).await;
    // Switch to AlwaysFailNetwork and confirm error surfaces.
    transport
        .set(ChaosBehavior::AlwaysFailNetwork("after toggle".into()))
        .await;
    let res = sender.find_node(&peer, &key).await;
    assert!(matches!(res, Err(QueryError::Network(_))));
}

// ─────────────────────────────────────────────────────────────────
// §6.4.2 — ProviderRecord with adversarial inputs doesn't panic
// ─────────────────────────────────────────────────────────────────

#[test]
fn provider_record_handles_adversarial_strings() {
    // Adversarial provider_addr values that used to confuse
    // the colon-delimited signing format (e.g. `":0:1:2:3:4"`,
    // empty string, very long string, embedded NULs) must
    // round-trip safely through sign/verify.
    struct IdentitySigner;
    impl crate::record::Signer for IdentitySigner {
        fn sign(&self, data: &[u8]) -> Vec<u8> { data.to_vec() }
    }
    struct IdentityVerifier;
    impl crate::record::Verifier for IdentityVerifier {
        fn verify(&self, data: &[u8], sig: &[u8]) -> bool { data == sig }
    }

    let adversarial = [
        "",
        ":",
        ":::",
        "127.0.0.1:8080:fake",
        "a:b:c:d:e:f:g:h",
        &"x".repeat(10_000),
        "\0\0\0",
        "🦀🚀",
        "very-long-address-with-many-colons:1:2:3:4:5:6:7:8:9:10:11:12",
    ];
    for addr in adversarial {
        let mut rec = crate::record::ProviderRecord::new(
            DhtKey::from_bytes(vec![1u8; 32]),
            NodeId::random(),
            addr.to_string(),
        );
        rec.sign(&IdentitySigner);
        assert!(
            rec.verify_signature(&IdentityVerifier),
            "verify failed for adversarial addr {addr:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────
// §6.4.4 — DhtMessageBuilder accepts degenerate inputs
// ─────────────────────────────────────────────────────────────────

#[test]
fn message_builder_accepts_degenerate_inputs() {
    let builder = DhtMessageBuilder::new(NodeId::random());
    // Empty key
    let m = builder.find_node(Vec::new());
    assert!(DhtCodec::encode(&m).is_ok());
    // Huge key (1 MiB — matches MAX_VALUE_SIZE)
    let m = builder.find_node(vec![0xAB; 1024 * 1024]);
    assert!(DhtCodec::encode(&m).is_ok());
    // Empty addr list
    let m = builder.add_provider(Vec::new(), NodeId::random(), Vec::new(), 60);
    assert!(DhtCodec::encode(&m).is_ok());
}