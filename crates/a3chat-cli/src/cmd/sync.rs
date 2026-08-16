//! `a3chat sync …` subcommands. Implements DO-178C §7.2
//! (reproducibility) by writing SHA-256 sidecars next to every
//! snapshot.

use std::path::PathBuf;

use clap::Subcommand;
use sha2::{Digest, Sha256};

use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum SyncCmd {
    /// Dump the local snapshot as JSON to stdout or `--out`.
    Snapshot {
        /// Optional output file (defaults to stdout).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Emit the SHA-256 of the payload as a sidecar `<file>.sha256`.
        #[arg(long)]
        sidecar: bool,
    },
    /// Fetch incremental delta from a list of cursor positions.
    Delta {
        /// JSON-encoded array of `[conversation_id, since_sequence]` pairs.
        #[arg(long)]
        cursors: String,
        /// Optional output file.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Request the zstd-compressed snapshot, write to file with
    /// sidecar SHA-256.
    Compressed {
        #[arg(long)]
        out: PathBuf,
    },
}

pub async fn run(cmd: SyncCmd, cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    match cmd {
        SyncCmd::Snapshot { out, sidecar } => snapshot(cfg, client, out, sidecar).await,
        SyncCmd::Delta { cursors, out } => delta(cfg, client, &cursors, out).await,
        SyncCmd::Compressed { out } => compressed(cfg, client, out).await,
    }
}

async fn snapshot(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    out: Option<PathBuf>,
    sidecar: bool,
) -> CliResult<()> {
    let v: serde_json::Value = client
        .call(A3chatRpcMethod::CHAT_SYNC_SNAPSHOT, serde_json::json!({}))
        .await?;
    let payload = serde_json::to_vec(&v)
        .map_err(|e| CliError::Internal(format!("encode snapshot: {e}")))?;
    write_with_sidecar(&payload, out.as_deref(), sidecar)?;
    if out.is_none() {
        // Also print the value for human consumption.
        output::print(cfg.effective_output(), &v)?;
    }
    Ok(())
}

async fn delta(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    cursors: &str,
    out: Option<PathBuf>,
) -> CliResult<()> {
    let parsed: serde_json::Value = serde_json::from_str(cursors).map_err(|e| {
        CliError::Usage(format!("--cursors must be JSON: {e}"))
    })?;
    let v: serde_json::Value = client
        .call(A3chatRpcMethod::CHAT_SYNC_DELTA, serde_json::json!({ "cursors": parsed }))
        .await?;
    if let Some(p) = out.as_ref() {
        let payload = serde_json::to_vec(&v)
            .map_err(|e| CliError::Internal(format!("encode delta: {e}")))?;
        std::fs::write(p, payload)?;
    } else {
        output::print(cfg.effective_output(), &v)?;
    }
    Ok(())
}

async fn compressed(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    out: PathBuf,
) -> CliResult<()> {
    // The daemon returns either raw bytes (if we wire binary mode)
    // or a base64 string. For now, we go via the JSON method and
    // expect base64; the helper `decode_b64` accepts both.
    let v: serde_json::Value = client
        .call(A3chatRpcMethod::CHAT_SYNC_COMPRESSED, serde_json::json!({}))
        .await?;
    let bytes = decode_b64(&v)?;
    write_with_sidecar(&bytes, Some(&out), true)?;
    eprintln!("wrote {} bytes to {} (+ .sha256)", bytes.len(), out.display());
    if cfg.effective_output() == crate::config::OutputFormat::Json {
        // Mirror the metadata for scripting.
        output::print(cfg.effective_output(), &serde_json::json!({
            "out": out,
            "bytes": bytes.len(),
            "sha256": hex::encode(Sha256::digest(&bytes)),
        }))?;
    }
    Ok(())
}

fn write_with_sidecar(payload: &[u8], out: Option<&std::path::Path>, sidecar: bool) -> CliResult<()> {
    match out {
        Some(p) => {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::write(p, payload)?;
            if sidecar {
                let hash = hex::encode(Sha256::digest(payload));
                let side = p.with_extension(format!(
                    "{}.sha256",
                    p.extension().and_then(|s| s.to_str()).unwrap_or("")
                ));
                std::fs::write(&side, format!("{hash}  {}\n", p.display()))?;
            }
            Ok(())
        }
        None => {
            std::io::Write::write_all(&mut std::io::stdout(), payload).map_err(CliError::Io)?;
            if sidecar {
                // No file → no sidecar. Emit a hint on stderr so the
                // operator knows why.
                eprintln!(
                    "warning: --sidecar ignored (no --out given); sha256 = {}",
                    hex::encode(Sha256::digest(payload))
                );
            }
            Ok(())
        }
    }
}

/// Decode the value returned by `CHAT_SYNC_COMPRESSED` into bytes.
/// Accepts either a plain string (base64) or an object with a
/// `payload_b64` field for forward compatibility.
fn decode_b64(v: &serde_json::Value) -> CliResult<Vec<u8>> {
    match v {
        serde_json::Value::String(s) => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(s)
                .map_err(|e| CliError::Internal(format!("base64: {e}")))
        }
        serde_json::Value::Object(map) => {
            let s = map
                .get("payload_b64")
                .and_then(|v| v.as_str())
                .ok_or_else(|| CliError::Internal("missing payload_b64".into()))?;
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(s)
                .map_err(|e| CliError::Internal(format!("base64: {e}")))
        }
        _ => Err(CliError::Internal(
            "compressed sync: unexpected payload shape".into(),
        )),
    }
}