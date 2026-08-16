//! Thin wrapper around an alloy [`RootProvider`].
//!
//! The provider is what owns the JSON-RPC HTTP connection; everything
//! read-only in this crate goes through it. We keep this wrapper
//! deliberately small — the read-only API lives in [`crate::read`] and
//! dispatches through [`EvmChainClient::provider`].
//!
//! ## Why `hyper` (not `reqwest`)
//!
//! Alloy 2.1.x's default `reqwest` feature pins `reqwest = "0.13"`. The
//! A3Net workspace is on `reqwest = "0.12"` and 30+ call sites depend on
//! it, so we instead opt into the `hyper` feature: the underlying
//! transport is `hyper-util` (which `a3net-gateway` already pulls in),
//! keeping the dependency surface flat.
//!
//! In alloy 2.1.x [`RootProvider::new_http`] is `#[cfg(feature = "reqwest")]`-
//! gated, so the hyper path goes through
//! `RpcClient::new(HyperTransport::new_hyper(url), false)` followed by
//! `RootProvider::new(rpc)`.
//!
//! ## Why no signer / no transaction support
//!
//! This crate is **read-only by design** (Phase 1 scope). Sending
//! transactions requires key-management policy that crosses the
//! `a3net-identity::Wallet` boundary; that belongs in a future
//! `a3net-wallet-tx` crate that imports this one as its read backbone.

use std::sync::Arc;

use alloy_network::Ethereum;
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_client::RpcClient;
use alloy_transport_http::HyperTransport;
use tracing::debug;
use url::Url;

use crate::error::{WalletError, WalletResult};

/// Concrete transport type — `hyper-util` based HTTP, not `reqwest`.
type Transport = HyperTransport;

/// Concrete provider type — `RootProvider` over the Ethereum network.
///
/// Note: in alloy 2.1.x `RootProvider<N: Network = Ethereum>` takes
/// only one generic parameter; the transport is held as a field of
/// `RootProviderInner`, not as a second generic.
pub(crate) type EthProvider = RootProvider<Ethereum>;

/// Read-only EVM JSON-RPC client.
///
/// Cheap to clone: the inner [`RootProvider`] is internally an `Arc`,
/// so cloning the wrapper is just a refcount bump. The URL is also
/// stored in an `Arc<str>` so cloning is allocation-free.
#[derive(Clone)]
pub struct EvmChainClient {
    inner: EthProvider,
    rpc_url: Arc<str>,
    /// Cached `eth_chainId` result. We probe once at construction and
    /// never refresh — callers needing fresh chain IDs can call
    /// [`EvmChainClient::fetch_chain_id`].
    chain_id: u64,
}

impl std::fmt::Debug for EvmChainClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvmChainClient")
            .field("rpc_url", &self.rpc_url)
            .field("chain_id", &self.chain_id)
            .finish_non_exhaustive()
    }
}

impl EvmChainClient {
    /// Construct from a raw RPC URL string.
    ///
    /// Performs a synchronous `eth_chainId` probe so the client is ready
    /// for [`crate::read`] calls without a second round-trip on first use.
    ///
    /// # Errors
    ///
    /// - [`WalletError::Invalid`] if `rpc_url` is not a syntactically
    ///   valid HTTP/HTTPS URL.
    /// - [`WalletError::Transport`] if the endpoint is unreachable.
    /// - [`WalletError::Rpc`] if the endpoint rejects the chain-id probe.
    pub async fn new(rpc_url: &str) -> WalletResult<Self> {
        let url = Url::parse(rpc_url)
            .map_err(|e| WalletError::Invalid(format!("rpc url {rpc_url:?}: {e}")))?;
        Self::from_url(url).await
    }

    /// Construct from an already-parsed [`Url`].
    ///
    /// Use this when the URL is produced programmatically (e.g. from
    /// chain-id lookup tables) and doesn't need re-parsing.
    pub async fn from_url(url: Url) -> WalletResult<Self> {
        let provider = build_hyper_provider(url.clone());

        // Probe chain id synchronously here so we surface startup-time
        // errors before the caller makes their first "real" call.
        let chain_id: u64 = provider
            .get_chain_id()
            .await
            .map_err(WalletError::from)?;

        debug!(rpc_url = %url, chain_id, "a3net-wallet-evm: connected");

        Ok(Self {
            inner: provider,
            rpc_url: Arc::from(url.as_str()),
            chain_id,
        })
    }

    /// The RPC endpoint URL this client talks to.
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// The cached chain id from the startup probe.
    ///
    /// Cheap (no I/O) because we cached the result of the first
    /// `eth_chainId` call inside [`Self::new`].
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Re-fetch the chain id from the remote endpoint (ignoring the
    /// startup cache).
    ///
    /// Use this if you suspect the cached value is stale (rare — chain
    /// ids are stable for the life of a network) or for diagnostic
    /// "is this endpoint still the same chain" checks.
    pub async fn fetch_chain_id(&self) -> WalletResult<u64> {
        self.inner.get_chain_id().await.map_err(WalletError::from)
    }

    /// Borrow the inner alloy provider. Used by [`crate::read`] to make
    /// typed eth_* calls (`get_balance`, `get_block_number`, …).
    ///
    /// We intentionally return `&EthProvider`, not an owned value, so
    /// cloning the outer [`EvmChainClient`] shares the same HTTP stack.
    /// The return type is crate-internal (`pub(crate)`) so external
    /// callers cannot pin to alloy's type hierarchy.
    pub(crate) fn provider(&self) -> &EthProvider {
        &self.inner
    }
}

/// Build a hyper-backed `RootProvider<Ethereum>` from a parsed URL.
///
/// This is the single point in the crate that knows about the
/// `RpcClient → RootProvider → HyperTransport` dance. Centralizing it
/// here means a future swap to a different transport (e.g. WebSocket
/// via `WsConnect`) touches one function, not three.
fn build_hyper_provider(url: Url) -> EthProvider {
    // 1. Spin up the hyper-util HTTP transport. `new_hyper` uses
    //    `hyper-util`'s default client with HTTP (not HTTPS) and no
    //    custom tower layers. Endpoints that require HTTPS still work
    //    because `a3net-gateway` already pulls in `rustls`, but the
    //    hyper feature we selected here does *not* enable TLS by
    //    default; production deployments that need TLS should switch
    //    to the `hyper-tls` feature.
    let transport: Transport = HyperTransport::new_hyper(url);

    // 2. Wrap it in an `RpcClient`. `is_local = false` because we don't
    //    poll the transport for block subscriptions in read-only mode.
    let rpc_client = RpcClient::new(transport, false);

    // 3. Hand the client to a bare `RootProvider`. We deliberately
    //    skip `ProviderBuilder` because its filler-stacked variants
    //    are designed for *signing* workflows (nonce management, gas
    //    price filling, chain-id checks). Read-only calls don't need
    //    any of that and the fillers would just be dead weight on
    //    the dependency tree.
    RootProvider::<Ethereum>::new(rpc_client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_garbage_url() {
        // `new` is async; we test the URL parser directly here so the
        // assertion is synchronous and doesn't need a runtime.
        let err = Url::parse("not a url");
        assert!(err.is_err());
    }
}