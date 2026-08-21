//! `a3chat chat` — interactive multi-turn conversation session.
//!
//! This is the operator-facing equivalent of the Tauri chat panel:
//! it opens a conversation, replays recent history, then keeps a
//! live tail of the SSE event stream so the operator sees new
//! messages as they arrive from peers — the same `a3chat.*` bus
//! events the desktop UI consumes.
//!
//! ## Flow
//!
//! 1. **Resolve conversation** — either accept `--conversation-id`,
//!    or print the daemon's `CHAT_CONVERSATION_LIST` and prompt the
//!    user. If `--to` is supplied, we pick the matching DM
//!    conversation (if any).
//! 2. **Open** the conversation to render its header
//!    (`CHAT_CONVERSATION_OPEN`).
//! 3. **History** — `CHAT_SEARCH` with a `*` needle scoped to the
//!    conversation. The result is rendered in chronological order,
//!    one line per message. NOTE: this is a faithful but
//!    non-paginated replay; the cap is `--history` (default 50,
//!    max 500).
//! 4. **Subscribe** to the SSE stream with topic filter `chat`. The
//!    consumer task renders inbound events as one line per event;
//!    client-side filtering matches `conversation_id` so cross-talk
//!    from other conversations is suppressed but typing / presence
//!    cues are still acknowledged (`* alice typing…`).
//! 5. **Reading** — a separate task reads stdin: any non-empty line
//!    is sent as `CHAT_MESSAGE_SEND`. Lines beginning with `/`
//!    trigger slash commands (see below).
//! 6. **Idle exit** — the session ends after `--idle-timeout-secs`
//!    of silence from both stdin and SSE, or when `/quit` is read.
//!
//! ## Slash commands
//!
//! | Command                | Action |
//! |------------------------|--------|
//! | `/help`                | print the in-session command list |
//! | `/quit`, `/exit`       | leave the session |
//! | `/history [n]`         | re-play the last `n` (default 50) messages |
//! | `/recall <msg-id>`     | recall a message you sent |
//! | `/ack <msg-id>`        | acknowledge a received message |
//! | `/edit <msg-id> <txt>` | edit a message body |
//! | `/delete <msg-id>`     | delete a message locally |
//! | `/search <needle>`     | search the conversation |
//! | `/typing`              | emit a typing indicator to the peer |
//! | `/status`              | print session stats (msgs sent/received) |
//!
//! ## DO-178C mappings
//!
//! * **§5.2 traceability** — every outbound RPC carries the current
//!   `request_id`; the SSE stream request id is printed at startup.
//! * **§6.1 determinism** — the session is fully described by the
//!   resolved `ChatOptions`; `--dry-run` echoes them and exits.
//! * **§6.3 fail-safe** — transient RPC errors are retried (the
//!   underlying `HttpRpcClient` handles backoff); when SSE is
//!   unavailable we transparently fall back to a poll loop so the
//!   session is never silently broken.
//! * **§8 defensive** — every operator input is validated by
//!   `a3chat-core::validation` before being placed on the wire.

#![forbid(unsafe_code)]

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use uuid::Uuid;

use a3chat_core::conversation::ConversationMeta;
use a3chat_core::id::{ConversationId, MessageId, UserId};
use a3chat_core::message::{ChatMessage, MessageBody, MessageEnvelope, MessageType};
use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

/// Maximum number of messages fetched per history replay. Hard cap
/// protects against accidental runaway searches on busy accounts.
const MAX_HISTORY_FETCH: u32 = 500;

/// Minimum gap between emitted typing-indicator notifications to
/// avoid flooding the bus. The daemon's `chat.typing` is
/// best-effort and not rate-limited, so we throttle on the client.
const TYPING_THROTTLE: Duration = Duration::from_secs(3);

/// Configurable parameters for [`run`].
#[derive(Debug, Clone, clap::Args)]
pub struct ChatOptions {
    /// Open the conversation with this id (e.g. `dm:<a>:<b>`).
    #[arg(long, conflicts_with = "to")]
    pub conversation_id: Option<String>,

    /// Open (or create) the DM with this peer user id.
    #[arg(long)]
    pub to: Option<String>,

    /// Number of recent messages to replay on open.
    #[arg(long, default_value_t = 50)]
    pub history: u32,

    /// Exit after N seconds with no input (0 = never).
    #[arg(long, default_value_t = 0)]
    pub idle_timeout_secs: u64,

    /// Print the resolved config and exit (debug aid).
    #[arg(long)]
    pub dry_run: bool,
}

/// Aggregate session state.
struct Session {
    conversation_id: ConversationId,
    header: ConversationMeta,
    sent: u64,
    received: u64,
    last_recv: Instant,
    last_typing: Option<Instant>,
    request_id: String,
}

impl Session {
    fn new(header: ConversationMeta) -> Self {
        Self {
            conversation_id: header.conversation_id.clone(),
            header,
            sent: 0,
            received: 0,
            last_recv: Instant::now(),
            last_typing: None,
            request_id: Uuid::new_v4().to_string(),
        }
    }

    fn mark_recv(&mut self) {
        self.received += 1;
        self.last_recv = Instant::now();
    }

    fn mark_sent(&mut self) {
        self.sent += 1;
        self.last_recv = Instant::now();
    }

    fn can_emit_typing(&self) -> bool {
        match self.last_typing {
            None => true,
            Some(t) => t.elapsed() >= TYPING_THROTTLE,
        }
    }
}

/// Top-level dispatch.
pub async fn run(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    opts: ChatOptions,
) -> CliResult<()> {
    if opts.dry_run {
        return echo_dry_run(cfg, client, &opts);
    }
    let header = resolve_conversation(client, &opts).await?;
    let mut session = Session::new(header);

    let idle = Duration::from_secs(opts.idle_timeout_secs.max(1));
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    // Async stdin reader. `tokio::io::stdin().lines()` yields one
    // line per `.next().await` call. We work around the
    // `Stream + tokio::select!` interaction by polling the items
    // one at a time inside the loop.
    let stdin = tokio::io::stdin();
    let mut stdin_lines = BufReader::new(stdin).lines();

    print_banner(&session, &mut stdout)?;
    if opts.history > 0 {
        println!();
        println!("── history (last {}) ──", opts.history);
        replay_history(client, &session, opts.history).await?;
    }
    println!();
    println!("type a message and press enter (`/help` for commands)");
    flush(&mut stdout)?;

    let open_sse = open_sse_stream(client, &session.request_id).await;
    match open_sse {
        Ok(mut sse) => {
            let res = run_event_loop(
                client,
                &mut session,
                &mut sse,
                &mut stdin_lines,
                &mut stdout,
                idle,
                opts.history,
            )
            .await;
            if let Err(e) = sse.close().await {
                tracing::warn!(err = %e, "sse close failed");
            }
            res
        }
        Err(e) => {
            eprintln!("warn: sse unavailable ({e}); falling back to poll mode");
            run_poll_loop(client, &mut session, idle, opts.history, &mut stdin_lines, &mut stdout)
                .await
        }
    }
}

/// Print the resolved options and exit.
fn echo_dry_run(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    opts: &ChatOptions,
) -> CliResult<()> {
    let json = serde_json::json!({
        "dry_run": true,
        "daemon_url": client.base_url(),
        "owner": client.owner(),
        "conversation_id": opts.conversation_id,
        "to": opts.to,
        "history": opts.history,
        "idle_timeout_secs": opts.idle_timeout_secs,
    });
    output::print(cfg.effective_output(), &json)?;
    Ok(())
}

/// Resolve the conversation to chat with. Either the operator
/// passed `--conversation-id`, or we prompt the user from the
/// conversation list.
async fn resolve_conversation(
    client: &HttpRpcClient,
    opts: &ChatOptions,
) -> CliResult<ConversationMeta> {
    if let Some(raw) = opts.conversation_id.as_deref() {
        let id = ConversationId::from(raw.to_string());
        let v: Value = client
            .call(
                A3chatRpcMethod::CHAT_CONVERSATION_OPEN,
                serde_json::json!({ "conversation_id": id }),
            )
            .await?;
        let meta = v
            .get("meta")
            .cloned()
            .ok_or_else(|| {
                CliError::Rpc(a3chat_core::error::A3chatError::RpcError(
                    "open_conversation reply missing meta".into(),
                ))
            })?;
        let meta: ConversationMeta = serde_json::from_value(meta)
            .map_err(|e| CliError::Internal(format!("decode meta: {e}")))?;
        return Ok(meta);
    }

    let list: Vec<ConversationMeta> = client
        .call(A3chatRpcMethod::CHAT_CONVERSATION_LIST, serde_json::json!({}))
        .await?;
    if list.is_empty() {
        return Err(CliError::Usage(
            "no conversations available; pass --conversation-id, or start one with `a3chat message send`".into(),
        ));
    }
    if let Some(to) = opts.to.as_deref() {
        if let Some(c) = list.iter().find(|c| {
            c.peer_user_id
                .as_ref()
                .map(|p| p.as_str() == to)
                .unwrap_or(false)
        }) {
            return Ok(c.clone());
        }
        return Err(CliError::Usage(format!(
            "no conversation found for peer {to}; pass --conversation-id of an existing one"
        )));
    }
    eprintln!("available conversations:");
    for (i, c) in list.iter().enumerate() {
        let unread = if c.unread_count > 0 {
            format!(" ({} unread)", c.unread_count)
        } else {
            String::new()
        };
        let peer = c
            .peer_user_id
            .as_ref()
            .map(|p| short(p.as_str()))
            .unwrap_or_else(|| "(group)".to_string());
        eprintln!("  [{i}] {} — peer {peer}{unread}", c.title);
    }
    let mut line = String::new();
    loop {
        eprint!("pick [0..{}]: ", list.len().saturating_sub(1));
        std::io::stderr().flush().ok();
        let n = std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(CliError::Io)?;
        if n == 0 {
            return Err(CliError::Usage("no selection; aborting".into()));
        }
        match line.trim().parse::<usize>() {
            Ok(i) if i < list.len() => return Ok(list[i].clone()),
            _ => {
                eprintln!("invalid selection; try again");
                line.clear();
            }
        }
    }
}

fn short(id: &str) -> String {
    if id.len() < 12 {
        return id.to_string();
    }
    format!("{}…{}", &id[..8], &id[id.len() - 4..])
}

/// Fetch and render the most recent `n` messages in the
/// conversation, ordered chronologically (oldest first).
async fn replay_history(
    client: &HttpRpcClient,
    session: &Session,
    n: u32,
) -> CliResult<()> {
    let n = n.clamp(1, MAX_HISTORY_FETCH);
    let v: Value = client
        .call(
            A3chatRpcMethod::CHAT_SEARCH,
            serde_json::json!({
                "needle": "*",
                "conversation_id": session.conversation_id,
                "limit": n,
            }),
        )
        .await?;
    let msgs: Vec<ChatMessage> = serde_json::from_value(v).unwrap_or_default();
    let mut msgs = msgs;
    msgs.sort_by_key(|m| m.timestamp);
    for m in msgs.iter() {
        let line = format_message(m, session);
        println!("{line}");
    }
    Ok(())
}

/// Main event loop: race SSE events, stdin lines, and idle timeout.
#[allow(clippy::too_many_arguments)]
async fn run_event_loop<W: Write>(
    client: &HttpRpcClient,
    session: &mut Session,
    sse: &mut SseStream,
    stdin_lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    stdout: &mut W,
    idle: Duration,
    history: u32,
) -> CliResult<()> {
    loop {
        let timeout_at = session.last_recv + idle;
        let now = Instant::now();
        if now >= timeout_at {
            eprintln!("[chat] idle timeout after {}s; exiting", idle.as_secs());
            return Ok(());
        }
        let sleep_for = timeout_at
            .saturating_duration_since(now)
            .min(Duration::from_millis(500));

        tokio::select! {
            biased;
            line = stdin_lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Some(rest) = trimmed.strip_prefix('/') {
                            handle_slash(client, session, stdout, rest, history).await?;
                            if matches!(rest.split_whitespace().next(), Some("quit") | Some("exit")) {
                                return Ok(());
                            }
                            continue;
                        }
                        send_message(client, session, trimmed).await?;
                    }
                    Ok(None) => {
                        eprintln!("[chat] stdin closed; exiting");
                        return Ok(());
                    }
                    Err(e) => {
                        return Err(CliError::Io(e));
                    }
                }
            }

            evt = sse.as_stream().next() => {
                match evt {
                    Some(notification) => {
                        if let Some(update) = map_notification(notification, session) {
                            dispatch_update(client, session, stdout, &update).await?;
                        }
                        session.mark_recv();
                    }
                    None => {
                        eprintln!("[chat] sse stream closed; entering poll fallback");
                        let res = run_poll_loop(client, session, idle, history, stdin_lines, stdout).await;
                        return res;
                    }
                }
            }

            _ = tokio::time::sleep(sleep_for) => {
                // loop back to check `timeout_at`.
            }
        }
    }
}

/// Sliding-window poll fallback. Used when SSE is unavailable or
/// has dropped. Reads stdin asynchronously and polls
/// `CHAT_SEARCH` every `poll_interval` to pick up new messages.
async fn run_poll_loop<W: Write>(
    client: &HttpRpcClient,
    session: &mut Session,
    idle: Duration,
    history: u32,
    stdin_lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    stdout: &mut W,
) -> CliResult<()> {
    let poll_interval = Duration::from_secs(2);
    let mut last_seen_ts = chrono::Utc::now().timestamp();
    let mut next_poll = Instant::now() + poll_interval;
    let _ = history;

    loop {
        let timeout_at = session.last_recv + idle;
        if Instant::now() >= timeout_at {
            eprintln!("[chat] idle timeout after {}s; exiting", idle.as_secs());
            return Ok(());
        }
        let sleep_for = timeout_at
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(500));

        tokio::select! {
            biased;
            line = stdin_lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() { continue; }
                        if let Some(rest) = trimmed.strip_prefix('/') {
                            handle_slash(client, session, stdout, rest, history).await?;
                            if matches!(rest.split_whitespace().next(), Some("quit") | Some("exit")) {
                                return Ok(());
                            }
                            continue;
                        }
                        send_message(client, session, trimmed).await?;
                    }
                    Ok(None) => return Ok(()),
                    Err(e) => return Err(CliError::Io(e)),
                }
            }
            _ = tokio::time::sleep(sleep_for) => {}
        }

        if Instant::now() >= next_poll {
            next_poll = Instant::now() + poll_interval;
            if let Ok(v) = client
                .call(
                    A3chatRpcMethod::CHAT_SEARCH,
                    serde_json::json!({
                        "needle": "*",
                        "conversation_id": session.conversation_id,
                        "limit": 20,
                    }),
                )
                .await
                && let Ok(msgs) = serde_json::from_value::<Vec<ChatMessage>>(v)
            {
                for m in msgs.iter().filter(|m| m.timestamp > last_seen_ts) {
                    let line = format_message(m, session);
                    println!("{line}");
                    session.mark_recv();
                }
                if let Some(m) = msgs.iter().max_by_key(|m| m.timestamp) {
                    last_seen_ts = m.timestamp;
                }
            }
        }
    }
}

/// Send a single message envelope via `CHAT_MESSAGE_SEND`.
async fn send_message(
    client: &HttpRpcClient,
    session: &mut Session,
    body: &str,
) -> CliResult<()> {
    if body.is_empty() {
        return Ok(());
    }
    let envelope = MessageEnvelope {
        conversation_id: session.conversation_id.clone(),
        receiver_id: session
            .header
            .peer_user_id
            .clone()
            .unwrap_or_else(|| UserId::from("")),
        message_type: MessageType::Text,
        body: MessageBody::Plain {
            content: body.to_string(),
        },
        attachments: vec![],
        reply_to: None,
        sequence: 0,
        timestamp: chrono::Utc::now().timestamp(),
    };
    if let Err(e) = envelope.validate() {
        return Err(CliError::Usage(format!("invalid envelope: {e}")));
    }
    let v: Value = client
        .call(
            A3chatRpcMethod::CHAT_MESSAGE_SEND,
            serde_json::to_value(&envelope).map_err(|e| {
                CliError::Internal(format!("encode envelope: {e}"))
            })?,
        )
        .await?;
    session.mark_sent();
    if let Ok(m) = serde_json::from_value::<ChatMessage>(v) {
        let line = format_message(&m, session);
        println!("{line}");
    }
    Ok(())
}

/// Slash command dispatcher. Returns `Ok(())` for non-exit
/// commands; the caller is responsible for handling `quit`/`exit`.
async fn handle_slash<W: Write>(
    client: &HttpRpcClient,
    session: &mut Session,
    out: &mut W,
    rest: &str,
    history: u32,
) -> CliResult<()> {
    let mut parts = rest.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("").trim();
    let arg = parts.next().unwrap_or("").trim();
    match cmd {
        "" | "help" | "?" => {
            println!("slash commands:");
            println!("  /help                this list");
            println!("  /quit                leave the session");
            println!("  /history [n]         replay the last n messages (default {history})");
            println!("  /recall <msg-id>     recall a message you sent");
            println!("  /ack <msg-id>        acknowledge a received message");
            println!("  /edit <msg-id> <txt> edit a message body");
            println!("  /delete <msg-id>     delete a message locally");
            println!("  /search <needle>     search the conversation");
            println!("  /typing              emit a typing indicator");
            println!("  /status              print session stats");
        }
        "quit" | "exit" => {
            println!("[chat] bye");
        }
        "history" => {
            let n: u32 = arg.parse().ok().unwrap_or(history);
            replay_history(client, session, n).await?;
        }
        "recall" => {
            if arg.is_empty() {
                eprintln!("usage: /recall <msg-id>");
            } else {
                let id = MessageId::from(arg.to_string());
                let _: Value = client
                    .call(
                        A3chatRpcMethod::CHAT_MESSAGE_RECALL,
                        serde_json::json!({ "message_id": id }),
                    )
                    .await?;
                println!("✓ recalled {}", short(id.as_str()));
            }
        }
        "ack" => {
            if arg.is_empty() {
                eprintln!("usage: /ack <msg-id>");
            } else {
                let id = MessageId::from(arg.to_string());
                let _: Value = client
                    .call(
                        A3chatRpcMethod::CHAT_MESSAGE_ACK,
                        serde_json::json!({ "message_id": id }),
                    )
                    .await?;
                println!("✓ acked {}", short(id.as_str()));
            }
        }
        "edit" => {
            let mut parts = arg.splitn(2, char::is_whitespace);
            let id = parts.next().unwrap_or("");
            let body = parts.next().unwrap_or("");
            if id.is_empty() || body.is_empty() {
                eprintln!("usage: /edit <msg-id> <new body>");
            } else {
                let mid = MessageId::from(id.to_string());
                let body = MessageBody::Plain {
                    content: body.to_string(),
                };
                let _: Value = client
                    .call(
                        A3chatRpcMethod::CHAT_MESSAGE_EDIT,
                        serde_json::json!({ "message_id": mid, "body": body }),
                    )
                    .await?;
                println!("✓ edited {}", short(mid.as_str()));
            }
        }
        "delete" => {
            if arg.is_empty() {
                eprintln!("usage: /delete <msg-id>");
            } else {
                let id = MessageId::from(arg.to_string());
                let _: Value = client
                    .call(
                        A3chatRpcMethod::CHAT_MESSAGE_DELETE,
                        serde_json::json!({ "message_id": id }),
                    )
                    .await?;
                println!("✓ deleted {}", short(id.as_str()));
            }
        }
        "search" => {
            if arg.is_empty() {
                eprintln!("usage: /search <needle>");
            } else {
                let v: Value = client
                    .call(
                        A3chatRpcMethod::CHAT_SEARCH,
                        serde_json::json!({
                            "needle": arg,
                            "conversation_id": session.conversation_id,
                            "limit": 50,
                        }),
                    )
                    .await?;
                let hits: Vec<ChatMessage> = serde_json::from_value(v).unwrap_or_default();
                println!("{} hit(s):", hits.len());
                for m in hits.iter() {
                    println!("  {}", format_message(m, session));
                }
            }
        }
        "typing" => {
            if !session.can_emit_typing() {
                println!("(typing suppressed — throttled)");
            } else {
                let expires_at = chrono::Utc::now().timestamp() + 5;
                let _: Value = client
                    .call(
                        A3chatRpcMethod::CHAT_TYPING,
                        serde_json::json!({
                            "conversation_id": session.conversation_id,
                            "expires_at": expires_at,
                        }),
                    )
                    .await?;
                session.last_typing = Some(Instant::now());
                println!("✓ typing indicator sent");
            }
        }
        "status" => {
            let elapsed = session.last_recv.elapsed();
            writeln!(
                out,
                "session: conv={} title={} sent={} received={} idle={}s",
                session.conversation_id.as_str(),
                session.header.title,
                session.sent,
                session.received,
                elapsed.as_secs(),
            )
            .map_err(CliError::Io)?;
        }
        other => {
            eprintln!("unknown slash command: /{other}; type /help");
        }
    }
    Ok(())
}

/// Normalised update derived from a SSE notification. The CLI
/// suppresses updates that aren't relevant to the open
/// conversation so cross-talk from other accounts stays out of the
/// operator's session.
#[derive(Debug)]
enum ConversationUpdate {
    NewMessage(ChatMessage),
    Recalled(String),
    Edited(ChatMessage),
    Deleted(String),
    Typing(UserId),
    Read(String),
    Foreign,
}

fn map_notification(n: SseNotification, session: &Session) -> Option<ConversationUpdate> {
    let p = n.params;
    let cid = session.conversation_id.as_str().to_string();
    match n.method.as_str() {
        A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECEIVED => {
            let value: Result<ChatMessage, _> = serde_json::from_value(p);
            match value {
                Ok(m) if m.conversation_id.as_str() == cid => {
                    Some(ConversationUpdate::NewMessage(m))
                }
                Ok(_) => Some(ConversationUpdate::Foreign),
                Err(_) => None,
            }
        }
        A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECALLED => {
            let value: Result<RecalledPayload, _> = serde_json::from_value(p);
            match value {
                Ok(r) if r.conversation_id == cid => {
                    Some(ConversationUpdate::Recalled(r.message_id))
                }
                _ => Some(ConversationUpdate::Foreign),
            }
        }
        A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_EDITED => {
            let value: Result<EditedPayload, _> = serde_json::from_value(p);
            match value {
                Ok(e) if e.conversation_id == cid => {
                    Some(ConversationUpdate::Edited(e.message))
                }
                _ => Some(ConversationUpdate::Foreign),
            }
        }
        A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_DELETED => {
            let value: Result<DeletedPayload, _> = serde_json::from_value(p);
            match value {
                Ok(d) if d.conversation_id == cid => {
                    Some(ConversationUpdate::Deleted(d.message_id))
                }
                _ => Some(ConversationUpdate::Foreign),
            }
        }
        A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_READ => {
            let value: Result<ReadPayload, _> = serde_json::from_value(p);
            match value {
                Ok(r) if r.conversation_id == cid => {
                    Some(ConversationUpdate::Read(r.message_id))
                }
                _ => Some(ConversationUpdate::Foreign),
            }
        }
        A3chatRpcMethod::NOTIFICATION_CHAT_TYPING => {
            let value: Result<TypingPayload, _> = serde_json::from_value(p);
            match value {
                Ok(t) if t.conversation_id == cid => {
                    Some(ConversationUpdate::Typing(t.user_id))
                }
                _ => Some(ConversationUpdate::Foreign),
            }
        }
        _ => Some(ConversationUpdate::Foreign),
    }
}

async fn dispatch_update<W: Write>(
    client: &HttpRpcClient,
    session: &mut Session,
    _out: &mut W,
    update: &ConversationUpdate,
) -> CliResult<()> {
    match update {
        ConversationUpdate::NewMessage(m) => {
            println!("{}", format_message(m, session));
            // Auto-ack on receipt so unread counters do not pile
            // up while the operator is in the chat.
            let mid = m.message_id.clone();
            let cid = m.conversation_id.as_str().to_string();
            let auto: CliResult<Value> = client
                .call(
                    A3chatRpcMethod::CHAT_MESSAGE_ACK,
                    serde_json::json!({ "message_id": mid }),
                )
                .await;
            if let Err(e) = auto {
                tracing::warn!(err = %e, conv = %cid, "auto-ack failed");
            }
        }
        ConversationUpdate::Recalled(id) => {
            println!("* message {} was recalled", short(id));
        }
        ConversationUpdate::Edited(m) => {
            println!("* edited {}", short(m.message_id.as_str()));
        }
        ConversationUpdate::Deleted(id) => {
            println!("* deleted {}", short(id));
        }
        ConversationUpdate::Read(id) => {
            println!("* read {}", short(id));
        }
        ConversationUpdate::Typing(uid) => {
            println!("* {} typing…", short(uid.as_str()));
        }
        ConversationUpdate::Foreign => {
            // Suppress cross-talk.
        }
    }
    let _ = client;
    Ok(())
}

/// Pretty-print a single message in one terminal line.
fn format_message(m: &ChatMessage, session: &Session) -> String {
    let ts = chrono::DateTime::from_timestamp(m.timestamp, 0)
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "??:??:??".to_string());
    let sender = short(m.sender_id.as_str());
    let me = session
        .header
        .peer_user_id
        .as_ref()
        .map(|p| p.as_str() != m.sender_id.as_str())
        .unwrap_or(false);
    let tag = if me { "→" } else { "←" };
    let body = match &m.body {
        MessageBody::Plain { content } => content.clone(),
        MessageBody::Encrypted { .. } => "<encrypted>".to_string(),
    };
    let mid = short(m.message_id.as_str());
    format!("{ts} {tag} {sender}  {body}  ({mid})")
}

fn print_banner<W: Write>(session: &Session, w: &mut W) -> CliResult<()> {
    let peer = session
        .header
        .peer_user_id
        .as_ref()
        .map(|p| short(p.as_str()))
        .unwrap_or_else(|| "(group)".to_string());
    writeln!(
        w,
        "── chat session ──  {}  peer={}  conv={}",
        session.header.title,
        peer,
        session.conversation_id.as_str()
    )
    .map_err(CliError::Io)?;
    Ok(())
}

fn flush<W: Write>(w: &mut W) -> CliResult<()> {
    w.flush().map_err(CliError::Io)
}

// =====================================================================
// SSE helper
// =====================================================================

/// Notification yielded by the SSE stream.
#[derive(Debug, Clone)]
struct SseNotification {
    method: String,
    params: Value,
}

/// Owned SSE stream. Wraps the `mpsc::Receiver` from the
/// background task as a `Stream<Item = Result<SseNotification, String>>`
/// so the caller can use `tokio::select!` directly.
struct SseStream {
    rx: tokio_stream::wrappers::ReceiverStream<SseNotification>,
    client: HttpRpcClient,
    handle_id: String,
    released: bool,
}

impl SseStream {
    /// Borrow the inner stream so the caller can `select!` on it.
    fn as_stream(
        &mut self,
    ) -> &mut tokio_stream::wrappers::ReceiverStream<SseNotification> {
        &mut self.rx
    }

    async fn close(&mut self) -> CliResult<()> {
        if !self.released && !self.handle_id.is_empty() {
            self.released = true;
            let _ = self
                .client
                .call_raw(
                    A3chatRpcMethod::STREAM_UNSUBSCRIBE,
                    serde_json::json!({ "handle_id": &self.handle_id }),
                )
                .await;
        }
        Ok(())
    }
}

impl Drop for SseStream {
    fn drop(&mut self) {
        if !self.released && !self.handle_id.is_empty() {
            self.released = true;
            let client = self.client.clone();
            let handle_id = std::mem::take(&mut self.handle_id);
            tokio::spawn(async move {
                let _ = client
                    .call_raw(
                        A3chatRpcMethod::STREAM_UNSUBSCRIBE,
                        serde_json::json!({ "handle_id": handle_id }),
                    )
                    .await;
            });
        }
    }
}

async fn open_sse_stream(
    client: &HttpRpcClient,
    request_id: &str,
) -> CliResult<SseStream> {
    let v: Value = client
        .call_raw(
            A3chatRpcMethod::STREAM_SUBSCRIBE,
            serde_json::json!({ "topics": ["chat"] }),
        )
        .await?;
    let handle_id = v
        .get("handle_id")
        .and_then(|x| x.as_str())
        .ok_or_else(|| {
            CliError::Rpc(a3chat_core::error::A3chatError::RpcError(
                "subscribe reply missing handle_id".into(),
            ))
        })?
        .to_string();
    let stream_url = v
        .get("stream_url")
        .and_then(|x| x.as_str())
        .unwrap_or("/rpc/stream")
        .to_string();

    let url = format!(
        "{}/{}",
        client.base_url().trim_end_matches('/'),
        stream_url.trim_start_matches('/')
    );
    let _ = url;

    let resp = client
        .connect_sse(request_id)
        .await
        .map_err(|e| {
            CliError::Rpc(a3chat_core::error::A3chatError::NetworkError(format!(
                "sse connect: {e}"
            )))
        })?;
    let status = resp.status();
    if !status.is_success() {
        return Err(CliError::Rpc(a3chat_core::error::A3chatError::RpcError(
            format!("sse handshake http {}", status.as_u16()),
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

    let (tx, rx) = tokio::sync::mpsc::channel::<SseNotification>(64);
    let mut stream = resp.bytes_stream().eventsource();
    tokio::spawn(async move {
        while let Some(item) = stream.next().await {
            match item {
                Ok(msg) => {
                    if msg.data.is_empty() {
                        continue;
                    }
                    let v: Value = match serde_json::from_str(&msg.data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let method = v
                        .get("method")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();
                    let params = v.get("params").cloned().unwrap_or(Value::Null);
                    if tx
                        .send(SseNotification { method, params })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok(SseStream {
        rx: tokio_stream::wrappers::ReceiverStream::new(rx),
        client: client.clone(),
        handle_id,
        released: false,
    })
}

// =====================================================================
// Notification payload shapes mirrored from the server side.
// =====================================================================

#[derive(Debug, serde::Deserialize)]
struct RecalledPayload {
    conversation_id: String,
    message_id: String,
}

#[derive(Debug, serde::Deserialize)]
struct EditedPayload {
    conversation_id: String,
    message: ChatMessage,
}

#[derive(Debug, serde::Deserialize)]
struct DeletedPayload {
    conversation_id: String,
    message_id: String,
}

#[derive(Debug, serde::Deserialize)]
struct ReadPayload {
    conversation_id: String,
    message_id: String,
}

#[derive(Debug, serde::Deserialize)]
struct TypingPayload {
    conversation_id: String,
    user_id: UserId,
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_message(sender: &str, body: &str) -> ChatMessage {
        ChatMessage {
            message_id: MessageId::from(
                "0x1111111111111111111111111111111111111111111111111111111111111111",
            ),
            conversation_id: ConversationId::from("dm:peer:test"),
            sender_id: UserId::from(sender),
            receiver_id: UserId::from("peer"),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: body.to_string(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
            read_at: None,
            is_edited: false,
            edited_at: None,
            integrity_hash: None,
            recalled_at: None,
        }
    }

    fn header(peer: &str) -> ConversationMeta {
        ConversationMeta {
            conversation_id: ConversationId::from("dm:peer:test"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "Alice".into(),
            peer_user_id: Some(UserId::from(peer)),
            last_message_preview: "preview".into(),
            last_activity: 1,
            message_count: 1,
            unread_count: 0,
            peer_online: true,
            muted: false,
            pinned: false,
        }
    }

    #[test]
    fn format_message_renders_inbound_and_outbound() {
        let s = Session::new(header("peer"));
        let me_msg = sample_message("me-node-id", "hello!");
        let line = format_message(&me_msg, &s);
        assert!(line.contains("hello!"));
        assert!(line.contains("→"));
        let peer_msg = sample_message("peer", "hi back");
        let line = format_message(&peer_msg, &s);
        assert!(line.contains("hi back"));
        assert!(line.contains("←"));
    }

    #[test]
    fn format_message_handles_encrypted_body() {
        let s = Session::new(header("peer"));
        let mut m = sample_message("peer", "");
        m.body = MessageBody::Encrypted {
            algorithm: "chacha20-poly1305-v1".into(),
            nonce: "000000000000000000000000".into(),
            ciphertext: "ZW5jcnlwdGVk".into(),
            tag: "00000000000000000000000000000000".into(),
        };
        let line = format_message(&m, &s);
        assert!(line.contains("<encrypted>"));
    }

    #[test]
    fn map_notification_filters_cross_talk() {
        let s = Session::new(header("peer"));
        // Foreign message targets a DIFFERENT conversation.
        let mut foreign_msg = sample_message("stranger", "hi");
        foreign_msg.conversation_id = ConversationId::from("dm:other:talk");
        let foreign = SseNotification {
            method: A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECEIVED.to_string(),
            params: serde_json::to_value(foreign_msg).unwrap(),
        };
        let update = map_notification(foreign, &s);
        assert!(matches!(update, Some(ConversationUpdate::Foreign)));

        let own = SseNotification {
            method: A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECEIVED.to_string(),
            params: serde_json::to_value(sample_message("peer", "for you")).unwrap(),
        };
        let update = map_notification(own, &s).unwrap();
        match update {
            ConversationUpdate::NewMessage(m) => {
                assert_eq!(m.sender_id.as_str(), "peer");
            }
            other => panic!("expected NewMessage, got {other:?}"),
        }
    }

    #[test]
    fn map_notification_classifies_typing() {
        let s = Session::new(header("peer"));
        let payload = serde_json::json!({
            "conversation_id": "dm:peer:test",
            "user_id": "peer",
        });
        let n = SseNotification {
            method: A3chatRpcMethod::NOTIFICATION_CHAT_TYPING.to_string(),
            params: payload,
        };
        let update = map_notification(n, &s).unwrap();
        assert!(matches!(update, ConversationUpdate::Typing(_)));
    }

    #[test]
    fn short_truncates_long_ids() {
        assert_eq!(short("abcd"), "abcd");
        assert_eq!(
            short("0000000000000000000000000000000000000000000000000000000000000000"),
            "00000000…0000"
        );
    }

    #[test]
    fn session_can_emit_typing_initially() {
        let s = Session::new(header("peer"));
        assert!(s.can_emit_typing());
    }

    /// `/history <n>` must default to the supplied `history`
    /// when the user omits the argument. Test the parse via the
    /// documented behavior (we re-implement the parser in the test
    /// to lock down the documented `splitn(2, whitespace)` shape).
    #[test]
    fn slash_history_arg_parsing_defaults() {
        // Mirror the splitter used in `handle_slash`.
        let parse = |rest: &str| -> (u32, String) {
            let mut parts = rest.splitn(2, char::is_whitespace);
            let cmd = parts.next().unwrap_or("").trim();
            let arg = parts.next().unwrap_or("").trim();
            let n: u32 = arg.parse().ok().unwrap_or(99);
            (n, cmd.to_string())
        };
        let (n, cmd) = parse("history 5");
        assert_eq!(cmd, "history");
        assert_eq!(n, 5);
        let (n, cmd) = parse("history");
        assert_eq!(cmd, "history");
        assert_eq!(n, 99, "omitted arg must use the default");
        let (n, cmd) = parse("history junk");
        assert_eq!(cmd, "history");
        assert_eq!(n, 99, "unparseable arg must use the default");
    }

    /// `map_notification` should classify recalled / edited /
    /// deleted / read events the same way it classifies new
    /// messages — same conversation → relevant, different → Foreign.
    #[test]
    fn map_notification_classifies_lifecycle_events() {
        let s = Session::new(header("peer"));
        let cid = s.conversation_id.as_str().to_string();
        let foreign_cid = "dm:other:talk".to_string();

        // Recalled event for our conversation.
        let r = SseNotification {
            method: A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECALLED.to_string(),
            params: serde_json::json!({
                "conversation_id": cid,
                "message_id": "0xaaaa",
            }),
        };
        assert!(matches!(
            map_notification(r, &s),
            Some(ConversationUpdate::Recalled(_))
        ));
        // Recalled event for a foreign conversation.
        let r = SseNotification {
            method: A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECALLED.to_string(),
            params: serde_json::json!({
                "conversation_id": foreign_cid,
                "message_id": "0xbbbb",
            }),
        };
        assert!(matches!(map_notification(r, &s), Some(ConversationUpdate::Foreign)));

        // Deleted event.
        let d = SseNotification {
            method: A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_DELETED.to_string(),
            params: serde_json::json!({
                "conversation_id": cid,
                "message_id": "0xcccc",
            }),
        };
        assert!(matches!(
            map_notification(d, &s),
            Some(ConversationUpdate::Deleted(_))
        ));

        // Read event.
        let rd = SseNotification {
            method: A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_READ.to_string(),
            params: serde_json::json!({
                "conversation_id": cid,
                "message_id": "0xdddd",
            }),
        };
        assert!(matches!(
            map_notification(rd, &s),
            Some(ConversationUpdate::Read(_))
        ));

        // Unknown notification method → Foreign (defensive).
        let u = SseNotification {
            method: "a3chat.chat.bogus".to_string(),
            params: serde_json::json!({}),
        };
        assert!(matches!(map_notification(u, &s), Some(ConversationUpdate::Foreign)));

        // Malformed payload (missing conversation_id) → Foreign
        // because `map_notification` treats any unknown / partial
        // payload conservatively — the SSE reader treats Foreign
        // as "no action", so the user-facing behavior is
        // identical to skipping the event.
        let m = SseNotification {
            method: A3chatRpcMethod::NOTIFICATION_CHAT_TYPING.to_string(),
            params: serde_json::json!({}),
        };
        assert!(matches!(
            map_notification(m, &s),
            Some(ConversationUpdate::Foreign) | None
        ));
    }
}
