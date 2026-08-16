//! Minimal example — round-trip a BLAKE3-anchored `Cid` and a
//! `NodeId` through every serialisation format the fuzz targets
//! exercise (postcard, JSON, hex). Mirrors the assertions in
//! `fuzz_targets/parse_announcement.rs` and `fuzz_targets/parse_cid.rs`
//! but runs deterministically — no fuzz input, no panic, no crash.
//!
//! Run with:
//!   cargo run -p a3net-fuzz --example fuzz_basic

use a3net_types::{Announcement, CdnContentKind, ContentHash, NodeId, RoomId};

fn main() {
    // ─── Round-trip an Announcement through postcard + JSON ─────
    let ann = Announcement {
        room_id: RoomId::from("bench-room"),
        content_hash: ContentHash::from_bytes(b"hello fuzz world"),
        node_id: NodeId::random(),
        title: "Fuzz Basic Example".into(),
        kind: CdnContentKind::Article,
        size_bytes: 21,
        mime_type: None,
        source_url: None,
        ticket: None,
        timestamp: chrono::Utc::now(),
        message_id: None,
        ttl_secs: None,
        signer: None,
        signature: None,
    };

    // postcard binary path
    let bytes = postcard::to_allocvec(&ann).expect("postcard encode");
    let decoded: Announcement = postcard::from_bytes(&bytes).expect("postcard decode");
    assert_eq!(decoded.title, ann.title);
    println!("postcard round-trip ok ({} bytes)", bytes.len());

    // JSON path
    let json = serde_json::to_string(&ann).expect("json encode");
    let decoded: Announcement = serde_json::from_str(&json).expect("json decode");
    assert_eq!(decoded.title, ann.title);
    println!("json round-trip ok ({} bytes)", json.len());

    // ─── Round-trip a NodeId through hex + raw bytes ─────────────
    let node = NodeId::random();
    let hex = node.as_hex();
    let parsed = NodeId::from_hex(hex).expect("hex parse");
    assert_eq!(parsed.as_hex(), hex);
    println!("node id hex round-trip ok ({})", &hex[..16]);

    let bytes = node.as_bytes();
    let parsed = NodeId::from_bytes(&bytes).expect("bytes parse");
    assert_eq!(parsed.as_hex(), hex);
    println!("node id bytes round-trip ok ({} bytes)", bytes.len());

    // ─── XOR-distance invariants (same shape as fuzz target) ──────
    let dist_self = node.xor_distance(&node);
    assert!(dist_self.iter().all(|b| *b == 0));
    let other = NodeId::random();
    let dist_other = node.xor_distance(&other);
    assert!(dist_other.iter().any(|b| *b != 0));
    println!("xor distance invariants ok ({} bytes)", dist_other.len());
}
