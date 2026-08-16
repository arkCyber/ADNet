//! `a3net-wallet-evm` — read-only EVM JSON-RPC client for A3Net nodes.
//!
//! This crate gives every A3Net node the ability to *read* state from
//! any EVM-compatible chain (Ethereum mainnet, Sepolia, Polygon, Base,
//! Arbitrum, Optimism, etc.) without pulling in any chain-specific
//! binary. The connection is a plain HTTP JSON-RPC client built on the
//! [`alloy`] stack, **deliberately scoped to read-only operations**:
//!
//! - [`EvmChainClient::new`] dials a single RPC endpoint and probes
//!   `eth_chainId` once at startup.
//! - [`block_number`] / [`gas_price`] — chain liveness.
//! - [`balance_of`] — native (ETH-class) balance for an address.
//! - [`nonce_of`] — EVM transaction count.
//! - [`erc20_balance_of`] — ERC-20 `balanceOf` via `eth_call`.
//!
//! ## Why no signer / no `eth_sendRawTransaction`
//!
//! Transaction signing crosses the
//! `a3net_identity::Wallet` boundary. That coupling — and the
//! key-management policy it implies — belongs in a future
//! `a3net-wallet-tx` crate that *imports* this one as its read
//! backbone. Keeping signing out of Phase 1 lets us land the read path
//! end-to-end (CLI, IPC, FFI) without committing to a transaction
//! policy yet.
//!
//! ## Why `hyper` transport (not `reqwest`)
//!
//! `alloy-provider`'s default features pin `reqwest = "0.13"`. The
//! A3Net workspace is on `reqwest = "0.12"` with 30+ call sites
//! depending on it, so opting into the default features would force a
//! workspace-wide `reqwest` bump. We instead select the `hyper`
//! feature, which uses `hyper-util` (already in the tree via
//! `a3net-gateway`) and avoids touching `reqwest`. In alloy 2.1.x
//! `RootProvider::new_http` is reqwest-only, so we wire the transport
//! by hand in [`provider::build_hyper_provider`]:
//!
//! ```text
//!   HyperTransport::new_hyper(url) → RpcClient::new(...) → RootProvider::new(...)
//! ```
//!
//! ## Layering
//!
//! ```text
//!   a3net-cli / a3net-ipc-adapter (future)
//!                  │
//!                  ▼
//!   a3net-wallet-evm        ← this crate
//!                  │
//!                  ├──► a3net-types     (WalletAddress — crypto-free address)
//!                  └──► alloy 2.1.1     (RPC types, hyper transport)
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod chain;
pub mod error;
pub mod nonce_pool;
pub mod provider;
pub mod read;
pub mod tx;

pub use chain::{ChainInfo, KNOWN_CHAINS, by_chain_id, preferred_rpc, rpc_urls};
pub use error::{WalletError, WalletResult};
pub use nonce_pool::NoncePool;
pub use provider::EvmChainClient;
// Re-export the read-side API at the crate root for ergonomics —
// `a3net_wallet_evm::balance_of(...)` reads better than
// `a3net_wallet_evm::read::balance_of(...)`.
pub use read::{
    balance_of, block_number, erc20_balance_of, gas_price, nonce_of,
};
pub use tx::{
    Erc20Metadata, FeeEstimate1559, UnsignedTx1559, UnsignedTxLegacy,
    build_unsigned_request, build_unsigned_request_legacy, erc20_allowance,
    erc20_approve_request, erc20_metadata, erc20_transfer_request,
    estimate_fees_eip1559, estimate_gas, send_eip1559, send_legacy, sign_eip1559,
    sign_legacy, wait_for_receipt,
};