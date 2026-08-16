//! `a3chat audit` — offline static audit. Produces a deterministic
//! report covering the a3chat API surface and schema invariants.

use clap::Subcommand;

use crate::audit_report::{generate_report, AuditReport};
use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum AuditCmd {
    /// Pure offline audit of the a3chat API surface. No daemon is
    /// contacted.
    Static,
    /// Probe a running daemon and report which `a3chat.*` methods
    /// it actually implements.
    Live {
        /// Stop probing after this many seconds (per call). Default 10.
        #[arg(long, default_value_t = 10)]
        timeout_secs: u64,
    },
    /// Static + live combined.
    Full {
        #[arg(long, default_value_t = 10)]
        timeout_secs: u64,
    },
}

pub async fn run(cmd: AuditCmd, cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    match cmd {
        AuditCmd::Static => static_only(cfg).await,
        AuditCmd::Live { timeout_secs } => live_only(cfg, client, timeout_secs).await,
        AuditCmd::Full { timeout_secs } => full(cfg, client, timeout_secs).await,
    }
}

async fn static_only(cfg: &CliConfig) -> CliResult<()> {
    let report = generate_report();
    output::print(cfg.effective_output(), &report)?;
    if report.summary.failed > 0 {
        return Err(CliError::Internal(format!(
            "audit: {} invariant(s) failed",
            report.summary.failed
        )));
    }
    Ok(())
}

async fn live_only(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    timeout_secs: u64,
) -> CliResult<()> {
    let report = probe_live(client, timeout_secs).await;
    output::print(cfg.effective_output(), &report)?;
    if report.failed > 0 {
        return Err(CliError::Internal(format!(
            "live audit: {} method(s) not implemented",
            report.failed
        )));
    }
    Ok(())
}

async fn full(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    timeout_secs: u64,
) -> CliResult<()> {
    let static_report = generate_report();
    let live_report = probe_live(client, timeout_secs).await;
    let report = serde_json::json!({
        "static": static_report,
        "live": live_report,
    });
    output::print(cfg.effective_output(), &report)?;
    if static_report.summary.failed > 0 || live_report.failed > 0 {
        return Err(CliError::Internal(format!(
            "audit failed: {} static, {} live",
            static_report.summary.failed, live_report.failed
        )));
    }
    Ok(())
}

/// Probe every `A3chatRpcMethod::ALL` against the live daemon.
/// The report lists each method's outcome — `implemented`,
/// `method_not_found`, or a transient error.
#[derive(Debug, serde::Serialize)]
struct LiveAuditReport {
    generated_at_unix: i64,
    daemon_url: String,
    owner: String,
    timeout_secs: u64,
    methods: Vec<LiveMethodOutcome>,
    passed: usize,
    failed: usize,
    errors: usize,
}

#[derive(Debug, serde::Serialize)]
struct LiveMethodOutcome {
    method: &'static str,
    outcome: &'static str, // "implemented" | "method_not_found" | "transient" | "internal"
    detail: String,
}

async fn probe_live(client: &HttpRpcClient, timeout_secs: u64) -> LiveAuditReport {
    use a3chat_core::rpc::A3chatRpcMethod;
    use std::time::Duration;
    let timeout = Duration::from_secs(timeout_secs.max(1));
    let mut methods = Vec::with_capacity(A3chatRpcMethod::ALL.len());
    let mut passed: usize = 0;
    let mut failed: usize = 0;
    let mut errors: usize = 0;
    for m in A3chatRpcMethod::ALL {
        let outcome = tokio::time::timeout(
            timeout,
            client.call_raw(*m, serde_json::json!({})),
        )
        .await;
        let (outcome_str, detail, counts) = match outcome {
            Ok(Ok(_)) => ("implemented", String::new(), (1usize, 0usize, 0usize)),
            Ok(Err(crate::error::CliError::Rpc(e))) => {
                let m = e.to_string();
                if m.contains("method not found") || m.contains("-32601") {
                    ("method_not_found", m, (0, 1, 0))
                } else if m.contains("A3chatApp does not handle")
                    || m.contains("ChatService does not handle")
                {
                    // The dispatcher reached the service but the
                    // service doesn't implement the method — i.e.
                    // it's a stub.
                    ("stub_no_handler", m, (0, 1, 0))
                } else if e.is_retryable() {
                    ("transient", m, (0, 0, 1))
                } else {
                    ("internal", m, (0, 1, 0))
                }
            }
            Ok(Err(other)) => ("internal", other.to_string(), (0, 1, 0)),
            Err(_) => ("transient", format!("timeout after {timeout_secs}s"), (0, 0, 1)),
        };
        passed += counts.0;
        failed += counts.1;
        errors += counts.2;
        methods.push(LiveMethodOutcome {
            method: m,
            outcome: outcome_str,
            detail,
        });
    }
    LiveAuditReport {
        generated_at_unix: chrono::Utc::now().timestamp(),
        daemon_url: client.base_url().to_string(),
        owner: client.owner().to_string(),
        timeout_secs,
        methods,
        passed,
        failed,
        errors,
    }
}

// Re-export so `lib.rs` doesn't have to know the module path.
pub use crate::audit_report::generate_report as generate;

#[allow(dead_code)]
fn _assert_report_send(_: &AuditReport) {} // type-level witness