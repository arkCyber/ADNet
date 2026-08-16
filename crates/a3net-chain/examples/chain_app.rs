//! App-level a3net-chain example.
//!
//! Walks the realistic "I want to bolt a chain role onto my
//! existing A3Net NAS" path:
//!
//!  1. Start disabled, confirm no-op behavior;
//!  2. Construct a config tailored for an EVM observer node with
//!     a custom data subdir and a bind address;
//!  3. Confirm `start()` returns `Unimplemented` for the chosen
//!     chain (the framework scaffold is intentional — no real
//!     backend yet);
//!  4. Pretty-print the final config JSON so an operator can see
//!     what would be persisted once the backend ships.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p a3net-chain --example chain_app
//! ```

use a3net_chain::{ChainError, ChainKind, ChainNode, ChainNodeConfig, ChainRole};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("== Disabled baseline ==");
    let cfg = ChainNodeConfig::default();
    let handle = ChainNode::new(cfg).start().await?;
    assert!(handle.is_none());
    println!("ok: disabled NAS continues unchanged");

    println!("\n== Targeted EVM observer ==");
    let mut cfg = ChainNodeConfig::enabled(ChainKind::Evm, ChainRole::Observer);
    cfg.data_subdir = "evm/obol".into();
    cfg.bind = Some("0.0.0.0:8545".parse::<SocketAddr>()?);

    println!("configured kind        : {}", cfg.kind);
    println!("configured role        : {:?}", cfg.role);
    println!("configured data_subdir : {}", cfg.data_subdir);
    println!("configured bind        : {:?}", cfg.bind);

    let node = ChainNode::new(cfg.clone());
    match node.start().await {
        Err(ChainError::Unimplemented(msg)) => {
            println!("start -> ChainError::Unimplemented: {msg}");
        }
        Err(other) => println!("start -> unexpected error: {other}"),
        Ok(Some(_)) => println!("start -> success (would be unexpected here)"),
        Ok(None) => println!("start -> disabled (would be unexpected here)"),
    }

    println!("\n== Config pretty-print ==");
    let json = serde_json::to_string_pretty(&cfg)?;
    println!("{json}");

    Ok(())
}
