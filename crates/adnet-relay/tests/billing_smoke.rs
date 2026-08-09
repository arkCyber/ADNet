//! Integration tests for the billing endpoints. Run with:
//!
//! ```text
//! cargo test -p adnet-relay --features billing --test billing_smoke
//! ```

#![cfg(feature = "billing")]

use std::sync::Arc;

use adnet_identity::{Treasury, Wallet};
use adnet_relay::{BillingMode, BillingState, RelayServer};
use adnet_token::{Pledge, Receipt};

/// 64 hex chars (32 bytes) of zeros — fine as a nonce, the bytes don't
/// need to be random for the protocol to work; they're just unique within
/// a pledgor's namespace.
const FIXED_NONCE: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn fixed_nonce() -> String {
    FIXED_NONCE.into()
}

#[tokio::test]
async fn pledge_then_redeem_round_trip() {
    // Spin up a relay with billing enabled.
    let relay_wallet = Arc::new(Wallet::generate());
    let mode = BillingMode::Enabled {
        wallet: relay_wallet.clone(),
        state: Arc::new(BillingState::default()),
    };
    // Port 0 → ephemeral port.
    let handle = RelayServer::start("127.0.0.1", 0, mode)
        .await
        .expect("relay start");
    let base = format!("{}/relay/billing", handle.base_url);
    let pledge_url = format!("{base}/v1/pledge");
    let redeem_url = format!("{base}/v1/redeem");
    let status_url = format!("{base}/v1/status");

    // Build a pledge from a different wallet, addressed to the relay.
    let pledgor = Wallet::generate();
    let body = Pledge::body(
        1, // mainnet
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
        1_000_000,
        relay_wallet.public().address(),
        fixed_nonce(),
        chrono::Utc::now().timestamp() + 3600,
    )
    .expect("body");
    let pledge = Pledge::sign(body, &pledgor).expect("sign");
    let url = pledge.to_url();

    // POST /v1/pledge
    let client = reqwest::Client::new();
    let resp = client
        .post(&pledge_url)
        .body(url)
        .send()
        .await
        .expect("pledge post");
    assert!(
        resp.status().is_success(),
        "pledge status: {}",
        resp.status()
    );

    // Issue a receipt from the relay wallet.
    let receipt = Receipt::issue(&pledge, 250_000, &relay_wallet).expect("issue");
    let resp = client
        .post(&redeem_url)
        .json(&receipt)
        .send()
        .await
        .expect("redeem post");
    assert!(
        resp.status().is_success(),
        "redeem status: {}",
        resp.status()
    );

    // GET /v1/status
    let status: serde_json::Value = client
        .get(&status_url)
        .send()
        .await
        .expect("status get")
        .json()
        .await
        .expect("status json");
    assert_eq!(status["open_amount_atomic"].as_u64().unwrap(), 750_000);
    assert_eq!(status["issued_receipts"].as_u64().unwrap(), 1);

    handle.shutdown();
}

#[tokio::test]
async fn rejects_pledge_addressed_to_other_relay() {
    let relay_wallet = Arc::new(Wallet::generate());
    let mode = BillingMode::Enabled {
        wallet: relay_wallet.clone(),
        state: Arc::new(BillingState::default()),
    };
    let handle = RelayServer::start("127.0.0.1", 0, mode)
        .await
        .expect("relay start");
    let url = format!("{}/relay/billing/v1/pledge", handle.base_url);

    let pledgor = Wallet::generate();
    let other_relay = Wallet::generate();
    let body = Pledge::body(
        1,
        "0x00".into(),
        "0x00".into(),
        100,
        other_relay.public().address(), // not us
        fixed_nonce(),
        chrono::Utc::now().timestamp() + 3600,
    )
    .expect("body");
    let pledge = Pledge::sign(body, &pledgor).expect("sign");

    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .body(pledge.to_url())
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status().as_u16(), 400);

    handle.shutdown();
}

#[tokio::test]
async fn from_treasury_uses_root_wallet() {
    // Build a treasury, hand it to the relay, sign receipts through the
    // treasury's root wallet, and confirm the relay accepts them.
    let (treasury_root, secret) = Treasury::new();
    let treasury = Arc::new(treasury_root);
    let mode = BillingMode::from_treasury(treasury.clone()).expect("treasury has root");
    let handle = RelayServer::start("127.0.0.1", 0, mode)
        .await
        .expect("relay start");

    // Pledge addressed to the treasury's root.
    let pledgor = Wallet::generate();
    let root_pubkey = treasury.root_public();
    let body = Pledge::body(
        1,
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
        500_000,
        root_pubkey.address(),
        fixed_nonce(),
        chrono::Utc::now().timestamp() + 3600,
    )
    .expect("body");
    let pledge = Pledge::sign(body, &pledgor).expect("sign");

    let base = format!("{}/relay/billing/v1", handle.base_url);
    let client = reqwest::Client::new();

    // Accept the pledge.
    let resp = client
        .post(format!("{base}/pledge"))
        .body(pledge.to_url())
        .send()
        .await
        .expect("pledge");
    assert!(
        resp.status().is_success(),
        "pledge status: {}",
        resp.status()
    );

    // Now issue a receipt using the *original* wallet the treasury was
    // built from — the relay must accept it because it signed with the
    // same root key.
    let relay_wallet = Wallet::from_bytes(&secret).expect("reload root");
    let receipt = Receipt::issue(&pledge, 100_000, &relay_wallet).expect("issue");
    let resp = client
        .post(format!("{base}/redeem"))
        .json(&receipt)
        .send()
        .await
        .expect("redeem");
    assert!(
        resp.status().is_success(),
        "redeem status: {}",
        resp.status()
    );

    handle.shutdown();
}

#[tokio::test]
async fn from_treasury_fails_when_root_missing() {
    // A treasury built from a view (no root) cannot be turned into
    // billing — the caller forgot to call `with_root`.
    let view = adnet_identity::TreasuryView {
        root_public: Wallet::generate().public().clone(),
        ephemeral: vec![],
    };
    let treasury = Arc::new(Treasury::from_view(view));
    let err = BillingMode::from_treasury(treasury).unwrap_err();
    assert!(err.to_string().contains("treasury root"));
}

#[tokio::test]
async fn rejects_orphan_receipt() {
    // A receipt that points at a nonce we've never seen (i.e. no
    // matching pledge was accepted) must be rejected. Otherwise the
    // operator would pay out for work that was never pledged.
    let relay_wallet = Arc::new(Wallet::generate());
    let mode = BillingMode::Enabled {
        wallet: relay_wallet.clone(),
        state: Arc::new(BillingState::default()),
    };
    let handle = RelayServer::start("127.0.0.1", 0, mode)
        .await
        .expect("relay start");

    // Build a pledge *and* sign a receipt — but **never POST** the
    // pledge, so the relay has no open balance for that nonce.
    let pledgor = Wallet::generate();
    let body = Pledge::body(
        1,
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
        1_000_000,
        relay_wallet.public().address(),
        fixed_nonce(),
        chrono::Utc::now().timestamp() + 3600,
    )
    .expect("body");
    let pledge = Pledge::sign(body, &pledgor).expect("sign");
    let receipt = Receipt::issue(&pledge, 100_000, &relay_wallet).expect("issue");

    let url = format!("{}/relay/billing/v1/redeem", handle.base_url);
    let resp = reqwest::Client::new()
        .post(url)
        .json(&receipt)
        .send()
        .await
        .expect("post");
    assert_eq!(
        resp.status().as_u16(),
        400,
        "orphan receipt must be rejected with 400"
    );

    handle.shutdown();
}

#[tokio::test]
async fn rejects_over_redeem() {
    // Try to redeem more than the open balance. The relay must reject
    // rather than silently clamp at 0 (which would let the relay pay
    // out more than was pledged).
    let relay_wallet = Arc::new(Wallet::generate());
    let mode = BillingMode::Enabled {
        wallet: relay_wallet.clone(),
        state: Arc::new(BillingState::default()),
    };
    let handle = RelayServer::start("127.0.0.1", 0, mode)
        .await
        .expect("relay start");
    let base = format!("{}/relay/billing", handle.base_url);

    let pledgor = Wallet::generate();
    let body = Pledge::body(
        1,
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
        1_000_000,
        relay_wallet.public().address(),
        fixed_nonce(),
        chrono::Utc::now().timestamp() + 3600,
    )
    .expect("body");
    let pledge = Pledge::sign(body, &pledgor).expect("sign");

    // Pledge 1,000,000 then try to redeem 1,500,000.
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/pledge"))
        .body(pledge.to_url())
        .send()
        .await
        .expect("pledge");
    assert!(resp.status().is_success());

    // `Receipt::issue` itself enforces `charged_atomic <= pledge.amount_atomic`
    // so we can't forge a receipt from the relay wallet for more than
    // was pledged. The exploit we still want to guard against is a
    // *legitimately-issued* receipt that's later redeemed twice; verify
    // the open balance tracking catches that.
    let receipt = Receipt::issue(&pledge, 700_000, &relay_wallet).expect("issue");
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/redeem"))
        .json(&receipt)
        .send()
        .await
        .expect("redeem 1");
    assert!(resp.status().is_success(), "first redeem should succeed");

    // Second redeem of the SAME receipt: the open balance is now
    // 300_000 and the receipt is for 700_000 — must be rejected.
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/redeem"))
        .json(&receipt)
        .send()
        .await
        .expect("redeem 2");
    assert_eq!(
        resp.status().as_u16(),
        400,
        "second redeem of the same receipt must fail"
    );

    handle.shutdown();
}
