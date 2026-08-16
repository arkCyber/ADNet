//! `a3chat message …` subcommands.

use clap::Subcommand;

use a3chat_core::id::{ConversationId, MessageId, UserId};
use a3chat_core::message::{AttachmentKind, MessageBody, MessageEnvelope, MessageType};
use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum MessageCmd {
    /// Send a DM (or group) message to a peer.
    Send {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        to: String,
        #[arg(long, default_value = "text")]
        kind: String,
        /// Plaintext body (ignored when `--kind` is not text).
        #[arg(long)]
        body: Option<String>,
        /// Client-supplied per-sender sequence number.
        #[arg(long, default_value_t = 1)]
        sequence: u32,
        /// Unix timestamp. Defaults to "now".
        #[arg(long)]
        timestamp: Option<i64>,
        /// DO-178C §6.1 — print the envelope without sending.
        #[arg(long)]
        dry_run: bool,
    },
    /// Acknowledge a received message (mark as read).
    Ack {
        #[arg(long)]
        message_id: String,
    },
    /// Recall a message you previously sent.
    Recall {
        #[arg(long)]
        message_id: String,
    },
    /// Edit the body of a message you previously sent.
    Edit {
        #[arg(long)]
        message_id: String,
        #[arg(long)]
        body: String,
    },
    /// Locally delete a message ("delete for me").
    Delete {
        #[arg(long)]
        message_id: String,
    },
    /// Full-text search across visible conversations.
    Search {
        #[arg(long)]
        needle: String,
        /// Limit hits to a specific conversation.
        #[arg(long)]
        conversation_id: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Emit a typing indicator (best-effort, not persisted).
    Typing {
        #[arg(long)]
        conversation_id: String,
        #[arg(long, default_value_t = 0)]
        expires_at: i64,
    },
}

pub async fn run(cmd: MessageCmd, cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    match cmd {
        MessageCmd::Send {
            conversation_id,
            to,
            kind,
            body,
            sequence,
            timestamp,
            dry_run,
        } => send(cfg, client, &conversation_id, &to, &kind, body, sequence, timestamp, dry_run).await,
        MessageCmd::Ack { message_id } => ack(cfg, client, &message_id).await,
        MessageCmd::Recall { message_id } => recall(cfg, client, &message_id).await,
        MessageCmd::Edit { message_id, body } => edit(cfg, client, &message_id, &body).await,
        MessageCmd::Delete { message_id } => delete(cfg, client, &message_id).await,
        MessageCmd::Search { needle, conversation_id, limit } => {
            search(cfg, client, &needle, conversation_id, limit).await
        }
        MessageCmd::Typing { conversation_id, expires_at } => {
            typing(cfg, client, &conversation_id, expires_at).await
        }
    }
}

fn parse_kind(s: &str) -> CliResult<MessageType> {
    Ok(match s {
        "text" => MessageType::Text,
        "image" => MessageType::Image,
        "file" => MessageType::File,
        "voice" => MessageType::Voice,
        "video" => MessageType::Video,
        "system" => MessageType::System,
        "call" => MessageType::Call,
        other => {
            return Err(CliError::Usage(format!(
                "unknown --kind {other:?}; expected text|image|file|voice|video|system|call"
            )));
        }
    })
}

#[allow(clippy::too_many_arguments)]
async fn send(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: &str,
    to: &str,
    kind: &str,
    body: Option<String>,
    sequence: u32,
    timestamp: Option<i64>,
    dry_run: bool,
) -> CliResult<()> {
    let kind = parse_kind(kind)?;
    let content = body.unwrap_or_default();
    let envelope = MessageEnvelope {
        conversation_id: ConversationId::from(conversation_id.to_string()),
        receiver_id: UserId::from(to.to_string()),
        message_type: kind,
        body: MessageBody::Plain { content },
        attachments: vec![],
        reply_to: None,
        sequence,
        timestamp: timestamp.unwrap_or_else(|| chrono::Utc::now().timestamp()),
    };
    if let Err(e) = envelope.validate() {
        return Err(CliError::Usage(format!("invalid envelope: {e}")));
    }
    if dry_run {
        output::print(cfg.effective_output(), &serde_json::json!({
            "dry_run": true,
            "method": A3chatRpcMethod::CHAT_MESSAGE_SEND,
            "params": envelope,
        }))?;
        return Ok(());
    }
    let v: serde_json::Value = client
        .call(
            A3chatRpcMethod::CHAT_MESSAGE_SEND,
            serde_json::to_value(&envelope).map_err(|e| {
                CliError::Internal(format!("encode envelope: {e}"))
            })?,
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn ack(cfg: &CliConfig, client: &HttpRpcClient, message_id: &str) -> CliResult<()> {
    let id = MessageId::from(message_id.to_string());
    let v: serde_json::Value = client
        .call(
            A3chatRpcMethod::CHAT_MESSAGE_ACK,
            serde_json::json!({ "message_id": id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn recall(cfg: &CliConfig, client: &HttpRpcClient, message_id: &str) -> CliResult<()> {
    let id = MessageId::from(message_id.to_string());
    let v: serde_json::Value = client
        .call(
            A3chatRpcMethod::CHAT_MESSAGE_RECALL,
            serde_json::json!({ "message_id": id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn edit(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    message_id: &str,
    body: &str,
) -> CliResult<()> {
    let id = MessageId::from(message_id.to_string());
    let body = MessageBody::Plain { content: body.to_string() };
    let v: serde_json::Value = client
        .call(
            A3chatRpcMethod::CHAT_MESSAGE_EDIT,
            serde_json::json!({
                "message_id": id,
                "body": body,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn delete(cfg: &CliConfig, client: &HttpRpcClient, message_id: &str) -> CliResult<()> {
    let id = MessageId::from(message_id.to_string());
    let v: serde_json::Value = client
        .call(
            A3chatRpcMethod::CHAT_MESSAGE_DELETE,
            serde_json::json!({ "message_id": id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn search(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    needle: &str,
    conversation_id: Option<String>,
    limit: u32,
) -> CliResult<()> {
    let params = serde_json::json!({
        "needle": needle,
        "conversation_id": conversation_id.map(ConversationId::from),
        "limit": limit,
    });
    let v: serde_json::Value = client
        .call(A3chatRpcMethod::CHAT_SEARCH, params)
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn typing(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: &str,
    expires_at: i64,
) -> CliResult<()> {
    let id = ConversationId::from(conversation_id.to_string());
    let v: serde_json::Value = client
        .call(
            A3chatRpcMethod::CHAT_TYPING,
            serde_json::json!({
                "conversation_id": id,
                "expires_at": expires_at,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

// Suppress unused import warning for AttachmentKind (kept for future
// attachment commands).
#[allow(dead_code)]
fn _attachment_kind_smoke() -> AttachmentKind {
    AttachmentKind::File
}