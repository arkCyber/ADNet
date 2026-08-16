//! Build a `BlobTicket` (whole-blob and sub-range), serialize / parse it,
//! and render the HTTP base URL the mesh fallback expects.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-types --example blob_ticket_demo
//! ```

use a3net_types::{
    BlobTicket, ByteRange, ContentHash, Endpoint, NodeAddr, NodeId, RangeSpec, RelayUrl,
};

fn main() {
    let peer = NodeId::random();
    let addr = NodeAddr::new(peer.clone())
        .with_direct(Endpoint::new("10.0.0.42", 9000))
        .with_relay(RelayUrl::new("https://relay.example.com"));
    let hash = ContentHash::from_bytes(b"hello a3net");

    // --- 1. Whole-blob ticket ------------------------------------------
    let whole = BlobTicket::whole(&peer, &addr, &hash);
    let raw_whole = whole.encode();
    let parsed_whole = BlobTicket::parse(&raw_whole).expect("whole ticket must parse");
    assert_eq!(parsed_whole, whole);
    println!("whole ticket:\n  {raw_whole}");
    println!("http base   : {}", whole.http_base().unwrap_or_default());

    // --- 2. Sub-range ticket (single) -----------------------------------
    let ranged = whole.clone().with_range(RangeSpec::Single(
        ByteRange::new(0, 1024).expect("valid range"),
    ));
    let raw_range = ranged.encode();
    let parsed_range = BlobTicket::parse(&raw_range).expect("ranged ticket must parse");
    assert_eq!(
        parsed_range.range,
        RangeSpec::Single(ByteRange::new(0, 1024).unwrap(),)
    );
    println!("\nrange ticket:\n  {raw_range}");

    // --- 3. Multi-range ticket ------------------------------------------
    let multi = whole.with_range(RangeSpec::Multi(vec![
        ByteRange::new(0, 50).unwrap(),
        ByteRange::new(100, 200).unwrap(),
        ByteRange::new(9000, 9500).unwrap(),
    ]));
    let raw_multi = multi.encode();
    let parsed_multi = BlobTicket::parse(&raw_multi).expect("multi ticket must parse");
    match parsed_multi.range {
        RangeSpec::Multi(rs) => assert_eq!(rs.len(), 3),
        _ => panic!("expected multi"),
    }
    println!("\nmulti-range ticket:\n  {raw_multi}");

    // --- 4. RangeSpec <-> HTTP Range: header ---------------------------
    let header = RangeSpec::Single(ByteRange::new(100, 200).unwrap()).to_http_header();
    assert_eq!(header.as_deref(), Some("bytes=100-199"));
    let parsed_header = RangeSpec::from_http_header("bytes=100-199", 10_000).expect("valid header");
    assert_eq!(
        parsed_header,
        RangeSpec::Single(ByteRange::new(100, 200).unwrap())
    );
    println!("\nHTTP Range: header -> RangeSpec: ok");
}
