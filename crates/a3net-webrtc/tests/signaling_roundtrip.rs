//! Integration tests for the `a3net-webrtc::signaling` module:
//!
//! - `InMemorySignaling::publish/subscribe` round-trip
//! - `SignalingPayload` JSON round-trip
//!
//! These exercise the public surface of the `signaling` module without
//! pulling in the heavy `webrtc-rs` runtime. They live in `tests/` so
//! that the same code path is verified both from the consumer side and
//! from inside the crate (the unit tests in `src/signaling.rs` exercise
//! the same paths but have access to private items).

#![cfg(feature = "webrtc")]

use a3net_webrtc::signaling::{
    InMemorySignaling, SignalingChannel, SignalingKind, SignalingPayload,
};
use a3net_types::NodeId;

#[tokio::test]
async fn publish_then_subscribe_delivers_offer() {
    let sig = InMemorySignaling::new();
    let alice = NodeId::random();

    let mut rx = sig
        .subscribe(alice.clone(), SignalingKind::WebRtcOffer)
        .await
        .expect("subscribe");

    let offer = SignalingPayload::offer(alice.clone(), "ZmFrZS1zZHA=".into());
    sig.publish(offer.clone()).await.expect("publish");

    let got = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
        .await
        .expect("delivery within timeout")
        .expect("payload");
    assert_eq!(got.kind, offer.kind);
    assert_eq!(got.node_id, offer.node_id);
    assert_eq!(got.sdp, offer.sdp);
}

#[tokio::test]
async fn payload_json_roundtrip_offer_answer_ice() {
    for (kind, p) in [
        (
            "offer",
            SignalingPayload::offer(NodeId::random(), "off".into()),
        ),
        (
            "answer",
            SignalingPayload::answer(NodeId::random(), "ans".into()),
        ),
        (
            "ice",
            SignalingPayload::ice(NodeId::random(), "{\"foo\":1}".into()),
        ),
    ] {
        let json = serde_json::to_string(&p).expect("encode");
        let back: SignalingPayload = serde_json::from_str(&json).expect("decode");
        assert_eq!(back.kind, p.kind, "{kind} kind");
        assert_eq!(back.node_id, p.node_id, "{kind} node_id");
        assert_eq!(back.sdp, p.sdp, "{kind} sdp");
        assert_eq!(back.candidate, p.candidate, "{kind} candidate");
    }
}
