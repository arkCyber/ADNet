//! `a3chat media …` — blob-store upload / download helpers.

use std::path::PathBuf;

use clap::Subcommand;

use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum MediaCmd {
    /// Probe the media subsystem and report its config.
    Health,
    /// Begin a chunked upload. Prints the returned `token`.
    UploadInit {
        /// MIME type of the blob (e.g. `image/png`).
        #[arg(long)]
        mime: Option<String>,
    },
    /// Append a single chunk to an in-flight upload.
    UploadChunk {
        /// Token returned by `media upload-init`.
        #[arg(long)]
        token: String,
        /// Path to the chunk file (raw bytes).
        #[arg(long)]
        file: PathBuf,
    },
    /// Finalize a chunked upload and return the BLAKE3 hash.
    UploadFinalize {
        #[arg(long)]
        token: String,
        /// Optional original filename for the record.
        #[arg(long)]
        filename: Option<String>,
    },
    /// Fetch a blob by hash. Writes to `--out` (default stdout).
    DownloadGet {
        /// 64-hex BLAKE3 hash of the blob.
        #[arg(long)]
        hash: String,
        /// Output file; defaults to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

pub async fn run(
    cmd: MediaCmd,
    cfg: &CliConfig,
    client: &HttpRpcClient,
) -> CliResult<()> {
    match cmd {
        MediaCmd::Health => health(cfg, client).await,
        MediaCmd::UploadInit { mime } => upload_init(cfg, client, mime).await,
        MediaCmd::UploadChunk { token, file } => upload_chunk(cfg, client, token, file).await,
        MediaCmd::UploadFinalize { token, filename } => {
            upload_finalize(cfg, client, token, filename).await
        }
        MediaCmd::DownloadGet { hash, out } => download(cfg, client, hash, out).await,
    }
}

async fn health(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v = client
        .call_raw(A3chatRpcMethod::MEDIA_HEALTH, serde_json::json!({}))
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn upload_init(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    mime: Option<String>,
) -> CliResult<()> {
    let params = if let Some(m) = mime {
        serde_json::json!({ "mimeType": m })
    } else {
        serde_json::json!({})
    };
    let v = client
        .call_raw(A3chatRpcMethod::MEDIA_UPLOAD_INIT, params)
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn upload_chunk(
    _cfg: &CliConfig,
    client: &HttpRpcClient,
    token: String,
    file: PathBuf,
) -> CliResult<()> {
    if token.is_empty() {
        return Err(CliError::Usage("--token is required".into()));
    }
    if !file.is_file() {
        return Err(CliError::Usage(format!(
            "--file does not exist or is not a regular file: {}",
            file.display()
        )));
    }
    let bytes = std::fs::read(&file)?;
    let data_hex = hex::encode(&bytes);
    let v = client
        .call_raw(
            A3chatRpcMethod::MEDIA_UPLOAD_CHUNK,
            serde_json::json!({
                "token": token,
                "dataHex": data_hex,
            }),
        )
        .await?;
    output::print(_cfg.effective_output(), &v)
}

async fn upload_finalize(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    token: String,
    filename: Option<String>,
) -> CliResult<()> {
    if token.is_empty() {
        return Err(CliError::Usage("--token is required".into()));
    }
    let params = if let Some(f) = filename {
        serde_json::json!({ "token": token, "filename": f })
    } else {
        serde_json::json!({ "token": token })
    };
    let v = client
        .call_raw(A3chatRpcMethod::MEDIA_UPLOAD_FINALIZE, params)
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn download(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    hash: String,
    out: Option<PathBuf>,
) -> CliResult<()> {
    if hash.len() < 16 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CliError::Usage(
            "--hash must be ≥16 hex chars".into(),
        ));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::MEDIA_DOWNLOAD_GET,
            serde_json::json!({ "hash": hash }),
        )
        .await?;
    // The server returns an object with `data_hex` (lowercase hex) and
    // `size`. Decode hex → bytes, then write to `--out` or stdout.
    let data_hex = v
        .get("data_hex")
        .and_then(|x| x.as_str())
        .ok_or_else(|| CliError::Usage("media.download_get missing 'data_hex'".into()))?;
    let bytes = hex::decode(data_hex)
        .map_err(|e| CliError::Internal(format!("decode data_hex: {e}")))?;
    match out.as_ref() {
        Some(p) => {
            if let Some(parent) = p.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(p, &bytes)?;
            eprintln!("wrote {} bytes to {}", bytes.len(), p.display());
            if cfg.effective_output() == crate::config::OutputFormat::Json {
                output::print(
                    cfg.effective_output(),
                    &serde_json::json!({
                        "out": p,
                        "bytes": bytes.len(),
                    }),
                )?;
            }
        }
        None => {
            use std::io::Write;
            std::io::stdout().write_all(&bytes).map_err(CliError::Io)?;
        }
    }
    Ok(())
}
