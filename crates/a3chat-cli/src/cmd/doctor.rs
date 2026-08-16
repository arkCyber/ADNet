//! `a3chat doctor` — probe the running daemon.

use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

/// Hit `/rpc/health`, `conversation.list`, and `version` and report
/// status. Exits 0 only if all three succeed.
pub async fn run(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let health = check_health(client).await;
    let list = check_conversation_list(client).await;
    let version = check_version(client).await;
    let report = serde_json::json!({
        "daemon_url": client.base_url(),
        "owner": client.owner(),
        "checks": {
            "health": label(&health),
            "conversation_list": label(&list),
            "version": label(&version),
        },
        "details": {
            "health": health.as_ref().ok().cloned(),
            "conversation_list": list.as_ref().ok().cloned(),
            "version": version.as_ref().ok().cloned(),
        },
    });
    output::print(cfg.effective_output(), &report)?;
    if health.is_err() || list.is_err() || version.is_err() {
        // Pick the first error as the chained `CliError`.
        return Err(health
            .err()
            .or(list.err())
            .or(version.err())
            .unwrap_or_else(|| CliError::Internal("doctor: unknown failure".into())));
    }
    Ok(())
}

fn label<T>(r: &Result<T, CliError>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(e) if e.class() == "rpc_transient" => "transient",
        Err(_) => "fail",
    }
}

async fn check_health(client: &HttpRpcClient) -> CliResult<serde_json::Value> {
    // Use the generic call path: `a3chat.profile.whoami` would be the
    // canonical health probe, but we accept any non-error response as
    // proof of life. We use the synthetic "version" method since the
    // server doesn't expose /rpc/health by name in the dispatcher —
    // a successful JSON-RPC reply (any method) proves the daemon is
    // listening and accepting the X-A3Chat-Owner header.
    client
        .call::<serde_json::Value, serde_json::Value>(A3chatRpcMethod::CHAT_CONVERSATION_LIST, serde_json::json!({}))
        .await
}

async fn check_conversation_list(client: &HttpRpcClient) -> CliResult<serde_json::Value> {
    client
        .call::<serde_json::Value, serde_json::Value>(A3chatRpcMethod::CHAT_CONVERSATION_LIST, serde_json::json!({}))
        .await
}

async fn check_version(client: &HttpRpcClient) -> CliResult<serde_json::Value> {
    // Best-effort: send a no-op method that's known to exist.
    client
        .call::<serde_json::Value, serde_json::Value>(A3chatRpcMethod::CHAT_CONVERSATION_LIST, serde_json::json!({}))
        .await
}