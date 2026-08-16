//! Integration tests for `a3net-wallet-evm`.
//!
//! These spin up a tiny in-process JSON-RPC server (axum) and exercise
//! the full read path against it. The server is intentionally minimal —
//! it answers each method with a canned response — but it goes through
//! the same `hyper`-backed transport the production client uses, so
//! any wiring mistakes in `provider.rs` / `read.rs` surface here.
//!
//! We deliberately **do not** depend on any external network: the
//! tests must pass with no internet access and no anvil binary.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use a3net_types::WalletAddress;
use a3net_wallet_evm::{
    EvmChainClient, WalletError,
    balance_of, block_number, erc20_balance_of, gas_price, nonce_of,
};
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use alloy_sol_types::sol;
use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
use serde_json::{Value, json};

// Mirror the IERC20 interface used in `read.rs`. We re-declare it here
// (rather than importing from the production module) because the
// production `sol!` invocation is private — and the whole point of this
// test is to verify that the *bytes* sent on the wire match the
// expected selector, independent of any in-process helper.
sol! {
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256);
    }
}

// -- Test fixture --------------------------------------------------------

/// Per-stub mutable state. We pass it into each handler via closure
/// capture (not axum's `with_state`) so the resulting router stays
/// `Router<()>` — `Router<S>` cannot be served by `axum::serve`,
/// which only accepts a `MakeService`.
#[derive(Clone, Default)]
struct StubState {
    /// Counter for assigning JSON-RPC request IDs so responses look
    /// properly framed to the alloy client.
    next_id: Arc<AtomicU64>,
}

impl StubState {
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// What behaviour the stub should emulate for the test that owns it.
#[derive(Clone, Copy, Debug, Default)]
struct StubOptions {
    /// If `true`, the stub returns a JSON-RPC "method not found" error
    /// for `eth_chainId` — used to exercise the client's startup-probe
    /// failure path.
    reject_chain_id: bool,
}

/// Default handler for every method our read-path tests exercise.
/// `state` is captured via closure so we don't need `with_state`.
async fn rpc_handler(state: StubState, req: Value) -> impl IntoResponse {
    let id = req
        .get("id")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| state.next_id());

    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");

    let payload = match method {
        "eth_chainId" => json!("0x1"), // mainnet
        "eth_blockNumber" => json!("0x10d6280"), // 17,654,400
        "eth_gasPrice" => json!("0x3b9aca00"), // 1 gwei
        "eth_getBalance" => json!("0x0de0b6b3a7640000"), // 1 ETH = 1e18 wei
        "eth_getTransactionCount" => json!("0x2a"), // 42
        "eth_call" => {
            // Verify the calldata: a 4-byte selector (70a08231) plus a
            // 32-byte padded address. We return a canned 1e18 uint256.
            validate_and_respond_balanceof(&req)
        }
        "web3_clientVersion" => json!("a3net-wallet-evm/test"),
        // Intentionally unsupported so we can exercise the
        // method-not-found error path in tests that ask for it.
        m => return method_not_found(m, id),
    };

    success(id, payload)
}

/// Confirm `eth_call` to `balanceOf(address)` arrives with the expected
/// 36-byte calldata, then return a canned 1e18 wei uint256.
fn validate_and_respond_balanceof(req: &Value) -> Value {
    let params = req.get("params").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let tx = params.first().cloned().unwrap_or(json!({}));
    let data_hex = tx
        .get("input")
        .or_else(|| tx.get("data"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Strip 0x prefix.
    let raw = data_hex.strip_prefix("0x").unwrap_or(data_hex);
    let bytes = match hex::decode(raw) {
        Ok(b) => b,
        Err(_) => return json!("0x"),
    };

    assert_eq!(
        &bytes[..4],
        &[0x70, 0xa0, 0x82, 0x31],
        "balanceOf selector mismatch"
    );
    assert_eq!(bytes.len(), 36, "balanceOf calldata should be 36 bytes");
    // Return 1e18 (0x0de0b6b3a7640000) left-padded to 32 bytes.
    json!("0x0000000000000000000000000000000000000000000000000de0b6b3a7640000")
}

/// Build a JSON-RPC success envelope.
fn success(id: u64, result: Value) -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        })),
    )
}

/// Build a JSON-RPC "method not found" error response.
fn method_not_found(method: &str, id: u64) -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("method not found: {method}"),
            }
        })),
    )
}

async fn spawn_stub() -> String {
    // Capture the stub state in the closure so we don't need `with_state`,
    // which would yield a `Router<StubState>` that `axum::serve` cannot
    // accept. The router below is `Router<()>` and is convertible into a
    // `MakeService` via `into_make_service()`.
    let state = StubState::default();
    let app = Router::new().route(
        "/",
        post(move |Json(req): Json<Value>| async move {
            rpc_handler(state, req).await
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .ok();
    });
    format!("http://{addr}")
}

/// Spawn a stub with non-default behaviour. Currently only
/// `reject_chain_id` is honoured; we keep the struct open so future
/// tests (custom block numbers, etc.) can extend it without churn.
async fn spawn_stub_with_options(opts: StubOptions) -> String {
    // Same trick as `spawn_stub`: capture state via closure, keep router
    // stateless so `axum::serve` can consume it.
    let state = StubState::default();
    let app = Router::new().route(
        "/",
        post(move |Json(req): Json<Value>| async move {
            let id = req
                .get("id")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| state.next_id());
            let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
            let payload = match method {
                m if opts.reject_chain_id && m == "eth_chainId" => {
                    return method_not_found(m, id);
                }
                "eth_chainId" => json!("0x1"),
                "eth_blockNumber" => json!("0x10d6280"),
                "eth_gasPrice" => json!("0x3b9aca00"),
                "eth_getBalance" => json!("0x0de0b6b3a7640000"),
                "eth_getTransactionCount" => json!("0x2a"),
                "eth_call" => validate_and_respond_balanceof(&req),
                "web3_clientVersion" => json!("a3net-wallet-evm/test"),
                m => return method_not_found(m, id),
            };
            success(id, payload)
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .ok();
    });
    format!("http://{addr}")
}

// -- Tests ----------------------------------------------------------------

#[tokio::test]
async fn chain_id_is_probed_at_startup() {
    let url = spawn_stub().await;
    let client = EvmChainClient::new(&url).await.unwrap();
    assert_eq!(client.chain_id(), 1);
    // `HyperTransport::new_hyper` normalises the URL by appending a
    // trailing slash if absent, so we compare against `url + "/"`
    // rather than the raw URL we passed in.
    let mut expected = url.clone();
    if !expected.ends_with('/') {
        expected.push('/');
    }
    assert_eq!(client.rpc_url(), expected.as_str());
}

#[tokio::test]
async fn chain_id_refetch_returns_same_value() {
    let url = spawn_stub().await;
    let client = EvmChainClient::new(&url).await.unwrap();
    let fresh = client.fetch_chain_id().await.unwrap();
    assert_eq!(fresh, client.chain_id());
}

#[tokio::test]
async fn block_number_decodes_u64() {
    let url = spawn_stub().await;
    let client = EvmChainClient::new(&url).await.unwrap();
    let n = block_number(&client).await.unwrap();
    assert_eq!(n, 17_654_400);
}

#[tokio::test]
async fn gas_price_decodes_u128() {
    let url = spawn_stub().await;
    let client = EvmChainClient::new(&url).await.unwrap();
    let p = gas_price(&client).await.unwrap();
    assert_eq!(p, 1_000_000_000);
}

#[tokio::test]
async fn balance_of_returns_u256() {
    let url = spawn_stub().await;
    let client = EvmChainClient::new(&url).await.unwrap();
    let addr = WalletAddress::from_bytes([0x42u8; 20]);
    let bal = balance_of(&client, addr).await.unwrap();
    assert_eq!(bal, U256::from(1_000_000_000_000_000_000u128));
}

#[tokio::test]
async fn nonce_of_returns_u64() {
    let url = spawn_stub().await;
    let client = EvmChainClient::new(&url).await.unwrap();
    let addr = WalletAddress::from_bytes([0x42u8; 20]);
    let n = nonce_of(&client, addr).await.unwrap();
    assert_eq!(n, 42);
}

#[tokio::test]
async fn erc20_balance_of_sends_correct_calldata_and_decodes() {
    let url = spawn_stub().await;
    let client = EvmChainClient::new(&url).await.unwrap();

    // USDC mainnet contract (just for the bytes; the stub ignores
    // the address and returns 1e18).
    let token = WalletAddress::from_hex("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48").unwrap();
    // vitalik.eth
    let holder = WalletAddress::from_hex("0xd8da6bf26964af9d7eed9e03e53415d37aa96045").unwrap();

    let bal = erc20_balance_of(&client, token, holder).await.unwrap();
    assert_eq!(bal, U256::from(1_000_000_000_000_000_000u128));
}

#[tokio::test]
async fn jsonrpc_error_response_maps_to_rpc_bucket() {
    // Spawn a stub that rejects `eth_chainId`. The client's startup
    // probe will fail with a JSON-RPC error, which we expect to land
    // in the `Rpc` bucket.
    let url = spawn_stub_with_options(StubOptions { reject_chain_id: true }).await;
    let err = EvmChainClient::new(&url).await.unwrap_err();
    match err {
        WalletError::Rpc(msg) => {
            assert!(msg.contains("method not found"), "msg was: {msg}");
        }
        other => panic!("expected WalletError::Rpc, got {other:?}"),
    }
}

#[tokio::test]
async fn invalid_url_rejected_before_network_io() {
    let err = EvmChainClient::new("not a url").await.unwrap_err();
    assert!(matches!(err, WalletError::Invalid(_)));
}

#[tokio::test]
async fn unreachable_host_maps_to_transport_bucket() {
    // Port 1 is reserved and almost certainly unbound on the test
    // host. We don't *guarantee* the OS will refuse the connect (some
    // kernels return a different error) but on macOS / Linux this
    // surfaces as ECONNREFUSED → TransportErrorKind::Custom →
    // WalletError::Transport.
    let client = EvmChainClient::new("http://127.0.0.1:1").await;
    let err = client.unwrap_err();
    // The exact error class may be `Transport` or `Rpc` depending on
    // the OS's behavior; both are acceptable for an unreachable host.
    assert!(
        matches!(err, WalletError::Transport(_) | WalletError::Rpc(_)),
        "expected network error, got {err:?}"
    );
    assert!(err.is_network());
}

#[tokio::test]
async fn client_is_clone_shares_inner_state() {
    let url = spawn_stub().await;
    let a = EvmChainClient::new(&url).await.unwrap();
    let b = a.clone();
    assert_eq!(a.chain_id(), b.chain_id());
    assert_eq!(a.rpc_url(), b.rpc_url());
    // Two separate `block_number` calls share the same inner provider
    // (no panics, both return identical values).
    assert_eq!(
        block_number(&a).await.unwrap(),
        block_number(&b).await.unwrap()
    );
}

#[tokio::test]
async fn erc20_calldata_matches_generated_encoding() {
    // Cross-check: the bytes our crate *would* send for a given holder
    // address must be byte-identical to what a hand-rolled
    // IERC20::balanceOf(holder).abi_encode() produces. If anything
    // changes in `sol!`'s output between alloy versions, the `erc20`
    // integration test above will catch it, but this test pins the
    // expectation explicitly.
    let mut holder_bytes = [0u8; 20];
    holder_bytes.copy_from_slice(
        &hex::decode("d8da6bf26964af9d7eed9e03e53415d37aa96045").unwrap(),
    );
    let holder = Address::from(holder_bytes);

    let call = IERC20::balanceOfCall { account: holder };
    let encoded = call.abi_encode();

    assert_eq!(encoded.len(), 36);
    assert_eq!(&encoded[..4], &[0x70, 0xa0, 0x82, 0x31]);
    // Last 20 bytes are the holder address, with 12 leading zero bytes.
    assert!(encoded[4..16].iter().all(|b| *b == 0));
    assert_eq!(&encoded[16..], &holder_bytes);
}