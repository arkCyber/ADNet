//! Public-chain registry: chain-id → known RPC endpoint(s).
//!
//! A3Net does **not** ship its own RPC infrastructure for public chains
//! (Ethereum mainnet, Sepolia, Polygon, Base, …). To make a developer
//! first-call simple we bundle a small table of the most-used public
//! RPC endpoints per chain id. Every entry is opt-in: the caller still
//! has to construct an [`EvmChainClient`](crate::provider::EvmChainClient)
//! with one of these URLs, but `ChainRegistry::by_chain_id` removes the
//! "where do I find an RPC URL?" friction.
//!
//! ## Why a static table (vs. a dynamic DNS lookup)?
//!
//! - **Determinism.** Tests that exercise the registry get the same URL
//!   on every machine; no surprises from a flaky DNS resolver.
//! - **Reproducibility.** Build-time choice, runtime choice.
//! - **No service discovery.** We are not running a chain-registry
//!   service; this is a static manifest.
//!
//! ## Adding chains
//!
//! Add a new entry to [`ChainInfo`] and append it to [`KNOWN_CHAINS`].
//! Multi-RPC chains store both in [`ChainInfo::rpc_urls`]; the first
//! one is the "preferred" endpoint and the rest are fallbacks.

use crate::error::{WalletError, WalletResult};

/// One public-chain entry. Only the fields A3Net actually uses
/// (chain id + RPC URL) are populated today; the rest is left as a
/// stub for future enrichment (currency symbol, block-explorer, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainInfo {
    /// EIP-155 chain id.
    pub chain_id: u64,
    /// Human-readable name (e.g. `"Ethereum Mainnet"`).
    pub name: &'static str,
    /// RPC endpoints, **first entry preferred** for new clients.
    pub rpc_urls: &'static [&'static str],
    /// Native currency symbol (e.g. `"ETH"`, `"MATIC"`, `"POL"`).
    pub symbol: &'static str,
}

/// Curated public chains. The list is intentionally small — adding
/// every EVM testnet/mainnet would balloon the binary and bring in
/// frequently-changing RPC endpoints that drift faster than this
/// crate can be re-released.
pub const KNOWN_CHAINS: &[ChainInfo] = &[
    ChainInfo {
        chain_id: 1,
        name: "Ethereum Mainnet",
        symbol: "ETH",
        rpc_urls: &[
            "https://cloudflare-eth.com",
            "https://ethereum-rpc.publicnode.com",
        ],
    },
    ChainInfo {
        chain_id: 10,
        name: "OP Mainnet",
        symbol: "ETH",
        rpc_urls: &["https://mainnet.optimism.io"],
    },
    ChainInfo {
        chain_id: 56,
        name: "BNB Smart Chain",
        symbol: "BNB",
        rpc_urls: &[
            "https://bsc-dataseed.binance.org",
            "https://bsc-dataseed-public.bnbchain.com",
        ],
    },
    ChainInfo {
        chain_id: 100,
        name: "Gnosis",
        symbol: "xDAI",
        rpc_urls: &["https://rpc.gnosischain.com"],
    },
    ChainInfo {
        chain_id: 137,
        name: "Polygon Mainnet",
        symbol: "POL",
        rpc_urls: &[
            "https://polygon-rpc.com",
            "https://polygon-bor-rpc.publicnode.com",
        ],
    },
    ChainInfo {
        chain_id: 8453,
        name: "Base Mainnet",
        symbol: "ETH",
        rpc_urls: &[
            "https://mainnet.base.org",
            "https://base-public.node.realworldmservices.com",
        ],
    },
    ChainInfo {
        chain_id: 42161,
        name: "Arbitrum One",
        symbol: "ETH",
        rpc_urls: &[
            "https://arb1.arbitrum.io/rpc",
            "https://arbitrum-one.publicnode.com",
        ],
    },
    ChainInfo {
        chain_id: 43114,
        name: "Avalanche C-Chain",
        symbol: "AVAX",
        rpc_urls: &["https://api.avax.network/ext/bc/C/rpc"],
    },
    ChainInfo {
        chain_id: 11155111,
        name: "Sepolia Testnet",
        symbol: "ETH",
        rpc_urls: &[
            "https://rpc.sepolia.org",
            "https://ethereum-sepolia-rpc.publicnode.com",
        ],
    },
    ChainInfo {
        chain_id: 84532,
        name: "Base Sepolia",
        symbol: "ETH",
        rpc_urls: &["https://sepolia.base.org"],
    },
    ChainInfo {
        chain_id: 80002,
        name: "Polygon Amoy",
        symbol: "POL",
        rpc_urls: &["https://rpc-amoy.polygon.technology"],
    },
    ChainInfo {
        chain_id: 421614,
        name: "Arbitrum Sepolia",
        symbol: "ETH",
        rpc_urls: &["https://sepolia-rollup.arbitrum.io/rpc"],
    },
    ChainInfo {
        chain_id: 31337,
        name: "Anvil (local dev)",
        symbol: "ETH",
        rpc_urls: &["http://127.0.0.1:8545"],
    },
];

/// Look up a chain by EIP-155 chain id.
pub fn by_chain_id(chain_id: u64) -> WalletResult<&'static ChainInfo> {
    KNOWN_CHAINS
        .iter()
        .find(|c| c.chain_id == chain_id)
        .ok_or_else(|| WalletError::Invalid(format!("unknown chain id {chain_id}")))
}

/// Pick the preferred RPC URL for a chain.
pub fn preferred_rpc(chain_id: u64) -> WalletResult<&'static str> {
    let info = by_chain_id(chain_id)?;
    info.rpc_urls
        .first()
        .copied()
        .ok_or_else(|| WalletError::Invalid(format!("chain {chain_id} has no rpc urls")))
}

/// Return **all** RPC URLs (preferred first, fallbacks after) for a
/// chain. Useful when the caller wants to retry on transport failures.
pub fn rpc_urls(chain_id: u64) -> WalletResult<&'static [&'static str]> {
    Ok(by_chain_id(chain_id)?.rpc_urls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_chains_are_unique_by_id() {
        for (i, a) in KNOWN_CHAINS.iter().enumerate() {
            for b in KNOWN_CHAINS.iter().skip(i + 1) {
                assert_ne!(a.chain_id, b.chain_id, "duplicate chain id {}", a.chain_id);
            }
        }
    }

    #[test]
    fn every_chain_has_at_least_one_url() {
        for c in KNOWN_CHAINS {
            assert!(
                !c.rpc_urls.is_empty(),
                "chain {} has no rpc urls",
                c.chain_id
            );
        }
    }

    #[test]
    fn by_chain_id_known() {
        let eth = by_chain_id(1).unwrap();
        assert_eq!(eth.name, "Ethereum Mainnet");
        assert_eq!(eth.symbol, "ETH");
    }

    #[test]
    fn by_chain_id_unknown() {
        let err = by_chain_id(0xdeadbeef).unwrap_err();
        assert!(matches!(err, WalletError::Invalid(_)));
    }

    #[test]
    fn preferred_rpc_returns_first() {
        assert_eq!(
            preferred_rpc(1).unwrap(),
            "https://cloudflare-eth.com"
        );
    }

    #[test]
    fn rpc_urls_preserves_order() {
        let urls = rpc_urls(1).unwrap();
        assert_eq!(urls[0], "https://cloudflare-eth.com");
        assert!(urls.len() >= 2);
    }
}