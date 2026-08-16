//! Build an `Announcement`, serialize it as JSON (`AnnouncementPayload`),
//! and ingest it back. Demonstrates the wire format that the gossip bus
//! uses to ferry content metadata between peers.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-types --example announcement_demo
//! ```

use a3net_types::{
    Announcement, AnnouncementPayload, BlobTicket, CdnContentKind, ContentHash, Endpoint, NodeAddr,
    NodeId, RoomId,
};
use chrono::Utc;

fn main() {
    let me = NodeId::random();
    let addr = NodeAddr::new(me.clone()).with_direct(Endpoint::new("192.168.1.5", 8080));
    let hash = ContentHash::from_bytes(b"a3net-blob-payload");
    let ticket = BlobTicket::whole(&me, &addr, &hash);

    let ann = Announcement {
        room_id: RoomId::new("lobby"),
        content_hash: hash.clone(),
        node_id: me.clone(),
        title: "A3Net README".into(),
        kind: CdnContentKind::Article,
        size_bytes: 4_315,
        mime_type: Some("text/markdown".into()),
        source_url: Some("https://example.com/README.md".into()),
        ticket: Some(ticket.clone()),
        timestamp: Utc::now(),
        message_id: None,
        ttl_secs: None,
        signer: None,
        signature: None,
    };

    // --- 1. JSON wire format -------------------------------------------
    let payload = AnnouncementPayload::from(&ann);
    let raw = serde_json::to_string_pretty(&payload).expect("serialize");
    println!("announcement JSON (wire format):\n{raw}");

    // --- 2. JSON -> Announcement roundtrip -----------------------------
    let parsed_payload: AnnouncementPayload = serde_json::from_str(&raw).expect("deserialize");
    let parsed_ann: Announcement = (&parsed_payload)
        .try_into()
        .expect("payload must convert to announcement");
    assert_eq!(parsed_ann.content_hash, ann.content_hash);
    assert_eq!(parsed_ann.title, ann.title);
    assert_eq!(parsed_ann.ticket, ann.ticket);
    println!("\nroundtrip JSON -> Announcement: ok");

    // --- 3. Looser parse path (snake_case room_id) ---------------------
    let looser = serde_json::json!({
        "room_id": "lobby",
        "content_hash": hash.as_hex(),
        "node_id": me.as_hex(),
        "title": "loose",
        "kind": "article",
        "size_bytes": 12,
        "timestamp": Utc::now(),
    });
    // `RoomId` deserializes from a plain string via its own From impl,
    // but the wrapper struct is `rename_all = "camelCase"`, so use the
    // canonical camelCase here:
    let canonical = serde_json::json!({
        "roomId": "lobby",
        "contentHash": hash.as_hex(),
        "nodeId": me.as_hex(),
        "title": "loose",
        "kind": "article",
        "sizeBytes": 12,
        "timestamp": Utc::now(),
    });
    let from_loose: Announcement = serde_json::from_value(canonical).expect("canonical parse");
    assert_eq!(from_loose.kind, CdnContentKind::Article);
    println!("loose + canonical parse (camelCase wire format): ok");
    // --- 4. from_ai_recommendation helper (snake_case ingest) ---------
    let ai_payload = serde_json::json!({
        "content_hash": hash.as_hex(),
        "title": "Llama GGUF",
        "kind": "llm",
        "size_bytes": 5_000_000_000_u64,
    });
    let from_ai =
        Announcement::from_ai_recommendation("lobby", &me, &ai_payload).expect("ai parse");
    assert_eq!(from_ai.kind, CdnContentKind::AiModel);
    assert_eq!(from_ai.size_bytes, 5_000_000_000);
    println!("from_ai_recommendation (snake_case): ok");
    // Suppress unused variable warning for `looser`.
    let _ = looser;
}
