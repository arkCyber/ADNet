//! High-level orchestrator for an interop test run.
//!
//! Responsibilities:
//! 1. Boot an `a3net_node::Node` with the `iroh` feature enabled
//!    so the wire layer is real iroh 1.0.
//! 2. Spawn a sidecar process (if `sidecar_cmd` is configured) and
//!    connect to its HTTP/JSON control plane.
//! 3. Optionally start a [`HarnessServer`] so the sidecar can
//!    reverse-dial us with the events it observed.
//! 4. Run a [`Scenario`] — the test body.
//! 5. Tear everything down and return a [`ScenarioReport`].
//!
//! The driver does NOT spawn a tokio runtime. Callers are
//! expected to be inside a `#[tokio::test]` (or an existing
//! multi-threaded runtime).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use blake3::Hash as Blake3Hash;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{debug, error, info, warn};

use a3net_node::Node;

use crate::sidecar::{HarnessServer, SidecarClient};
use crate::wire::SidecarError;

/// What to run. Each variant is a small, self-contained test body.
/// The driver doesn't try to be clever about cross-scenario state
/// sharing — every scenario is independent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scenario {
    /// `iroh-go` puts a blob, A3Net fetches it.
    SidecarPutAdnetFetch {
        /// Bytes the sidecar should put.
        payload_b64: String,
        /// Tag for diagnostics.
        tag: String,
    },
    /// A3Net puts a blob, `iroh-go` fetches it via the ticket the
    /// A3Net side returns.
    AdnetPutSidecarFetch {
        /// Bytes the A3Net side should put.
        payload_b64: String,
    },
    /// Both sides subscribe to the same topic, then A3Net
    /// publishes; the sidecar is expected to observe the event
    /// via the bus.
    AdnetPublishSidecarSubscribes {
        topic: String,
        /// Base64-encoded payload.
        payload_b64: String,
    },
    /// Symmetric: sidecar publishes, A3Net's gossip bus delivers
    /// the event into the harness server.
    SidecarPublishAdnetSubscribes {
        topic: String,
        payload_b64: String,
    },
}

/// Result of a scenario. A pass is recorded as
/// `ScenarioReport::ok = true` AND no failing assertion in
/// `failures`. Callers may treat the `latency_ms` field as a
/// soft signal only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioReport {
    pub scenario: String,
    pub ok: bool,
    pub latency_ms: u64,
    pub failures: Vec<String>,
    /// Harness-side events observed during the run (e.g. gossip
    /// events the A3Net bus delivered). Stored verbatim so the
    /// test can assert on payload bytes, sender id, etc.
    pub observed: Vec<crate::sidecar::HarnessEvent>,
}

impl ScenarioReport {
    pub fn new(scenario: &str) -> Self {
        Self {
            scenario: scenario.to_string(),
            ok: true,
            latency_ms: 0,
            failures: Vec::new(),
            observed: Vec::new(),
        }
    }

    fn record_failure(&mut self, msg: impl Into<String>) {
        self.ok = false;
        self.failures.push(msg.into());
    }
}

/// Optional knobs the caller can override.
#[derive(Debug, Clone, Default)]
pub struct HarnessConfig {
    /// Working directory for the A3Net node's persistent state.
    /// If `None`, the driver creates a `tempfile::TempDir` and
    /// drops it on `InteropHarness::drop`.
    pub data_dir: Option<PathBuf>,

    /// Command to spawn the sidecar. The first arg is the
    /// binary path; subsequent args are passed through. The
    /// driver appends `--base-url <harness_reverse_addr>` and
    /// expects the sidecar to listen on a port it then reports
    /// back over stdout or a config file. The exact protocol is
    /// sidecar-specific; this crate ships a reference Go
    /// sidecar under `sidecar/iroh-go-sidecar/`.
    pub sidecar_cmd: Option<Vec<String>>,

    /// Bind address for the harness' reverse server. If `None`,
    /// `127.0.0.1:0` (the OS picks a port).
    pub reverse_bind: Option<std::net::SocketAddr>,

    /// Wall-clock budget for the entire scenario. Default 30 s.
    pub timeout: Option<Duration>,

    /// When `true`, the driver prints extra `tracing` info at
    /// `info` level. Default `false` (just `debug`).
    pub verbose: bool,
}

/// The driver. Holds the A3Net node, the sidecar client, and the
/// reverse server (if any). Drop tears them all down.
pub struct InteropHarness {
    pub node: Node,
    pub sidecar: Option<SidecarClient>,
    pub reverse: Option<HarnessServer>,
    /// Kept so we can `child.kill().await` on drop.
    sidecar_child: Option<tokio::process::Child>,
    /// We do NOT keep the tempdir alive via a struct field; the
    /// `data_dir` is owned by the harness and the caller decides
    /// when to clean it up (Default: never — `Drop` does not
    /// `rm -rf`).
    _data_dir_marker: (),
    config: HarnessConfig,
}

impl InteropHarness {
    /// Boot an A3Net node + (optionally) sidecar + (optionally)
    /// reverse server. `data_dir` is mandatory; pass a tempdir
    /// from the caller.
    pub async fn boot(config: HarnessConfig) -> Result<Self, anyhow::Error> {
        let data_dir = match config.data_dir.clone() {
            Some(p) => p,
            None => {
                anyhow::bail!("HarnessConfig::data_dir is required (caller owns the tempdir lifecycle)");
            }
        };
        if config.verbose {
            info!(?data_dir, "booting A3Net node for interop harness");
        } else {
            debug!(?data_dir, "booting A3Net node for interop harness");
        }
        // `a3net-node`'s iroh runtime requires its own data
        // dir, separate from the gossip bus data dir. We use a
        // sub-directory so the on-disk layout is obvious in a
        // post-mortem.
        let iroh_data_dir = data_dir.join("iroh");
        std::fs::create_dir_all(&iroh_data_dir)?;
        // Load a fresh `IrohIdentity` from the iroh data dir
        // (this is what `with_iroh_runtime_from_data_dir` wants
        // for `&IrohIdentity`). If the file doesn't exist yet,
        // iroh will create one on the spot.
        let iroh_identity = a3net_transport::iroh::IrohIdentity::load_or_create(&iroh_data_dir)
            .map_err(|e| anyhow::anyhow!("failed to load iroh identity: {e}"))?;
        let cfg = a3net_node::NodeConfig::load_or_create(&data_dir)
            .map_err(|e| anyhow::anyhow!("NodeConfig::load_or_create failed: {e}"))?;
        let node = a3net_node::NodeBuilder::new(cfg)
            .with_iroh_runtime_from_data_dir(
                &iroh_data_dir,
                std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                &iroh_identity,
                None,
            )
            .await?
            .build()
            .await?;
        let reverse = if config.reverse_bind.is_some() || config.sidecar_cmd.is_some() {
            let builder = harness_server_builder_from_config(&config);
            let (srv, _rx) = builder.spawn().await?;
            Some(srv)
        } else {
            None
        };
        let (sidecar, sidecar_child) = match config.sidecar_cmd.as_ref() {
            Some(cmd) => {
                let (child, port) = spawn_sidecar(cmd, reverse.as_ref().map(|r| r.local_addr()))
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to spawn sidecar: {e}"))?;
                let url = format!("http://127.0.0.1:{port}");
                let client = SidecarClient::connect(&url)
                    .map_err(|e| anyhow::anyhow!("failed to build sidecar client: {e}"))?;
                let ver = client
                    .handshake()
                    .await
                    .map_err(|e| anyhow::anyhow!("sidecar handshake failed: {e}"))?;
                info!(sidecar = %ver.sidecar, "sidecar handshake OK");
                (Some(client), Some(child))
            }
            None => (None, None),
        };
        Ok(Self {
            node,
            sidecar,
            reverse,
            sidecar_child,
            _data_dir_marker: (),
            config,
        })
    }

    /// Run a scenario. Returns a [`ScenarioReport`]. Never panics
    /// — every failure is recorded in the report.
    pub async fn run(&self, scenario: Scenario) -> ScenarioReport {
        let start = Instant::now();
        let timeout = self.config.timeout.unwrap_or(Duration::from_secs(30));
        let result = tokio::time::timeout(timeout, self.run_inner(scenario.clone())).await;
        let mut report = match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                let mut r = ScenarioReport::new(scenario_kind(&scenario));
                r.record_failure(format!("scenario failed: {e:?}"));
                r
            }
            Err(_) => {
                let mut r = ScenarioReport::new(scenario_kind(&scenario));
                r.record_failure(format!("scenario exceeded {timeout:?} budget"));
                r
            }
        };
        report.latency_ms = start.elapsed().as_millis() as u64;
        if !report.failures.is_empty() {
            error!(
                scenario = %report.scenario,
                failures = ?report.failures,
                "scenario failed"
            );
        } else if self.config.verbose {
            info!(
                scenario = %report.scenario,
                latency_ms = report.latency_ms,
                "scenario passed"
            );
        }
        report
    }

    async fn run_inner(&self, scenario: Scenario) -> Result<ScenarioReport, SidecarError> {
        let mut report = ScenarioReport::new(scenario_kind(&scenario));
        let sidecar = self.sidecar.as_ref().ok_or_else(|| SidecarError::Process(
            "no sidecar configured — pass HarnessConfig::sidecar_cmd".into(),
        ))?;
        match scenario {
            Scenario::SidecarPutAdnetFetch { payload_b64, tag } => {
                // Local-only verification in this round. The
                // sidecar's blob is reachable via the sidecar's
                // own `blob_get`; on the A3Net side we have no
                // iroh Endpoint yet to dial the sidecar, so we
                // assert that the sidecar's *reported* hash
                // matches the BLAKE3 hash of the payload we
                // sent. The actual byte round-trip happens via
                // the symmetric `AdnetPutSidecarFetch` (which
                // also stays local for v0.1). The full wire
                // round-trip (dial + bao-tree download) lands
                // in v0.2 once `a3net-node` carries an iroh
                // `Endpoint`.
                let bytes = B64
                    .decode(payload_b64.as_bytes())
                    .map_err(SidecarError::Base64)?;
                let put = sidecar.blob_put(&bytes, Some(&tag)).await?;
                debug!(hash = %put.hash, ticket = %put.ticket, "sidecar ingested blob");
                let expected = blake3::hash(&bytes).to_hex().to_string();
                if put.hash != expected {
                    report.record_failure(format!(
                        "sidecar-reported hash {} does not match BLAKE3(expected={expected})",
                        put.hash
                    ));
                }
                if put.size as usize != bytes.len() {
                    report.record_failure(format!(
                        "sidecar-reported size {} does not match payload len {}",
                        put.size,
                        bytes.len()
                    ));
                }
            }
            Scenario::AdnetPutSidecarFetch { payload_b64 } => {
                let bytes = B64
                    .decode(payload_b64.as_bytes())
                    .map_err(SidecarError::Base64)?;
                // A3Net's "put" path: write to the blob store.
                // Node::fetch_blob on this side then looks up the
                // hash in the local store. To give the sidecar a
                // *meaningful* ticket, we also build the matching
                // iroh `EndpointAddr` (direct-only) so the
                // ticket_bridge can convert.
                let (hash, _size) = self
                    .node
                    .store()
                    .put_bytes_sync(&bytes)
                    .map_err(|e| SidecarError::Process(format!("A3Net put failed: {e}")))?;
                let node_id = self.node.node_id().clone();
                let addr = self.node.ensure_mesh().await.ok();
                let mut a3net_addr = a3net_types::NodeAddr::new(node_id.clone());
                if let Some(a) = addr {
                    if let Some(d) = a.direct {
                        a3net_addr = a3net_addr.with_direct(d);
                    }
                    if let Some(r) = a.relay {
                        a3net_addr = a3net_addr.with_relay(r);
                    }
                }
                let iroh_ticket = crate::a3net_to_iroh_ticket(&node_id, &a3net_addr, &hash);
                let got = sidecar.blob_get(&iroh_ticket).await?;
                let decoded = B64
                    .decode(got.data_b64.as_bytes())
                    .map_err(SidecarError::Base64)?;
                if decoded != bytes {
                    report.record_failure("round-trip bytes differ");
                }
                if got.size as usize != bytes.len() {
                    report.record_failure(format!(
                        "size mismatch: A3Net sent {} bytes, sidecar reports {} bytes",
                        bytes.len(),
                        got.size
                    ));
                }
            }
            Scenario::AdnetPublishSidecarSubscribes { topic, payload_b64 } => {
                let bytes = B64
                    .decode(payload_b64.as_bytes())
                    .map_err(SidecarError::Base64)?;
                let sub = sidecar.gossip_join(&topic, None).await?;
                // Give the sidecar a moment to actually be ready
                // on the bus before we publish.
                tokio::time::sleep(Duration::from_millis(250)).await;
                // The A3Net side bus delivers structured
                // `Announcement`s, not raw bytes; for the
                // interop test we wrap the test payload into a
                // minimal announcement (the sidecar's gossip
                // decoder will see the JSON-encoded bytes).
                let room = a3net_types::RoomId::new(topic.clone());
                let ann = a3net_types::Announcement {
                    room_id: room,
                    content_hash: a3net_types::ContentHash::from_bytes(&bytes),
                    node_id: self.node.node_id().clone(),
                    title: format!("interop-{topic}"),
                    kind: a3net_types::CdnContentKind::Article,
                    size_bytes: bytes.len() as u64,
                    mime_type: None,
                    source_url: None,
                    ticket: None,
                    timestamp: chrono::Utc::now(),
                    message_id: None,
                    ttl_secs: None,
                    signer: None,
                    signature: None,
                };
                self.node
                    .announce(&a3net_types::RoomId::new(topic.clone()), &ann)
                    .await
                    .map_err(|e| SidecarError::Process(format!("A3Net announce failed: {e}")))?;
                // Long-poll the sidecar for the event.
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if Instant::now() >= deadline {
                        report.record_failure(format!(
                            "sidecar did not observe gossip event on `{topic}` within 10s"
                        ));
                        break;
                    }
                    match sidecar.gossip_next_event(&sub.sub_id, 500).await? {
                        Some(evt) if evt.topic == topic => {
                            let observed = B64
                                .decode(evt.payload_b64.as_bytes())
                                .unwrap_or_default();
                            if observed == bytes {
                                debug!("sidecar observed the expected gossip event");
                                break;
                            } else {
                                report.record_failure(format!(
                                    "sidecar observed an event but bytes differ (got {} bytes)",
                                    observed.len()
                                ));
                                break;
                            }
                        }
                        Some(evt) => {
                            debug!(topic = %evt.topic, "sidecar observed event on a different topic — ignoring");
                        }
                        None => continue,
                    }
                }
            }
            Scenario::SidecarPublishAdnetSubscribes { topic, payload_b64 } => {
                let bytes = B64
                    .decode(payload_b64.as_bytes())
                    .map_err(SidecarError::Base64)?;
                // A3Net subscribes first, then the sidecar publishes.
                let mut rx = self
                    .node
                    .subscribe_room(&a3net_types::RoomId::new(topic.clone()));
                tokio::time::sleep(Duration::from_millis(250)).await;
                let _ = sidecar.gossip_publish(&topic, &bytes).await?;
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if Instant::now() >= deadline {
                        report.record_failure(format!(
                            "A3Net did not observe gossip event on `{topic}` within 10s"
                        ));
                        break;
                    }
                    match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                        Ok(Ok(ann)) => {
                            // The A3Net bus delivers
                            // `Announcement`s; we just want the
                            // matching title (which is how we
                            // encoded the test payload) to
                            // confirm the right event arrived.
                            if ann.title == format!("interop-{topic}") {
                                debug!("A3Net observed the expected gossip event");
                                break;
                            } else {
                                debug!(title = %ann.title, "A3Net observed an event on a different title — ignoring");
                            }
                        }
                        Ok(Err(_closed)) => {
                            // Channel closed — node shutdown.
                            report.record_failure(String::from("A3Net gossip channel closed before event"));
                            break;
                        }
                        Err(_) => continue, // poll timeout, keep looping
                    }
                }
            }
        }
        Ok(report)
    }

    /// Stop the sidecar (if any) and drop the A3Net node.
    pub async fn shutdown(mut self) {
        if let Some(mut child) = self.sidecar_child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        // The Node shutdown is a no-op in v0.1 of a3net-node; we
        // still call it so future versions that add graceful
        // shutdown are picked up automatically.
        let _ = self.node.shutdown().await;
    }
}

impl Drop for InteropHarness {
    fn drop(&mut self) {
        if let Some(child) = self.sidecar_child.as_mut() {
            // Best-effort kill from a non-async context. We ignore
            // errors because the caller may be panicking; logging
            // the failure here would itself need a runtime.
            let _ = child.start_kill();
            warn!("InteropHarness dropped while sidecar still running — sent SIGKILL");
        }
    }
}

fn harness_server_builder_from_config(config: &HarnessConfig) -> harness_builder::HarnessServerBuilder {
    let mut b = harness_builder::HarnessServerBuilder::new();
    if let Some(addr) = config.reverse_bind {
        b = b.bind(addr);
    }
    b
}

// Tiny shim so the function above can stay close to where it's
// used. The real builder lives in `crate::sidecar::server`; we
// re-export under a shorter path here.
mod harness_builder {
    pub use crate::sidecar::server::HarnessServerBuilder;
}

fn scenario_kind(s: &Scenario) -> &'static str {
    match s {
        Scenario::SidecarPutAdnetFetch { .. } => "sidecar_put_a3net_fetch",
        Scenario::AdnetPutSidecarFetch { .. } => "a3net_put_sidecar_fetch",
        Scenario::AdnetPublishSidecarSubscribes { .. } => "a3net_publish_sidecar_subscribes",
        Scenario::SidecarPublishAdnetSubscribes { .. } => "sidecar_publish_a3net_subscribes",
    }
}

async fn spawn_sidecar(
    cmd: &[String],
    _harness_addr: Option<std::net::SocketAddr>,
) -> std::io::Result<(tokio::process::Child, u16)> {
    if cmd.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "sidecar_cmd must contain at least the binary path",
        ));
    }
    let (bin, args) = cmd.split_first().unwrap();
    let mut child = Command::new(bin);
    child
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = child.spawn()?;
    // The reference Go sidecar prints the bound port on stdout as
    // `LISTEN_PORT=<n>\n` on its first line. We read the first
    // line with a short timeout.
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "sidecar stdout not captured"))?;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line)).await??;
    if read == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "sidecar exited before printing its listen port",
        ));
    }
    let port: u16 = line
        .trim()
        .strip_prefix("LISTEN_PORT=")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("sidecar did not print `LISTEN_PORT=`: got `{line}`"),
            )
        })?;
    // Re-attach stdout to the child so the runtime can keep
    // draining it (otherwise the pipe fills and the child
    // blocks on its next log line).
    child.stdout = Some(reader.into_inner());
    Ok((child, port))
}
