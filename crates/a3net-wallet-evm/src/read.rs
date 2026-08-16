//! Read-only EVM JSON-RPC methods.
//!
//! Each free function takes a reference to an [`EvmChainClient`] (or to
//! its inner alloy provider) plus the request parameters and returns a
//! typed A3Net-friendly result.
//!
//! Methods exposed:
//!
//! | Function                       | RPC method                | Returns                  |
//! |--------------------------------|---------------------------|--------------------------|
//! | [`block_number`]               | `eth_blockNumber`         | `u64`                    |
//! | [`gas_price`]                  | `eth_gasPrice`            | `u128` wei               |
//! | [`balance_of`]                 | `eth_getBalance`          | `U256` wei               |
//! | [`nonce_of`]                   | `eth_getTransactionCount` | `u64`                    |
//! | [`erc20_balance_of`]           | `eth_call` (balanceOf)    | `U256` token base units  |
//!
//! All methods accept A3Net's own [`WalletAddress`] instead of an
//! `alloy::Address` — we do the conversion in one place so callers don't
//! need to know which 20-byte type alloy uses.
//!
//! ## ERC-20 balanceOf ABI encoding
//!
//! `IERC20::balanceOf(address)` is fixed:
//!
//! - 4-byte selector: `keccak256("balanceOf(address)")[:4]` = `0x70a08231`
//! - 32-byte word: address left-padded with zeros (high 12 bytes `0x00`,
//!   low 20 bytes the address itself)
//!
//! We use `alloy_sol_types::sol!` to generate the call struct. The
//! `sol!` macro is **a proc-macro** (re-exported from
//! `alloy-sol-macro`); we declare that dependency explicitly in
//! `Cargo.toml` so the build doesn't fail with an unresolved macro.
//! Generating the call type at compile time gives us a typed
//! `IERC20::balanceOfCall { account }` whose `.abi_encode()` matches
//! the hand-rolled selector+padded-address bytes bit-for-bit.

use alloy_primitives::{Address, U256};
use alloy_provider::Provider;
use alloy_rpc_types_eth::TransactionRequest;
use alloy_sol_types::SolCall;
use alloy_sol_types::sol;

use a3net_types::WalletAddress;

use crate::error::{WalletError, WalletResult};
use crate::provider::EvmChainClient;

// -- ABI: ERC-20 `balanceOf(address) view returns (uint256)` --------------
//
// The `sol!` invocation below expands at compile time to a module
// containing `IERC20::balanceOfCall` (the typed call struct) plus its
// selector, encoder, and decoder. We only use the encoder here; the
// response is decoded by hand because the single-uint256 return is
// trivial and we don't want to pull in extra `JsonAbiExt` machinery.
sol! {
    /// Single-function interface for ERC-20 `balanceOf`. Keeps the ABI
    /// surface to the one method we actually call.
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256);
    }
}

// -- Public read API -------------------------------------------------------

/// Current head block number (`eth_blockNumber`).
///
/// Returns the latest block number known to the connected node. Useful
/// for "is the chain alive?" smoke tests and as the basis for
/// `eth_getBalance` block-tagging in a future iteration.
pub async fn block_number(client: &EvmChainClient) -> WalletResult<u64> {
    let n = client
        .provider()
        .get_block_number()
        .await
        .map_err(WalletError::from)?;
    // `alloy_primitives::BlockNumber` is a type alias for `u64`, so
    // `n` is already a `u64` — no conversion needed. The block number
    // is in the latest safe / pending block by default.
    Ok(n)
}

/// Current gas price in wei (`eth_gasPrice`).
///
/// Returned as `u128` because that's what fits comfortably without a
/// bigint; on every EVM chain the gas price is well within `u128::MAX`.
pub async fn gas_price(client: &EvmChainClient) -> WalletResult<u128> {
    let p = client
        .provider()
        .get_gas_price()
        .await
        .map_err(WalletError::from)?;
    Ok(p)
}

/// Native (ETH/MATIC/etc.) balance of `addr` in wei (`eth_getBalance`).
///
/// "Wei" here means the smallest indivisible unit of the chain's
/// native asset — on Ethereum mainnet that is wei, on Polygon it's
/// also wei but the displayed unit is MATIC. The caller is responsible
/// for knowing the unit name; this function only reports the raw
/// `U256` count of base units.
pub async fn balance_of(
    client: &EvmChainClient,
    addr: WalletAddress,
) -> WalletResult<U256> {
    let alloy_addr = wallet_to_alloy(addr)?;
    client
        .provider()
        .get_balance(alloy_addr)
        .await
        .map_err(WalletError::from)
}

/// EVM account nonce — count of transactions sent from this address
/// (`eth_getTransactionCount`).
///
/// This is the *confirmed* nonce at the latest block; the pending
/// nonce may be higher if there are unconfirmed transactions in the
/// mempool. We do not currently expose a "pending" variant.
pub async fn nonce_of(client: &EvmChainClient, addr: WalletAddress) -> WalletResult<u64> {
    let alloy_addr = wallet_to_alloy(addr)?;
    let n = client
        .provider()
        .get_transaction_count(alloy_addr)
        .await
        .map_err(WalletError::from)?;
    // Already a `u64` (alloy's `TxNonce` is a `u64` alias).
    Ok(n)
}

/// ERC-20 token balance of `holder` against `token` (in token base units).
///
/// Implemented as an `eth_call` to `balanceOf(address)` — i.e. a
/// read-only call that costs no gas and never hits the mempool. The
/// return value is the token's smallest unit (typically 10^decimals);
/// a USDC contract with `decimals = 6` will return micro-USDC.
pub async fn erc20_balance_of(
    client: &EvmChainClient,
    token: WalletAddress,
    holder: WalletAddress,
) -> WalletResult<U256> {
    let token_addr = wallet_to_alloy(token)?;
    let holder_addr = wallet_to_alloy(holder)?;

    // Build the calldata via the typed IERC20 interface we declared
    // above. `balanceOfCall::abi_encode()` produces the same 36 bytes
    // we would get by hand-rolling the selector + padded address.
    let call = IERC20::balanceOfCall { account: holder_addr };
    let calldata: alloy_primitives::Bytes = call.abi_encode().into();

    // `eth_call` to the token contract at the latest block. We use
    // `pending` here (matching `Provider::call`'s default) so the
    // balance reflects any just-mined state; if a caller wants
    // historical balance they should reach for the alloy `EthCall`
    // builder directly.
    //
    // `TransactionRequest::input` takes a `TransactionInput` (not a
    // plain `Bytes`); the `From` impl converts for us.
    let tx = TransactionRequest::default()
        .to(token_addr)
        .input(calldata.into());

    let raw: alloy_primitives::Bytes = client
        .provider()
        .call(tx)
        .await
        .map_err(WalletError::from)?;

    // balanceOf returns a single uint256. ABI-encodes as a single
    // 32-byte big-endian word.
    decode_uint256(&raw)
}

// -- Internal helpers ------------------------------------------------------

/// Convert an A3Net [`WalletAddress`] (20 raw bytes, defined in
/// `a3net-types` to keep protocol crates crypto-free) into alloy's
/// `Address`. The bytes are the same length so this is a straight copy.
fn wallet_to_alloy(addr: WalletAddress) -> WalletResult<Address> {
    let mut bytes = [0u8; 20];
    bytes.copy_from_slice(addr.as_bytes());
    Ok(Address::from(bytes))
}

/// Decode a single `uint256` ABI return value.
///
/// ERC-20 `balanceOf` is documented to return exactly one `uint256`,
/// which ABI-encodes as a single 32-byte big-endian word. The response
/// is therefore expected to be exactly 32 bytes; anything else (a
/// reverter returning error data, a non-standard token) is a
/// [`WalletError::Decode`].
fn decode_uint256(raw: &[u8]) -> WalletResult<U256> {
    if raw.len() != 32 {
        return Err(WalletError::Decode(format!(
            "expected 32-byte uint256 return, got {} bytes",
            raw.len()
        )));
    }
    U256::try_from_be_slice(raw)
        .ok_or_else(|| WalletError::Decode("uint256 bytes out of range".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_uint256_accepts_32_bytes() {
        // 1 ETH = 1e18 wei = 0x0de0_b6b3_a764_0000 in the low 8 bytes.
        let mut input = [0u8; 32];
        input[24..32].copy_from_slice(&0x0de0_b6b3_a764_0000u64.to_be_bytes());
        let v = decode_uint256(&input).unwrap();
        assert_eq!(v, U256::from(1_000_000_000_000_000_000u128));
    }

    #[test]
    fn decode_uint256_rejects_wrong_length() {
        let short = [0u8; 31];
        let err = decode_uint256(&short).unwrap_err();
        assert!(matches!(err, WalletError::Decode(_)));

        let long = [0u8; 33];
        let err = decode_uint256(&long).unwrap_err();
        assert!(matches!(err, WalletError::Decode(_)));
    }

    #[test]
    fn decode_uint256_max_u64() {
        let mut input = [0u8; 32];
        // Top 24 bytes are zero, last 8 bytes = u64::MAX.
        input[24..32].copy_from_slice(&u64::MAX.to_be_bytes());
        let v = decode_uint256(&input).unwrap();
        assert_eq!(v, U256::from(u64::MAX));
    }

    #[test]
    fn wallet_to_alloy_preserves_bytes() {
        let mut bytes = [0u8; 20];
        bytes[0] = 0xAB;
        bytes[19] = 0xCD;
        let w = WalletAddress::from_bytes(bytes);
        let a = wallet_to_alloy(w).unwrap();
        assert_eq!(a.0.as_slice(), &bytes[..]);
    }

    #[test]
    fn erc20_balanceof_calldata_is_36_bytes() {
        // Sanity-check that the typed ABI encoding produces what we
        // expect: a 4-byte selector (`0x70a08231`) followed by the
        // 32-byte left-padded address.
        let mut addr_bytes = [0u8; 20];
        addr_bytes[19] = 0x42;
        let holder = Address::from(addr_bytes);
        let call = IERC20::balanceOfCall { account: holder };
        let bytes = call.abi_encode();
        assert_eq!(bytes.len(), 36);
        assert_eq!(&bytes[..4], &[0x70, 0xa0, 0x82, 0x31]);
        // Padded address: first 12 bytes zero (after the 4-byte selector),
        // last 20 bytes the address.
        assert!(bytes[4..16].iter().all(|b| *b == 0));
        // Last 20 bytes carry the holder address (little-endian on the wire
        // is big-endian per ABI encoding rules — these are raw bytes, not
        // hex strings, so we compare directly).
        assert_eq!(&bytes[16..], &addr_bytes[..]);
    }

    #[test]
    fn erc20_balanceof_selector_matches_keccak() {
        // The first 4 bytes of keccak256("balanceOf(address)") are
        // 0x70a08231. If a future refactor changes the macro input,
        // this test will catch the divergence.
        assert_eq!(
            IERC20::balanceOfCall::SELECTOR,
            [0x70, 0xa0, 0x82, 0x31],
        );
    }
}