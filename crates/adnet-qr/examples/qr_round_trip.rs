//! Round-trip demo: parse a few QR payloads from each supported
//! scheme, then re-encode them as SVG.

use adnet_qr::generator;
use adnet_qr::payload::QrPayload;
use adnet_qr::scan::{check_qr, encode_qr};

fn main() {
    let samples = [
        "mailto:alice@example.com?subject=Hi&body=Hello%20there",
        "MATMSG:TO:bob@example.com;SUB:Greetings;BODY:Howdy;",
        "BEGIN:VCARD\nN:Doe;Alice;;;\nEMAIL:alice@example.com\nEND:VCARD",
        "SMTP:carol@example.com:subject:body",
        "DCACCOUNT:example.org",
        "dclogin://dave@chat.example.com?p=secret&v=1&ih=imap.chat.example.com&is=ssl",
        "OPENPGP4FPR:ABCDEF1234#a=alice%40example.com&n=Alice&i=inv123&s=auth456",
        "DCBACKUP5:auth-token-xyz&{\"node_id\":\"abc\"}",
        "https://t.me/socks?server=proxy.example.com&port=1080",
    ];

    for raw in samples {
        let parsed = check_qr(raw).unwrap_or_else(|e| {
            eprintln!("{raw} -> ERROR: {e}");
            QrPayload::Text { text: raw.into() }
        });
        let tag = parsed.tag();
        let encoded = encode_qr(&parsed).unwrap_or_else(|e| {
            eprintln!("  encode: {e}");
            raw.to_string()
        });
        let svg = generator::create_qr_svg(&encoded).unwrap_or_default();
        println!(
            "{tag:>16} | parsed: {parsed:?}\n{tag:>16} | encoded: {encoded}\n{tag:>16} | svg-bytes: {}\n",
            svg.len(),
        );
    }
}
