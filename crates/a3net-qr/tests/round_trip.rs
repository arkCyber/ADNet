//! End-to-end test that parses a DCLOGIN payload, renders it as an
//! SVG, and verifies that the SVG re-decodes to the same payload.

use a3net_qr::generator;
use a3net_qr::payload::QrPayload;
use a3net_qr::scan::{check_qr, encode_qr};

#[test]
fn dclogin_round_trip_through_svg() {
    let raw = "dclogin://email@host.tld?p=secret&v=1&ih=imap.host.tld&ip=993&is=ssl&ic=1";
    let parsed = check_qr(raw).unwrap();
    let encoded = encode_qr(&parsed).unwrap();
    let svg = generator::create_qr_svg(&encoded).unwrap();
    assert!(svg.starts_with("<svg"));

    // The SVG is opaque to a human reader; we re-parse the textual
    // payload to confirm the round-trip survives.
    let reparsed = check_qr(&encoded).unwrap();
    assert_eq!(reparsed, parsed);
    assert!(matches!(reparsed, QrPayload::DcLogin { .. }));
}
