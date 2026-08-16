//! `a3chat trace` — subscribe to the daemon's SSE event stream and
//! print notifications as they arrive. Designed for live debugging,
//! audit trails, and CI smoke tests.
//!
//! DO-178C §5.2 — every event is printed with the request_id we
//! negotiated so it can be cross-referenced with daemon logs.

use std::time::Duration;

use clap::Subcommand;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use uuid::Uuid;

use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

/// Maximum idle time before declaring the stream stale. 5 minutes
/// covers long-running tests; production callers should also wire
/// `--max-events` to bound the run.
pub const TRACE_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Default upper bound on the number of events to print before
/// exiting. `0` means "run forever" (until `--idle-timeout`).
pub const TRACE_DEFAULT_MAX_EVENTS: u32 = 0;

#[derive(Debug, Subcommand)]
pub enum TraceCmd {
    /// Subscribe to the daemon SSE event stream and print
    /// notifications as they arrive.
    Follow {
        /// Stop after N events. 0 = unlimited.
        #[arg(long, default_value_t = 0)]
        max_events: u32,
        /// Stop after this many seconds with no events.
        #[arg(long, default_value_t = 300)]
        idle_timeout_secs: u64,
        /// Stop after this many seconds total (wall-clock).
        #[arg(long)]
        max_duration_secs: Option<u64>,
        /// Filter: only print events whose `method` starts with this
        /// prefix (e.g. `a3chat.chat.message`).
        #[arg(long)]
        filter: Option<String>,
        /// Print one compact JSON object per line instead of pretty
        /// multi-line objects. Useful for `jq` pipelines.
        #[arg(long)]
        compact: bool,
        /// Echo the resolved connection params and exit (debug aid).
        #[arg(long)]
        dry_run: bool,
    },
    /// Print every notification kind the server may emit.
    Events,
}

pub async fn run(cmd: TraceCmd, cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    match cmd {
        TraceCmd::Follow {
            max_events,
            idle_timeout_secs,
            max_duration_secs,
            filter,
            compact,
            dry_run,
        } => {
            follow(
                cfg,
                client,
                max_events,
                idle_timeout_secs,
                max_duration_secs,
                filter,
                compact,
                dry_run,
            )
            .await
        }
        TraceCmd::Events => list_events(cfg),
    }
}

fn list_events(cfg: &CliConfig) -> CliResult<()> {
    output::print(cfg.effective_output(), &notification_methods())
}

async fn follow(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    max_events: u32,
    idle_timeout_secs: u64,
    max_duration_secs: Option<u64>,
    filter: Option<String>,
    compact: bool,
    dry_run: bool,
) -> CliResult<()> {
    if dry_run {
        output::print(
            cfg.effective_output(),
            &serde_json::json!({
                "dry_run": true,
                "url": format!("{}/rpc/stream", client.base_url()),
                "owner": client.owner(),
                "filter": filter,
                "max_events": max_events,
                "idle_timeout_secs": idle_timeout_secs,
            }),
        )?;
        return Ok(());
    }

    let request_id = Uuid::new_v4().to_string();
    eprintln!(
        "[trace] request_id={request_id} url={}/rpc/stream owner={}",
        client.base_url(),
        client.owner()
    );

    let resp = client.connect_sse(&request_id).await.map_err(|e| {
        CliError::Rpc(a3chat_core::error::A3chatError::NetworkError(format!(
            "sse connect: {e}"
        )))
    })?;
    if !resp.status().is_success() {
        return Err(CliError::Rpc(a3chat_core::error::A3chatError::RpcError(
            format!("sse handshake http {}", resp.status().as_u16()),
        )));
    }
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !ct.starts_with("text/event-stream") {
        return Err(CliError::Rpc(a3chat_core::error::A3chatError::RpcError(
            format!("expected text/event-stream, got {ct:?}"),
        )));
    }

    let mut stream = resp.bytes_stream().eventsource();

    let idle = Duration::from_secs(idle_timeout_secs.max(1));
    let started = std::time::Instant::now();
    let max_duration = max_duration_secs.map(Duration::from_secs);

    let mut count: u32 = 0;
    loop {
        if let Some(md) = max_duration {
            if started.elapsed() >= md {
                eprintln!("[trace] max_duration reached, exiting");
                break;
            }
        }
        if max_events > 0 && count >= max_events {
            break;
        }
        let next = tokio::time::timeout(idle, stream.next()).await;
        match next {
            Err(_) => {
                return Err(CliError::Rpc(
                    a3chat_core::error::A3chatError::NetworkError(format!(
                        "trace idle for {}s",
                        idle.as_secs()
                    )),
                ));
            }
            Ok(None) => {
                eprintln!("[trace] server closed the stream");
                break;
            }
            Ok(Some(Err(e))) => {
                return Err(CliError::Rpc(
                    a3chat_core::error::A3chatError::RpcError(format!(
                        "sse parse: {e}"
                    )),
                ));
            }
            Ok(Some(Ok(msg))) => {
                let method = msg.event.clone();
                let payload: serde_json::Value = serde_json::from_str(&msg.data)
                    .unwrap_or(serde_json::Value::String(msg.data.clone()));
                if let Some(ref f) = filter {
                    if !method.starts_with(f) {
                        continue;
                    }
                }
                let env = serde_json::json!({
                    "kind": "event",
                    "seq": count,
                    "method": method,
                    "data": payload,
                });
                if compact {
                    println!("{}", serde_json::to_string(&env).unwrap_or_default());
                } else {
                    output::print(cfg.effective_output(), &env)?;
                }
                count += 1;
            }
        }
    }
    eprintln!("[trace] delivered {count} events");
    Ok(())
}

/// Convenience for tests — list every notification kind.
pub fn notification_methods() -> Vec<&'static str> {
    vec![
        A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECEIVED,
        A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECALLED,
        A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_READ,
        A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_EDITED,
        A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_DELETED,
        A3chatRpcMethod::NOTIFICATION_CHAT_TYPING,
        A3chatRpcMethod::NOTIFICATION_PRESENCE_CHANGED,
        A3chatRpcMethod::NOTIFICATION_GROUP_MEMBER_JOINED,
        A3chatRpcMethod::NOTIFICATION_GROUP_INVITATION_RECEIVED,
        A3chatRpcMethod::NOTIFICATION_CONTACT_REQUEST_RECEIVED,
    ]
}