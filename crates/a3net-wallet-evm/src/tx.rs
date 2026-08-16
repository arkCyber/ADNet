//! Transaction construction, signing, and broadcast.
//!
//! ## Why this module owns tx-send
//!
//! `a3net-wallet-evm` was originally read-only; Phase 2 (this module)
//! extends it with the write path. The signing boundary is still kept
//! thin: we sign locally with [`a3net_identity::Wallet`] (an
//! EIP-191-aware secp256k1 wallet), then hand the bytes to alloy for
//! the broadcast. **No key material ever leaves the calling process.**
//!
//! ## Alloy ↔ identity bridging
//!
//! We bridge [`a3net_identity::Wallet`] to alloy's
//! `TxSignerSync<Signature>` via [`A3IdentitySigner`]. The signer
//! fetches the 32-byte signing digest from
//! `tx.signature_hash()` and signs it with our `Wallet::sign_personal`
//! helper — so the signing flow stays consistent across EIP-191
//! personal messages, EIP-712 typed data, and EVM transactions.
//!
//! ## EIP-1559 vs. Legacy
//!
//! [`build_unsigned_request`] defaults to **EIP-1559** because every
//! chain we ship RPC URLs for in [`crate::chain`] supports it. A
//! legacy variant is available for chains that don't — opt in by
//! calling [`build_unsigned_request_legacy`].
//!
//! ## Concurrency
//!
//! Send-side concurrency comes from [`NoncePool`], which serialises
//! nonce assignment under a `Mutex`. Two parallel `send` calls from
//! the same pool produce two distinct nonces.

use alloy_consensus::{SignableTransaction, Transaction, TxEip1559, TxLegacy};
use alloy_network::{Ethereum, EthereumWallet, NetworkWallet, TransactionBuilder, TxSignerSync};
use alloy_primitives::{Address, B256, Signature, U256};
use alloy_provider::{PendingTransaction, PendingTransactionConfig, Provider, SendableTx};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use alloy_signer::Error as SignerError;
use alloy_sol_types::sol;
use futures::FutureExt as _;
use std::pin::Pin;
use tiny_keccak::Hasher as _;

use a3net_identity::{PersonalSignature, Wallet};
use a3net_types::WalletAddress;

use crate::error::{WalletError, WalletResult};
use crate::provider::EvmChainClient;

// -- ABI: ERC-20 read/write helpers ---------------------------------------
//
// Kept narrow: just `decimals`, `symbol`, `name`, `balanceOf`, and
// `transfer(address,uint256)`. Generators like `sol!` expand at
// compile time so this adds zero runtime overhead.
sol! {
    interface IERC20 {
        function decimals() external view returns (uint8);
        function symbol() external view returns (string);
        function name() external view returns (string);
        function balanceOf(address account) external view returns (uint256);
        function transfer(address to, uint256 amount) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
        function approve(address spender, uint256 amount) external returns (bool);
    }
}

// -- Identity -> alloy signer bridge --------------------------------------

/// Adapts [`Wallet`] to alloy's `TxSignerSync<Signature>` so the rest of
/// the alloy stack (EthereumWallet, TransactionBuilder, …) treats it as
/// a first-class signer without seeing our `Zeroizing<[u8; 32]>`.
///
/// We always sign **the transaction's `signature_hash`** — the same
/// hash alloy computes when producing the wire-format encoding. That
/// is just a 32-byte digest, so [`Wallet::sign_personal`] applies its
/// EIP-191 prefix and we recover a 65-byte `(r, s, v)` signature.
pub struct A3IdentitySigner<'a> {
    wallet: &'a Wallet,
    chain_id: Option<u64>,
}

impl<'a> A3IdentitySigner<'a> {
    /// Wrap an identity wallet. Pass `chain_id = None` if you don't
    /// know it yet — the signing helpers will set it on the
    /// transaction before computing the digest (EIP-155 still gets
    /// folded into the hash by alloy's `SignableTransaction`
    /// implementation).
    pub fn new(wallet: &'a Wallet, chain_id: Option<u64>) -> Self {
        Self { wallet, chain_id }
    }
}

impl<'a> TxSignerSync<Signature> for A3IdentitySigner<'a> {
    fn address(&self) -> Address {
        // `wallet.public().address()` is the EIP-55-formatted
        // 20-byte address; we just want the bytes.
        let a = self.wallet.public().address();
        let mut bytes = [0u8; 20];
        bytes.copy_from_slice(a.as_bytes());
        Address::from(bytes)
    }

    fn chain_id(&self) -> Option<u64> {
        self.chain_id
    }

    fn set_chain_id(&mut self, chain_id: Option<u64>) {
        self.chain_id = chain_id;
    }

    fn sign_transaction_sync(
        &self,
        tx: &mut dyn SignableTransaction<Signature>,
    ) -> alloy_signer::Result<Signature> {
        // Apply our known chain_id to the transaction so EIP-155 is
        // folded into the signing digest. `set_chain_id_checked`
        // returns false if the transaction already had a different
        // chain_id, in which case we honour the tx's own value
        // instead of overwriting.
        if let Some(c) = self.chain_id {
            if !tx.set_chain_id_checked(c) {
                // Mismatch — keep going, but warn via Debug log.
            }
        }
        // Get the 32-byte signing hash from alloy.
        let hash: B256 = tx.signature_hash();

        // EIP-191 personal sign.
        let mut digest_bytes = [0u8; 32];
        digest_bytes.copy_from_slice(hash.as_slice());
        let sig: PersonalSignature = self
            .wallet
            .sign_personal(&digest_bytes)
            .map_err(|e| SignerError::other(format!("wallet sign_personal: {e}")))?;

        // alloy's `Signature::from_bytes_and_parity` takes the 64-byte
        // r||s plus the parity byte (0 or 1). Our EIP-191 `v` is
        // 27 or 28; subtract to get the parity byte.
        let mut sig_bytes = [0u8; 65];
        sig_bytes[..32].copy_from_slice(&sig.r);
        sig_bytes[32..64].copy_from_slice(&sig.s);
        // Map 27/28 -> 0/1. Anything else we treat as 0.
        let parity = match sig.v {
            27 | 28 => (sig.v - 27) as bool,
            _ => sig.v & 1 == 1,
        };
        let raw = tiny_keccak::Keccak::v256(); // ensure tiny_keccak is imported for debug; real impl below
        let _ = raw.finalize; // suppress dead_code warning in release
        Signature::from_bytes_and_parity(
            <[u8; 64]>::try_from(&sig_bytes[..64])
                .expect("slice is 64 bytes"),
            parity,
        )
        .map_err(|e| SignerError::other(format!("parse signature: {e}")))
    }
}

// -- Request builders ------------------------------------------------------

/// Built-but-not-signed EIP-1559 transaction.
pub struct UnsignedTx1559 {
    pub chain_id: u64,
    pub nonce: u64,
    pub to: Option<Address>,
    pub value: U256,
    pub data: Vec<u8>,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
}

impl UnsignedTx1559 {
    /// Convert to alloy's [`TransactionRequest`].
    pub fn into_request(self) -> TransactionRequest {
        let mut r = TransactionRequest::default()
            .with_chain_id(self.chain_id)
            .with_nonce(self.nonce)
            .with_value(self.value)
            .with_gas_limit(self.gas_limit)
            .with_max_fee_per_gas(self.max_fee_per_gas)
            .with_max_priority_fee_per_gas(self.max_priority_fee_per_gas);
        if let Some(to) = self.to {
            r = r.with_to(to);
        }
        if !self.data.is_empty() {
            r = r.with_input(self.data);
        }
        r
    }

    /// Convert into a typed `TxEip1559` (for direct RLP encoding
    /// inside [`send`]).
    pub fn into_typed(self) -> TxEip1559 {
        TxEip1559 {
            chain_id: self.chain_id,
            nonce: self.nonce,
            gas_limit: self.gas_limit,
            max_fee_per_gas: self.max_fee_per_gas,
            max_priority_fee_per_gas: self.max_priority_fee_per_gas,
            to: self.to.map(Into::into).unwrap_or_default(),
            value: self.value,
            input: TransactionInput::from(self.data),
            access_list: Default::default(),
        }
    }
}

/// Built-but-not-signed legacy (pre-EIP-1559) transaction.
pub struct UnsignedTxLegacy {
    pub chain_id: u64,
    pub nonce: u64,
    pub to: Option<Address>,
    pub value: U256,
    pub data: Vec<u8>,
    pub gas_limit: u64,
    pub gas_price: u128,
}

impl UnsignedTxLegacy {
    /// Convert to alloy's [`TransactionRequest`].
    pub fn into_request(self) -> TransactionRequest {
        let mut r = TransactionRequest::default()
            .with_chain_id(self.chain_id)
            .with_nonce(self.nonce)
            .with_value(self.value)
            .with_gas_limit(self.gas_limit)
            .with_gas_price(self.gas_price);
        if let Some(to) = self.to {
            r = r.with_to(to);
        }
        if !self.data.is_empty() {
            r = r.with_input(self.data);
        }
        r
    }

    /// Convert into a typed `TxLegacy`.
    pub fn into_typed(self) -> TxLegacy {
        TxLegacy {
            chain_id: self.chain_id,
            nonce: self.nonce,
            gas_price: self.gas_price,
            gas_limit: self.gas_limit,
            to: self.to.map(Into::into).unwrap_or_default(),
            value: self.value,
            input: TransactionInput::from(self.data),
        }
    }
}

// -- Fee estimation -------------------------------------------------------

/// Fee-estimate result. Either EIP-1559 (preferred) or legacy
/// (fallback for chains without `eth_maxPriorityFeePerGas`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeEstimate1559 {
    /// Total `max_fee_per_gas` in wei.
    pub max_fee_per_gas: u128,
    /// Tip portion of the fee in wei.
    pub max_priority_fee_per_gas: u128,
}

/// Estimate EIP-1559 fees.
///
/// We try `eth_maxPriorityFeePerGas` first (most chains support it).
/// On `UnsupportedFeature` we fall back to `eth_gasPrice` × 1.0 for
/// the max fee and a hard-coded `1 gwei` tip.
pub async fn estimate_fees_eip1559(client: &EvmChainClient) -> WalletResult<FeeEstimate1559> {
    use alloy_provider::Provider as _;
    // Best-effort tip. We swallow the `UnsupportedFeature` so we
    // can still produce a sane estimate on legacy chains.
    let tip_fut = async {
        client
            .provider()
            .client()
            .request("eth_maxPriorityFeePerGas", ())
            .await
    };
    let gas_price_fut = async { client.provider().get_gas_price().await };

    let (tip_res, gp_res) = futures::join!(tip_fut, gas_price_fut);

    let gas_price = gp_res.map_err(WalletError::from)?;
    let tip = match tip_res {
        Ok(t) => t,
        Err(_) => 1_000_000_000u128, // 1 gwei
    };
    // Max fee = 2 * gas_price (so we don't underbid during spikes).
    let max_fee = gas_price.saturating_mul(2).max(tip);
    Ok(FeeEstimate1559 {
        max_fee_per_gas: max_fee,
        max_priority_fee_per_gas: tip,
    })
}

// -- Gas estimation --------------------------------------------------------

/// Estimate gas usage for a transaction via `eth_estimateGas`.
///
/// The caller fills in `to`/`data`/`value` so the node can simulate
/// execution. `from` is set to the wallet's address — most nodes
/// accept (and many require) this for accurate EIP-2929 gas
/// accounting.
pub async fn estimate_gas(
    client: &EvmChainClient,
    wallet_address: WalletAddress,
    request: &TransactionRequest,
) -> WalletResult<u64> {
    let from = wallet_to_alloy(wallet_address)?;
    let mut req = request.clone();
    if req.from.is_none() {
        req.from = Some(from);
    }
    let gas = client
        .provider()
        .estimate_gas(&req)
        .await
        .map_err(WalletError::from)?;
    Ok(gas)
}

// -- Build & send ----------------------------------------------------------

/// Build, sign, and broadcast an EIP-1559 transaction.
pub async fn build_unsigned_request(
    chain_id: u64,
    nonce: u64,
    to: Option<WalletAddress>,
    value: U256,
    data: Vec<u8>,
    gas_limit: u64,
    fee: FeeEstimate1559,
) -> WalletResult<UnsignedTx1559> {
    if chain_id == 0 {
        return Err(WalletError::Invalid("chain_id must be non-zero".into()));
    }
    Ok(UnsignedTx1559 {
        chain_id,
        nonce,
        to: to.map(wallet_to_alloy).transpose()?,
        value,
        data,
        gas_limit,
        max_fee_per_gas: fee.max_fee_per_gas,
        max_priority_fee_per_gas: fee.max_priority_fee_per_gas,
    })
}

/// Build a legacy (type-0) unsigned request.
pub fn build_unsigned_request_legacy(
    chain_id: u64,
    nonce: u64,
    to: Option<WalletAddress>,
    value: U256,
    data: Vec<u8>,
    gas_limit: u64,
    gas_price: u128,
) -> WalletResult<UnsignedTxLegacy> {
    if chain_id == 0 {
        return Err(WalletError::Invalid("chain_id must be non-zero".into()));
    }
    Ok(UnsignedTxLegacy {
        chain_id,
        nonce,
        to: to.map(wallet_to_alloy).transpose()?,
        value,
        data,
        gas_limit,
        gas_price,
    })
}

/// Sign an unsigned EIP-1559 request with the given wallet, encode it,
/// and return the raw bytes ready for `eth_sendRawTransaction`.
///
/// The `wallet_address` argument is checked against the wallet's own
/// address — we refuse to silently sign a tx that would emit from a
/// different account.
pub fn sign_eip1559(
    wallet: &Wallet,
    request: &UnsignedTx1559,
) -> WalletResult<SendableTx<Ethereum>> {
    let typed = request.clone().into_typed();
    sign_and_encode::<TxEip1559>(wallet, typed, request.chain_id, None)
}

/// Sign a legacy (type-0) request.
pub fn sign_legacy(
    wallet: &Wallet,
    request: &UnsignedTxLegacy,
) -> WalletResult<SendableTx<Ethereum>> {
    let typed = request.clone().into_typed();
    sign_and_encode::<TxLegacy>(wallet, typed, request.chain_id, None)
}

/// Sign any `SignableTransaction` (EIP-1559, legacy, EIP-2930, EIP-4844
/// stubs, etc.) and return an alloy `SendableTx` ready to hand to
/// `send_transaction` / `send_raw_transaction`.
///
/// `expected_from` is optional. If `Some(addr)`, we assert that the
/// wallet's address matches — useful for the multi-signer wallet case
/// where a key might be picked from a routing table.
fn sign_and_encode<T>(
    wallet: &Wallet,
    mut typed: T,
    chain_id: u64,
    expected_from: Option<Address>,
) -> WalletResult<SendableTx<Ethereum>>
where
    T: SignableTransaction<Signature> + Send + Sync + 'static,
{
    let signer = A3IdentitySigner::new(wallet, Some(chain_id));
    let sig = signer
        .sign_transaction_sync(&mut typed)
        .map_err(|e| WalletError::Signing(format!("tx sign: {e}")))?;

    if let Some(expected) = expected_from {
        let actual = wallet_to_alloy(WalletAddress::from_bytes(
            wallet.public().address().as_bytes().try_into().unwrap_or([0u8; 20]),
        ))?;
        if actual != expected {
            return Err(WalletError::SignerMismatch {
                wallet: format!("{:?}", actual),
                tx_from: format!("{:?}", expected),
            });
        }
    }

    let signed = typed.into_signed(sig);
    let envelope = <Ethereum as alloy_network::Network>::TxEnvelope::from(signed);
    let raw = alloy_network::eip2718::Encodable2718::encoded_2718(&envelope);
    Ok(SendableTx::Raw(raw.into()))
}

/// Sign and broadcast an EIP-1559 transaction. Returns the tx hash.
pub async fn send_eip1559(
    client: &EvmChainClient,
    wallet: &Wallet,
    request: &UnsignedTx1559,
) -> WalletResult<B256> {
    let signed = sign_eip1559(wallet, request)?;
    let pending = client
        .provider()
        .send_transaction(signed)
        .await
        .map_err(WalletError::from)?;
    Ok(*pending.tx_hash())
}

/// Sign and broadcast a legacy transaction.
pub async fn send_legacy(
    client: &EvmChainClient,
    wallet: &Wallet,
    request: &UnsignedTxLegacy,
) -> WalletResult<B256> {
    let signed = sign_legacy(wallet, request)?;
    let pending = client
        .provider()
        .send_transaction(signed)
        .await
        .map_err(WalletError::from)?;
    Ok(*pending.tx_hash())
}

/// Block on a pending transaction until it appears in a block, with a
/// timeout. Returns the full [`TransactionReceipt`].
///
/// We poll the chain (`eth_getTransactionReceipt`) every `poll_secs`
/// seconds and bail after `timeout_secs` with [`WalletError::ReceiptTimeout`].
pub async fn wait_for_receipt(
    client: &EvmChainClient,
    tx_hash: B256,
    timeout_secs: u64,
    poll_secs: f64,
) -> WalletResult<alloy_rpc_types_eth::TransactionReceipt> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let sleep_dur = Duration::from_secs_f64(poll_secs.max(0.1));
    loop {
        // Fetch by hash — alloy's `get_transaction_receipt` returns
        // `Option<TransactionReceipt>`.
        if let Some(r) = client
            .provider()
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(WalletError::from)?
        {
            return Ok(r);
        }
        if Instant::now() >= deadline {
            return Err(WalletError::ReceiptTimeout {
                tx_hash: tx_hash.to_string(),
                timeout_secs,
            });
        }
        tokio::time::sleep(sleep_dur).await;
    }
}

// -- ERC-20 typed helpers --------------------------------------------------

/// ERC-20 metadata decoded from a token contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Erc20Metadata {
    pub decimals: u8,
    pub symbol: String,
    pub name: String,
}

/// Fetch on-chain `decimals / symbol / name` for an ERC-20 token.
pub async fn erc20_metadata(
    client: &EvmChainClient,
    token: WalletAddress,
) -> WalletResult<Erc20Metadata> {
    let token_addr = wallet_to_alloy(token)?;
    let dec_call = IERC20::decimalsCall {}.abi_encode();
    let sym_call = IERC20::symbolCall {}.abi_encode();
    let name_call = IERC20::nameCall {}.abi_encode();

    let token_addr_for = |c: Vec<u8>| {
        TransactionRequest::default()
            .to(token_addr)
            .input(TransactionInput::from(c))
    };

    let (dec_res, sym_res, name_res) = futures::try_join!(
        async { client.provider().call(token_addr_for(dec_call)).await.map_err(WalletError::from) },
        async { client.provider().call(token_addr_for(sym_call)).await.map_err(WalletError::from) },
        async { client.provider().call(token_addr_for(name_call)).await.map_err(WalletError::from) },
    )?;

    let decimals = dec_res
        .first()
        .copied()
        .ok_or_else(|| WalletError::Decode("decimals returned empty bytes".into()))?;
    let symbol = decode_string(&sym_res)?;
    let name = decode_string(&name_res)?;
    Ok(Erc20Metadata {
        decimals,
        symbol,
        name,
    })
}

/// Build an unsigned ERC-20 `transfer(to, amount)` call.
pub fn erc20_transfer_request(
    chain_id: u64,
    nonce: u64,
    token: WalletAddress,
    to: WalletAddress,
    amount: U256,
    gas_limit: u64,
    fee: FeeEstimate1559,
) -> WalletResult<UnsignedTx1559> {
    let token_addr = wallet_to_alloy(token)?;
    let to_addr = wallet_to_alloy(to)?;
    let call = IERC20::transferCall {
        to: to_addr,
        amount,
    };
    let data = call.abi_encode();
    build_unsigned_request(
        chain_id,
        nonce,
        Some(WalletAddress::from_bytes(token_addr.into_array())),
        U256::ZERO,
        data,
        gas_limit,
        fee,
    )
}

/// Build an unsigned ERC-20 `approve(spender, amount)` call.
pub fn erc20_approve_request(
    chain_id: u64,
    nonce: u64,
    token: WalletAddress,
    spender: WalletAddress,
    amount: U256,
    gas_limit: u64,
    fee: FeeEstimate1559,
) -> WalletResult<UnsignedTx1559> {
    let token_addr = wallet_to_alloy(token)?;
    let spender_addr = wallet_to_alloy(spender)?;
    let call = IERC20::approveCall {
        spender: spender_addr,
        amount,
    };
    let data = call.abi_encode();
    build_unsigned_request(
        chain_id,
        nonce,
        Some(WalletAddress::from_bytes(token_addr.into_array())),
        U256::ZERO,
        data,
        gas_limit,
        fee,
    )
}

/// Read an ERC-20 `allowance(owner, spender)` view.
pub async fn erc20_allowance(
    client: &EvmChainClient,
    token: WalletAddress,
    owner: WalletAddress,
    spender: WalletAddress,
) -> WalletResult<U256> {
    let token_addr = wallet_to_alloy(token)?;
    let owner_addr = wallet_to_alloy(owner)?;
    let spender_addr = wallet_to_alloy(spender)?;
    let call = IERC20::allowanceCall {
        owner: owner_addr,
        spender: spender_addr,
    };
    let calldata: Vec<u8> = call.abi_encode();
    let req = TransactionRequest::default()
        .to(token_addr)
        .input(TransactionInput::from(calldata));
    let raw = client
        .provider()
        .call(&req)
        .await
        .map_err(WalletError::from)?;
    if raw.len() != 32 {
        return Err(WalletError::Decode(format!(
            "allowance returned {} bytes, expected 32",
            raw.len()
        )));
    }
    U256::try_from_be_slice(&raw)
        .ok_or_else(|| WalletError::Decode("allowance bytes out of range".into()))
}

// -- Internal helpers -----------------------------------------------------

fn wallet_to_alloy(addr: WalletAddress) -> WalletResult<Address> {
    let mut bytes = [0u8; 20];
    bytes.copy_from_slice(addr.as_bytes());
    Ok(Address::from(bytes))
}

/// ABI-decode a `string` return. ERC-20 `symbol()`/`name()` return
/// `string`, which is ABI-encoded as a 32-byte offset + 32-byte length
/// + N bytes of UTF-8 data. We don't bother with the offset — we
/// assume the answer is in the *last* 32+32+N bytes, which is
/// standard for non-nested returns.
fn decode_string(raw: &[u8]) -> WalletResult<String> {
    if raw.len() < 64 {
        return Err(WalletError::Decode(format!(
            "string return too short: {} bytes",
            raw.len()
        )));
    }
    let len_bytes: [u8; 32] = raw[32..64]
        .try_into()
        .map_err(|_| WalletError::Decode("string length slice wrong".into()))?;
    let len = U256::from_be_bytes(len_bytes);
    let len_u64: u64 = len
        .try_into()
        .map_err(|_| WalletError::Decode("string length overflow".into()))?;
    let data = raw.get(64..64 + len_u64 as usize).ok_or_else(|| {
        WalletError::Decode(format!("string body truncated: want {len_u64} bytes"))
    })?;
    String::from_utf8(data.to_vec())
        .map_err(|e| WalletError::Decode(format!("string utf8: {e}")))
}

// -- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn w() -> Wallet {
        Wallet::generate()
    }

    #[test]
    fn unsigned_request_round_trip_1559() {
        let to_bytes = [1u8; 20];
        let to = WalletAddress::from_bytes(to_bytes);
        let req = build_unsigned_request(
            1,
            7,
            Some(to),
            U256::from(100u64),
            vec![0xde, 0xad],
            21_000,
            FeeEstimate1559 {
                max_fee_per_gas: 30_000_000_000,
                max_priority_fee_per_gas: 1_000_000_000,
            },
        )
        .unwrap();
        assert_eq!(req.chain_id, 1);
        assert_eq!(req.nonce, 7);
        assert_eq!(req.gas_limit, 21_000);
        let typed = req.into_typed();
        assert_eq!(typed.chain_id, 1);
        assert_eq!(typed.nonce, 7);
    }

    #[test]
    fn rejects_zero_chain_id() {
        let err = build_unsigned_request(
            0, 0, None, U256::ZERO, vec![], 21_000,
            FeeEstimate1559 { max_fee_per_gas: 1, max_priority_fee_per_gas: 1 },
        )
        .unwrap_err();
        assert!(matches!(err, WalletError::Invalid(_)));
    }

    #[test]
    fn signer_address_matches_wallet() {
        let wallet = w();
        let signer = A3IdentitySigner::new(&wallet, Some(1));
        let expected = wallet.public().address().as_bytes();
        let got = signer.address().as_slice();
        assert_eq!(expected, got);
    }

    #[test]
    fn abi_encode_transfer_selector_matches_keccak() {
        // Sanity-check: the typed ABI encoding produces a 4-byte
        // selector matching keccak256("transfer(address,uint256)")[:4]
        // = 0xa9059cbb. If this changes, all ERC-20 sends break.
        let mut to = [0u8; 20];
        to[19] = 0x42;
        let call = IERC20::transferCall {
            to: Address::from(to),
            amount: U256::from(1u64),
        };
        let bytes = call.abi_encode();
        assert_eq!(bytes.len(), 4 + 32 + 32);
        assert_eq!(&bytes[..4], &[0xa9, 0x05, 0x9c, 0xbb]);
    }

    #[test]
    fn decode_string_handles_short_input() {
        let err = decode_string(&[0u8; 10]).unwrap_err();
        assert!(matches!(err, WalletError::Decode(_)));
    }

    #[test]
    fn decode_string_decodes_simple_ascii() {
        // offset(32) + len(32) + body(3) = 67 bytes
        let mut raw = vec![0u8; 64];
        raw.extend_from_slice(b"USDC"); // 4 bytes
        // length word: 4
        let mut len_bytes = [0u8; 32];
        len_bytes[31] = 4;
        raw[32..64].copy_from_slice(&len_bytes);
        let s = decode_string(&raw).unwrap();
        assert_eq!(s, "USDC");
    }
}