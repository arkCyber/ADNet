//! End-to-end integration tests for the `a3chat-crypto` public API.
//!
//! These tests cross module boundaries: they run a real Noise_XX
//! handshake, derive session keys from the shared secret, and use
//! those keys to [`seal`] a payload that the peer recovers with
//! [`open`]. They also exercise the group [`SenderKeyChain`] and
//! the [`kek`] bundle format to make sure the public surface stays
//! self-consistent as we evolve.
//!
//! The inline `#[cfg(test)] mod tests` in each source file covers
//! individual primitives; this file exists so we can catch
//! regressions where a primitive change in one file breaks the
//! contracts another file depends on (e.g. AD format, key length).

use a3chat_crypto::{
    CryptoError, DmSession, SenderKey, SenderKeyChain, SenderKeyDistribution, SenderKeyId,
    SessionKeys, decrypt_bundle, derive_kek, encrypt_bundle, handshake_initiator,
    handshake_responder, open, random_bytes, seal,
};
use a3chat_crypto::kek::{BundlePayload, BundleSenderKey, KdfParams};
use a3chat_crypto::session::{
    initiator_first_message, initiator_final_message, responder_second_message,
};
use a3chat_core::id::ConversationId;
use a3chat_core::message::{MessageBody, MessageType};

// -- DM round-trip ----------------------------------------------------------

/// Run a full Noise_XX handshake between an initiator (Alice) and a
/// responder (Bob). Both sides end up with [`DmSession`]s whose
/// directional keys match the peer's opposite direction.
fn handshake_pair(label: &str) -> (DmSession, DmSession) {
    let mut alice = handshake_initiator().expect("initiator");
    let mut bob = handshake_responder().expect("responder");

    let m1 = initiator_first_message(&mut alice).expect("m1");
    let m2 = responder_second_message(&mut bob, &m1).expect("m2");
    let (m3, _) = initiator_final_message(&mut alice, &m2).expect("m3");
    let mut tmp = vec![0u8; 96];
    bob.read_message(&m3.payload, &mut tmp)
        .expect("bob reads m3");

    let alice_secret = alice.get_handshake_hash();
    let bob_secret = bob.get_handshake_hash();
    assert_eq!(
        alice_secret, bob_secret,
        "{label}: handshake hashes must match"
    );

    let alice_keys = SessionKeys::from_shared_secret(alice_secret, true).expect("alice keys");
    let bob_keys = SessionKeys::from_shared_secret(bob_secret, false).expect("bob keys");

    (
        DmSession::new(alice_keys, Some(format!("{label}/alice"))),
        DmSession::new(bob_keys, Some(format!("{label}/bob"))),
    )
}

#[test]
fn handshake_then_seal_open_round_trip() {
    let (alice, bob) = handshake_pair("dm-1");

    // Build the canonical AD a real product caller would use.
    let ad = b"alice|bob|conv:dm:1|seq:1|ts:1700000000";
    let plaintext = b"the quick brown fox jumps over the lazy dog";

    let (nonce, ct) = seal(alice.send_key(), ad, plaintext).expect("alice seals");
    assert_eq!(nonce.len(), 12, "nonce must be 12 bytes for IETF chacha20-poly1305");

    let recovered = open(bob.recv_key(), &nonce, ad, &ct).expect("bob opens");
    assert_eq!(recovered, plaintext, "bob must recover alice's plaintext");

    // Reverse direction: bob writes, alice reads.
    let ad_back = b"alice|bob|conv:dm:1|seq:2|ts:1700000001";
    let payload_back = b"hi back";
    let (n2, ct2) = seal(bob.send_key(), ad_back, payload_back).expect("bob seals");
    let recovered_back = open(alice.recv_key(), &n2, ad_back, &ct2).expect("alice opens");
    assert_eq!(recovered_back, payload_back);
}

#[test]
fn handshake_pair_keys_are_swapped_between_parties() {
    let (alice, bob) = handshake_pair("dm-2");
    // What alice sends, bob must receive on the opposite key.
    assert_eq!(
        alice.send_key().as_bytes(),
        bob.recv_key().as_bytes(),
        "alice.send must equal bob.recv"
    );
    assert_eq!(
        alice.recv_key().as_bytes(),
        bob.send_key().as_bytes(),
        "alice.recv must equal bob.send"
    );
    // The two halves must be distinct (we'd catch a catastrophic
    // "split = same byte" regression here).
    assert_ne!(
        alice.send_key().as_bytes(),
        alice.recv_key().as_bytes(),
        "alice's send/recv keys must differ"
    );
}

#[test]
fn seal_then_open_with_wrong_ad_fails() {
    let (alice, bob) = handshake_pair("dm-3");
    let (nonce, ct) = seal(alice.send_key(), b"ad-correct", b"hello").expect("seal");
    let result = open(bob.recv_key(), &nonce, b"ad-tampered", &ct);
    assert!(
        matches!(result, Err(CryptoError::AeadTagMismatch)),
        "expected AEAD-tag mismatch on tampered AD, got {result:?}"
    );
}

#[test]
fn serializing_real_message_body_round_trips_through_handshake() {
    // Take a real `ChatMessage` (the type a3chat-app hands to the
    // crypto layer) and prove we can encrypt its body and recover it
    // on the other side.
    let (alice, bob) = handshake_pair("dm-msg");
    let alice_id = a3chat_core::id::UserId::from("alice-node");
    let conv = ConversationId::from("dm:alice:bob");
    let original = a3chat_core::message::ChatMessage {
        message_id: a3chat_core::id::generate_message_id(alice_id.as_str()),
        conversation_id: conv.clone(),
        sender_id: alice_id,
        receiver_id: a3chat_core::id::UserId::from("bob-node"),
        message_type: MessageType::Text,
        body: MessageBody::Plain {
            content: "👋 hello bob, this is alice".into(),
        },
        attachments: vec![],
        reply_to: None,
        sequence: 42,
        timestamp: 1_700_000_042,
        read_at: None,
        is_edited: false,
        edited_at: None,
        integrity_hash: None,
        recalled_at: None,
    };
    let plaintext = serde_json::to_vec(&original).expect("encode message");
    let ad = format!("alice|bob|{conv}|seq:42|ts:{}", 1_700_000_042);
    let (nonce, ct) = seal(alice.send_key(), ad.as_bytes(), &plaintext).expect("seal");

    let recovered_bytes = open(bob.recv_key(), &nonce, ad.as_bytes(), &ct).expect("open");
    let recovered: a3chat_core::message::ChatMessage =
        serde_json::from_slice(&recovered_bytes).expect("decode message");
    assert_eq!(recovered.body, original.body, "body must round-trip");
    assert_eq!(recovered.message_id, original.message_id);
}

// -- KEK bundle --------------------------------------------------------------

/// Lightweight KDF params for unit-test speed. The default
/// (`KdfParams::DEFAULT`) is 64 MiB / t=2 / p=1 which is too slow
/// to run on every CI invocation. The lowered params still
/// exercise the same Argon2id code path.
fn fast_kdf_params() -> KdfParams {
    KdfParams {
        time_cost: 1,
        memory_kib: 8 * 1024,
        parallelism: 1,
    }
}

fn sample_payload() -> BundlePayload {
    BundlePayload {
        identity_seed: hex::encode([0x42u8; 32]),
        sender_keys: vec![BundleSenderKey {
            conversation_id: ConversationId::from("grp:family").as_str().to_string(),
            sender_key_id_hex: SenderKeyId::random().as_hex(),
            chain_key_hex: hex::encode([0x99u8; 32]),
            iteration: 0,
        }],
        display_name: "Alice".into(),
        avatar_hash: Some("a".repeat(64)),
    }
}

#[test]
fn kek_bundle_round_trips_through_real_password() {
    let password = b"correct horse battery staple";
    let payload = sample_payload();
    let bundle = encrypt_bundle(&payload, password, "device-laptop-01").expect("encrypt");

    // Serialize → deserialize to catch format drift.
    let blob = serde_json::to_vec(&bundle).expect("serialize bundle");
    let parsed: a3chat_crypto::EncryptedBundle =
        serde_json::from_slice(&blob).expect("parse bundle");

    let recovered = decrypt_bundle(&parsed, password).expect("decrypt bundle");
    assert_eq!(recovered.display_name, payload.display_name);
    assert_eq!(recovered.identity_seed, payload.identity_seed);
    assert_eq!(recovered.sender_keys, payload.sender_keys);
}

#[test]
fn kek_bundle_rejects_wrong_password() {
    let payload = sample_payload();
    let bundle = encrypt_bundle(&payload, b"right-password", "dev-1").expect("encrypt");
    let wrong = decrypt_bundle(&bundle, b"wrong-password");
    assert!(
        wrong.is_err(),
        "wrong password must fail to decrypt, got {:?}",
        wrong
    );
}

#[test]
fn kek_kdf_is_deterministic_and_salt_sensitive() {
    // Same password + salt → same KEK. Different salt → different
    // KEK. Cheap test that doesn't need a full bundle round trip.
    let salt = random_bytes(16);
    let k1 = derive_kek(b"p", &salt, fast_kdf_params()).expect("kdf 1");
    let k2 = derive_kek(b"p", &salt, fast_kdf_params()).expect("kdf 2");
    assert_eq!(k1, k2, "same input must give the same KEK");

    let other_salt = random_bytes(16);
    let k3 = derive_kek(b"p", &other_salt, fast_kdf_params()).expect("kdf 3");
    assert_ne!(k1, k3, "different salt must change the KEK");
}

// -- Group sender-keys -------------------------------------------------------

fn fresh_chain() -> (SenderKeyChain, SenderKeyChain) {
    // Two peers sharing the same starting chain key — what a
    // Sender Key Distribution would deliver.
    let id = SenderKeyId::random();
    let key = SenderKey::generate(id);
    (SenderKeyChain::new(&key), SenderKeyChain::new(&key))
}

#[test]
fn sender_key_chain_round_trips_100_messages() {
    let (mut alice, mut bob) = fresh_chain();

    for i in 0..DmSession::MAX_MESSAGES {
        let (iteration, blob) = alice.seal_next(b"ad", b"hi").expect("seal");
        let pt = bob
            .open_next(b"ad", &blob, iteration)
            .expect("open");
        assert_eq!(pt, b"hi", "message {i} must round-trip");
    }
    assert_eq!(
        alice.iteration(),
        DmSession::MAX_MESSAGES as u32,
        "alice chain should be at MAX_MESSAGES after 100 sends"
    );
    assert_eq!(
        bob.iteration(),
        DmSession::MAX_MESSAGES as u32,
        "bob chain should be at MAX_MESSAGES after 100 receives"
    );
}

#[test]
fn sender_key_chain_out_of_sync_is_rejected() {
    // Bob's iteration is behind alice's, so opening a future
    // iteration must fail with a clear error (not panic, not
    // garbage).
    let (mut alice, mut bob) = fresh_chain();
    // Alice seals one message (iter 0) but bob hasn't received it
    // yet — bob is at iteration 0 too, so opening iter 0 actually
    // succeeds. We need a stale-by-one scenario: alice has sealed
    // two messages (iter 0, iter 1) but bob only received iter 0,
    // so bob.iteration == 1. Now alice seals iter 2 and bob is
    // asked to open iteration 2 — should succeed; we ask bob to
    // open a blob stamped with iter 1 instead.
    let (_i0, blob0) = alice.seal_next(b"ad", b"m0").unwrap();
    let _ = bob.open_next(b"ad", &blob0, 0).unwrap();
    let (_i1, blob1) = alice.seal_next(b"ad", b"m1").unwrap();
    // bob still at iter 1, alice now at iter 2. The blob1 was
    // sealed for iteration 1, so open with expected_iteration=1
    // actually succeeds. To prove the rejection, seal a third and
    // ask bob to open with a *stale* iteration.
    let (_i2, _blob2) = alice.seal_next(b"ad", b"m2").unwrap();
    // Now bob.iteration == 1, alice.iteration == 3.
    // Asking bob to open with expected_iteration == 2 (which he
    // already missed) must fail.
    let result = bob.open_next(b"ad", &blob1, 2);
    assert!(
        result.is_err(),
        "stale-iteration open must error, got {:?}",
        result
    );
}

#[test]
fn sender_key_distribution_round_trips() {
    // Wire-format envelope that a group owner hands to a new member.
    let dist = SenderKeyDistribution {
        conversation_id: ConversationId::from("grp:xyz"),
        sender_key_id: SenderKeyId::random(),
        chain_key: hex::encode([0x33u8; 32]),
        iteration: 7,
    };
    let bytes = dist.encode().expect("encode");
    let decoded = SenderKeyDistribution::decode(&bytes).expect("decode");
    assert_eq!(dist, decoded);
}

#[test]
fn sender_key_rotate_resets_iteration() {
    // When a member joins/leaves the group, the chain rotates. The
    // old chain must no longer decrypt under the new state — and
    // the new chain must work from iteration 0.
    let (mut alice, mut bob) = fresh_chain();
    // Synchronize bob with alice before the rotate.
    let (iter1, blob1) = alice.seal_next(b"ad", b"hi-1").unwrap();
    let _ = bob.open_next(b"ad", &blob1, iter1).unwrap();
    assert_eq!(alice.iteration(), 1);
    assert_eq!(bob.iteration(), 1);

    // Rotate: fresh key for both sides.
    let new_id = SenderKeyId::random();
    let new_key = SenderKey::generate(new_id);
    alice.rotate(new_id, *new_key.chain_key());
    bob.rotate(new_id, *new_key.chain_key());
    assert_eq!(alice.iteration(), 0, "alice reset after rotate");
    assert_eq!(bob.iteration(), 0, "bob reset after rotate");

    // Post-rotate send/decrypt works from iteration 0.
    let (iter2, blob2) = alice.seal_next(b"ad", b"post-rotate").unwrap();
    let pt = bob.open_next(b"ad", &blob2, iter2).expect("open post-rotate");
    assert_eq!(pt, b"post-rotate");
    assert_eq!(alice.iteration(), 1);
    assert_eq!(bob.iteration(), 1);
}