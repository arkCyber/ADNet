//! Integration test: Noise_XX handshake + frame codec over a mock
//! DataChannel.
//!
//! This test exercises the same handshake + framing pipeline that the
//! real `DcSession` would drive over a `webrtc::RTCDataChannel`, but
//! skips the SDP/ICE bring-up which is exercised separately by the
//! smoke example and is fundamentally environment-dependent (see
//! `webrtc-rs/webrtc#662`).
//!
//! What we cover:
//!
//! - Noise_XX_25519_ChaChaPoly_SHA256 reaches `transport_mode` for both
//!   peers using the same handshake helper the production code uses
//!   (`a3net_webrtc::noise_dc::run_noise_handshake`).
//! - The remote-static key derived by each side matches the other
//!   peer's static public key, and the corresponding NodeId is the
//!   BLAKE3 hash of that key.
//! - After the handshake, each side can `encrypt` → frame → "send" →
//!   recv → frame → `decrypt` a payload of arbitrary length, in both
//!   directions, with no leakage between streams (a tampered ciphertext
//!   fails decryption).

#![cfg(feature = "webrtc")]

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::mpsc;

use a3net_webrtc::frame_codec::{encode_frame, try_decode};
use a3net_webrtc::noise_dc::{generate_keypair, run_noise_handshake, NoiseSession, Role, StaticPub};

/// A pair of `mpsc` channels that mimics a WebRTC DataChannel: each
/// side has a `tx` (send) and a `rx` (recv), and the two sides are
/// cross-routed.
struct MockDcPair {
    alice_tx: mpsc::Sender<Bytes>,
    bob_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Bytes>>>,
    bob_tx: mpsc::Sender<Bytes>,
    alice_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Bytes>>>,
}

fn mock_dc_pair() -> MockDcPair {
    let (alice_to_bob_tx, alice_to_bob_rx) = mpsc::channel::<Bytes>(64);
    let (bob_to_alice_tx, bob_to_alice_rx) = mpsc::channel::<Bytes>(64);
    MockDcPair {
        alice_tx: alice_to_bob_tx,
        bob_rx: Arc::new(tokio::sync::Mutex::new(alice_to_bob_rx)),
        bob_tx: bob_to_alice_tx,
        alice_rx: Arc::new(tokio::sync::Mutex::new(bob_to_alice_rx)),
    }
}

/// Bridge the noise `FnMut` IO closures to a `mpsc` DataChannel pair.
/// `tx` is the sender for outbound Noise frames; `rx` is the receiver
/// for inbound Noise frames. We frame each Noise message with the
/// standard `u32 BE | payload` length prefix.
async fn run_handshake(
    tx: mpsc::Sender<Bytes>,
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Bytes>>>,
    kp: &snow::Keypair,
    role: Role,
) -> Result<NoiseSession, a3net_webrtc::error::WebRtcError> {
    use a3net_webrtc::error::WebRtcError;

    let io_send = move |msg: Vec<u8>| {
        let tx = tx.clone();
        async move {
            let framed = encode_frame(&msg).map_err(|e| WebRtcError::Frame(e.to_string()))?;
            tx.send(framed).await.map_err(|_| WebRtcError::PeerClosed)?;
            Ok(())
        }
    };

    let io_recv = move || {
        let rx = rx.clone();
        async move {
            let msg = {
                let mut g = rx.lock().await;
                g.recv().await
            }
            .ok_or_else(|| WebRtcError::PeerClosed)?;
            let (payload, _consumed) = try_decode(&msg)
                .map_err(|e| WebRtcError::Frame(e.to_string()))?
                .ok_or_else(|| WebRtcError::Frame("short frame".into()))?;
            Ok(payload.to_vec())
        }
    };

    run_noise_handshake(role, kp, io_send, io_recv).await
}

#[tokio::test]
async fn handshake_yields_matching_remote_node_ids() {
    let pair = mock_dc_pair();
    let alice_kp = generate_keypair().unwrap();
    let bob_kp = generate_keypair().unwrap();

    let alice_fut = run_handshake(
        pair.alice_tx.clone(),
        pair.alice_rx.clone(),
        &alice_kp,
        Role::Initiator,
    );
    let bob_fut = run_handshake(
        pair.bob_tx.clone(),
        pair.bob_rx.clone(),
        &bob_kp,
        Role::Responder,
    );

    let (alice_noise, bob_noise) = tokio::try_join!(alice_fut, bob_fut).unwrap();

    let alice_pub: [u8; 32] = alice_kp.public[..32].try_into().unwrap();
    let bob_pub: [u8; 32] = bob_kp.public[..32].try_into().unwrap();
    assert_eq!(
        alice_noise.remote_node_id(),
StaticPub::from_bytes(&bob_pub).unwrap().to_node_id()
        );
        assert_eq!(
            bob_noise.remote_node_id(),
            StaticPub::from_bytes(&alice_pub).unwrap().to_node_id()
    );
    assert_ne!(alice_noise.remote_node_id(), bob_noise.remote_node_id());
}

#[tokio::test]
async fn handshake_then_encrypt_decrypt_round_trip() {
    let pair = mock_dc_pair();
    let alice_kp = generate_keypair().unwrap();
    let bob_kp = generate_keypair().unwrap();

    let alice_fut = run_handshake(
        pair.alice_tx.clone(),
        pair.alice_rx.clone(),
        &alice_kp,
        Role::Initiator,
    );
    let bob_fut = run_handshake(
        pair.bob_tx.clone(),
        pair.bob_rx.clone(),
        &bob_kp,
        Role::Responder,
    );
    let (alice_noise, bob_noise) = tokio::try_join!(alice_fut, bob_fut).unwrap();

    // alice → bob
    let ct_a = alice_noise.encrypt(b"hello bob").await.unwrap();
    pair.alice_tx
        .send(encode_frame(&ct_a).unwrap())
        .await
        .unwrap();

    let framed_b = {
        let mut g = pair.bob_rx.lock().await;
        g.recv().await.unwrap()
    };
    let (ct_payload_b, _) = try_decode(&framed_b).unwrap().unwrap();
    let pt_b = bob_noise.decrypt(&ct_payload_b).await.unwrap();
    assert_eq!(pt_b, b"hello bob");

    // bob → alice
    let ct_b = bob_noise.encrypt(b"hi alice").await.unwrap();
    pair.bob_tx
        .send(encode_frame(&ct_b).unwrap())
        .await
        .unwrap();

    let framed_a = {
        let mut g = pair.alice_rx.lock().await;
        g.recv().await.unwrap()
    };
    let (ct_payload_a, _) = try_decode(&framed_a).unwrap().unwrap();
    let pt_a = alice_noise.decrypt(&ct_payload_a).await.unwrap();
    assert_eq!(pt_a, b"hi alice");
}

#[tokio::test]
async fn multiple_messages_in_flight_round_trip() {
    // Sends many small messages in one direction and reads them in
    // order on the other side. Confirms the cipher state advances
    // correctly across many frames.
    let pair = mock_dc_pair();
    let alice_kp = generate_keypair().unwrap();
    let bob_kp = generate_keypair().unwrap();

    let alice_fut = run_handshake(
        pair.alice_tx.clone(),
        pair.alice_rx.clone(),
        &alice_kp,
        Role::Initiator,
    );
    let bob_fut = run_handshake(
        pair.bob_tx.clone(),
        pair.bob_rx.clone(),
        &bob_kp,
        Role::Responder,
    );
    let (alice_noise, bob_noise) = tokio::try_join!(alice_fut, bob_fut).unwrap();

    let n = 16;
    for i in 0..n {
        let pt = format!("msg-{i}");
        let ct = alice_noise.encrypt(pt.as_bytes()).await.unwrap();
        pair.alice_tx
            .send(encode_frame(&ct).unwrap())
            .await
            .unwrap();
    }

    for i in 0..n {
        let framed = {
            let mut g = pair.bob_rx.lock().await;
            g.recv().await.unwrap()
        };
        let (ct_payload, _) = try_decode(&framed).unwrap().unwrap();
        let pt = bob_noise.decrypt(&ct_payload).await.unwrap();
        assert_eq!(pt, format!("msg-{i}").as_bytes());
    }
}

#[tokio::test]
async fn tampered_ciphertext_is_rejected() {
    let pair = mock_dc_pair();
    let alice_kp = generate_keypair().unwrap();
    let bob_kp = generate_keypair().unwrap();

    let alice_fut = run_handshake(
        pair.alice_tx.clone(),
        pair.alice_rx.clone(),
        &alice_kp,
        Role::Initiator,
    );
    let bob_fut = run_handshake(
        pair.bob_tx.clone(),
        pair.bob_rx.clone(),
        &bob_kp,
        Role::Responder,
    );
    let (alice_noise, bob_noise) = tokio::try_join!(alice_fut, bob_fut).unwrap();

    // Alice encrypts (uses her *send* cipher); bob receives that
    // ciphertext on his *recv* cipher.
    let ct = alice_noise.encrypt(b"original").await.unwrap();
    let mut tampered = ct.clone();
    // Flip a byte deep in the ciphertext so the Poly1305 tag won't match.
    if tampered.len() > 8 {
        tampered[8] ^= 0x01;
    }

    // Bob's decrypt of the tampered bytes must fail.
    let result = bob_noise.decrypt(&tampered).await;
    assert!(
        result.is_err(),
        "decrypting a tampered ciphertext must fail"
    );

    // Sanity: the unmodified ciphertext decrypts cleanly.
    let pt = bob_noise.decrypt(&ct).await.unwrap();
    assert_eq!(pt, b"original");
}

#[tokio::test]
async fn oversized_payload_fails_loudly() {
    let pair = mock_dc_pair();
    let alice_kp = generate_keypair().unwrap();
    let bob_kp = generate_keypair().unwrap();
    let alice_fut = run_handshake(
        pair.alice_tx.clone(),
        pair.alice_rx.clone(),
        &alice_kp,
        Role::Initiator,
    );
    let bob_fut = run_handshake(
        pair.bob_tx.clone(),
        pair.bob_rx.clone(),
        &bob_kp,
        Role::Responder,
    );
    let (alice_noise, _bob_noise) = tokio::try_join!(alice_fut, bob_fut).unwrap();

    // The Noise layer itself has a maximum plaintext size per message
    // (~64 KiB for XX_25519_ChaChaPoly_SHA256 with the current
    // builder); larger payloads should be rejected with an explicit
    // error rather than silently truncated or corrupted.
    let too_big = vec![0xAAu8; 128 * 1024];
    let result = alice_noise.encrypt(&too_big).await;
    assert!(
        result.is_err(),
        "encrypting a >max plaintext should fail with a clear error"
    );
}
