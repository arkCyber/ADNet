//! Minimal end-to-end example: dial a public mainnet endpoint, probe
//! the chain id, and report the head block number.
//!
//! Run with:
//!
//! ```text
//! cargo run --example chain_liveness -- https://eth.llamarpc.com
//! ```
//!
//! The endpoint argument is required (no defaults — we want the example
//! to be a forcing function for choosing a real, reachable endpoint).

use a3net_wallet_evm::{EvmChainClient, WalletError, WalletResult, block_number};

#[tokio::main]
async fn main() -> WalletResult<()> {
    let rpc_url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://eth.llamarpc.com".to_string());

    eprintln!("connecting to {rpc_url}...");
    let client = EvmChainClient::new(&rpc_url).await?;

    println!("rpc_url : {}", client.rpc_url());
    println!("chain_id: {}", client.chain_id());

    match block_number(&client).await {
        Ok(n) => println!("head    : block #{n}"),
        Err(WalletError::Transport(msg)) => {
            eprintln!("network down: {msg}");
            std::process::exit(2);
        }
        Err(WalletError::Rpc(msg)) => {
            eprintln!("endpoint rejected the call: {msg}");
            std::process::exit(3);
        }
        Err(e) => return Err(e),
    }

    Ok(())
}