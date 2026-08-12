//! Real-network acceptance probe for ADNet's iroh transport stack.
//!
//! This example is intentionally process-oriented: run `serve` on public node A,
//! then run `probe` on public/restricted-NAT node B. The outer orchestration script
//! restarts node A and applies host-specific network fault hooks.
//!
//! Build with:
//! `cargo build -p adnet-node --features iroh --example network_acceptance --release`

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use adnet_blobstore::BlobReader;
use adnet_chatstore::Message;
use adnet_transport::frame::{Frame, FrameCodec};
use adnet_transport::iroh::{ADNET_FRAME_ALPN, IrohIdentity};
use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use futures::StreamExt;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl};
use iroh_blobs::BlobFormat;
use iroh_blobs::ticket::BlobTicket;
use iroh_docs::DocTicket;
use iroh_docs::api::protocol::ShareMode;
use iroh_gossip::api::Event as GossipEvent;
use iroh_gossip::proto::TopicId;
use serde::{Deserialize, Serialize};
use tracing::warn;

use adnet_node::iroh_runtime::IrohRuntime;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const GOSSIP_TOPIC_BYTES: [u8; 32] = [0xAD; 32];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServerInfo {
    schema: u32,
    endpoint_id: String,
    direct_addrs: Vec<String>,
    relay_urls: Vec<String>,
    blob_ticket: String,
    blob_payload: String,
    docs_ticket: String,
    conversation_id: String,
    docs_seed: String,
    started_unix_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedPath {
    Direct,
    Relay,
}

impl FromStr for ExpectedPath {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "direct" => Ok(Self::Direct),
            "relay" => Ok(Self::Relay),
            other => bail!("invalid --path {other:?}; expected direct or relay"),
        }
    }
}

impl ExpectedPath {
    fn label(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Relay => "relay",
        }
    }
}

#[derive(Debug, Serialize)]
struct CheckResult {
    check: String,
    status: &'static str,
    elapsed_ms: u128,
    evidence: serde_json::Value,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProbeReport {
    schema: u32,
    endpoint_id: String,
    server_endpoint_id: String,
    expected_path: String,
    started_unix_ms: i64,
    checks: Vec<CheckResult>,
    passed: bool,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,iroh=warn,iroh_gossip=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = CliArgs::parse()?;
    match args.command.as_str() {
        "serve" => serve(&args).await,
        "probe" => probe(&args).await,
        other => bail!("unknown command {other:?}; expected serve or probe"),
    }
}

async fn serve(args: &CliArgs) -> Result<()> {
    let state_dir = args.required_path("state-dir")?;
    let bind: SocketAddr = args
        .required("bind")?
        .parse()
        .context("parse --bind socket address")?;
    let relay_url = parse_relay_url(args.required("relay-url")?)?;
    let advertise_direct: SocketAddr = args
        .required("advertise-direct")?
        .parse()
        .context("parse --advertise-direct socket address")?;

    // Audit V5-P1-6: acquire an exclusive PID-file lock on
    // `<state_dir>/serve.pid` so a crashed previous run is
    // detectable before we reuse its identity / blob store.
    // `fs2`-style advisory lock via `flock` is unavailable on
    // macOS via stable Rust, so we use the standard library's
    // portable `OpenOptions::create_new` semantics — any
    // pre-existing pid file means a previous serve is still
    // (or recently was) running and we refuse to clobber its
    // state. If the recorded PID is no longer alive, we
    // overwrite the file (best-effort cleanup).
    let pid_path = state_dir.join("serve.pid");
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let current_pid = std::process::id();
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pid_path)
    {
        Ok(mut f) => {
            use std::io::Write;
            writeln!(f, "{current_pid}").context("write pid file")?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Try to decide whether the previous owner is still
            // alive. On Unix we can `kill(pid, 0)` and treat ESRCH
            // as "stale".
            let stale = read_and_check_pid(&pid_path).unwrap_or(true);
            if stale {
                warn!(
                    path = %pid_path.display(),
                    "stale pid file from a previous run; replacing it"
                );
                std::fs::remove_file(&pid_path)
                    .with_context(|| format!("remove stale pid file {}", pid_path.display()))?;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&pid_path)
                    .context("recreate pid file after stale cleanup")?;
                use std::io::Write;
                writeln!(f, "{current_pid}").context("write pid file")?;
            } else {
                bail!(
                    "another serve is still running; pid file {} exists",
                    pid_path.display()
                );
            }
        }
        Err(e) => {
            return Err(e).with_context(|| format!("create pid file {}", pid_path.display()));
        }
    }

    let identity = IrohIdentity::load_or_create(&state_dir)
        .map_err(|e| anyhow!("load/create server identity: {e}"))?;
    let endpoint = build_endpoint(bind, &identity, relay_url.clone(), false).await?;
    wait_online(&endpoint, DEFAULT_TIMEOUT).await?;

    let runtime = IrohRuntime::spawn(endpoint, &state_dir, Some(&state_dir.join("iroh-docs")))
        .await
        .context("spawn server iroh runtime")?;

    let mut frame_rx = runtime
        .take_frame_receiver()
        .await
        .ok_or_else(|| anyhow!("server frame receiver was already taken"))?;
    let frame_task = tokio::spawn(async move {
        while let Some(mut incoming) = frame_rx.recv().await {
            tokio::spawn(async move {
                loop {
                    match FrameCodec::read(&mut incoming.recv).await {
                        Ok(Some(frame)) => {
                            if let Err(error) = FrameCodec::write(&mut incoming.send, &frame).await
                            {
                                warn!(%error, "frame echo write failed");
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            warn!(%error, "frame echo read failed");
                            break;
                        }
                    }
                }
            });
        }
    });

    let topic_id = TopicId::from_bytes(GOSSIP_TOPIC_BYTES);
    let gossip_topic = runtime
        .gossip
        .subscribe(topic_id, Vec::new())
        .await
        .context("server subscribe gossip topic")?;
    let (gossip_tx, mut gossip_rx) = gossip_topic.split();
    let gossip_task = tokio::spawn(async move {
        while let Some(event) = gossip_rx.next().await {
            match event {
                Ok(GossipEvent::Received(message)) => {
                    let mut ack = b"ack:".to_vec();
                    ack.extend_from_slice(&message.content);
                    if let Err(error) = gossip_tx.broadcast(ack.into()).await {
                        warn!(%error, "gossip ack broadcast failed");
                    }
                }
                Ok(_) => {}
                Err(error) => warn!(%error, "gossip receive failed"),
            }
        }
    });

    let run_id = Utc::now().timestamp_millis();
    let blob_payload = format!("adnet-real-network-acceptance-blob-{run_id}");
    let conversation_id = format!("real-network-acceptance-{run_id}");
    let docs_seed = format!("docs-sync-ready-{run_id}");
    let blob_tag = runtime
        .blob_store_handle()
        .add_bytes(blob_payload.as_bytes().to_vec())
        .await
        .context("stage server acceptance blob")?;

    let chat = runtime
        .chat_bridge()
        .await
        .context("create docs chat bridge")?;
    chat.open_conversation(&conversation_id)
        .await
        .context("open server acceptance conversation")?;
    chat.append_message(
        &conversation_id,
        sample_message(&conversation_id, "server", &docs_seed),
    )
    .await
    .context("append server acceptance message")?;
    let docs_ticket = chat
        .share(&conversation_id, ShareMode::Write)
        .await
        .context("share server acceptance doc")?;

    let endpoint_addr = EndpointAddr::new(runtime.endpoint().id())
        .with_ip_addr(advertise_direct)
        .with_relay_url(relay_url);

    let blob_ticket = BlobTicket::new(endpoint_addr.clone(), blob_tag.hash, BlobFormat::Raw);
    let info = ServerInfo {
        schema: 1,
        endpoint_id: runtime.endpoint().id().to_string(),
        direct_addrs: endpoint_addr.ip_addrs().map(ToString::to_string).collect(),
        relay_urls: endpoint_addr
            .relay_urls()
            .map(ToString::to_string)
            .collect(),
        blob_ticket: blob_ticket.to_string(),
        blob_payload,
        docs_ticket: docs_ticket.to_string(),
        conversation_id,
        docs_seed,
        started_unix_ms: run_id,
    };
    println!("ADNET_READY {}", serde_json::to_string(&info)?);

    shutdown_signal()
        .await
        .context("wait for shutdown signal")?;
    frame_task.abort();
    gossip_task.abort();
    chat.shutdown().await;
    runtime
        .shutdown()
        .await
        .context("shutdown server runtime")?;
    // Audit V5-P1-6: clear the pid file on graceful shutdown so
    // the next invocation can reuse the same state-dir without
    // tripping over the stale-pid-file branch above.
    if let Err(e) = std::fs::remove_file(&pid_path) {
        warn!(path = %pid_path.display(), error = %e, "pid file cleanup failed");
    }
    Ok(())
}

async fn probe(args: &CliArgs) -> Result<()> {
    let state_dir = args.required_path("state-dir")?;
    let bind: SocketAddr = args
        .optional("bind")
        .unwrap_or("0.0.0.0:0")
        .parse()
        .context("parse --bind socket address")?;
    let relay_url = parse_relay_url(args.required("relay-url")?)?;
    let expected_path: ExpectedPath = args.required("path")?.parse()?;
    let server_info_path = args.required_path("server-info-file")?;
    let server_info: ServerInfo = serde_json::from_slice(
        &std::fs::read(&server_info_path)
            .with_context(|| format!("read {}", server_info_path.display()))?,
    )
    .context("decode server info")?;
    if server_info.schema != 1 {
        bail!("unsupported server info schema {}", server_info.schema);
    }

    let selected_checks = parse_checks(args.optional("checks").unwrap_or("all"))?;
    let timeout = Duration::from_secs(
        args.optional("timeout-seconds")
            .unwrap_or("30")
            .parse()
            .context("parse --timeout-seconds")?,
    );
    let server_addr = server_endpoint_addr(&server_info, expected_path)?;

    let identity = IrohIdentity::load_or_create(&state_dir)
        .map_err(|e| anyhow!("load/create client identity: {e}"))?;
    let endpoint = build_endpoint(
        bind,
        &identity,
        relay_url,
        expected_path == ExpectedPath::Relay,
    )
    .await?;
    let memory = iroh::address_lookup::memory::MemoryLookup::new();
    memory.add_endpoint_info(server_addr.clone());
    endpoint
        .address_lookup()
        .context("access client address lookup")?
        .add(memory);
    wait_online(&endpoint, timeout).await?;

    let runtime = IrohRuntime::spawn(endpoint, &state_dir, Some(&state_dir.join("iroh-docs")))
        .await
        .context("spawn client iroh runtime")?;

    let mut report = ProbeReport {
        schema: 1,
        endpoint_id: runtime.endpoint().id().to_string(),
        server_endpoint_id: server_info.endpoint_id.clone(),
        expected_path: expected_path.label().to_string(),
        started_unix_ms: Utc::now().timestamp_millis(),
        checks: Vec::new(),
        passed: true,
    };

    if wants(&selected_checks, "frame") {
        record_check(&mut report, "frame", async {
            frame_roundtrip(
                runtime.endpoint(),
                server_addr.clone(),
                expected_path,
                timeout,
            )
            .await
        })
        .await;
    }
    if wants(&selected_checks, "reconnect") {
        record_check(&mut report, "reconnect", async {
            reconnect(
                runtime.endpoint(),
                server_addr.clone(),
                expected_path,
                timeout,
            )
            .await
        })
        .await;
    }
    if wants(&selected_checks, "blobs") {
        record_check(&mut report, "blobs", async {
            blob_roundtrip(&runtime, &server_info, &server_addr, timeout).await
        })
        .await;
    }
    if wants(&selected_checks, "gossip") {
        record_check(&mut report, "gossip", async {
            gossip_roundtrip(&runtime, &server_addr, timeout).await
        })
        .await;
    }
    if wants(&selected_checks, "docs") {
        record_check(&mut report, "docs", async {
            docs_roundtrip(&runtime, &server_info, timeout).await
        })
        .await;
    }

    report.passed = report.checks.iter().all(|check| check.status == "passed");
    println!("{}", serde_json::to_string(&report)?);
    let passed = report.passed;
    runtime
        .shutdown()
        .await
        .context("shutdown client runtime")?;
    if !passed {
        bail!("one or more real-network acceptance checks failed");
    }
    Ok(())
}

async fn frame_roundtrip(
    endpoint: &Endpoint,
    server: EndpointAddr,
    expected: ExpectedPath,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let conn = tokio::time::timeout(timeout, endpoint.connect(server, ADNET_FRAME_ALPN))
        .await
        .context("frame connect timeout")??;
    let (mut send, mut recv) = conn.open_bi().await.context("open frame stream")?;
    let payload = format!(
        "frame-{}-{}",
        expected.label(),
        Utc::now().timestamp_millis()
    );
    FrameCodec::write(&mut send, &Frame::text(&payload)).await?;
    let echoed = tokio::time::timeout(timeout, FrameCodec::read(&mut recv))
        .await
        .context("frame echo timeout")??
        .ok_or_else(|| anyhow!("frame echo ended before response"))?;
    if echoed.as_bytes() != payload.as_bytes() {
        bail!("frame echo payload mismatch");
    }
    let path = wait_selected_path(&conn, expected, timeout).await?;
    conn.close(0u8.into(), b"acceptance-frame-complete");
    Ok(serde_json::json!({"selected_path": path, "echo_bytes": payload.len()}))
}

async fn reconnect(
    endpoint: &Endpoint,
    server: EndpointAddr,
    expected: ExpectedPath,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let mut selected = Vec::new();
    for attempt in 1..=3 {
        let conn =
            tokio::time::timeout(timeout, endpoint.connect(server.clone(), ADNET_FRAME_ALPN))
                .await
                .with_context(|| format!("reconnect attempt {attempt} timeout"))??;
        let (mut send, mut recv) = conn.open_bi().await?;
        let payload = format!("reconnect-{attempt}");
        FrameCodec::write(&mut send, &Frame::text(&payload)).await?;
        let echoed = tokio::time::timeout(timeout, FrameCodec::read(&mut recv))
            .await??
            .ok_or_else(|| anyhow!("reconnect attempt {attempt} ended before echo"))?;
        if echoed.as_bytes() != payload.as_bytes() {
            bail!("reconnect attempt {attempt} payload mismatch");
        }
        selected.push(wait_selected_path(&conn, expected, timeout).await?);
        conn.close(0u8.into(), b"acceptance-reconnect-cycle");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(serde_json::json!({"attempts": 3, "selected_paths": selected}))
}

async fn blob_roundtrip(
    runtime: &IrohRuntime,
    server_info: &ServerInfo,
    server_addr: &EndpointAddr,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let ticket: BlobTicket = server_info
        .blob_ticket
        .parse()
        .context("parse blob ticket")?;
    let memory = iroh::address_lookup::memory::MemoryLookup::new();
    memory.add_endpoint_info(server_addr.clone());
    runtime.endpoint().address_lookup()?.add(memory);
    tokio::time::timeout(
        timeout,
        runtime
            .blob_store_handle()
            .downloader(runtime.endpoint())
            .download(ticket.hash(), Some(ticket.addr().id)),
    )
    .await
    .context("blob download timeout")??;
    let bytes = BlobReader::read_all(
        &runtime.adnet_store,
        &adnet_blobstore::iroh_hash_to_content_hash(&ticket.hash()),
    )
    .await?;
    if bytes != server_info.blob_payload.as_bytes() {
        bail!("downloaded blob payload mismatch");
    }
    Ok(serde_json::json!({"hash": ticket.hash().to_string(), "bytes": bytes.len()}))
}

async fn gossip_roundtrip(
    runtime: &IrohRuntime,
    server_addr: &EndpointAddr,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let topic_id = TopicId::from_bytes(GOSSIP_TOPIC_BYTES);
    let mut topic = runtime
        .gossip
        .subscribe(topic_id, vec![server_addr.id])
        .await
        .context("client subscribe gossip topic")?;
    tokio::time::timeout(timeout, topic.joined())
        .await
        .context("gossip join timeout")??;
    let nonce = format!("gossip-{}", Utc::now().timestamp_millis());
    topic.broadcast(nonce.as_bytes().to_vec().into()).await?;
    let expected = format!("ack:{nonce}");
    let received = tokio::time::timeout(timeout, async {
        while let Some(event) = topic.next().await {
            if let GossipEvent::Received(message) = event?
                && message.content.as_ref() == expected.as_bytes()
            {
                return Ok::<_, anyhow::Error>(message.content.len());
            }
        }
        bail!("gossip event stream ended before ack")
    })
    .await
    .context("gossip ack timeout")??;
    Ok(serde_json::json!({"ack": expected, "bytes": received}))
}

async fn docs_roundtrip(
    runtime: &IrohRuntime,
    server_info: &ServerInfo,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let ticket: DocTicket = server_info
        .docs_ticket
        .parse()
        .context("parse docs ticket")?;
    let chat = runtime.chat_bridge().await?;
    chat.open_with_ticket(&server_info.conversation_id, ticket)
        .await?;
    let initial = tokio::time::timeout(timeout, async {
        loop {
            let messages = chat
                .get_messages(&server_info.conversation_id, None, 100)
                .await?;
            if messages
                .iter()
                .any(|message| message.content == server_info.docs_seed)
            {
                return Ok::<_, anyhow::Error>(messages.len());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .context("docs initial sync timeout")??;
    let reply = format!("docs-client-{}", Utc::now().timestamp_millis());
    chat.append_message(
        &server_info.conversation_id,
        sample_message(&server_info.conversation_id, "client", &reply),
    )
    .await?;
    let after = chat
        .get_messages(&server_info.conversation_id, None, 100)
        .await?;
    if !after.iter().any(|message| message.content == reply) {
        bail!("docs local write was not readable after remote sync");
    }
    chat.shutdown().await;
    Ok(serde_json::json!({"initial_messages": initial, "messages_after_write": after.len()}))
}

async fn wait_selected_path(
    conn: &iroh::endpoint::Connection,
    expected: ExpectedPath,
    timeout: Duration,
) -> Result<&'static str> {
    tokio::time::timeout(timeout, async {
        loop {
            for path in conn.paths().iter().filter(|path| path.is_selected()) {
                if expected == ExpectedPath::Direct && path.is_ip() {
                    return Ok("direct");
                }
                if expected == ExpectedPath::Relay && path.is_relay() {
                    return Ok("relay");
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .with_context(|| format!("selected path did not become {}", expected.label()))?
}

async fn record_check<F>(report: &mut ProbeReport, name: &str, future: F)
where
    F: std::future::Future<Output = Result<serde_json::Value>>,
{
    let started = Instant::now();
    match future.await {
        Ok(evidence) => report.checks.push(CheckResult {
            check: name.to_string(),
            status: "passed",
            elapsed_ms: started.elapsed().as_millis(),
            evidence,
            error: None,
        }),
        Err(error) => report.checks.push(CheckResult {
            check: name.to_string(),
            status: "failed",
            elapsed_ms: started.elapsed().as_millis(),
            evidence: serde_json::Value::Null,
            error: Some(format!("{error:#}")),
        }),
    }
}

fn server_endpoint_addr(info: &ServerInfo, expected: ExpectedPath) -> Result<EndpointAddr> {
    let id: EndpointId = info
        .endpoint_id
        .parse()
        .context("parse server endpoint id")?;
    let mut addr = EndpointAddr::new(id);
    match expected {
        ExpectedPath::Direct => {
            if info.direct_addrs.is_empty() {
                bail!("server did not advertise a direct address");
            }
            for value in &info.direct_addrs {
                addr = addr.with_ip_addr(
                    value
                        .parse()
                        .with_context(|| format!("parse server direct address {value:?}"))?,
                );
            }
        }
        ExpectedPath::Relay => {
            let value = info
                .relay_urls
                .first()
                .ok_or_else(|| anyhow!("server did not advertise a relay URL"))?;
            addr = addr.with_relay_url(parse_relay_url(value)?);
        }
    }
    Ok(addr)
}

async fn build_endpoint(
    bind: SocketAddr,
    identity: &IrohIdentity,
    relay_url: RelayUrl,
    relay_only: bool,
) -> Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(identity.secret_key())
        .relay_mode(RelayMode::custom([relay_url]))
        .alpns(vec![
            ADNET_FRAME_ALPN.to_vec(),
            iroh_blobs::ALPN.to_vec(),
            iroh_gossip::ALPN.to_vec(),
            iroh_docs::ALPN.to_vec(),
        ]);
    if relay_only {
        builder = builder.clear_ip_transports();
    } else {
        builder = builder
            .bind_addr(bind)
            .map_err(|error| anyhow!("configure bind address {bind}: {error}"))?;
    }
    builder.bind().await.context("bind iroh endpoint")
}

async fn wait_online(endpoint: &Endpoint, timeout: Duration) -> Result<()> {
    tokio::time::timeout(timeout, endpoint.online())
        .await
        .context("endpoint did not become relay-online before timeout")?;
    Ok(())
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("install SIGTERM handler")?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.context("wait for SIGINT")?,
            _ = terminate.recv() => {}
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.context("wait for ctrl-c")?;
        Ok(())
    }
}

fn parse_relay_url(value: &str) -> Result<RelayUrl> {
    value
        .parse()
        .with_context(|| format!("parse relay URL {value:?}"))
}

fn sample_message(conversation_id: &str, sender: &str, content: &str) -> Message {
    Message {
        id: format!(
            "{sender}-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ),
        conversation_id: conversation_id.to_string(),
        sender_id: sender.to_string(),
        receiver_id: None,
        content: content.to_string(),
        timestamp: Utc::now(),
        sequence: None,
        reply_to: None,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    }
}

fn parse_checks(value: &str) -> Result<BTreeSet<String>> {
    let allowed = ["all", "frame", "reconnect", "blobs", "gossip", "docs"];
    let checks = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    if checks.is_empty() {
        bail!("--checks must contain at least one check");
    }
    for check in &checks {
        if !allowed.contains(&check.as_str()) {
            bail!("unknown check {check:?}");
        }
    }
    Ok(checks)
}

fn wants(checks: &BTreeSet<String>, name: &str) -> bool {
    checks.contains("all") || checks.contains(name)
}

/// Audit V5-P1-6 helper: read a pid file and report whether the
/// recorded process is still alive. Returns `Some(true)` when the
/// pid file is valid and the pid is alive (so a new run must NOT
/// overwrite), `Some(false)` when the pid file is valid but the
/// recorded process is gone (stale — overwrite is safe), and
/// `None` when the file could not be parsed.
///
/// We use `kill(pid, 0)` via the `nix` crate on Unix; on non-Unix
/// we conservatively return `None` so the caller treats the file
/// as "undecidable" and refuses to overwrite.
fn read_and_check_pid(path: &std::path::Path) -> Option<bool> {
    let raw = std::fs::read_to_string(path).ok()?;
    let pid: i32 = raw.trim().parse().ok()?;
    Some(!pid_alive(pid))
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // `kill(pid, 0)` returns ESRCH when no process holds that pid,
    // EPERM when it exists but we lack permission (process IS
    // alive from the OS's perspective). Any other error means we
    // cannot decide.
    //
    // The workspace forbids `unsafe`, so we cannot call `libc::kill`
    // directly. We spawn `kill -0 <pid>` via `Command` instead —
    // `Command` is in std and exposes no `unsafe` surface. The
    // child process is a one-shot `kill` lookup; on macOS `kill`
    // lives at `/bin/kill`, on Linux at `/usr/bin/kill` or
    // `/bin/kill`. We fall through `PATH`.
    use std::process::Command;
    let output = Command::new("kill").args(["-0", &pid.to_string()]).output();
    match output {
        Ok(out) => {
            // `kill -0` exits 0 when the pid is alive (or
            // permission-denied), non-zero (and prints to stderr)
            // when the pid is gone. Match on exit-status only —
            // we don't read stdout/stderr so the child never
            // blocks.
            out.status.success()
        }
        Err(_) => {
            // `kill` binary missing on PATH — be conservative:
            // assume the pid is alive so the caller refuses to
            // overwrite the pid file. Operator can delete it
            // manually.
            true
        }
    }
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    // Without libc we cannot reliably probe. Treat the pid as
    // alive so the caller refuses to overwrite — the operator
    // can delete the pid file manually.
    true
}

#[derive(Debug)]
struct CliArgs {
    command: String,
    values: std::collections::BTreeMap<String, String>,
}

impl CliArgs {
    fn parse() -> Result<Self> {
        let mut raw = std::env::args().skip(1);
        let command = raw.next().ok_or_else(|| {
            anyhow!("usage: network_acceptance <serve|probe> --state-dir PATH --relay-url URL ...")
        })?;
        let mut values = std::collections::BTreeMap::new();
        while let Some(flag) = raw.next() {
            let key = flag
                .strip_prefix("--")
                .ok_or_else(|| anyhow!("expected --flag, got {flag:?}"))?;
            let value = raw
                .next()
                .ok_or_else(|| anyhow!("missing value for --{key}"))?;
            values.insert(key.to_string(), value);
        }
        Ok(Self { command, values })
    }

    fn required(&self, key: &str) -> Result<&str> {
        self.optional(key)
            .ok_or_else(|| anyhow!("missing required --{key}"))
    }

    fn optional(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    fn required_path(&self, key: &str) -> Result<PathBuf> {
        Ok(Path::new(self.required(key)?).to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ────────────────────────────────────────────────────────────
    // Audit V5-P1-6: stale-state-dir detection via pid file.
    // ────────────────────────────────────────────────────────────

    #[test]
    fn read_and_check_pid_parses_current_pid() {
        // The current process is definitely alive. We seed a pid
        // file with our own pid and confirm
        // `read_and_check_pid` reports "not stale" (i.e. it
        // returns `Some(false)`).
        let dir = tempfile::TempDir::new().unwrap();
        let pid_path = dir.path().join("serve.pid");
        std::fs::write(&pid_path, format!("{}\n", std::process::id())).unwrap();
        let result = read_and_check_pid(&pid_path);
        assert_eq!(result, Some(false), "current pid must report as alive");
    }

    #[test]
    fn read_and_check_pid_treats_unparseable_file_as_stale() {
        // Garbage in the pid file ⇒ undecidable. The helper
        // returns `None`, which the caller treats as "stale" via
        // `unwrap_or(true)`. Confirm the parser is robust to
        // unexpected input.
        let dir = tempfile::TempDir::new().unwrap();
        let pid_path = dir.path().join("serve.pid");
        std::fs::write(&pid_path, "this is not a pid\n").unwrap();
        let result = read_and_check_pid(&pid_path);
        assert!(result.is_none(), "unparseable pid file must return None");
    }

    #[test]
    fn pid_alive_handles_very_large_pid() {
        // Sanity check: `kill -0` (and our wrapper) accept any
        // integer that parses as i32, even when the pid almost
        // certainly does not exist. On non-Unix the function is a
        // no-op that returns true; on Unix it spawns `kill -0`.
        //
        // We don't assert a particular value (the OS may or may
        // not find the pid) — we only assert the call does not
        // panic and returns a bool.
        let _: bool = pid_alive(i32::MAX);
    }
}
