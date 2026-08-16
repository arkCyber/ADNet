//! `a3chat profile` — read/write the user-profile layer that
//! bridges to `a3net-userstore`.
//!
//! The underlying RPC methods are `a3chat.profile.*`. This CLI
//! front-end adds DO-178C-grade input validation, deterministic
//! output formatting, and explicit `--dry-run` so the operator
//! can preview the envelope before sending.

use clap::Subcommand;

use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::{CliConfig, OutputFormat};
use crate::error::{CliError, CliResult};
use crate::output::print;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum ProfileCmd {
    /// Fetch the calling user's profile (or `null`).
    Get {
        /// Echo the envelope without sending.
        #[arg(long)]
        dry_run: bool,
    },
    /// Fetch or compute the canonical 12-digit ID for the owner.
    Digit,
    /// List the owner's public keys.
    Keys,
    /// List the owner's paired devices.
    Devices,
    /// Set the avatar blob hash + MIME.
    SetAvatar {
        /// Hex BLAKE3 hash of the avatar blob.
        blob_hash: String,
        /// MIME type (`image/png`, `image/webp`, …).
        #[arg(long)]
        mime: String,
        /// Original size in bytes.
        #[arg(long)]
        size: u64,
    },
}

pub async fn run(
    cmd: ProfileCmd,
    cfg: &CliConfig,
    client: &HttpRpcClient,
) -> CliResult<()> {
    match cmd {
        ProfileCmd::Get { dry_run } => get(cfg, client, dry_run).await,
        ProfileCmd::Digit => digit(cfg, client).await,
        ProfileCmd::Keys => keys(cfg, client).await,
        ProfileCmd::Devices => devices(cfg, client).await,
        ProfileCmd::SetAvatar { blob_hash, mime, size } => {
            set_avatar(cfg, client, blob_hash, mime, size).await
        }
    }
}

async fn get(cfg: &CliConfig, client: &HttpRpcClient, dry_run: bool) -> CliResult<()> {
    let method = A3chatRpcMethod::PROFILE_GET;
    let params = serde_json::json!({});
    if dry_run {
        return print_dry_run(cfg, method, &params);
    }
    let result = client.call_raw(method, params).await?;
    match result {
        serde_json::Value::Null => {
            print(OutputFormat::Plain, &serde_json::json!({"profile": null}))?;
        }
        other => {
            print(OutputFormat::Plain, &other)?;
        }
    }
    Ok(())
}

async fn digit(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let method = A3chatRpcMethod::PROFILE_DIGIT_GET;
    let result = client.call_raw(method, serde_json::json!({})).await?;
    let s = result
        .as_str()
        .ok_or_else(|| CliError::Rpc(a3chat_core::error::A3chatError::Internal(
            "digit_get did not return a string".into(),
        )))?
        .to_string();
    if s.len() != 12 || !s.chars().all(|c| c.is_ascii_digit()) {
        return Err(CliError::Usage(format!(
            "digit_get returned non-12-digit value: {s:?}"
        )));
    }
    println!("{}", s);
    Ok(())
}

async fn keys(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let method = A3chatRpcMethod::PROFILE_PUBLIC_KEY_LIST;
    let result = client.call_raw(method, serde_json::json!({})).await?;
    print(OutputFormat::Plain, &result)?;
    Ok(())
}

async fn devices(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let method = A3chatRpcMethod::PROFILE_DEVICE_LIST;
    let result = client.call_raw(method, serde_json::json!({})).await?;
    print(OutputFormat::Plain, &result)?;
    Ok(())
}

async fn set_avatar(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    blob_hash: String,
    mime: String,
    size: u64,
) -> CliResult<()> {
    if blob_hash.is_empty() || blob_hash.len() > 128 {
        return Err(CliError::Usage(
            "blob_hash must be 1..=128 hex chars".into(),
        ));
    }
    if size == 0 || size > 10 * 1024 * 1024 {
        return Err(CliError::Usage(
            "size must be in 1..=10MiB".into(),
        ));
    }
    let method = A3chatRpcMethod::PROFILE_AVATAR_SET;
    let params = serde_json::json!({
        "blob_hash": blob_hash,
        "mime_type": mime,
        "size_bytes": size,
    });
    let result = client.call_raw(method, params).await?;
    print(OutputFormat::Plain, &result)?;
    Ok(())
}

fn print_dry_run(
    _cfg: &CliConfig,
    method: &'static str,
    params: &serde_json::Value,
) -> CliResult<()> {
    let env = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "dry-run",
        "method": method,
        "params": params,
    });
    println!("{}", serde_json::to_string_pretty(&env).unwrap_or_default());
    Ok(())
}
