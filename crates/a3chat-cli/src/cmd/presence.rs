//! `a3chat presence …` — publish / subscribe to presence.

use clap::Subcommand;

use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::{CliConfig, OutputFormat};
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum PresenceCmd {
    /// Publish our own presence status.
    Publish {
        /// `online` | `away` | `busy` | `invisible` | `offline`
        #[arg(long)]
        status: String,
        /// Optional human-readable status message (≤ 256 chars).
        #[arg(long, default_value = "")]
        message: String,
    },
    /// Fetch presence for a list of peers.
    Subscribe {
        /// Comma-separated list of 64-hex NodeIds.
        #[arg(long, value_delimiter = ',')]
        peers: Vec<String>,
    },
}

pub async fn run(
    cmd: PresenceCmd,
    cfg: &CliConfig,
    client: &HttpRpcClient,
) -> CliResult<()> {
    match cmd {
        PresenceCmd::Publish { status, message } => {
            publish(cfg, client, status, message).await
        }
        PresenceCmd::Subscribe { peers } => subscribe(cfg, client, peers).await,
    }
}

fn parse_status(s: &str) -> CliResult<&'static str> {
    Ok(match s {
        "online" => "online",
        "away" => "away",
        "busy" => "busy",
        "invisible" => "invisible",
        "offline" => "offline",
        other => {
            return Err(CliError::Usage(format!(
                "--status must be online|away|busy|invisible|offline; got {other:?}"
            )));
        }
    })
}

async fn publish(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    status: String,
    message: String,
) -> CliResult<()> {
    let status_norm = parse_status(&status)?.to_string();
    if message.len() > 256 {
        return Err(CliError::Usage(
            "--message exceeds 256 chars".into(),
        ));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::PRESENCE_PUBLISH,
            serde_json::json!({
                "status": status_norm,
                "status_message": message,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn subscribe(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    peers: Vec<String>,
) -> CliResult<()> {
    if peers.is_empty() {
        return Err(CliError::Usage(
            "--peers must contain at least one 64-hex NodeId".into(),
        ));
    }
    let mut normalized = Vec::with_capacity(peers.len());
    for p in &peers {
        let p = p.trim();
        if p.len() != 64 || !p.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(CliError::Usage(format!(
                "--peers entry must be a 64-char hex NodeId; got len={}",
                p.len()
            )));
        }
        normalized.push(p.to_string());
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::PRESENCE_SUBSCRIBE,
            serde_json::json!({ "peers": normalized }),
        )
        .await?;
    match cfg.effective_output() {
        OutputFormat::Plain => {
            // One line per peer: `user_id status`
            if let serde_json::Value::Array(items) = v {
                for item in items {
                    let uid = item
                        .get("user_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let st = item
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let sm = item
                        .get("status_message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if sm.is_empty() {
                        println!("{uid}  {st}");
                    } else {
                        println!("{uid}  {st}  {sm}");
                    }
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            }
        }
        _ => output::print(cfg.effective_output(), &v)?,
    }
    Ok(())
}
