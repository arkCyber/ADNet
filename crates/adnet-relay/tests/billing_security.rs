//! End-to-end security tests for the billing endpoints.
//!
//! These tests stand up a real relay + billing stack and exercise the
//! security-critical paths identified in the original audit:
//!
//! - nonce replay (same pledgor, same nonce, second pledge rejected)
//! - receipt replay (same receipt, second redeem rejected)
//! - same nonce across different pledgors (allowed)
//! - concurrent pledges / redeems don't race past the invariants
//! - pledged-and-redeemed nonce cannot be re-pledged
//!
//! Run with:
//!
//! ```text
//! cargo test -p adnet-relay --features billing --test billing_security
//! ```

#![cfg(feature = "billing")]

use std::sync::Arc;

use adnet_identity::{Treasury, Wallet};
use adnet_relay::{BillingMode, BillingState, BillingStore, RelayServer};
use adnet_token::{Pledge, Receipt};

const FIXED_NONCE: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn new_pledge(pledgor: &Wallet, relay: &Wallet, nonce: &str, amount: u128, expiry: i64) -> Pledge {
    let body = Pledge::body(
        1,
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
        amount,
        relay.public().address(),
        nonce.to_string(),
        expiry,
    )
    .expect("body");
    Pledge::sign(body, pledgor).expect("sign")
}

async fn start_relay(wallet: Arc<Wallet>) -> adnet_relay::RelayServerHandle {
    let state = Arc::new(BillingState::default());
    let mode = BillingMode::Enabled {
        wallet: wallet.clone(),
        state: state.clone(),
    };
    RelayServer::start("127.0.0.1", 0, mode)
        .await
        .expect("relay start")
}

#[tokio::test]
async fn duplicate_pledge_rejected() {
    let relay = Arc::new(Wallet::generate());
    let handle = start_relay(relay.clone()).await;
    let pledgor = Wallet::generate();
    let pledge = new_pledge(
        &pledgor,
        &relay,
        FIXED_NONCE,
        1_000_000,
        chrono::Utc::now().timestamp() + 3600,
    );

    let client = reqwest::Client::new();
    let url = format!("{}/relay/billing/v1/pledge", handle.base_url);

    let r1 = client
        .post(&url)
        .body(pledge.to_url())
        .send()
        .await
        .expect("pledge 1");
    assert!(r1.status().is_success(), "first pledge should succeed");

    let r2 = client
        .post(&url)
        .body(pledge.to_url())
        .send()
        .await
        .expect("pledge 2");
    assert_eq!(
        r2.status().as_u16(),
        400,
        "second pledge with same (pledgor, nonce) must be rejected"
    );
    let body = r2.text().await.unwrap();
    assert!(
        body.contains("pledge already exists"),
        "expected duplicate-pledge error, got: {body}"
    );
    handle.shutdown();
}

#[tokio::test]
async fn same_nonce_different_pledgor_accepted() {
    let relay = Arc::new(Wallet::generate());
    let handle = start_relay(relay.clone()).await;
    let p1 = Wallet::generate();
    let p2 = Wallet::generate();

    let pledge1 = new_pledge(
        &p1,
        &relay,
        FIXED_NONCE,
        1_000_000,
        chrono::Utc::now().timestamp() + 3600,
    );
    let pledge2 = new_pledge(
        &p2,
        &relay,
        FIXED_NONCE,
        1_000_000,
        chrono::Utc::now().timestamp() + 3600,
    );

    let client = reqwest::Client::new();
    let url = format!("{}/relay/billing/v1/pledge", handle.base_url);
    let r1 = client
        .post(&url)
        .body(pledge1.to_url())
        .send()
        .await
        .expect("p1");
    let r2 = client
        .post(&url)
        .body(pledge2.to_url())
        .send()
        .await
        .expect("p2");
    assert!(r1.status().is_success(), "p1 pledge");
    assert!(
        r2.status().is_success(),
        "p2 pledge (same nonce, different pledgor)"
    );
    handle.shutdown();
}

#[tokio::test]
async fn duplicate_redeem_rejected() {
    let relay = Arc::new(Wallet::generate());
    let handle = start_relay(relay.clone()).await;
    let pledgor = Wallet::generate();
    let pledge = new_pledge(
        &pledgor,
        &relay,
        FIXED_NONCE,
        1_000_000,
        chrono::Utc::now().timestamp() + 3600,
    );
    let receipt = Receipt::issue(&pledge, 500_000, &relay).expect("issue");

    let client = reqwest::Client::new();
    let pledge_url = format!("{}/relay/billing/v1/pledge", handle.base_url);
    let redeem_url = format!("{}/relay/billing/v1/redeem", handle.base_url);

    // Pledge first so the relay has an open balance.
    let rp = client
        .post(&pledge_url)
        .body(pledge.to_url())
        .send()
        .await
        .expect("pledge");
    assert!(rp.status().is_success(), "pledge should succeed");

    let r1 = client
        .post(&redeem_url)
        .json(&receipt)
        .send()
        .await
        .expect("redeem 1");
    let s1 = r1.status();
    assert!(
        s1.is_success(),
        "first redeem should succeed, got {s1}: {}",
        r1.text().await.unwrap_or_default()
    );

    let r2 = client
        .post(&redeem_url)
        .json(&receipt)
        .send()
        .await
        .expect("redeem 2");
    assert_eq!(
        r2.status().as_u16(),
        400,
        "second redeem of the same receipt must be rejected"
    );
    let body = r2.text().await.unwrap();
    assert!(
        body.contains("already redeemed"),
        "expected replay error, got: {body}"
    );
    handle.shutdown();
}

#[tokio::test]
async fn re_pledging_after_redeem_does_not_double_count() {
    // The original audit found a "faucet" attack: pledge-amount X
    // redeem-receipt, then re-pledge with the same nonce, the open
    // balance gets +X again. The fix is to reject the second pledge.
    let relay = Arc::new(Wallet::generate());
    let handle = start_relay(relay.clone()).await;
    let pledgor = Wallet::generate();
    let pledge = new_pledge(
        &pledgor,
        &relay,
        FIXED_NONCE,
        1_000_000,
        chrono::Utc::now().timestamp() + 3600,
    );
    let receipt = Receipt::issue(&pledge, 500_000, &relay).expect("issue");

    let client = reqwest::Client::new();
    let pledge_url = format!("{}/relay/billing/v1/pledge", handle.base_url);
    let redeem_url = format!("{}/relay/billing/v1/redeem", handle.base_url);
    let status_url = format!("{}/relay/billing/v1/status", handle.base_url);

    // 1. Pledge.
    let r = client
        .post(&pledge_url)
        .body(pledge.to_url())
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());

    // 2. Redeem the receipt.
    let r = client
        .post(&redeem_url)
        .json(&receipt)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());

    // 3. Re-pledge with the same nonce — must be rejected.
    let r = client
        .post(&pledge_url)
        .body(pledge.to_url())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);

    // 4. Status should reflect the redeemed state, not 2x the open balance.
    let status: serde_json::Value = client
        .get(&status_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["open_amount_atomic"].as_u64().unwrap(), 500_000);
    assert_eq!(status["redeemed_receipts"].as_u64().unwrap(), 1);
    handle.shutdown();
}

#[tokio::test]
async fn concurrent_pledges_one_wins() {
    // Two concurrent pledges with the same (pledgor, nonce) — exactly
    // one must succeed.
    let relay = Arc::new(Wallet::generate());
    let handle = start_relay(relay.clone()).await;
    let pledgor = Arc::new(Wallet::generate());
    let pledge = Arc::new(new_pledge(
        &pledgor,
        &relay,
        FIXED_NONCE,
        1_000_000,
        chrono::Utc::now().timestamp() + 3600,
    ));

    let client = Arc::new(reqwest::Client::new());
    let url = format!("{}/relay/billing/v1/pledge", handle.base_url);
    let mut handles = Vec::new();
    for _ in 0..8 {
        let c = client.clone();
        let u = url.clone();
        let p = pledge.to_url();
        handles.push(tokio::spawn(async move {
            c.post(&u).body(p).send().await.unwrap().status().as_u16()
        }));
    }
    let mut successes = 0;
    for h in handles {
        let s = h.await.unwrap();
        if s == 200 {
            successes += 1;
        } else {
            assert_eq!(s, 400, "unexpected status");
        }
    }
    assert_eq!(
        successes, 1,
        "exactly one of 8 concurrent pledges should succeed"
    );
    handle.shutdown();
}

#[tokio::test]
async fn status_includes_redeemed_count() {
    let relay = Arc::new(Wallet::generate());
    let handle = start_relay(relay.clone()).await;
    let pledgor = Wallet::generate();
    let pledge = new_pledge(
        &pledgor,
        &relay,
        FIXED_NONCE,
        1_000_000,
        chrono::Utc::now().timestamp() + 3600,
    );
    let receipt = Receipt::issue(&pledge, 200_000, &relay).expect("issue");

    let client = reqwest::Client::new();
    let status_url = format!("{}/relay/billing/v1/status", handle.base_url);

    // Initial state should have zero open and zero redeemed.
    let s: serde_json::Value = client
        .get(&status_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["open_amount_atomic"].as_u64().unwrap(), 0);
    assert_eq!(s["redeemed_receipts"].as_u64().unwrap(), 0);

    client
        .post(format!("{}/relay/billing/v1/pledge", handle.base_url))
        .body(pledge.to_url())
        .send()
        .await
        .unwrap();
    client
        .post(format!("{}/relay/billing/v1/redeem", handle.base_url))
        .json(&receipt)
        .send()
        .await
        .unwrap();

    let s: serde_json::Value = client
        .get(&status_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["open_amount_atomic"].as_u64().unwrap(), 800_000);
    assert_eq!(s["redeemed_receipts"].as_u64().unwrap(), 1);
    handle.shutdown();
}

#[tokio::test]
async fn from_treasury_still_works() {
    // Sanity check: the BillingMode::from_treasury path still works
    // after the BillingState refactor.
    let (treasury, _secret) = Treasury::new();
    let treasury = Arc::new(treasury);
    let mode = BillingMode::from_treasury(treasury.clone()).expect("from_treasury");
    let handle = RelayServer::start("127.0.0.1", 0, mode)
        .await
        .expect("start");
    let client = reqwest::Client::new();
    let r = client
        .get(format!("{}/health", handle.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    handle.shutdown();
}

// Tracking-store test: prove that a custom BillingStore implementation
// plugged in via `BillingMode::from_wallet_and_store` actually receives
// the relay's calls.
#[tokio::test]
async fn custom_store_receives_calls() {
    use adnet_relay::PledgeKey;
    use std::sync::Mutex;

    #[derive(Default)]
    struct CountingStore {
        insert_open_count: Mutex<u64>,
        sub_open_count: Mutex<u64>,
        mark_redeemed_count: Mutex<u64>,
    }

    #[async_trait::async_trait]
    impl BillingStore for CountingStore {
        async fn get_open(&self, _key: &PledgeKey) -> Option<u128> {
            Some(1_000_000)
        }
        async fn insert_open(&self, _key: PledgeKey, _amount: u128) -> Result<(), String> {
            *self.insert_open_count.lock().unwrap() += 1;
            Ok(())
        }
        async fn sub_open(&self, _key: &PledgeKey, _charged: u128) -> Result<(), String> {
            *self.sub_open_count.lock().unwrap() += 1;
            Ok(())
        }
        async fn mark_redeemed(&self, _key: &PledgeKey) -> Result<(), String> {
            *self.mark_redeemed_count.lock().unwrap() += 1;
            Ok(())
        }
        async fn restore_open(&self, _key: &PledgeKey, _amount: u128) -> Result<(), String> {
            Ok(())
        }
        async fn is_redeemed(&self, _key: &PledgeKey) -> bool {
            false
        }
        async fn total_open(&self) -> u128 {
            0
        }
        async fn redeemed_count(&self) -> u64 {
            0
        }
    }

    let store = Arc::new(CountingStore::default());
    let wallet = Arc::new(Wallet::generate());
    let mode = BillingMode::from_wallet_and_store(wallet.clone(), store.clone());
    let handle = RelayServer::start("127.0.0.1", 0, mode)
        .await
        .expect("start");

    let pledgor = Wallet::generate();
    let pledge = new_pledge(
        &pledgor,
        &wallet,
        FIXED_NONCE,
        1_000_000,
        chrono::Utc::now().timestamp() + 3600,
    );
    let receipt = Receipt::issue(&pledge, 200_000, &wallet).expect("issue");

    let client = reqwest::Client::new();
    client
        .post(format!("{}/relay/billing/v1/pledge", handle.base_url))
        .body(pledge.to_url())
        .send()
        .await
        .unwrap();
    client
        .post(format!("{}/relay/billing/v1/redeem", handle.base_url))
        .json(&receipt)
        .send()
        .await
        .unwrap();

    assert_eq!(*store.insert_open_count.lock().unwrap(), 1);
    assert_eq!(*store.sub_open_count.lock().unwrap(), 1);
    assert_eq!(*store.mark_redeemed_count.lock().unwrap(), 1);

    handle.shutdown();
}

/// V4-deferred pledge/redeem 交错并发测试:
/// 8 个并发 pledge(全部成功,因为 nonce 不同)+ 8 个并发 redeem
/// (每个 redeem 一个 receipt),交错执行。断言:
/// - 所有 pledge 成功,`open_balance` 等于 Σ pledged
/// - 所有 redeem 成功,`redeemed_count` 等于 receipts 总数
/// - `open_balance` 守恒 = Σ pledged − Σ redeemed
///
/// 这个测试的目的是覆盖 pledge 与 redeem 同时进行时的并发模型
/// 失效模式:如果 `mark_redeemed` 与 `sub_open` 之间的窗口期没有
/// 正确的锁顺序,可能会出现 `redeemed_count` 已增但 `open_balance`
/// 还没扣减(或反之),导致 `/status` 在并发窗口内返回不一致。
#[tokio::test]
async fn concurrent_pledge_redeem_interleaved_keeps_invariants() {
    let relay = Arc::new(Wallet::generate());
    let handle = start_relay(relay.clone()).await;

    const N: usize = 8;
    let amount: u128 = 1_000_000;
    let charged: u128 = 400_000;
    let expiry = chrono::Utc::now().timestamp() + 3600;

    // Generate N distinct (nonce, pledge, receipt) tuples.
    // We use distinct nonces AND distinct pledgors so we exercise
    // the full "8 independent pledgers, each pledging a distinct
    // nonce, then redeeming their own receipt" path — without
    // distinct nonces we'd be testing only the
    // "different-pledgor-same-nonce" carve-out.
    let mut tuples = Vec::with_capacity(N);
    for i in 0..N {
        let pledgor = Wallet::generate();
        // Pad to 64 hex chars; the high bytes encode `i` so each
        // nonce is unique.
        let nonce = format!("{:064x}", i as u128);
        assert_eq!(nonce.len(), 64, "nonce must be exactly 64 hex chars");
        let pledge = new_pledge(&pledgor, &relay, &nonce, amount, expiry);
        let receipt = Receipt::issue(&pledge, charged, &relay).expect("issue");
        tuples.push((pledgor, pledge, receipt));
    }

    let client = Arc::new(reqwest::Client::new());
    let pledge_url = format!("{}/relay/billing/v1/pledge", handle.base_url);
    let redeem_url = format!("{}/relay/billing/v1/redeem", handle.base_url);
    let status_url = format!("{}/relay/billing/v1/status", handle.base_url);

    // Interleave: for each tuple, spawn pledge + redeem together,
    // so we have N×2 tasks running concurrently. The relay's
    // pledge handler and redeem handler each take separate locks,
    // and we want to detect any interleaving that breaks
    // ledger consistency.
    let mut joins = Vec::with_capacity(N * 2);
    for (_pledgor, pledge, receipt) in &tuples {
        let c1 = client.clone();
        let u1 = pledge_url.clone();
        let p = pledge.to_url();
        joins.push(tokio::spawn(async move {
            c1.post(&u1).body(p).send().await.unwrap().status().as_u16()
        }));
        let c2 = client.clone();
        let u2 = redeem_url.clone();
        let r = receipt.clone();
        joins.push(tokio::spawn(async move {
            c2.post(&u2)
                .json(&r)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }));
    }

    let mut pledge_ok = 0usize;
    let mut redeem_ok = 0usize;
    for (i, j) in joins.into_iter().enumerate() {
        let s = j.await.unwrap();
        // Every pledge / redeem for a *distinct* nonce should
        // succeed. The (pledgor, nonce) pair is unique per task.
        assert_eq!(s, 200, "task #{i} returned status {s}");
        if i % 2 == 0 {
            pledge_ok += 1;
        } else {
            redeem_ok += 1;
        }
    }
    assert_eq!(pledge_ok, N, "all pledges should succeed");
    assert_eq!(redeem_ok, N, "all redeems should succeed");

    // Final invariant: open = N * amount - N * charged, redeemed = N.
    let status: serde_json::Value = client
        .get(&status_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let expected_open = (amount - charged) * (N as u128);
    assert_eq!(
        status["open_amount_atomic"].as_u64().unwrap(),
        expected_open as u64,
        "open balance must equal Σ pledged − Σ redeemed ({expected_open})"
    );
    assert_eq!(
        status["redeemed_receipts"].as_u64().unwrap(),
        N as u64,
        "redeemed count must equal number of receipts redeemed"
    );

    handle.shutdown();
}
