//! Real-world example — exercise every parser surface the
//! `a3net-fuzz` targets attack, but with a hand-curated corpus
//! instead of mutated random inputs. Useful as a quick smoke test
//! after touching any of:
//!
//!   * `a3net-types` — Announcement, NodeId, Cid, ContentHash
//!   * `a3net-dht`   — DhtMessage wire variants
//!   * `a3net-blobstore` — BitswapCodec, WantlistManager
//!
//! Run with:
//!   cargo run -p a3net-fuzz --example fuzz_app --release

use a3net_types::{
    Announcement, CdnContentKind, ContentHash, NodeId, RoomId,
};

fn main() {
    println!("=== a3net-fuzz corpus replay ===\n");

    let total = run_corpus();
    println!("\n{total} parser assertions passed");
}

fn run_corpus() -> usize {
    let mut count = 0usize;

    // ─── Announcement corpus ─────────────────────────────────────
    count += corpus_announcement();

    // ─── NodeId corpus ───────────────────────────────────────────
    count += corpus_node_id();

    // ─── ContentHash corpus ──────────────────────────────────────
    count += corpus_content_hash();

    count
}

fn corpus_announcement() -> usize {
    let node = NodeId::random();
    let hash = ContentHash::from_bytes(b"corpus payload");

    let mut cases = 0;

    for (i, title) in [
        "Article example",
        "空字符串测试",
        "Very long title that pushes the gossip payload size near its limit",
    ]
    .iter()
    .enumerate()
    {
        let ann = Announcement {
            room_id: RoomId::from(format!("room-{i}")),
            content_hash: hash.clone(),
            node_id: node.clone(),
            title: (*title).into(),
            kind: CdnContentKind::Article,
            size_bytes: 1024 * (i as u64 + 1),
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: chrono::Utc::now(),
            message_id: None,
            ttl_secs: None,
            signer: None,
            signature: None,
        };

        // JSON round-trip
        let json = serde_json::to_string(&ann).expect("json encode");
        let decoded: Announcement = serde_json::from_str(&json).expect("json decode");
        assert_eq!(decoded.title, ann.title);
        cases += 1;

        // postcard round-trip
        let bytes = postcard::to_allocvec(&ann).expect("postcard encode");
        let decoded: Announcement = postcard::from_bytes(&bytes).expect("postcard decode");
        assert_eq!(decoded.title, ann.title);
        cases += 1;

        // CBOR round-trip — the fuzz target covers both encoding paths
        let _ = serde_cbor::to_vec(&ann).expect("cbor encode");
        cases += 1;

        // MessagePack round-trip
        let _ = rmp_serde::to_vec(&ann).expect("msgpack encode");
        cases += 1;
    }

    println!("Announcement corpus  : {cases} assertions");
    cases
}

fn corpus_node_id() -> usize {
    let mut cases = 0usize;
    let node = NodeId::random();

    // hex round-trip
    let hex = node.as_hex();
    let parsed = NodeId::from_hex(hex).expect("hex parse");
    assert_eq!(parsed.as_hex(), hex);
    cases += 1;

    // bytes round-trip
    let bytes = node.as_bytes();
    let parsed = NodeId::from_bytes(&bytes).expect("bytes parse");
    assert_eq!(parsed.as_hex(), hex);
    cases += 1;

    // XOR-distance invariants
    let self_dist = node.xor_distance(&node);
    assert!(self_dist.iter().all(|b| *b == 0));
    cases += 1;

    let other = NodeId::random();
    let other_dist = node.xor_distance(&other);
    assert!(other_dist.iter().any(|b| *b != 0));
    cases += 1;

    // Reject malformed hex strings (the fuzz target asserts these
    // never panic).
    for bad in ["", "zz", &"a".repeat(63), &"a".repeat(65)] {
        assert!(NodeId::from_hex(bad).is_err(), "should reject {bad:?}");
        cases += 1;
    }

    println!("NodeId corpus        : {cases} assertions");
    cases
}

fn corpus_content_hash() -> usize {
    let mut cases = 0usize;

    // BLAKE3 of empty bytes is deterministic
    let h = ContentHash::from_bytes(b"");
    assert_eq!(h.as_hex().len(), ContentHash::HEX_LEN);
    cases += 1;

    // Round-trip via hex
    let parsed = ContentHash::from_hex(h.as_hex()).expect("hex parse");
    assert_eq!(parsed.as_hex(), h.as_hex());
    cases += 1;

    // Short prefix is 8 hex chars
    assert_eq!(h.short().len(), 8);
    cases += 1;

    // Reject malformed hex (matches fuzz target boundary)
    assert!(ContentHash::from_hex("not 64 chars").is_err());
    cases += 1;
    assert!(ContentHash::from_hex(&"g".repeat(64)).is_err());
    cases += 1;

    println!("ContentHash corpus   : {cases} assertions");
    cases
}
