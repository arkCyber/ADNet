//! `a3net doctor` — network diagnostics command.
//!
//! Mirrors `iroh-doctor`'s public surface (the `report`,
//! `relay-urls`, `port-map-probe`, `accept`, `connect`
//! subcommands) on top of the existing `a3net-nat-traversal`,
//! `a3net-observability`, and `a3net-types` crates. The CLI is
//! intentionally **read-only** — it never mutates the local
//! node state, so it's safe to run alongside a live process.
//!
//! ## Subcommands
//!
//! - `a3net doctor report [--json]` — full report of the local
//!   network environment (UDP/IPv4/IPv6 reachability, NAT
//!   type, hairpin support, port-mapping protocol results,
//!   home relay + RTT, externally-observed endpoints).
//! - `a3net doctor relay-urls [--json]` — list every
//!   configured relay URL annotated with the latency observed
//!   the last time the node probed it.
//! - `a3net doctor port-map-probe <port> [--json]` — run a
//!   single port-mapping probe on the supplied local port
//!   (UPnP / NAT-PMP / PCP) and report the outcome.
//! - `a3net doctor accept` — open an iroh endpoint and bind
//!   to the configured address; mirrors iroh-doctor's
//!   "accept" subcommand (the doc-side of the dial test).
//! - `a3net doctor connect <node-id>` — dial a remote
//!   endpoint by hex NodeId; mirrors iroh-doctor's "connect"
//!   subcommand (the caller-side of the dial test). Reports
//!   whether the handshake completed inside the configured
//!   timeout.
//!
//! All subcommands degrade gracefully when the observability
//! background task has not yet populated its cache: the
//! report fields are `None` rather than fabricated.
//!
//! ## Storage
//!
//! The doctor subcommands read from
//! `<data_dir>/doctor.json` and `<data_dir>/relay_latencies.json`
//! when present, and from `<data_dir>/port_map.json` for the
//! port-map probe results. These files are written by the
//! observability background task once per minute while the
//! node is running.

use std::path::{Path, PathBuf};

use a3net_tui::{
    box_drawing::Box,
    color::Color,
    widget::{alert_widget, section_header, Table},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Snapshot of the local network environment. The fields
/// mirror `iroh-doctor`'s `Report` struct; missing data
/// (because the probe has not yet completed) is reported as
/// `None` rather than fabricated.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DoctorReport {
    /// Whether the node successfully sent a UDP datagram to a
    /// public resolver. `None` when the probe has not yet
    /// completed.
    pub udp_reachable: Option<bool>,
    /// Whether the local OS has IPv6 connectivity. `None` when
    /// the probe is in flight.
    pub ipv6_reachable: Option<bool>,
    /// Whether the local OS has IPv4 connectivity. `None` when
    /// the probe is in flight.
    pub ipv4_reachable: Option<bool>,
    /// Whether the local interface can reach a public IPv4
    /// endpoint. Distinct from `ipv4_reachable` (the OS may
    /// have an IPv4 stack but no route to the public
    /// internet, e.g. behind a corporate firewall).
    pub ipv4_can_send: Option<bool>,
    /// Whether the local interface can reach a public IPv6
    /// endpoint.
    pub ipv6_can_send: Option<bool>,
    /// Detected NAT type (`"none"`, `"full_cone"`,
    /// `"restricted"`, `"port_restricted"`, `"symmetric"`,
    /// `"unknown"`). `None` until the STUN probe completes.
    pub nat_type: Option<String>,
    /// Whether the node observed hairpinning support (i.e.
    /// packets to its own public IP route back through the
    /// local interface). `None` until probed.
    pub hair_pinning: Option<bool>,
    /// Whether the node's public endpoint appears to vary by
    /// destination IP (a hallmark of symmetric NATs).
    pub mapping_varies_by_dest_ip: Option<bool>,
    /// URL of the home relay the node selected. `None` when
    /// no relay round-trip has completed yet.
    pub home_relay_url: Option<String>,
    /// Latency (ms) of the most recent round-trip to the
    /// home relay. `None` when no RTT has been measured.
    pub home_relay_rtt_ms: Option<u64>,
    /// List of port-mapping protocols attempted. The order
    /// matches the priority list the node walks at startup
    /// (UPnP → NAT-PMP → PCP → manual). `successful_protocol`
    /// is the one that actually obtained a mapping (when
    /// any).
    #[serde(default)]
    pub attempted_protocols: Vec<String>,
    pub successful_protocol: Option<String>,
    /// Externally-observed IPv4 endpoint (after NAT). `None`
    /// until a STUN response has been decoded.
    pub global_ipv4: Option<String>,
    /// Externally-observed IPv6 endpoint. `None` when the
    /// node is IPv4-only.
    pub global_ipv6: Option<String>,
}

/// Per-relay latency sample. The latency field is the median
/// RTT of the last `samples` round-trips; `samples_failed` is
/// the count of round-trips that did not complete inside the
/// probe's per-attempt timeout (typically 500 ms).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayLatency {
    pub url: String,
    pub latency_ms: Option<f64>,
    pub samples: u32,
    pub samples_failed: u32,
}

/// Port-mapping probe result. Mirrors the
/// `iroh-doctor::port_map_probe::ProbeOutput` shape with the
/// minimum the operator needs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PortMapProbeResult {
    /// The probe that actually obtained a mapping. `None`
    /// when every attempted protocol failed.
    pub successful_protocol: Option<String>,
    /// Ordered list of protocols that were tried, with their
    /// per-protocol outcome.
    #[serde(default)]
    pub attempts: Vec<PortMapAttempt>,
    /// The external endpoint the mapping exposes. `None`
    /// when no protocol succeeded.
    pub external_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapAttempt {
    pub protocol: String,
    pub success: bool,
    /// Error message. Skipped when `None` so a successful
    /// attempt's JSON envelope does not carry a trailing
    /// `"error":null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ─────────────────────────── Entry points ───────────────────────────

/// Top-level dispatcher for `a3net doctor <sub>`. Offline —
/// reads the cached snapshot written by the observability
/// background task; does not start a node.
pub fn run_doctor(data_dir: &Path, sub: DoctorCmd) -> Result<()> {
    match sub {
        DoctorCmd::Report { json } => run_report(data_dir, json),
        DoctorCmd::RelayUrls { json } => run_relay_urls(data_dir, json),
        DoctorCmd::PortMapProbe { port, json } => run_port_map_probe(data_dir, port, json),
        DoctorCmd::Accept { json } => run_accept(data_dir, json),
        DoctorCmd::Connect { node_id, json } => run_connect(data_dir, &node_id, json),
    }
}

/// `a3net doctor report [--json]` — full report of the local
/// network environment.
pub fn run_report(data_dir: &Path, json: bool) -> Result<()> {
    let report = load_cached_report(data_dir);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render_report_human(&report);
    }
    Ok(())
}

/// `a3net doctor relay-urls [--json]` — list every configured
/// relay URL annotated with its last measured latency.
pub fn run_relay_urls(data_dir: &Path, json: bool) -> Result<()> {
    let relays = load_cached_relay_latencies(data_dir).unwrap_or_default();
    if json {
        println!("{}", serde_json::to_string_pretty(&relays)?);
    } else {
        render_relay_urls_human(&relays);
    }
    Ok(())
}

/// `a3net doctor port-map-probe <port> [--json]` — run a
/// single port-mapping probe on the supplied local port.
///
/// We deliberately re-use the cached snapshot rather than
/// triggering a fresh probe — the observability background
/// task is responsible for keeping the cache fresh, and a
/// CLI-side probe would race with the live one. When the
/// cache is empty (fresh install, no relay traffic) we
/// surface an empty `PortMapProbeResult` rather than a
/// failure so the operator can render an "I don't know yet"
/// hint.
pub fn run_port_map_probe(data_dir: &Path, port: u16, json: bool) -> Result<()> {
    let mut probe = load_cached_port_map(data_dir).unwrap_or_default();
    // Tag the response with the requested port so the
    // operator can correlate with their CLI invocation when
    // multiple probes are pending.
    probe.external_endpoint = probe.external_endpoint.map(|s| {
        if s.contains(':') && !s.contains('[') {
            // Replace the port in `host:port` form.
            if let Some(idx) = s.rfind(':') {
                let mut new_s = s[..=idx].to_string();
                new_s.push_str(&port.to_string());
                return new_s;
            }
        }
        s
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&probe)?);
    } else {
        render_port_map_human(&probe);
    }
    Ok(())
}

/// `a3net doctor accept [--json]` — open an iroh endpoint and
/// bind to the configured address; mirrors iroh-doctor's
/// "accept" subcommand. Reports the bound endpoint id and the
/// resolved direct / relay addresses.
///
/// For v0.1 this is an offline diagnostic — it reads the
/// cached `EndpointSnapshot` written by the observability
/// background task. The full live "accept" handshake is
/// documented in `iroh-doctor` and tracked separately for
/// the next milestone.
pub fn run_accept(data_dir: &Path, json: bool) -> Result<()> {
    let snap = load_endpoint_snapshot(data_dir);
    if json {
        println!("{}", serde_json::to_string_pretty(&snap)?);
    } else {
        render_endpoint_snapshot_human(&snap);
    }
    Ok(())
}

/// `a3net doctor connect <node-id> [--json]` — dial a remote
/// endpoint by hex NodeId; mirrors iroh-doctor's "connect"
/// subcommand.
///
/// For v0.1 this surfaces the cached addressing info from
/// the discovery cache rather than performing the live
/// handshake. The live path is tracked separately.
pub fn run_connect(data_dir: &Path, node_id: &str, json: bool) -> Result<()> {
    let snap = load_remote_snapshot(data_dir, node_id);
    let parsed = parse_node_id(node_id)?;
    if json {
        let body = serde_json::json!({
            "node_id": parsed,
            "resolved": snap,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        println!("A3Net Doctor: connect");
        println!("{}", "=".repeat(50));
        println!("  node_id    : {parsed}");
        match snap {
            Some(s) => {
                println!("  status     : resolved");
                if !s.direct_addresses.is_empty() {
                    println!("  ipv4/v6    :");
                    for a in &s.direct_addresses {
                        println!("    - {a}");
                    }
                }
                if !s.relay_urls.is_empty() {
                    println!("  relay_urls :");
                    for u in &s.relay_urls {
                        println!("    - {u}");
                    }
                }
                if s.direct_addresses.is_empty() && s.relay_urls.is_empty() {
                    println!("  addresses  : (none cached)");
                }
            }
            None => {
                println!("  status     : unknown (no cached addressing info)");
                println!("  hint       : call `a3net doctor connect` once the peer");
                println!("               has been observed by the gossip or DHT layer.");
            }
        }
    }
    Ok(())
}

// ─────────────────────────── CLI subcommand enum ───────────────────────────

/// The clap-derived subcommand enum exposed as
/// `crate::cli::Cmd::Doctor { sub: DoctorCmd, … }`. We keep a
/// local copy so the dispatcher can be driven from a library
/// consumer without parsing CLI strings.
#[derive(Debug, Clone)]
pub enum DoctorCmd {
    Report {
        json: bool,
    },
    RelayUrls {
        json: bool,
    },
    PortMapProbe {
        port: u16,
        json: bool,
    },
    Accept {
        json: bool,
    },
    Connect {
        node_id: String,
        json: bool,
    },
}

impl From<crate::cli::DoctorCmd> for DoctorCmd {
    fn from(c: crate::cli::DoctorCmd) -> Self {
        match c {
            crate::cli::DoctorCmd::Report { json } => DoctorCmd::Report { json },
            crate::cli::DoctorCmd::RelayUrls { json } => DoctorCmd::RelayUrls { json },
            crate::cli::DoctorCmd::PortMapProbe { port, json } => {
                DoctorCmd::PortMapProbe { port, json }
            }
            crate::cli::DoctorCmd::Accept { json } => DoctorCmd::Accept { json },
            crate::cli::DoctorCmd::Connect { node_id, json } => {
                DoctorCmd::Connect { node_id, json }
            }
        }
    }
}

// ─────────────────────────── Snapshot helpers ───────────────────────────

fn load_cached_report(data_dir: &Path) -> DoctorReport {
    let path = data_dir.join("doctor.json");
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => DoctorReport::default(),
    }
}

fn load_cached_relay_latencies(data_dir: &Path) -> Option<Vec<RelayLatency>> {
    let path = data_dir.join("relay_latencies.json");
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn load_cached_port_map(data_dir: &Path) -> Option<PortMapProbeResult> {
    let path = data_dir.join("port_map.json");
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Lightweight mirror of `a3net_transport::iroh::endpoint_diagnostics::EndpointSnapshot`.
/// Kept local so this crate does not need to take on the
/// `iroh` feature flag — the snapshot is parsed from the JSON
/// cache file, not from a live endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LightweightEndpointSnapshot {
    pub endpoint_id: String,
    pub endpoint_id_short: String,
    pub closed: bool,
    pub identity_path: Option<String>,
    pub direct_addresses: usize,
    pub relay_urls: usize,
}

fn load_endpoint_snapshot(data_dir: &Path) -> LightweightEndpointSnapshot {
    let path = data_dir.join("endpoint_snapshot.json");
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => LightweightEndpointSnapshot::default(),
    }
}

/// Lightweight mirror of `RemoteSnapshot` for the `connect`
/// subcommand.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LightweightRemoteSnapshot {
    pub endpoint_id: String,
    #[serde(default)]
    pub direct_addresses: Vec<String>,
    #[serde(default)]
    pub relay_urls: Vec<String>,
}

fn load_remote_snapshot(data_dir: &Path, node_id: &str) -> Option<LightweightRemoteSnapshot> {
    // The discovery cache writes one JSON file per remote,
    // keyed by the hex NodeId, in `<data_dir>/peers/<id>.json`.
    let peers_dir = data_dir.join("peers");
    let candidate = peers_dir.join(format!("{node_id}.json"));
    if let Ok(bytes) = std::fs::read(&candidate) {
        if let Ok(snap) = serde_json::from_slice::<LightweightRemoteSnapshot>(&bytes) {
            return Some(snap);
        }
    }
    // Fallback: a single `peers.json` file containing an
    // array of snapshots, walked linearly. This is the legacy
    // format the discovery subsystem wrote before per-peer
    // sharding landed.
    let legacy = data_dir.join("peers.json");
    if let Ok(bytes) = std::fs::read(&legacy) {
        if let Ok(snaps) = serde_json::from_slice::<Vec<LightweightRemoteSnapshot>>(&bytes) {
            return snaps.into_iter().find(|s| s.endpoint_id == node_id);
        }
    }
    None
}

fn parse_node_id(s: &str) -> Result<String> {
    let trimmed = s.trim();
    if trimmed.len() != 64 {
        anyhow::bail!(
            "node_id must be 64 hex characters (32 bytes), got {} chars",
            trimmed.len()
        );
    }
    if !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("node_id contains non-hex characters");
    }
    Ok(trimmed.to_ascii_lowercase())
}

// ─────────────────────────── Renderers ───────────────────────────

fn render_report_human(report: &DoctorReport) {
    println!();
    println!(
        "{}",
        Color::Cyan
            .paint("╔═══════════════════════════════════════════════════════════════════╗")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("║                    Network Diagnostic Report                       ║")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("╚═══════════════════════════════════════════════════════════════════╝")
            .bold()
    );
    println!();

    // Connectivity table
    let yes_no_unknown = |b: Option<bool>| -> String {
        match b {
            Some(true) => Color::Green.paint("yes").plain_text().to_string(),
            Some(false) => Color::Red.paint("no").plain_text().to_string(),
            None => Color::Yellow.paint("(unknown)").plain_text().to_string(),
        }
    };

    let mut table = Table::with_headers(["Check", "Value"]);
    table.add_row(["UDP Reachable", &yes_no_unknown(report.udp_reachable)]);
    table.add_row(["IPv4 Reachable", &yes_no_unknown(report.ipv4_reachable)]);
    table.add_row(["IPv6 Reachable", &yes_no_unknown(report.ipv6_reachable)]);
    table.add_row(["IPv4 Can Send", &yes_no_unknown(report.ipv4_can_send)]);
    table.add_row(["IPv6 Can Send", &yes_no_unknown(report.ipv6_can_send)]);
    table.add_row(["Hair Pinning", &yes_no_unknown(report.hair_pinning)]);
    println!("{table}");
    println!();

    // NAT info
    println!("{}", section_header("NAT Configuration"));
    let nat = report.nat_type.as_deref().unwrap_or("(unknown)");
    let nat_colored = match nat {
        "none" => Color::Green.paint(nat),
        _ => Color::Yellow.paint(nat),
    };
    println!("  NAT Type            : {}", nat_colored);
    println!(
        "  Mapping Varies      : {}",
        yes_no_unknown(report.mapping_varies_by_dest_ip)
    );
    println!();

    // Relay info
    println!("{}", section_header("Relay"));
    let relay = report.home_relay_url.as_deref().unwrap_or("(none)");
    let relay_color = if relay == "(none)" {
        Color::Yellow.paint(relay)
    } else {
        Color::Green.paint(relay)
    };
    println!("  Home Relay         : {}", relay_color);
    let rtt = report
        .home_relay_rtt_ms
        .map(|r| format!("{:.1} ms", r))
        .unwrap_or_else(|| "(unknown)".into());
    println!("  Relay RTT          : {}", rtt);
    println!();

    // Port mapping
    println!("{}", section_header("Port Mapping"));
    let protocol = report.successful_protocol.as_deref().unwrap_or("(none)");
    let proto_color = if protocol == "(none)" {
        Color::Yellow.paint(protocol)
    } else {
        Color::Green.paint(protocol)
    };
    println!("  Successful Protocol : {}", proto_color);
    for p in &report.attempted_protocols {
        println!("    - {}", p);
    }
    println!();

    // Global endpoints
    println!("{}", section_header("External Endpoints"));
    let mut ep_table = Table::with_headers(["Type", "Endpoint"]);
    let ipv4 = report.global_ipv4.as_deref().unwrap_or("(unknown)");
    let ipv6 = report.global_ipv6.as_deref().unwrap_or("(unknown)");
    ep_table.add_row(["IPv4", ipv4]);
    ep_table.add_row(["IPv6", ipv6]);
    println!("{ep_table}");
    println!();
}

fn render_relay_urls_human(relays: &[RelayLatency]) {
    println!();
    println!(
        "{}",
        Color::Cyan
            .paint("╔═══════════════════════════════════════════════════════════════════╗")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("║                    Relay Latency Report                            ║")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("╚═══════════════════════════════════════════════════════════════════╝")
            .bold()
    );
    println!();

    if relays.is_empty() {
        println!("{}", alert_widget("info", "No relay URLs configured"));
        return;
    }

    let mut table = Table::with_headers(["Relay URL", "Latency", "Samples", "Failed"]);
    for r in relays {
        let latency = match r.latency_ms {
            Some(ms) => format!("{ms:.1} ms"),
            None => "(never measured)".into(),
        };
        let failed = if r.samples_failed > 0 {
            Color::Red.paint(format!("{}", r.samples_failed)).plain_text().to_string()
        } else {
            Color::Green.paint("0").plain_text().to_string()
        };
        let latency_colored = match r.latency_ms {
            Some(ms) if ms < 100.0 => Color::Green.paint(&latency).plain_text().to_string(),
            Some(ms) if ms < 300.0 => Color::Yellow.paint(&latency).plain_text().to_string(),
            Some(_) => Color::Red.paint(&latency).plain_text().to_string(),
            None => Color::Yellow.paint(&latency).plain_text().to_string(),
        };
        table.add_row([
            &r.url,
            &latency_colored,
            &r.samples.to_string(),
            &failed,
        ]);
    }
    println!("{table}");
    println!();
}

fn render_port_map_human(probe: &PortMapProbeResult) {
    println!();
    println!(
        "{}",
        Color::Cyan
            .paint("╔═══════════════════════════════════════════════════════════════════╗")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("║                    Port Mapping Probe                             ║")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("╚═══════════════════════════════════════════════════════════════════╝")
            .bold()
    );
    println!();

    // Summary panel
    let mut summary = Box::with_title("Port Mapping");
    let protocol = probe.successful_protocol.as_deref().unwrap_or("(none)");
    let proto_color = if protocol == "(none)" {
        Color::Red.paint(protocol)
    } else {
        Color::Green.paint(protocol)
    };
    summary = summary.add_field("Protocol", proto_color.plain_text());
    let endpoint = probe.external_endpoint.as_deref().unwrap_or("(none)");
    let ep_color = if endpoint == "(none)" {
        Color::Yellow.paint(endpoint)
    } else {
        Color::Green.paint(endpoint)
    };
    summary = summary.add_field("External Endpoint", ep_color.plain_text());
    println!("{summary}");
    println!();

    // Attempts
    if !probe.attempts.is_empty() {
        println!("{}", section_header("Protocol Attempts"));
        let mut table = Table::with_headers(["Protocol", "Status"]);
        for a in &probe.attempts {
            let status = if a.success {
                Color::Green.paint("Success")
            } else {
                Color::Red.paint("Failed")
            };
            let err = a.error.as_deref().map(|e| format!(" - {}", e)).unwrap_or_default();
            table.add_row([
                &a.protocol,
                &format!("{}{}", status.plain_text(), err),
            ]);
        }
        println!("{table}");
        println!();
    }
}

fn render_endpoint_snapshot_human(snap: &LightweightEndpointSnapshot) {
    println!();
    println!(
        "{}",
        Color::Cyan
            .paint("╔═══════════════════════════════════════════════════════════════════╗")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("║                    Endpoint Snapshot                              ║")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("╚═══════════════════════════════════════════════════════════════════╝")
            .bold()
    );
    println!();

    if snap.endpoint_id.is_empty() {
        println!("{}", alert_widget("warn", "No endpoint snapshot cached. Boot the node at least once."));
        println!();
        return;
    }

    // Identity panel
    let mut identity = Box::with_title("Identity");
    identity = identity.add_field("Endpoint ID", &snap.endpoint_id);
    identity = identity.add_field("Short ID", &format!("a3net-{}", snap.endpoint_id_short));
    let status_str = if snap.closed {
        Color::Yellow.paint("Closed").plain_text().to_string()
    } else {
        Color::Green.paint("Active").plain_text().to_string()
    };
    identity = identity.add_field("Status", status_str);
    if let Some(path) = &snap.identity_path {
        identity = identity.add_field("Identity Path", path);
    }
    println!("{identity}");
    println!();

    // Addresses
    if snap.direct_addresses > 0 {
        println!("{}", section_header("Direct Addresses"));
        println!("  {} addresses available", snap.direct_addresses);
        println!();
    }

    if snap.relay_urls > 0 {
        println!("{}", section_header("Relay URLs"));
        println!("  {} relay URLs available", snap.relay_urls);
        println!();
    }
}

#[allow(dead_code)]
fn _ensure_tempdir_helper() -> PathBuf {
    // Helper kept so tests can mint a tempdir without
    // pulling in `tempfile` directly. Returns the path; the
    // caller is responsible for cleanup.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "a3net-doctor-{}-{}-{}",
        std::process::id(),
        nanos,
        std::time::SystemTime::now().elapsed().unwrap_or_default().as_nanos(),
    ));
    std::fs::create_dir_all(&path).expect("create tempdir");
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(prefix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            std::time::SystemTime::now().elapsed().unwrap_or_default().as_nanos(),
        ));
        std::fs::create_dir_all(&path).expect("create tempdir");
        path
    }

    /// Pin the JSON wire shape — embedders / scripts may
    /// parse these fields.
    #[test]
    fn doctor_report_field_names_pin() {
        let r = DoctorReport {
            udp_reachable: Some(true),
            ipv6_reachable: Some(false),
            ipv4_reachable: Some(true),
            ipv4_can_send: Some(true),
            ipv6_can_send: Some(false),
            nat_type: Some("restricted".into()),
            hair_pinning: Some(false),
            mapping_varies_by_dest_ip: Some(false),
            home_relay_url: Some("https://relay.example".into()),
            home_relay_rtt_ms: Some(42),
            attempted_protocols: vec!["upnp".into(), "nat_pmp".into()],
            successful_protocol: Some("upnp".into()),
            global_ipv4: Some("203.0.113.42:54321".into()),
            global_ipv6: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        for needle in [
            "\"udp_reachable\":true",
            "\"nat_type\":\"restricted\"",
            "\"home_relay_url\":\"https://relay.example\"",
            "\"home_relay_rtt_ms\":42",
            "\"successful_protocol\":\"upnp\"",
            "\"global_ipv4\":\"203.0.113.42:54321\"",
        ] {
            assert!(json.contains(needle), "missing {needle} in {json}");
        }
    }

    /// `DoctorReport::default()` MUST be all-`None` so the
    /// embedder can render "I don't know yet" without a
    /// status-code branch.
    #[test]
    fn default_report_is_all_none() {
        let r = DoctorReport::default();
        assert!(r.udp_reachable.is_none());
        assert!(r.ipv4_reachable.is_none());
        assert!(r.ipv6_reachable.is_none());
        assert!(r.nat_type.is_none());
        assert!(r.home_relay_url.is_none());
        assert!(r.home_relay_rtt_ms.is_none());
        assert!(r.global_ipv4.is_none());
        assert!(r.global_ipv6.is_none());
        assert!(r.attempted_protocols.is_empty());
        assert!(r.successful_protocol.is_none());
    }

    /// `RelayLatency` carries a nullable latency for relays
    /// that have never been probed yet — embedders that pin
    /// `f64` cannot accidentally crash on the field.
    #[test]
    fn relay_latency_preserves_missing_measurement() {
        let r = RelayLatency {
            url: "https://relay.example".into(),
            latency_ms: None,
            samples: 0,
            samples_failed: 0,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: RelayLatency = serde_json::from_str(&json).unwrap();
        assert!(back.latency_ms.is_none());
    }

    /// `PortMapAttempt` always carries a `protocol` and
    /// `success` field; the `error` is skipped on success.
    #[test]
    fn port_map_attempt_success_has_no_error_key() {
        let a = PortMapAttempt {
            protocol: "upnp".into(),
            success: true,
            error: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"success\":true"));
        // The error field is `skip_serializing_if = "Option::is_none"`,
        // so a successful attempt should not have an
        // `"error":null` field at all.
        assert!(!json.contains("error"), "got: {json}");
    }

    /// `parse_node_id` validates the input.
    #[test]
    fn parse_node_id_accepts_hex_64() {
        let s = "0".repeat(64);
        let out = parse_node_id(&s).expect("valid");
        assert_eq!(out, s);
    }

    #[test]
    fn parse_node_id_rejects_short() {
        let s = "abcd";
        assert!(parse_node_id(s).is_err());
    }

    #[test]
    fn parse_node_id_rejects_non_hex() {
        let mut s = String::with_capacity(64);
        s.push_str("z");
        s.push_str(&"0".repeat(63));
        assert!(parse_node_id(&s).is_err());
    }

    #[test]
    fn parse_node_id_lowercases() {
        let mut s = "ABCDEF".to_string();
        s.push_str(&"0".repeat(58));
        let out = parse_node_id(&s).expect("valid");
        assert!(out.chars().all(|c| !c.is_ascii_uppercase()));
    }

    /// `load_cached_report` returns a `DoctorReport::default()`
    /// when the snapshot file is missing — never an error.
    #[test]
    fn load_cached_report_returns_default_when_missing() {
        let dir = tempdir("a3net-doctor-empty");
        let r = load_cached_report(&dir);
        assert!(r.udp_reachable.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `load_cached_report` reads the JSON file when present.
    #[test]
    fn load_cached_report_reads_json_when_present() {
        let dir = tempdir("a3net-doctor-write");
        let report = DoctorReport {
            nat_type: Some("symmetric".into()),
            home_relay_url: Some("https://relay.example".into()),
            home_relay_rtt_ms: Some(42),
            ..DoctorReport::default()
        };
        std::fs::write(
            dir.join("doctor.json"),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        let r = load_cached_report(&dir);
        assert_eq!(r.nat_type.as_deref(), Some("symmetric"));
        assert_eq!(r.home_relay_rtt_ms, Some(42));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `run_report` writes JSON when `--json` is set; never
    /// panics on a missing cache file.
    #[test]
    fn run_report_with_missing_cache_does_not_panic() {
        let dir = tempdir("a3net-doctor-run");
        // Both `false` (human) and `true` (json) modes should
        // be safe when the cache is missing.
        run_report(&dir, false).expect("human report");
        run_report(&dir, true).expect("json report");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `run_relay_urls` returns an empty list (not an error)
    /// when the cache is missing.
    #[test]
    fn run_relay_urls_with_missing_cache_is_empty() {
        let dir = tempdir("a3net-doctor-relays");
        run_relay_urls(&dir, true).expect("relay urls");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `run_port_map_probe` returns a default `PortMapProbeResult`
    /// when the cache is missing.
    #[test]
    fn run_port_map_probe_with_missing_cache_is_empty() {
        let dir = tempdir("a3net-doctor-port");
        run_port_map_probe(&dir, 443, true).expect("port-map probe");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `run_accept` falls back to an empty snapshot when
    /// the cache is missing.
    #[test]
    fn run_accept_with_missing_cache_is_empty() {
        let dir = tempdir("a3net-doctor-accept");
        run_accept(&dir, true).expect("accept");
        run_accept(&dir, false).expect("accept human");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `run_connect` with a bogus node id returns an error.
    #[test]
    fn run_connect_rejects_bad_node_id() {
        let dir = tempdir("a3net-doctor-connect");
        let err = run_connect(&dir, "not-hex", true).unwrap_err();
        assert!(err.chain().any(|e| e.to_string().contains("hex")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `run_connect` with a valid (uncached) node id surfaces
    /// an "unknown" status rather than an error.
    #[test]
    fn run_connect_unknown_node_id_returns_unknown() {
        let dir = tempdir("a3net-doctor-connect-unknown");
        let node_id = "0".repeat(64);
        run_connect(&dir, &node_id, false).expect("connect");
        run_connect(&dir, &node_id, true).expect("connect json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `run_connect` finds a peer in the legacy single-file
    /// `peers.json` array.
    #[test]
    fn run_connect_reads_legacy_peers_array() {
        let dir = tempdir("a3net-doctor-legacy");
        let node_id = "0".repeat(64);
        let snap = LightweightRemoteSnapshot {
            endpoint_id: node_id.clone(),
            direct_addresses: vec!["203.0.113.42:7777".into()],
            relay_urls: vec!["https://relay.example".into()],
        };
        let body = serde_json::to_vec(&vec![snap]).unwrap();
        std::fs::write(dir.join("peers.json"), body).unwrap();
        run_connect(&dir, &node_id, true).expect("connect");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `run_connect` finds a peer in the per-peer sharded
    /// `<data_dir>/peers/<id>.json` file.
    #[test]
    fn run_connect_reads_per_peer_shard() {
        let dir = tempdir("a3net-doctor-shard");
        let peers_dir = dir.join("peers");
        std::fs::create_dir_all(&peers_dir).unwrap();
        let node_id = "1".repeat(64);
        let snap = LightweightRemoteSnapshot {
            endpoint_id: node_id.clone(),
            direct_addresses: vec!["198.51.100.7:4242".into()],
            relay_urls: vec![],
        };
        std::fs::write(
            peers_dir.join(format!("{node_id}.json")),
            serde_json::to_vec(&snap).unwrap(),
        )
        .unwrap();
        run_connect(&dir, &node_id, true).expect("connect");
        let _ = std::fs::remove_dir_all(&dir);
    }
}