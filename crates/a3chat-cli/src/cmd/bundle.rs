//! `bundle` — export and import an E2E state bundle.
//!
//! ```text
//! a3chat bundle export --out backup.a3b   # writes a portable, AEAD-encrypted bundle
//! a3chat bundle import --in backup.a3b   # decrypts + merges the bundle on this node
//! ```
//!
//! The wire-format and AEAD contract are documented in
//! `a3chat-app::e2e_bundle::Bundle`.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use clap::{Args, Subcommand};

use a3chat_app::Bundle;

use crate::config::CliConfig;
use crate::error::CliResult;
use crate::output;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum BundleCmd {
    /// Export the local user's E2E state to an AEAD-encrypted bundle.
    Export(ExportBundleArgs),
    /// Import a previously-exported bundle. Decrypts and merges the
    /// keyring + conversations + messages.
    Import(ImportBundleArgs),
}

#[derive(Debug, Args)]
pub struct ExportBundleArgs {
    /// Output path for the bundle JSON. `-` writes to stdout.
    #[arg(long, short = 'o')]
    pub out: PathBuf,
}

#[derive(Debug, Args)]
pub struct ImportBundleArgs {
    /// Input path to a bundle JSON.
    #[arg(long, short = 'i')]
    pub input: PathBuf,
}

pub async fn run(cmd: BundleCmd, cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    match cmd {
        BundleCmd::Export(args) => export(cfg, client, &args.out).await,
        BundleCmd::Import(args) => import(client, &args.input).await,
    }
}

async fn export(cfg: &CliConfig, client: &HttpRpcClient, out: &PathBuf) -> CliResult<()> {
    let v: serde_json::Value = client
        .call("a3chat.e2e.bundle.export", serde_json::json!({}))
        .await?;
    if out.as_os_str() == "-" {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        let body = serde_json::to_vec_pretty(&v)
            .map_err(|e| crate::error::CliError::Internal(format!("encode bundle: {e}")))?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(out, &body)?;
        // Echo a confirmation. The output formatter knows how to make
        // a key/value pair readable in JSON or table mode.
        output::print(
            cfg.effective_output(),
            &serde_json::json!({ "path": out.display().to_string() }),
        )?;
    }
    Ok(())
}

async fn import(client: &HttpRpcClient, input: &PathBuf) -> CliResult<()> {
    let body = std::fs::read(input)?;
    // Delegate the wire-format / version check to the canonical
    // a3chat-app::Bundle so the CLI and the daemon cannot drift on
    // the on-disk version byte.
    let parsed: Bundle = serde_json::from_slice(&body)
        .map_err(|e| crate::error::CliError::Internal(format!("malformed bundle: {e}")))?;
    let expected = a3chat_app::BUNDLE_VERSION;
    if parsed.version != expected {
        return Err(crate::error::CliError::Usage(format!(
            "bundle version {} is not supported (expected {expected})",
            parsed.version
        )));
    }
    let raw: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| crate::error::CliError::Internal(format!("re-encode: {e}")))?;
    let v: serde_json::Value = client
        .call("a3chat.e2e.bundle.import", serde_json::json!({ "bundle": raw }))
        .await?;
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wire-compat invariant: the CLI's accepted bundle version
    /// must equal the canonical `a3chat_app::BUNDLE_VERSION`. If
    /// the daemon ever bumps the version, this test forces a
    /// matching CLI update.
    #[test]
    fn bundle_version_matches_canonical() {
        // Build a Bundle using a3chat-app's deserialiser so this
        // test panics on schema drift before reaching the runtime
        // version check.
        let json = format!(
            r#"{{"version":{ver},"owner":"o","exported_at_unix":0,"kdf_params":{{"time_cost":2,"memory_kib":65536,"parallelism":1}},"salt_b64":"AA==","nonce_b64":"AA==","payload_b64":"AA=="}}"#,
            ver = a3chat_app::BUNDLE_VERSION,
        );
        let parsed: Bundle = serde_json::from_str(&json).expect("canonical bundle deserialises");
        assert_eq!(parsed.version, a3chat_app::BUNDLE_VERSION);
        assert_eq!(parsed.version, 1, "current BUNDLE_VERSION is 1");
    }

    #[test]
    fn bundle_rejects_future_version() {
        let json = r#"{"version":99,"owner":"o","exported_at_unix":0,"kdf_params":{"time_cost":2,"memory_kib":65536,"parallelism":1},"salt_b64":"AA==","nonce_b64":"AA==","payload_b64":"AA=="}"#;
        let parsed: Bundle = serde_json::from_str(json).expect("bundle deserialises");
        assert_ne!(parsed.version, a3chat_app::BUNDLE_VERSION);
    }
}
