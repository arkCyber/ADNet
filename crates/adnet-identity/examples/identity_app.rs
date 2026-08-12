//! Realistic example: a sealed env (`SealedEnvelope`) carrying a JSON
//! payload sent between two actors. The recipient's static X25519
//! key is the routing address; only the recipient's secret key can
//! open it.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-identity --example identity_app
//! ```

use adnet_identity::{EciesSecretKey, EncryptedPayload, SealedEnvelope, Wallet};
use adnet_types::{Announcement, CdnContentKind, ContentHash, NodeId, RoomId};
use chrono::Utc;

fn main() {
    // 1. The recipient's encryption keypair. (Encryption keys are
    //    separate from the wallet so they can be rotated without
    //    touching the EVM identity.)
    let recipient = EciesSecretKey::generate();
    let recipient_pub = recipient.public_key();
    println!("recipient pubkey: 0x{}", hex::encode(recipient_pub.to_bytes()));

    // 2. A wallet for the sender — used here only to label the
    //    payload as "from" the sender for the demo. The envelope
    //    itself is unauthenticated; the application is responsible
    //    for any sender attribution it needs.
    let sender = Wallet::generate();
    println!("sender: {}", sender.public().address());

    // 3. The payload is an `Announcement` JSON. Any `Serialize` type
    //    works — the envelope is opaque to our format.
    let ann = Announcement {
        room_id: RoomId::new("lobby"),
        content_hash: ContentHash::from_bytes(b"sealed payload"),
        node_id: NodeId::random(),
        title: "Sealed hello".into(),
        kind: CdnContentKind::Article,
        size_bytes: 14,
        mime_type: Some("text/plain".into()),
        source_url: None,
        ticket: None,
        timestamp: Utc::now(),
        message_id: None,
        ttl_secs: None,
        signer: None,
        signature: None,
    };
    let payload_bytes = serde_json::to_vec(&ann).expect("serialize");
    let payload = EncryptedPayload::from(payload_bytes);

    // 4. Seal: emit a fresh envelope with a new ephemeral key.
    let env = SealedEnvelope::seal(&recipient_pub, payload).expect("seal");
    println!("sealed envelope: ephemeral_pub=0x{}",
        hex::encode(env.inner.ephemeral_pub));

    // 5. The wire form is a small prefix + ECIES fields. Useful for
    //    debugging — this is the bytes that travel over gossip.
    let wire = env.encode();
    println!("wire bytes: {} (magic={:?}, version={})", wire.len(),
        &wire[..4], wire[4]);

    // 6. The recipient decodes the wire form back to a `SealedEnvelope`
    //    and opens it.
    let back = SealedEnvelope::decode(&wire).expect("decode");
    let opened = back.open(&recipient).expect("open");
    assert_eq!(opened.as_bytes(), &serde_json::to_vec(&ann).unwrap());

    let decoded: Announcement = serde_json::from_slice(opened.as_bytes()).unwrap();
    println!("opened room_id = {}", decoded.room_id);
    println!("opened title   = {}", decoded.title);

    // 7. A different key cannot open the envelope.
    let stranger = EciesSecretKey::generate();
    let err = opened.as_bytes(); // verify the *next* test opens correct bytes
    let _ = err;
    let alt = SealedEnvelope::decode(&wire).expect("decode");
    let fail = alt.open(&stranger);
    println!("stranger opens: {:?}", fail.is_err());
    assert!(fail.is_err());
    println!("stranger saw: ok");
}
