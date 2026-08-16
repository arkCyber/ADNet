//! Generate a random `NodeId`, print its short + hex form, and validate
//! the round-trip through `from_hex`.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-types --example node_id_roundtrip
//! ```

use a3net_types::{NodeAddr, NodeId, RelayUrl};

fn main() {
    // --- 1. Random NodeId -----------------------------------------------
    let me = NodeId::random();
    println!("fresh node id : {me}");
    println!("short         : {}", me.short());
    println!("hex length    : {}", me.as_hex().len());

    // --- 2. Round-trip via from_hex / as_bytes -------------------------
    let back = NodeId::from_hex(me.as_hex()).expect("hex decode must succeed");
    assert_eq!(me, back, "roundtrip must preserve identity");

    let bytes = me.as_bytes();
    let from_bytes = NodeId::from_bytes(&bytes).expect("bytes len must be 32");
    assert_eq!(me, from_bytes);
    println!("bytes len     : {}", bytes.len());

    // --- 3. NodeAddr composition ---------------------------------------
    let addr = NodeAddr::new(me.clone())
        .with_direct(a3net_types::Endpoint::new("127.0.0.1", 7777))
        .with_relay(RelayUrl::new("https://relay.example.com"));
    let rendered = addr.display();
    println!("\nNodeAddr      : {rendered}");
    let parsed = NodeAddr::parse(&rendered).expect("self-rendered addr must parse");
    assert_eq!(addr, parsed);
    println!("display==parse: ok");

    // --- 4. Bad inputs ---------------------------------------------------
    let bad = NodeId::from_hex("nope");
    assert!(bad.is_err());
    println!("\nrejected bad hex: ok");
}
