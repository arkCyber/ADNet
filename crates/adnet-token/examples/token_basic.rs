//! Tiny example: build a `Pledge`, sign it with a fresh `Wallet`, verify
//! it, then round-trip it through the printable URL form.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-token --example token_basic
//! ```

use adnet_identity::{Address, Wallet};
use adnet_token::{Pledge, MAX_AMOUNT_ATOMIC};

fn main() {
    // 1. Build the Pledgor wallet.
    let pledgor = Wallet::generate();
    println!("pledgor: {}", pledgor.public().address());

    // 2. Build a recipient EVM address (any 20-byte hex works — the
    //    relay address is just a checksum).
    let recipient = Address::from_hex("0x52908400098527886E0F7030069857D2E4169EE7")
        .expect("valid hex");

    // 3. Compose the pledge body.
    let nonce_hex: String = (0..32)
        .map(|i| format!("{:02x}", i))
        .collect();
    let body = Pledge::body(
        1,                                  // chain_id (Ethereum mainnet)
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(), // USDC contract
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(), // token == contract
        1_000_000,                          // 1.00 USDC (6 decimals)
        recipient,
        nonce_hex,
        chrono::Utc::now().timestamp() + 3600, // 1h into the future
    )
    .expect("valid body");

    // 4. Sign with the wallet.
    let pledge = Pledge::sign(body, &pledgor).expect("sign");
    println!("pledge: amount={}, recipient={}", pledge.amount_atomic, pledge.recipient);

    // 5. Verify. The `now_unix` argument is the relay's clock; we use
    //    the same clock the body was signed with so the test is
    //    deterministic.
    pledge.verify(chrono::Utc::now().timestamp()).expect("verify ok");
    println!("verify: ok");

    // 6. URL round-trip — what a QR code would carry.
    let url = pledge.to_url();
    println!("\nurl: {url}");
    let parsed = Pledge::from_url(&url).expect("parse url");
    let recovered = parsed
        .verify_with_recovered(chrono::Utc::now().timestamp())
        .expect("verify with recovered");
    println!("recovered signer: {recovered}");
    assert_eq!(recovered, pledgor.public().address());

    // 7. Sanity-check the cap.
    assert!(MAX_AMOUNT_ATOMIC > 1_000_000);
}
