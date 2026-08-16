//! `a3chat rpc <method> [--params '<json>']` — raw JSON-RPC fallback.
//!
//! Every subcommand in this CLI is a thin wrapper around
//! `a3chat-cli::rpc_client::HttpRpcClient`. The `rpc` subcommand
//! exposes that primitive directly so operators can drive any
//! `a3chat.*` method that doesn't yet have a dedicated subcommand
//! (contact, group, presence, media, e2e).
//!
//! DO-178C §5.2 — every `rpc` call still carries the
//! `X-A3Chat-Request-Id` header; we surface it in `--verbose` mode
//! for cross-referencing with daemon logs.

use std::time::Duration;

use clap::Subcommand;

use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

/// Maximum time the `rpc` subcommand will wait for a single reply.
pub const RPC_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Subcommand)]
pub enum RpcCmd {
    /// Call any `a3chat.*` method by name. The method is validated
    /// against the allow-list (`A3chatRpcMethod::ALL`) before being
    /// sent, so a typo can't crash the daemon.
    Call {
        /// Method name, e.g. `a3chat.contact.list`.
        method: String,
        /// JSON-encoded params object. Pass `null`, `{}`, or omit for
        /// no-arg methods.
        #[arg(long, short = 'p', default_value = "null")]
        params: String,
        /// Echo the request without sending.
        #[arg(long)]
        dry_run: bool,
        /// Per-call timeout override in milliseconds. Defaults to 30s.
        #[arg(long)]
        timeout_ms: Option<u64>,
        /// Skip retries entirely (single attempt).
        #[arg(long)]
        no_retry: bool,
    },
    /// List every method the CLI knows about (with grouping).
    Methods,
}

pub async fn run(cmd: RpcCmd, cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    match cmd {
        RpcCmd::Call {
            method,
            params,
            dry_run,
            timeout_ms,
            no_retry,
        } => call(cfg, client, &method, &params, dry_run, timeout_ms, no_retry).await,
        RpcCmd::Methods => list_methods(cfg),
    }
}

fn list_methods(cfg: &CliConfig) -> CliResult<()> {
    let grouped: std::collections::BTreeMap<&str, Vec<&str>> =
        A3chatRpcMethod::ALL
            .iter()
            .copied()
            .fold(std::collections::BTreeMap::new(), |mut acc, m| {
                let prefix = m
                    .strip_prefix("a3chat.")
                    .and_then(|s| s.split('.').next())
                    .unwrap_or("other");
                acc.entry(prefix).or_default().push(m);
                acc
            });
    output::print(cfg.effective_output(), &grouped)
}

async fn call(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    method: &str,
    raw_params: &str,
    dry_run: bool,
    timeout_ms: Option<u64>,
    no_retry: bool,
) -> CliResult<()> {
    // 1. Validate the method is on the allow-list. We refuse to
    //    forward unknown names so a typo can't crash the daemon.
    if !A3chatRpcMethod::ALL.contains(&method) {
        return Err(CliError::Usage(format!(
            "unknown method {method:?}; expected one of: {}",
            A3chatRpcMethod::ALL.join(", ")
        )));
    }

    // 2. Parse the params JSON.
    let params: serde_json::Value = if raw_params.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(raw_params).map_err(|e| {
            CliError::Usage(format!("--params must be JSON: {e}"))
        })?
    };

    if dry_run {
        output::print(
            cfg.effective_output(),
            &serde_json::json!({
                "dry_run": true,
                "method": method,
                "params": params,
            }),
        )?;
        return Ok(());
    }

    // 3. Issue the call. Use the typed call (call_raw) for
    //    arbitrary JSON.
    let t = tokio::time::timeout(
        timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(RPC_DEFAULT_TIMEOUT),
        client.call_raw_with_meta(
            method,
            params,
            if no_retry { 1 } else { client.retries() },
        ),
    );
    let result = match t.await {
        Ok(res) => res?,
        Err(_) => {
            return Err(CliError::Internal(format!(
                "rpc call timed out after {}ms",
                timeout_ms.unwrap_or(RPC_DEFAULT_TIMEOUT.as_millis() as u64)
            )))
        }
    };
    let env = serde_json::json!({
        "request_id": result.request_id,
        "attempts": result.attempts,
        "result": result.value,
    });
    output::print(cfg.effective_output(), &env)
}