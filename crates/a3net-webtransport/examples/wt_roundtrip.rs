//! WebTransport round-trip example (Round-1 scaffold).
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-webtransport --example wt_roundtrip --features webtransport
//! ```

use a3net_webtransport::config::WebTransportConfig;
use a3net_webtransport::WebTransportResult;
use a3net_types::NodeId;

#[tokio::main]
async fn main() -> WebTransportResult<()> {
    let cfg = WebTransportConfig::default();
    let node_id = NodeId::random();
    println!("WebTransport roundtrip scaffold: bind={} node={}", cfg.bind, node_id.short());

    // Round-2 will:
    //   let handle = WtServer::bind(cfg, node_id).await?;
    //   let token = mint_token(node_id, 60, &handle.token_secret);
    //   ...

    Ok(())
}
