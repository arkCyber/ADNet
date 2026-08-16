//! `a3chat conversation …` subcommands.

use clap::Subcommand;

use a3chat_core::id::ConversationId;
use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum ConversationCmd {
    /// List every conversation the local user can see.
    List,
    /// Open a conversation by id and dump its full record.
    Open {
        #[arg(long)]
        conversation_id: String,
    },
}

pub async fn run(
    cmd: ConversationCmd,
    cfg: &CliConfig,
    client: &HttpRpcClient,
) -> CliResult<()> {
    match cmd {
        ConversationCmd::List => list(cfg, client).await,
        ConversationCmd::Open { conversation_id } => open(cfg, client, &conversation_id).await,
    }
}

async fn list(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v: serde_json::Value = client
        .call(A3chatRpcMethod::CHAT_CONVERSATION_LIST, serde_json::json!({}))
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn open(cfg: &CliConfig, client: &HttpRpcClient, raw_id: &str) -> CliResult<()> {
    let id = ConversationId::from(raw_id.to_string());
    let v: serde_json::Value = client
        .call(
            A3chatRpcMethod::CHAT_CONVERSATION_OPEN,
            serde_json::json!({ "conversation_id": id }),
        )
        .await?;
    if v.is_null() {
        return Err(CliError::Rpc(a3chat_core::error::A3chatError::NotFound(
            format!("conversation {raw_id} not found"),
        )));
    }
    output::print(cfg.effective_output(), &v)
}