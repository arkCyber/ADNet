//! Minimal adnet-chain example.
//!
//! Uses the default config (chain disabled) and confirms that
//! `ChainNode::start()` is a no-op for ordinary NAS servers. Then
//! switches to an enabled config and shows the expected
//! `ChainError::Unimplemented` path. All JSON serialization is
//! exercised as a side check.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p adnet-chain --example chain_basic
//! ```

use adnet_chain::{ChainError, ChainKind, ChainNode, ChainNodeConfig, ChainRole};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("== Disabled (default) ==");
    let cfg = ChainNodeConfig::default();
    println!("enabled     : {}", cfg.enabled);
    println!("kind        : {}", cfg.kind);
    println!("role        : {:?}", cfg.role);
    println!("data_subdir : {}", cfg.data_subdir);
    let handle = ChainNode::new(cfg).start().await?;
    println!("start ->     : {:?}", handle.as_ref().map(|h| format!("{:?}", h.status())));

    println!("\n== Enabled (no backend yet) ==");
    let cfg = ChainNodeConfig::enabled(ChainKind::Evm, ChainRole::Observer);
    let node = ChainNode::new(cfg);
    match node.start().await {
        Ok(Some(h)) => println!("got handle: {:?}", h.status()),
        Ok(None) => println!("disabled"),
        Err(e) => match e {
            ChainError::Unimplemented(msg) => println!("Unimplemented: {msg}"),
            other => println!("other error: {other}"),
        },
    }

    println!("\n== JSON round-trip ==");
    let mut cfg = ChainNodeConfig::enabled(ChainKind::Substrate, ChainRole::Validator);
    cfg.data_subdir = "eth/mainnet".into();
    let json = serde_json::to_string_pretty(&cfg)?;
    println!("json:\n{json}");
    let back: ChainNodeConfig = serde_json::from_str(&json)?;
    println!("\ndecoded.kind   : {}", back.kind);
    println!("decoded.role   : {:?}", back.role);
    println!("decoded.data_subdir : {}", back.data_subdir);

    Ok(())
}
