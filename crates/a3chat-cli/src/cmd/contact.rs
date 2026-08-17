//! `a3chat contact …` — friend / blocklist management.

use std::path::PathBuf;

use clap::Subcommand;

use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum ContactCmd {
    /// List contacts + blocklist for the calling user.
    List,
    /// Send a friend request to a peer.
    Add {
        /// 64-hex NodeId of the destination user.
        #[arg(long)]
        to: String,
        /// Optional human-readable note (≤ 256 chars).
        #[arg(long, default_value = "")]
        message: String,
    },
    /// Directly add a contact without an inbound friend-request round-trip.
    /// Use this when the peer is reachable through a trusted channel
    /// (in-person, signed invitation) and you want to skip the request
    /// handshake. Calls `a3chat.contact.add`.
    AddDirect {
        /// 64-hex NodeId of the new contact.
        #[arg(long)]
        user_id: String,
        /// Display name to record against this contact.
        #[arg(long)]
        display_name: String,
        /// Optional note / tag.
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Accept an inbound friend request by id.
    Accept {
        #[arg(long)]
        request_id: String,
    },
    /// Block a user.
    Block {
        #[arg(long)]
        user_id: String,
    },
    /// Unblock a previously blocked user.
    Unblock {
        #[arg(long)]
        user_id: String,
    },
    /// Remove a contact from the local roster.
    Remove {
        #[arg(long)]
        user_id: String,
    },
    /// Fetch a single contact by id.
    Get {
        #[arg(long)]
        user_id: String,
    },
    /// Search contacts by name / tag substring.
    Search {
        #[arg(long)]
        query: String,
    },
    /// Toggle the favourite flag for a contact.
    ToggleFavorite {
        #[arg(long)]
        user_id: String,
    },
    /// Update mutable fields on an existing contact (display_name, note).
    Update {
        #[arg(long)]
        user_id: String,
        #[arg(long)]
        display_name: String,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Generate a base64-encoded QR invite payload for the calling user.
    QrInvite,
    /// Fetch the QR invite payload from the daemon and render it to an
    /// SVG file using [`a3net-qr`]. The QR encodes an
    /// `a3chat-contact://<base64>` URL so any a3net-aware scanner
    /// recognises it.
    QrInviteRender {
        /// Destination path for the rendered SVG. If omitted, writes
        /// to `qr-invite.svg` in the current directory.
        #[arg(long, default_value = "qr-invite.svg")]
        output: PathBuf,
        /// Optional description baked into the SVG as a caption
        /// (rendered below the QR).
        #[arg(long, default_value = "")]
        caption: String,
    },
}

pub async fn run(
    cmd: ContactCmd,
    cfg: &CliConfig,
    client: &HttpRpcClient,
) -> CliResult<()> {
    match cmd {
        ContactCmd::List => list(cfg, client).await,
        ContactCmd::Add { to, message } => add(cfg, client, to, message).await,
        ContactCmd::AddDirect { user_id, display_name, note } => {
            add_direct(cfg, client, user_id, display_name, note).await
        }
        ContactCmd::Accept { request_id } => accept(cfg, client, request_id).await,
        ContactCmd::Block { user_id } => block(cfg, client, user_id).await,
        ContactCmd::Unblock { user_id } => unblock(cfg, client, user_id).await,
        ContactCmd::Remove { user_id } => remove(cfg, client, user_id).await,
        ContactCmd::Get { user_id } => get(cfg, client, user_id).await,
        ContactCmd::Search { query } => search(cfg, client, query).await,
        ContactCmd::ToggleFavorite { user_id } => toggle_favorite(cfg, client, user_id).await,
        ContactCmd::Update { user_id, display_name, note } => {
            update(cfg, client, user_id, display_name, note).await
        }
        ContactCmd::QrInvite => qr_invite(cfg, client).await,
        ContactCmd::QrInviteRender { output, caption } => {
            qr_invite_render(cfg, client, &output, &caption).await
        }
    }
}

async fn list(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v = client
        .call_raw(A3chatRpcMethod::CONTACT_LIST, serde_json::json!({}))
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn add(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    to: String,
    message: String,
) -> CliResult<()> {
    if to.len() != 64 || !to.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CliError::Usage(format!(
            "--to must be a 64-char hex NodeId; got len={}",
            to.len()
        )));
    }
    if message.len() > 256 {
        return Err(CliError::Usage(
            "friend-request message exceeds 256 chars".into(),
        ));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::CONTACT_ADD_REQUEST,
            serde_json::json!({ "to_user_id": to, "message": message }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn add_direct(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    user_id: String,
    display_name: String,
    note: String,
) -> CliResult<()> {
    validate_hex_node_id(&user_id, "--user-id")?;
    if display_name.is_empty() {
        return Err(CliError::Usage("--display-name is required".into()));
    }
    if display_name.len() > 256 {
        return Err(CliError::Usage(
            "--display-name exceeds 256 chars".into(),
        ));
    }
    let now = chrono::Utc::now();
    let v = client
        .call_raw(
            A3chatRpcMethod::CONTACT_ADD,
            serde_json::json!({
                "user_id": user_id,
                "display_name": display_name,
                "avatar_url": serde_json::Value::Null,
                "note": note,
                "is_favorite": false,
                "is_blocked": false,
                "added_at": now.to_rfc3339(),
                "last_interaction_at": serde_json::Value::Null,
                "public_key": serde_json::Value::Null,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn remove(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    user_id: String,
) -> CliResult<()> {
    validate_hex_node_id(&user_id, "--user-id")?;
    let v = client
        .call_raw(
            A3chatRpcMethod::CONTACT_REMOVE,
            serde_json::json!({ "contact_id": user_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn get(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    user_id: String,
) -> CliResult<()> {
    validate_hex_node_id(&user_id, "--user-id")?;
    let v = client
        .call_raw(
            A3chatRpcMethod::CONTACT_GET,
            serde_json::json!({ "contact_id": user_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn search(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    query: String,
) -> CliResult<()> {
    if query.is_empty() {
        return Err(CliError::Usage("--query is required".into()));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::CONTACT_SEARCH,
            serde_json::json!({ "query": query }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn toggle_favorite(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    user_id: String,
) -> CliResult<()> {
    validate_hex_node_id(&user_id, "--user-id")?;
    let v = client
        .call_raw(
            A3chatRpcMethod::CONTACT_TOGGLE_FAVORITE,
            serde_json::json!({ "contact_id": user_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn update(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    user_id: String,
    display_name: String,
    note: String,
) -> CliResult<()> {
    validate_hex_node_id(&user_id, "--user-id")?;
    if display_name.is_empty() {
        return Err(CliError::Usage("--display-name is required".into()));
    }
    let now = chrono::Utc::now();
    let v = client
        .call_raw(
            A3chatRpcMethod::CONTACT_UPDATE,
            serde_json::json!({
                "user_id": user_id,
                "display_name": display_name,
                "avatar_url": serde_json::Value::Null,
                "note": note,
                "is_favorite": false,
                "is_blocked": false,
                "added_at": now.to_rfc3339(),
                "last_interaction_at": serde_json::Value::Null,
                "public_key": serde_json::Value::Null,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

fn validate_hex_node_id(s: &str, flag: &str) -> CliResult<()> {
    if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CliError::Usage(format!(
            "{flag} must be a 64-char hex NodeId; got len={}",
            s.len()
        )));
    }
    Ok(())
}

async fn accept(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    request_id: String,
) -> CliResult<()> {
    if request_id.is_empty() {
        return Err(CliError::Usage("--request-id is required".into()));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::CONTACT_ACCEPT_REQUEST,
            serde_json::json!({ "request_id": request_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn block(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    user_id: String,
) -> CliResult<()> {
    if user_id.len() != 64 || !user_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CliError::Usage(format!(
            "--user-id must be a 64-char hex NodeId; got len={}",
            user_id.len()
        )));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::CONTACT_BLOCK,
            serde_json::json!({ "user_id": user_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn unblock(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    user_id: String,
) -> CliResult<()> {
    if user_id.len() != 64 || !user_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CliError::Usage(format!(
            "--user-id must be a 64-char hex NodeId; got len={}",
            user_id.len()
        )));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::CONTACT_UNBLOCK,
            serde_json::json!({ "user_id": user_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn qr_invite(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v = client
        .call_raw(A3chatRpcMethod::CONTACT_QR_INVITE, serde_json::json!({}))
        .await?;
    output::print(cfg.effective_output(), &v)
}

/// Render the daemon's QR-invite payload as an SVG file.
///
/// The payload from the daemon is a base64url-encoded JSON blob
/// (see `ContactService::qr_invite` in `a3chat-app`). We wrap it in
/// an `a3chat-contact://<base64>` URL so any a3net-aware scanner can
/// recognise the scheme and dispatch it to the a3chat scanner flow.
///
/// SVG generation delegates to [`a3net_qr::generator::create_qr_svg`]
/// — the same library the rest of A3Net uses, so the QR codes are
/// visually consistent across `a3net-cli contact qr` and
/// `a3chat contact qr-invite-render`.
async fn qr_invite_render(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    output: &std::path::Path,
    caption: &str,
) -> CliResult<()> {
    let v = client
        .call_raw(A3chatRpcMethod::CONTACT_QR_INVITE, serde_json::json!({}))
        .await?;
    // The RPC returns `{"qr_payload": "<base64>"}` (an envelope),
    // not a raw string. Pull `qr_payload` out before wrapping it
    // in the URL scheme. Falling back to a raw-string envelope
    // keeps the command working with hand-crafted servers.
    let payload_b64 = v
        .get("qr_payload")
        .and_then(|x| x.as_str())
        .or_else(|| v.as_str())
        .ok_or_else(|| {
            CliError::Internal(format!(
                "qr_invite RPC returned neither {{qr_payload:..}} nor a string; got {v}"
            ))
        })?
        .to_string();
    let url = format!("a3chat-contact://{payload_b64}");

    let svg = if caption.is_empty() {
        a3net_qr::generator::create_qr_svg(&url)
            .map_err(|e| CliError::Internal(format!("a3net-qr render: {e}")))?
    } else {
        a3net_qr::generator::create_qr_card_svg(&url, caption)
            .map_err(|e| CliError::Internal(format!("a3net-qr card render: {e}")))?
    };

    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            CliError::Internal(format!("create_dir_all({}): {e}", parent.display()))
        })?;
    }
    std::fs::write(output, svg.as_bytes())
        .map_err(|e| CliError::Internal(format!("write qr svg: {e}")))?;

    // Inform the user on stdout regardless of the output format
    // setting — this is a side-effect command, not a query.
    eprintln!(
        "wrote {} bytes of QR-invite SVG to {} ({} bytes encoded)",
        svg.len(),
        output.display(),
        url.len()
    );
    let _ = cfg; // suppress unused
    Ok(())
}
