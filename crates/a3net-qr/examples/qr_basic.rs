//! Minimal a3net-qr example.
//!
//! Takes a few raw QR strings, classifies them with `check_qr`, and
//! prints the parsed payload. This is the smallest useful program
//! that exercises the public API without touching SVG rendering.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p a3net-qr --example qr_basic
//! ```

use a3net_qr::{check_qr, payload::QrPayload};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let samples = [
        "mailto:alice@example.com?subject=Hi&body=Hello",
        "MATMSG:TO:bob@example.com;SUB:Greetings;BODY:Howdy;",
        "SMTP:carol@example.com",
        "DCACCOUNT:chat.example.com",
        "https://example.com",
        "just a plain string",
    ];

    for raw in samples {
        match check_qr(raw) {
            Ok(payload) => {
                let tag = payload.tag();
                println!("{tag:>12} : {raw}\n            -> {payload:?}");
            }
            Err(e) => println!("{raw:>12} : error: {e}"),
        }
    }

    // Show the negative path: empty input is malformed.
    match check_qr("   ") {
        Ok(QrPayload::Text { text }) => println!("\nwhitespace input -> Text({text:?})"),
        Ok(other) => println!("\nwhitespace input -> {other:?}"),
        Err(e) => println!("\nwhitespace input -> error: {e}"),
    }

    Ok(())
}
