//! `a3net vless` command handlers.
//!
//! Implements the CLI surface for the `a3net-vless-client` crate:
//!
//! - `connect`  — start the local proxy
//! - `show`     — parse and display a vless:// URI
//! - `probe`    — TCP+TLS reachability check
//! - `convert`  — v2ray JSON → vless:// URI
//! - `doctor`   — backend binary discovery

use std::net::SocketAddr;
use std::path::Path;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::cli::VlessCmd;
use crate::error::CliError;

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

pub async fn run_vless(sub: VlessCmd) -> Result<()> {
    match sub {
        VlessCmd::Connect {
            uri,
            socks5_port,
            http_port,
            backend,
            log_level,
        } => run_connect(&uri, socks5_port, http_port, &backend, &log_level).await,

        VlessCmd::Show { uri, json } => run_show(&uri, json),

        VlessCmd::Probe {
            uri,
            timeout_secs,
            json,
        } => run_probe(&uri, timeout_secs, json).await,

        VlessCmd::Convert { path, json } => run_convert(&path, json),

        VlessCmd::Doctor { backend } => run_doctor(&backend).await,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// connect
// ─────────────────────────────────────────────────────────────────────────────

async fn run_connect(
    uri: &str,
    socks5_port: u16,
    http_port: Option<u16>,
    backend_str: &str,
    log_level: &str,
) -> Result<()> {
    use a3net_vless_client::{BackendKind, VlessClient, VlessClientConfig};

    let link = a3net_vless_client::VlessLink::parse(uri)
        .map_err(|e| anyhow!("{e}"))?;

    let backend = parse_backend_kind(backend_str)?;

    let socks5_addr: SocketAddr = if socks5_port == 0 {
        // Port 0 means "pick a free port"
        "127.0.0.1:0".parse().unwrap()
    } else {
        format!("127.0.0.1:{socks5_port}").parse().unwrap()
    };

    let http_addr = http_port.map(|p| {
        if p == 0 {
            "127.0.0.1:0".parse().unwrap()
        } else {
            format!("127.0.0.1:{p}").parse().unwrap()
        }
    });

    let cfg = VlessClientConfig {
        link,
        listen_socks5: socks5_addr,
        listen_http: http_addr,
        backend,
        log_level: log_level.to_string(),
        grace: None,
    };

    eprintln!(
        "Connecting to {}:{} via {:?} backend…",
        cfg.link.host, cfg.link.port, cfg.backend
    );

    let client = VlessClient::start_owned(cfg).await.map_err(|e| anyhow!("{e}"))?;

    // Report bound addresses
    let socks5_addr = client.handle().socks5_addr().await;
    if let Some(addr) = socks5_addr {
        println!("SOCKS5 proxy listening on {}", addr);
    }
    if let Some(addr) = client.handle().http_addr().await {
        println!("HTTP-CONNECT proxy listening on {}", addr);
    }
    println!("Press Ctrl+C to stop…");

    // Block until interrupted
    tokio::signal::ctrl_c().await?;

    println!("Shutting down…");
    client.shutdown().await.map_err(|e| anyhow!("{e}"))?;
    println!("Stopped.");
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// show
// ─────────────────────────────────────────────────────────────────────────────

fn run_show(uri: &str, json: bool) -> Result<()> {
    use a3net_vless_client::VlessLink;

    let link = VlessLink::parse(uri).map_err(|e| anyhow!("{e}"))?;

    if json {
        let payload = ShowJson {
            uuid: &link.uuid,
            host: &link.host,
            port: link.port,
            tag: link.tag.as_deref(),
            transport: link.transport.as_str(),
            security: link.security.as_str(),
            sni: link.sni.as_deref(),
            alpn: link.alpn.as_deref(),
            fingerprint: link.fingerprint.as_deref(),
            flow: link.flow.map(|f| f.as_str().to_string()),
            path: link.path.as_deref(),
            http_host: link.http_host.as_deref(),
            service_name: link.service_name.as_deref(),
            reality_pbk: link.reality_pbk.as_deref(),
            reality_sid: link.reality_sid.as_deref(),
            notes: &link.notes,
        };
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        println!("UUID       : {}", link.uuid);
        println!("Host       : {}", link.host);
        println!("Port       : {}", link.port);
        if let Some(tag) = &link.tag {
            println!("Tag        : {tag}");
        }
        println!("Transport  : {}", link.transport.as_str());
        println!("Security   : {}", link.security.as_str());
        if let Some(sni) = &link.sni {
            println!("SNI        : {sni}");
        }
        if let Some(alpn) = &link.alpn {
            println!("ALPN       : {alpn}");
        }
        if let Some(fp) = &link.fingerprint {
            println!("Fingerprint: {fp}");
        }
        if let Some(flow) = &link.flow {
            println!("Flow       : {}", flow.as_str());
        }
        if let Some(path) = &link.path {
            println!("Path       : {path}");
        }
        if let Some(host) = &link.http_host {
            println!("HTTP Host  : {host}");
        }
        if let Some(sn) = &link.service_name {
            println!("gRPC Svc   : {sn}");
        }
        if let Some(pbk) = &link.reality_pbk {
            println!("REALITY PBK: {pbk}");
        }
        if let Some(sid) = &link.reality_sid {
            println!("REALITY SID : {sid}");
        }
        if !link.notes.is_empty() {
            println!("\nNotes:");
            for note in &link.notes {
                println!("  - {note}");
            }
        }
    }

    Ok(())
}

#[derive(Serialize)]
struct ShowJson<'a> {
    uuid: &'a str,
    host: &'a str,
    port: u16,
    tag: Option<&'a str>,
    transport: &'a str,
    security: &'a str,
    sni: Option<&'a str>,
    alpn: Option<&'a str>,
    fingerprint: Option<&'a str>,
    flow: Option<String>,
    path: Option<&'a str>,
    http_host: Option<&'a str>,
    service_name: Option<&'a str>,
    reality_pbk: Option<&'a str>,
    reality_sid: Option<&'a str>,
    notes: &'a [String],
}

// ─────────────────────────────────────────────────────────────────────────────
// probe
// ─────────────────────────────────────────────────────────────────────────────

async fn run_probe(uri: &str, timeout_secs: u64, json: bool) -> Result<()> {
    use a3net_vless_client::VlessLink;

    let link = VlessLink::parse(uri).map_err(|e| anyhow!("{e}"))?;

    let dest: SocketAddr = format!("{}:{}", link.host, link.port)
        .parse()
        .map_err(|e: std::net::AddrParseError| anyhow!("invalid address: {e}"))?;

    let start = std::time::Instant::now();

    // TCP connect probe
    let tcp_result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        tokio::net::TcpStream::connect(dest),
    )
    .await;

    let rtt_ms = start.elapsed().as_millis() as u64;

    match tcp_result {
        Ok(Ok(_stream)) => {
            let result = ProbeResult {
                host: &link.host,
                port: link.port,
                reachable: true,
                rtt_ms,
                security: link.security.as_str(),
                sni: link.sni.as_deref(),
                error: None,
            };
            if json {
                serde_json::to_writer_pretty(std::io::stdout(), &result)?;
                println!();
            } else {
                println!("✓ {}:{} reachable  rtt={}ms", link.host, link.port, rtt_ms);
            }
        }
        Ok(Err(e)) => {
            let result = ProbeResult {
                host: &link.host,
                port: link.port,
                reachable: false,
                rtt_ms,
                security: link.security.as_str(),
                sni: link.sni.as_deref(),
                error: Some(e.to_string()),
            };
            if json {
                serde_json::to_writer_pretty(std::io::stdout(), &result)?;
                println!();
            } else {
                println!("✗ {}:{} unreachable  {}", link.host, link.port, e);
            }
        }
        Err(_) => {
            let result = ProbeResult {
                host: &link.host,
                port: link.port,
                reachable: false,
                rtt_ms,
                security: link.security.as_str(),
                sni: link.sni.as_deref(),
                error: Some(format!("timeout after {timeout_secs}s")),
            };
            if json {
                serde_json::to_writer_pretty(std::io::stdout(), &result)?;
                println!();
            } else {
                println!(
                    "✗ {}:{} timeout after {}s",
                    link.host, link.port, timeout_secs
                );
            }
        }
    }

    Ok(())
}

#[derive(Serialize)]
struct ProbeResult<'a> {
    host: &'a str,
    port: u16,
    reachable: bool,
    rtt_ms: u64,
    security: &'a str,
    sni: Option<&'a str>,
    error: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// convert
// ─────────────────────────────────────────────────────────────────────────────

fn run_convert(path: &str, json: bool) -> Result<()> {
    let input: String = if path == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(path)?
    };

    let v2ray: V2RayConfig = serde_json::from_str(&input)
        .map_err(|e| anyhow!("invalid v2ray JSON: {e}"))?;

    let mut outbounds = Vec::new();

    for ob in &v2ray.outbounds {
        if ob.protocol == "vless" {
            if let Some(vless) = ob.settings.vnext.first() {
                let uri = build_vless_uri(vless, ob.stream_settings.as_ref(), ob.tag.as_deref());
                outbounds.push(VlessOutboundJson {
                    tag: ob.tag.clone(),
                    uri,
                    transport: ob.stream_settings.as_ref().map(|s| s.network.clone()),
                    security: ob.stream_settings.as_ref().and_then(|s| s.security.clone()),
                    notes: Vec::new(),
                });
            }
        }
    }

    if json {
        serde_json::to_writer_pretty(std::io::stdout(), &outbounds)?;
        println!();
    } else {
        if outbounds.is_empty() {
            println!("No vless:// outbounds found in config.");
        } else {
            for ob in &outbounds {
                println!("─── {} ───", ob.tag.as_deref().unwrap_or("(untagged)"));
                println!("{}", ob.uri);
                if let Some(t) = &ob.transport {
                    println!("Transport  : {t}");
                }
                if let Some(s) = &ob.security {
                    println!("Security   : {s}");
                }
                println!();
            }
        }
    }

    Ok(())
}

// ── v2ray JSON config parsing ────────────────────────────────────────────────

#[derive(Deserialize)]
struct V2RayConfig {
    #[serde(default)]
    outbounds: Vec<V2RayOutbound>,
    #[serde(default)]
    routing: Option<V2RayRouting>,
    #[serde(default)]
    dns: Option<V2RayDns>,
    #[serde(default)]
    inbounds: Vec<V2RayInbound>,
}

#[derive(Deserialize)]
struct V2RayOutbound {
    #[serde(default)]
    tag: Option<String>,
    protocol: String,
    settings: V2RayOutboundSettings,
    #[serde(default)]
    stream_settings: Option<V2RayStreamSettings>,
}

#[derive(Deserialize, Default)]
struct V2RayOutboundSettings {
    #[serde(default)]
    vnext: Vec<V2RayVnext>,
}

#[derive(Deserialize)]
struct V2RayVnext {
    address: String,
    port: u16,
    #[serde(default)]
    users: Vec<V2RayUser>,
}

#[derive(Deserialize)]
struct V2RayUser {
    id: String,
    #[serde(default)]
    encryption: Option<String>,
    #[serde(default)]
    flow: Option<String>,
    #[serde(default)]
    level: Option<u32>,
}

#[derive(Deserialize)]
struct V2RayStreamSettings {
    #[serde(default)]
    network: String,
    #[serde(default)]
    security: Option<String>,
    #[serde(rename = "wsSettings", default)]
    ws_settings: Option<V2RayWsSettings>,
    #[serde(rename = "httpSettings", default)]
    http_settings: Option<V2RayHttpSettings>,
    #[serde(rename = "grpcSettings", default)]
    grpc_settings: Option<V2RayGrpcSettings>,
    #[serde(rename = "kcpSettings", default)]
    kcp_settings: Option<V2RayKcpSettings>,
    #[serde(rename = "tlsSettings", default)]
    tls_settings: Option<V2RayTlsSettings>,
    #[serde(rename = "realitySettings", default)]
    reality_settings: Option<V2RayRealitySettings>,
}

#[derive(Deserialize, Default)]
struct V2RayWsSettings {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    headers: Option<V2RayHttpHeaders>,
}

#[derive(Deserialize, Default)]
struct V2RayHttpSettings {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    host: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
struct V2RayGrpcSettings {
    #[serde(default)]
    serviceName: Option<String>,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Deserialize, Default)]
struct V2RayKcpSettings {
    #[serde(default)]
    mtu: Option<u32>,
    #[serde(default)]
    tti: Option<u32>,
    #[serde(default)]
    uplinkCapacity: Option<u32>,
    #[serde(default)]
    downlinkCapacity: Option<u32>,
    #[serde(default)]
    readBufferSize: Option<u32>,
    #[serde(default)]
    writeBufferSize: Option<u32>,
    #[serde(default)]
    packetHeader: Option<V2RayKcpHeader>,
    #[serde(default)]
    congestion: Option<bool>,
}

#[derive(Deserialize, Default)]
struct V2RayKcpHeader {
    #[serde(default)]
    type_: Option<String>,
}

#[derive(Deserialize, Default)]
struct V2RayTlsSettings {
    #[serde(default)]
    serverName: Option<String>,
    #[serde(default)]
    alpn: Option<Vec<String>>,
    #[serde(default)]
    fingerprint: Option<String>,
}

#[derive(Deserialize, Default)]
struct V2RayRealitySettings {
    #[serde(default)]
    publicKey: Option<String>,
    #[serde(default)]
    shortId: Option<String>,
    #[serde(default)]
    spiderX: Option<String>,
}

#[derive(Deserialize, Default)]
struct V2RayHttpHeaders {
    #[serde(default)]
    Host: Option<String>,
}

#[derive(Deserialize)]
struct V2RayRouting {
    #[serde(default)]
    domainStrategy: Option<String>,
    #[serde(default)]
    rules: Vec<V2RayRoutingRule>,
}

#[derive(Deserialize)]
struct V2RayRoutingRule {
    #[serde(default)]
    type_: Option<String>,
    #[serde(default)]
    ip: Option<Vec<String>>,
    #[serde(default)]
    domain: Option<Vec<String>>,
    #[serde(default)]
    protocol: Option<Vec<String>>,
    #[serde(default)]
    inboundTag: Option<Vec<String>>,
    #[serde(default)]
    outboundTag: Option<String>,
}

#[derive(Deserialize)]
struct V2RayDns {
    #[serde(default)]
    servers: Vec<V2RayDnsServer>,
}

#[derive(Deserialize)]
struct V2RayDnsServer {
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    domains: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct V2RayInbound {
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    port: Option<String>,
    #[serde(default)]
    listen: Option<String>,
}

#[derive(Serialize)]
struct VlessOutboundJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    security: Option<String>,
    notes: Vec<String>,
}

fn build_vless_uri(
    vnext: &V2RayVnext,
    stream: Option<&V2RayStreamSettings>,
    tag: Option<&str>,
) -> String {
    let mut parts: Vec<(&str, String)> = Vec::new();

    // transport
    let network = stream.map(|s| s.network.as_str()).unwrap_or("tcp");
    parts.push(("type", network.to_string()));

    // security
    let (security, tls_settings, reality_settings) = match stream {
        Some(s) => (s.security.clone(), s.tls_settings.as_ref(), s.reality_settings.as_ref()),
        None => (None, None, None),
    };

    let sec = reality_settings
        .map(|_| "reality")
        .or_else(|| tls_settings.map(|_| "tls"))
        .unwrap_or("none");
    parts.push(("security", sec.to_string()));

    // TLS / REALITY params
    if let Some(tls) = tls_settings {
        if let Some(sni) = &tls.serverName {
            parts.push(("sni", sni.clone()));
        }
        if let Some(alpn) = &tls.alpn {
            if !alpn.is_empty() {
                parts.push(("alpn", alpn.join(",")));
            }
        }
        if let Some(fp) = &tls.fingerprint {
            parts.push(("fp", fp.clone()));
        }
    }
    if let Some(real) = reality_settings {
        if let Some(pk) = &real.publicKey {
            parts.push(("pbk", pk.clone()));
        }
        if let Some(sid) = &real.shortId {
            parts.push(("sid", sid.clone()));
        }
    }

    // transport-specific params
    if let Some(s) = stream {
        match s.network.as_str() {
            "ws" => {
                if let Some(ws) = &s.ws_settings {
                    if let Some(p) = &ws.path {
                        parts.push(("path", p.clone()));
                    }
                    if let Some(h) = &ws.headers.as_ref().and_then(|h| h.Host.clone()) {
                        parts.push(("host", h.clone()));
                    }
                }
            }
            "http" => {
                if let Some(http) = &s.http_settings {
                    if let Some(p) = &http.path {
                        parts.push(("path", p.clone()));
                    }
                    if let Some(hosts) = &http.host {
                        if let Some(h) = hosts.first() {
                            parts.push(("host", h.clone()));
                        }
                    }
                }
            }
            "grpc" => {
                if let Some(grpc) = &s.grpc_settings {
                    if let Some(sn) = &grpc.serviceName {
                        parts.push(("serviceName", sn.clone()));
                    }
                    if let Some(m) = &grpc.mode {
                        parts.push(("mode", m.clone()));
                    }
                }
            }
            _ => {}
        }
    }

    // Build query string
    let query: String = parts
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                percent_encoding::utf8_percent_encode(k, percent_encoding::NON_ALPHANUMERIC),
                percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC),
            )
        })
        .collect::<Vec<_>>()
        .join("&");

    let mut uri = format!(
        "vless://{}@{}:{}",
        percent_encoding::utf8_percent_encode(&vnext.address, percent_encoding::NON_ALPHANUMERIC),
        vnext.address,
        vnext.port,
    );

    if !query.is_empty() {
        uri.push('?');
        uri.push_str(&query);
    }

    if let Some(t) = tag {
        uri.push('#');
        uri.push_str(t);
    }

    uri
}

// ─────────────────────────────────────────────────────────────────────────────
// doctor
// ─────────────────────────────────────────────────────────────────────────────

async fn run_doctor(backend_str: &str) -> Result<()> {
    use a3net_vless_client::subprocess::BackendKind;

    let kind = parse_backend_kind(backend_str)?;

    match kind {
        BackendKind::AutoDetect => {
            if a3net_vless_client::probe_for_test(&["xray", "xray-core"]).await {
                println!("✓ xray found on PATH");
            } else if a3net_vless_client::probe_for_test(&["sing-box", "sing_box"]).await {
                println!("✓ sing-box found on PATH");
            } else {
                return Err(anyhow!(
                    "no VLESS backend found on PATH (checked: xray, xray-core, sing-box, sing_box)"
                ));
            }
        }
        BackendKind::Xray => {
            if a3net_vless_client::probe_for_test(&["xray", "xray-core"]).await {
                println!("✓ xray found on PATH");
            } else {
                return Err(anyhow!("xray not found on PATH (checked: xray, xray-core)"));
            }
        }
        BackendKind::SingBox => {
            if a3net_vless_client::probe_for_test(&["sing-box", "sing_box"]).await {
                println!("✓ sing-box found on PATH");
            } else {
                return Err(anyhow!("sing-box not found on PATH (checked: sing-box, sing_box)"));
            }
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// helpers
// ─────────────────────────────────────────────────────────────────────────────

fn parse_backend_kind(s: &str) -> Result<a3net_vless_client::subprocess::BackendKind> {
    match s.to_ascii_lowercase().as_str() {
        "auto" | "" => Ok(a3net_vless_client::subprocess::BackendKind::AutoDetect),
        "xray" => Ok(a3net_vless_client::subprocess::BackendKind::Xray),
        "sing-box" | "singbox" | "sing_box" => {
            Ok(a3net_vless_client::subprocess::BackendKind::SingBox)
        }
        other => Err(anyhow!(
            "unknown backend kind: {other} (expected: auto, xray, sing-box)"
        )),
    }
}
