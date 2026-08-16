# `a3net-wallet-evm`

Read-only EVM JSON-RPC client built on [`alloy`](https://github.com/alloy-rs/alloy) 2.1.x,
for A3Net node wallets.

## Status

**Phase 1 — read-only.** No transaction signing, no transaction
broadcast. Sending signed transactions belongs in a future
`a3net-wallet-tx` crate that imports this one as its read backbone.

## What it gives you

Every A3Net node gets a typed interface to **read** state from any
EVM-compatible chain (Ethereum mainnet, Sepolia, Polygon, Base,
Arbitrum, Optimism, etc.) without pulling in any chain-specific
binary. Concretely:

| Function                | RPC method                | Returns                  |
|-------------------------|---------------------------|--------------------------|
| `block_number`          | `eth_blockNumber`         | `u64`                    |
| `gas_price`             | `eth_gasPrice`            | `u128` wei               |
| `balance_of`            | `eth_getBalance`          | `U256` wei               |
| `nonce_of`              | `eth_getTransactionCount` | `u64`                    |
| `erc20_balance_of`      | `eth_call` (balanceOf)    | `U256` token base units  |

All public functions take A3Net's own `WalletAddress` (the
crypto-free 20-byte type from `a3net-types`), not an `alloy::Address`.
This keeps callers from having to know which 20-byte type alloy uses.

## Quick start

```rust,no_run
use a3net_wallet_evm::{EvmChainClient, balance_of};
use a3net_types::WalletAddress;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = EvmChainClient::new("https://eth.llamarpc.com").await?;
    println!("chain id = {}", client.chain_id());
    println!("rpc url  = {}", client.rpc_url());

    let vitalik = WalletAddress::from_hex("0xd8dA6BF26964aF9d7eEd9e03E53415D37aA96045")?;
    let wei = balance_of(&client, vitalik).await?;
    println!("vitalik balance = {wei} wei");

    Ok(())
}
```

A runnable version of the snippet above lives in
[`examples/chain_liveness.rs`](examples/chain_liveness.rs).

## Error model

Every fallible function returns `Result<T, WalletError>` where
`WalletError` is one of four buckets:

| Variant    | Triggered by                                                     | Retry without changing the request? |
|------------|------------------------------------------------------------------|-------------------------------------|
| `Transport`| DNS / TCP / TLS / HTTP 5xx / backend gone                        | yes (transient)                      |
| `Rpc`      | JSON-RPC error response from server (e.g. `-32601 method not found`) | only if a backoff hint is present   |
| `Decode`   | server returned `null` for a non-nullable call, or bad JSON      | no (almost always a code bug)        |
| `Invalid`  | bad caller input, unsupported feature, local usage error          | no                                   |

Helpers `WalletError::is_permanent()` and `WalletError::is_network()`
make it cheap to fold this into a retry loop or a CLI message.

## Layering

```text
   a3net-cli / a3net-ipc-adapter (future Phase 2)
                  │
                  ▼
   a3net-wallet-evm        ← this crate
                  │
                  ├──► a3net-types     (WalletAddress — crypto-free address)
                  └──► alloy 2.1.1     (RPC types, hyper transport)
```

`a3net-identity::Wallet` is *not* a dependency of this crate yet.
Phase 2 (`a3net-wallet-tx`) will add it back to handle the signing
half of the workflow.

## Design notes

### Why `hyper` transport (not `reqwest`)

Alloy 2.1.x's default `reqwest` feature pins `reqwest = "0.13"`. The
A3Net workspace is on `reqwest = "0.12"` with 30+ call sites
depending on it, so opting into the default features would force a
workspace-wide `reqwest` bump. We instead select the `hyper`
feature, which uses `hyper-util` (already in the tree via
`a3net-gateway`) and avoids touching `reqwest`.

In alloy 2.1.x `RootProvider::new_http` is `#[cfg(feature = "reqwest")]`,
so the hyper path is wired by hand in
[`provider::build_hyper_provider`](src/provider.rs):

```text
HyperTransport::new_hyper(url) → RpcClient::new(...) → RootProvider::new(rpc)
```

`RootProvider` then takes a single generic — `RootProvider<Ethereum>` —
because the transport is held as a field, not as a generic parameter.

### Why a bare `RootProvider` (not a `ProviderBuilder`)

`ProviderBuilder::connect_http` returns a *filler-stacked* provider
that auto-fills nonces, gas prices, and chain IDs on every call.
That's the right shape for transaction *signing*, but for a read-only
client it adds overhead and pulls in filler crates. We hand-pick a
bare provider instead, which keeps the dep graph and the runtime
footprint flat.

### Why MSRV `=2.1.1` (not the latest `2.4.x`)

Alloy 2.2+ raised its MSRV to 1.94.1. A3Net's workspace MSRV is 1.91.
We pin to `=2.1.1` so we stay compilable on the current toolchain.
When the workspace bumps MSRV to ≥1.94 this pin can be relaxed.

### Why `sol!` for the ERC-20 call

We use `alloy_sol_types::sol!` to generate the typed call struct
(`IERC20::balanceOfCall`). The `sol!` macro is **a proc-macro**
(re-exported from `alloy-sol-macro`); we declare that dependency
explicitly in `Cargo.toml` so the build doesn't fail with an
unresolved-macro error. Generating the call type at compile time
gives us a typed `IERC20::balanceOfCall { account }` whose
`.abi_encode()` matches the hand-rolled selector + padded-address
bytes bit-for-bit, which keeps the integration test (`tests/integration.rs`)
straightforward.

## Tests

```bash
cargo test -p a3net-wallet-evm
```

The unit tests under `src/` cover the local-only paths (URL parsing,
uint256 decoding, ABI encoding sanity, error classification). The
end-to-end read path against an in-process axum-based JSON-RPC stub
lives in [`tests/integration.rs`](tests/integration.rs).