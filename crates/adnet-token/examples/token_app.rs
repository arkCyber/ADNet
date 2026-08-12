//! Realistic example: simulate a relay-billing flow between a client
//! (pledgor) and a relay (recipient). The client signs a `Pledge`,
//! the relay verifies it for its own chain, and the URL re-parses
//! cleanly via the QR code path.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-token --example token_app
//! ```

use adnet_identity::{Address, Wallet};
use adnet_token::Pledge;

const RELAY_ADDRESS: &str = "0x52908400098527886E0F7030069857D2E4169EE7";
const CHAIN_ID: u64 = 1;
const USDC_CONTRACT: &str = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48";

fn main() {
    // 1. Two actors: the client that signs the pledge, and the relay
    //    that receives it.
    let client = Wallet::generate();
    let relay_addr = Address::from_hex(RELAY_ADDRESS).expect("valid hex");

    println!("client   : {}", client.public().address());
    println!("relay    : {relay_addr}");
    println!("chain_id : {CHAIN_ID}\n");

    // 2. Client composes a pledge for 1.50 USDC.
    let nonce_hex: String = (0..64)
        .map(|i| match i % 2 {
            0 => format!("{:x}", (i / 2) % 16),
            _ => "0".to_string(),
        })
        .collect();
    let body = Pledge::body(
        CHAIN_ID,
        USDC_CONTRACT.into(),
        USDC_CONTRACT.into(),
        1_500_000, // 1.50 USDC
        relay_addr,
        nonce_hex,
        chrono::Utc::now().timestamp() + 1800, // 30 minutes
    )
    .expect("valid body");
    let pledge = Pledge::sign(body, &client).expect("sign");
    println!(
        "signed pledge: amount={}, expiry={}",
        pledge.amount_atomic, pledge.expiry_unix
    );

    // 3. The relay verifies the pledge for its own chain. This walks
    //    the EIP-191 signature, recovers the pledgor, and confirms the
    //    expiry is in the future.
    let now = chrono::Utc::now().timestamp();
    pledge
        .verify_for_relay(now, CHAIN_ID)
        .expect("relay accepts");
    println!("relay verified: ok");

    // 4. The relay may pass the URL through a QR code (so the human
    //    can audit it). The QR-decoded URL re-parses cleanly.
    let url = pledge.to_url();
    println!("\nurl: {url}\n");
    let parsed = Pledge::from_url(&url).expect("parse url");
    let decoded_signer = parsed
        .verify_with_recovered(now)
        .expect("verify with recovered");
    println!("qr-decoded signer: {decoded_signer}");
    assert_eq!(decoded_signer, client.public().address());

    // 5. A pledge signed by a different client would recover to a
    //    different signer.
    let other = Wallet::generate();
    let bad_body = Pledge::body(
        CHAIN_ID,
        USDC_CONTRACT.into(),
        USDC_CONTRACT.into(),
        1_500_000,
        relay_addr,
        "ab".repeat(32),
        now + 1800,
    )
    .expect("valid body");
    let bad = Pledge::sign(bad_body, &other).expect("sign");
    let bad_url = bad.to_url();
    let parsed_bad = Pledge::from_url(&bad_url).expect("parse url");
    let other_recovered = parsed_bad
        .verify_with_recovered(now)
        .expect("verify");
    println!("second pledge recovered: {other_recovered}");
    assert_eq!(other_recovered, other.public().address());
    assert_ne!(other_recovered, client.public().address());
    println!("second pledge recovered distinct signer: ok");
}
