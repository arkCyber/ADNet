//! `a3chat moderation …` — content / attachment policy gate.

use clap::Subcommand;

use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum ModerationCmd {
    /// Check arbitrary text against the local classifier.
    CheckContent {
        /// UTF-8 text to evaluate.
        #[arg(long)]
        text: String,
    },
    /// Check an attachment by its BLAKE3 hash.
    CheckAttachment {
        /// 64-hex BLAKE3 hash of the attachment blob.
        #[arg(long)]
        hash: String,
        /// Optional MIME type (forwarded for logging only).
        #[arg(long, default_value = "")]
        content_type: String,
        /// Optional size in bytes (forwarded for logging only).
        #[arg(long, default_value_t = 0)]
        size: u64,
    },
    /// List every blocklist entry currently on disk.
    ListBlocked,
    /// Toggle the "deny by default" fallback policy.
    SetDenyDefault {
        /// Pass `--on=true` to enable, `--on=false` to disable.
        #[arg(long, action = clap::ArgAction::Set)]
        on: bool,
    },
    /// Print blocklist engagement stats.
    Stats,
}

pub async fn run(
    cmd: ModerationCmd,
    cfg: &CliConfig,
    client: &HttpRpcClient,
) -> CliResult<()> {
    match cmd {
        ModerationCmd::CheckContent { text } => check_content(cfg, client, text).await,
        ModerationCmd::CheckAttachment {
            hash,
            content_type,
            size,
        } => check_attachment(cfg, client, hash, content_type, size).await,
        ModerationCmd::ListBlocked => list_blocked(cfg, client).await,
        ModerationCmd::SetDenyDefault { on } => set_deny_default(cfg, client, on).await,
        ModerationCmd::Stats => stats(cfg, client).await,
    }
}

async fn check_content(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    text: String,
) -> CliResult<()> {
    if text.is_empty() {
        return Err(CliError::Usage("--text is required".into()));
    }
    if text.len() > 256 * 1024 {
        return Err(CliError::Usage(
            "--text exceeds 256KiB cap".into(),
        ));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::MODERATION_CHECK_CONTENT,
            serde_json::json!({ "text": text }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn check_attachment(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    hash: String,
    content_type: String,
    size: u64,
) -> CliResult<()> {
    if hash.len() < 16 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CliError::Usage(
            "--hash must be ≥16 hex chars".into(),
        ));
    }
    let mut params = serde_json::json!({ "hash": hash });
    if !content_type.is_empty() {
        params["content_type"] = serde_json::Value::String(content_type);
    }
    if size > 0 {
        params["size"] = serde_json::Value::Number(serde_json::Number::from(size));
    }
    let v = client
        .call_raw(A3chatRpcMethod::MODERATION_CHECK_ATTACHMENT, params)
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn list_blocked(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v = client
        .call_raw(
            A3chatRpcMethod::MODERATION_LIST_BLOCKED,
            serde_json::json!({}),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn set_deny_default(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    on: bool,
) -> CliResult<()> {
    let v = client
        .call_raw(
            A3chatRpcMethod::MODERATION_SET_DENY_DEFAULT,
            serde_json::json!({ "on": on }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn stats(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v = client
        .call_raw(
            A3chatRpcMethod::MODERATION_STATS,
            serde_json::json!({}),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}
