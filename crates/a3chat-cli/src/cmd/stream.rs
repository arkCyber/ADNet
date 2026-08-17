//! `stream` — subscribe / unsubscribe / list event streams.
//!
//! Wraps the `a3chat.stream.*` RPC methods so operators can poke the
//! SSE subscription registry without `a3chat rpc <method>`.

#![forbid(unsafe_code)]

use clap::{Args, Subcommand};

use crate::config::CliConfig;
use crate::error::CliResult;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum StreamCmd {
    /// Subscribe to a topic pattern and print the handle_id + stream URL.
    Subscribe(SubscribeArgs),
    /// Release a previously acquired subscription handle.
    Unsubscribe(UnsubscribeArgs),
    /// List all currently-registered subscriptions.
    List,
}

#[derive(Debug, Args)]
pub struct SubscribeArgs {
    /// Topic pattern to subscribe to. Glob-style (`*` matches any segment).
    /// Default: `*` (everything).
    #[arg(long, default_value = "*")]
    pub topic: String,
}

#[derive(Debug, Args)]
pub struct UnsubscribeArgs {
    /// Handle id to release.
    #[arg(long)]
    pub handle_id: String,
}

pub async fn run(cmd: StreamCmd, _cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    match cmd {
        StreamCmd::Subscribe(args) => subscribe(client, &args.topic).await,
        StreamCmd::Unsubscribe(args) => unsubscribe(client, &args.handle_id).await,
        StreamCmd::List => list(client).await,
    }
}

async fn subscribe(client: &HttpRpcClient, topic: &str) -> CliResult<()> {
    let v: serde_json::Value = client
        .call(
            "a3chat.stream.subscribe",
            serde_json::json!({ "topic": topic }),
        )
        .await?;
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    Ok(())
}

async fn unsubscribe(client: &HttpRpcClient, handle_id: &str) -> CliResult<()> {
    let v: serde_json::Value = client
        .call(
            "a3chat.stream.unsubscribe",
            serde_json::json!({ "handle_id": handle_id }),
        )
        .await?;
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    Ok(())
}

async fn list(client: &HttpRpcClient) -> CliResult<()> {
    let v: serde_json::Value = client
        .call("a3chat.stream.list", serde_json::json!({}))
        .await?;
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    Ok(())
}
