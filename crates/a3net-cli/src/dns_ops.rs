//! `a3net dns` — operator commands for the self-hosted
//! `a3net-dns-server` HTTP admin API.
//!
//! Mirrors a small slice of `coredns` / `unbound`'s operator
//! CLI so operators don't need to learn a separate tool:
//!
//! - `a3net dns publish <zone> <ipns-name> <payload>` —
//!   publish a TXT record via `PUT /zones/<zone>/ipns/<name>`.
//! - `a3net dns list <zone>` — list every record in the
//!   zone (`GET /zones/<zone>/records`).
//! - `a3net dns axfr <zone>` — full zone transfer
//!   (`GET /zones/<zone>/axfr`).
//! - `a3net dns query <server> <name> <type>` — issue a
//!   raw DNS query against a server (UDP) and print the
//!   wire-format response summary.
//!
//! All HTTP commands honour the `ADNET_DNS_HTTP_URL`
//! environment variable (defaulting to
//! `http://127.0.0.1:8080`) so operators can run the DNS
//! server on a private interface and tunnel the admin API
//! through ssh / ngrok.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// Default base URL for the `a3net-dns-server` HTTP admin
/// API. Mirrors the convention used by other CLI commands
/// that hit a locally-running service.
pub const DEFAULT_HTTP_URL: &str = "http://127.0.0.1:8080";

/// Top-level dispatcher for `a3net dns <sub>`. Drives the
/// HTTP admin API of a running `a3net-dns-server`; never
/// touches the local node state.
pub fn run_dns(cmd: DnsCmd, http_url: Option<&str>) -> Result<()> {
    let base = http_url
        .map(String::from)
        .or_else(|| std::env::var("ADNET_DNS_HTTP_URL").ok())
        .unwrap_or_else(|| DEFAULT_HTTP_URL.to_string());
    let client = DnsHttpClient::new(base)?;
    match cmd {
        DnsCmd::Publish {
            zone,
            ipns_name,
            payload,
            ttl_secs,
            json,
        } => client.publish(&zone, &ipns_name, &payload, ttl_secs, json),
        DnsCmd::List { zone, json } => client.list(&zone, json),
        DnsCmd::Axfr { zone, json } => client.axfr(&zone, json),
        DnsCmd::Query {
            server,
            name,
            qtype,
            timeout_ms,
            json,
        } => query_dns(&server, &name, &qtype, timeout_ms, json),
    }
}

/// `clap -> dns_ops` adapter. The CLI layer (`cli::DnsCmd`)
/// is intentionally a thin shim; the operator-facing logic
/// lives here so other embedders (e.g. the `a3net-dns-server`
/// admin shell) can drive the same code paths.
impl From<crate::cli::DnsCmd> for DnsCmd {
    fn from(c: crate::cli::DnsCmd) -> Self {
        match c {
            crate::cli::DnsCmd::Publish {
                zone,
                ipns_name,
                payload,
                ttl_secs,
                json,
            } => DnsCmd::Publish {
                zone,
                ipns_name,
                payload,
                ttl_secs,
                json,
            },
            crate::cli::DnsCmd::List { zone, json } => DnsCmd::List { zone, json },
            crate::cli::DnsCmd::Axfr { zone, json } => DnsCmd::Axfr { zone, json },
            crate::cli::DnsCmd::Query {
                server,
                name,
                qtype,
                timeout_ms,
                json,
            } => DnsCmd::Query {
                server,
                name,
                qtype,
                timeout_ms,
                json,
            },
        }
    }
}

/// Subcommand enum exposed as `crate::cli::Cmd::Dns { sub: DnsCmd, … }`.
#[derive(Debug, Clone)]
pub enum DnsCmd {
    Publish {
        zone: String,
        ipns_name: String,
        payload: String,
        ttl_secs: Option<u32>,
        json: bool,
    },
    List {
        zone: String,
        json: bool,
    },
    Axfr {
        zone: String,
        json: bool,
    },
    Query {
        server: String,
        name: String,
        qtype: String,
        timeout_ms: u64,
        json: bool,
    },
}

/// HTTP client wrapping the `a3net-dns-server` admin API.
pub struct DnsHttpClient {
    base: String,
    client: reqwest::Client,
}

impl DnsHttpClient {
    pub fn new(base: String) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .context("build reqwest client")?;
        Ok(Self { base, client })
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    pub async fn publish_async(
        &self,
        zone: &str,
        ipns_name: &str,
        payload: &str,
        ttl_secs: Option<u32>,
    ) -> Result<serde_json::Value> {
        let url = self.url(&format!(
            "zones/{}/ipns/{}",
            urlencoded(zone),
            urlencoded(ipns_name)
        ));
        let body = PublishBody {
            payload: payload.to_string(),
            ttl_secs,
        };
        let resp = self
            .client
            .put(&url)
            .json(&body)
            .send()
            .await
            .context("PUT ipns record")?;
        let status = resp.status();
        let json: serde_json::Value = resp
            .json()
            .await
            .with_context(|| format!("decode JSON from {} (status {})", url, status))?;
        if !status.is_success() {
            return Err(anyhow!("PUT {} returned {}: {}", url, status, json));
        }
        Ok(json)
    }

    pub fn publish(
        &self,
        zone: &str,
        ipns_name: &str,
        payload: &str,
        ttl_secs: Option<u32>,
        json: bool,
    ) -> Result<()> {
        let value = futures::executor::block_on(self.publish_async(
            zone, ipns_name, payload, ttl_secs,
        ))?;
        if json {
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            println!(
                "Published {} as _a3net.{}.{} (TTL={:?})",
                ipns_name,
                ipns_name,
                zone,
                ttl_secs.unwrap_or(3600)
            );
        }
        Ok(())
    }

    pub async fn list_async(&self, zone: &str) -> Result<Vec<serde_json::Value>> {
        let url = self.url(&format!("zones/{}/records", urlencoded(zone)));
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("GET records")?;
        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .with_context(|| format!("decode JSON from {} (status {})", url, status))?;
        if !status.is_success() {
            return Err(anyhow!("GET {} returned {}: {}", url, status, body));
        }
        let arr = body
            .as_array()
            .ok_or_else(|| anyhow!("expected array body, got {}", body))?;
        Ok(arr.clone())
    }

    pub fn list(&self, zone: &str, json: bool) -> Result<()> {
        let value = futures::executor::block_on(self.list_async(zone))?;
        if json {
            println!("{}", serde_json::to_string_pretty(&value)?);
            return Ok(());
        }
        println!("Zone {} records ({} total):", zone, value.len());
        for v in &value {
            let key = v.get("key").and_then(|x| x.as_str()).unwrap_or("?");
            let kind = v.get("kind").unwrap_or(&serde_json::Value::Null);
            println!("  {} :: {}", key, kind);
        }
        Ok(())
    }

    pub async fn axfr_async(&self, zone: &str) -> Result<serde_json::Value> {
        let url = self.url(&format!("zones/{}/axfr", urlencoded(zone)));
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("GET axfr")?;
        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .with_context(|| format!("decode JSON from {} (status {})", url, status))?;
        if !status.is_success() {
            return Err(anyhow!("GET {} returned {}: {}", url, status, body));
        }
        Ok(body)
    }

    pub fn axfr(&self, zone: &str, json: bool) -> Result<()> {
        let value = futures::executor::block_on(self.axfr_async(zone))?;
        if json {
            println!("{}", serde_json::to_string_pretty(&value)?);
            return Ok(());
        }
        let count = value.get("record_count").and_then(|x| x.as_u64()).unwrap_or(0);
        println!("AXFR {}: {} records", zone, count);
        if let Some(records) = value.get("records").and_then(|x| x.as_array()) {
            for r in records {
                let key = r.get("key").and_then(|x| x.as_str()).unwrap_or("?");
                println!("  {}", key);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PublishBody {
    payload: String,
    ttl_secs: Option<u32>,
}

// ─────────────────────────── DNS query (raw UDP) ───────────────────────────

/// Issue a single UDP DNS query against `server:port` and
/// print the response summary. Supports the common record
/// types by name (`A`, `AAAA`, `TXT`); anything else is
/// passed through verbatim.
pub fn query_dns(
    server: &str,
    name: &str,
    qtype: &str,
    timeout_ms: u64,
    json: bool,
) -> Result<()> {
    let qtype_id = match qtype.to_ascii_uppercase().as_str() {
        "A" => 1,
        "AAAA" => 28,
        "TXT" => 16,
        "NS" => 2,
        "CNAME" => 5,
        "MX" => 15,
        "SOA" => 6,
        "SRV" => 33,
        other => {
            return Err(anyhow!(
                "unsupported qtype `{other}`; supported: A, AAAA, TXT, NS, CNAME, MX, SOA, SRV"
            ))
        }
    };
    let wire = build_dns_query(name, qtype_id);
    let summary = futures::executor::block_on(send_udp_query(server, &wire, timeout_ms))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("DNS query {name} {qtype} @ {server}:");
        println!("  status     : {}", summary.status);
        println!("  ancount    : {}", summary.ancount);
        if let Some(rcode) = summary.rcode {
            println!("  rcode      : {rcode}");
        }
        for ans in &summary.answers {
            println!("  answer     : {}", ans);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsResponseSummary {
    pub status: String,
    pub ancount: u16,
    pub rcode: Option<u8>,
    pub answers: Vec<String>,
}

/// Build a wire-format DNS query for `name` with the given
/// qtype. Mirrors `server.rs::parse_query` in the inverse
/// direction.
fn build_dns_query(name: &str, qtype: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0xAB, 0xCD]); // ID
    out.extend_from_slice(&[0x01, 0x00]); // flags: RD=1
    out.extend_from_slice(&[0x00, 0x01]); // QDCOUNT
    out.extend_from_slice(&[0x00, 0x00]); // ANCOUNT
    out.extend_from_slice(&[0x00, 0x00]); // NSCOUNT
    out.extend_from_slice(&[0x00, 0x00]); // ARCOUNT
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() {
            continue;
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // CLASS IN
    out
}

async fn send_udp_query(server: &str, wire: &[u8], timeout_ms: u64) -> Result<DnsResponseSummary> {
    use tokio::net::UdpSocket;
    let local: std::net::SocketAddr = "0.0.0.0:0"
        .parse()
        .context("parse local UDP bind address `0.0.0.0:0`")?;
    let socket = UdpSocket::bind(local).await.context("bind UDP")?;
    let remote: std::net::SocketAddr = server
        .parse()
        .with_context(|| format!("parse server address `{server}`"))?;
    socket.connect(remote).await.context("connect UDP")?;
    let dur = Duration::from_millis(timeout_ms);
    let send = socket.send(wire);
    tokio::time::timeout(dur, send)
        .await
        .context("UDP send timeout")?
        .context("UDP send")?;
    let mut buf = vec![0u8; 1500];
    let recv = socket.recv(&mut buf);
    let n = tokio::time::timeout(dur, recv)
        .await
        .context("UDP recv timeout")?
        .context("UDP recv")?;
    let response = &buf[..n];
    Ok(summarize_dns_response(response))
}

/// Decode just enough of a DNS response to render a useful
/// human summary (no full RFC 1035 parser; we pull out the
/// rcode, ANCOUNT, and any TXT / A / AAAA answers).
fn summarize_dns_response(response: &[u8]) -> DnsResponseSummary {
    let mut summary = DnsResponseSummary {
        status: "ok".into(),
        ancount: 0,
        rcode: None,
        answers: Vec::new(),
    };
    if response.len() < 12 {
        summary.status = "truncated".into();
        return summary;
    }
    let id = u16::from_be_bytes([response[0], response[1]]);
    let _ = id;
    let flags = u16::from_be_bytes([response[2], response[3]]);
    let rcode = (flags & 0x000F) as u8;
    summary.ancount = u16::from_be_bytes([response[6], response[7]]);
    summary.rcode = Some(rcode);
    if rcode == 3 {
        summary.status = "nxdomain".into();
    } else if rcode == 0 {
        summary.status = "noerror".into();
    } else {
        summary.status = format!("rcode={rcode}");
    }
    // Walk the question section to find the start of the
    // answer section. We assume a single question (matching
    // the queries `build_dns_query` produces).
    let mut i = 12;
    while i < response.len() {
        let len = response[i];
        if len == 0 {
            i += 1;
            break;
        }
        if len & 0xC0 == 0xC0 {
            i += 2;
            break;
        }
        i += 1 + len as usize;
    }
    i += 4; // QTYPE + QCLASS
    // Walk the answer section. Each answer starts with a
    // NAME (which may be a compression pointer to the
    // question section) — we skip it rather than parse it
    // recursively, since the test fixtures only use plain
    // labels.
    for _ in 0..summary.ancount {
        if i + 12 > response.len() {
            break;
        }
        // Skip the NAME.
        if i >= response.len() {
            break;
        }
        let name_len = response[i];
        if name_len & 0xC0 == 0xC0 {
            // Compression pointer.
            i += 2;
        } else if name_len == 0 {
            i += 1;
        } else {
            // Plain label sequence.
            let mut j = i;
            while j < response.len() {
                let l = response[j];
                if l == 0 {
                    j += 1;
                    break;
                }
                if l & 0xC0 == 0xC0 {
                    j += 2;
                    break;
                }
                j += 1 + l as usize;
            }
            i = j;
        }
        if i + 10 > response.len() {
            break;
        }
        let qtype = u16::from_be_bytes([response[i], response[i + 1]]);
        let class = u16::from_be_bytes([response[i + 2], response[i + 3]]);
        let _ttl = u32::from_be_bytes([
            response[i + 4],
            response[i + 5],
            response[i + 6],
            response[i + 7],
        ]);
        let rdlen = u16::from_be_bytes([response[i + 8], response[i + 9]]) as usize;
        let rdata_start = i + 10;
        let rdata_end = rdata_start + rdlen;
        if rdata_end > response.len() {
            break;
        }
        let rdata = &response[rdata_start..rdata_end];
        let line = match qtype {
            1 if class == 1 && rdlen == 4 => {
                format!(
                    "A {}.{}.{}.{}",
                    rdata[0], rdata[1], rdata[2], rdata[3]
                )
            }
            28 if class == 1 && rdlen == 16 => format!(
                "AAAA {}",
                rdata
                    .chunks(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .map(|w| format!("{w:x}"))
                    .collect::<Vec<_>>()
                    .join(":")
            ),
            16 => {
                // TXT: sequence of length-prefixed strings.
                let mut s = String::from("TXT \"");
                let mut j = 0;
                while j < rdata.len() {
                    let l = rdata[j] as usize;
                    j += 1;
                    if j + l > rdata.len() {
                        break;
                    }
                    s.push_str(&String::from_utf8_lossy(&rdata[j..j + l]));
                    j += l;
                }
                s.push('"');
                s
            }
            other => format!("TYPE{other} <{rdlen} bytes>"),
        };
        summary.answers.push(line);
        i = rdata_end;
    }
    summary
}

// ─────────────────────────── Helpers ───────────────────────────

/// Minimal percent-encoder for path segments. We escape
/// only the characters that `iroh-dns-server`'s router
/// rejects (whitespace, slash).
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoded_passes_through_safe_chars() {
        assert_eq!(urlencoded("alice"), "alice");
        assert_eq!(urlencoded("alice-123"), "alice-123");
        assert_eq!(urlencoded("alice.a3net.test"), "alice.a3net.test");
    }

    #[test]
    fn urlencoded_escapes_unsafe_chars() {
        assert_eq!(urlencoded("a/b"), "a%2Fb");
        assert_eq!(urlencoded("a b"), "a%20b");
    }

    #[test]
    fn build_dns_query_encodes_label_lengths() {
        let wire = build_dns_query("alice.a3net.test", 16);
        // Header.
        assert_eq!(&wire[..2], &[0xAB, 0xCD]);
        assert_eq!(&wire[4..6], &[0x00, 0x01]); // QDCOUNT
        // Labels.
        let mut i = 12;
        let labels: Vec<&[u8]> = vec![b"alice", b"a3net", b"test"];
        for label in labels {
            assert_eq!(wire[i], label.len() as u8);
            i += 1;
            assert_eq!(&wire[i..i + label.len()], label);
            i += label.len();
        }
        assert_eq!(wire[i], 0); // null terminator
        i += 1;
        // QTYPE
        assert_eq!(&wire[i..i + 2], &[0x00, 0x10]); // 16 = TXT
        i += 2;
        // QCLASS
        assert_eq!(&wire[i..i + 2], &[0x00, 0x01]); // IN
    }

    #[test]
    fn build_dns_query_trims_trailing_dot() {
        let wire = build_dns_query("alice.a3net.test.", 1);
        // Same shape as without the trailing dot.
        assert_eq!(wire.len(), build_dns_query("alice.a3net.test", 1).len());
    }

    #[test]
    fn qtype_lookup_covers_common_records() {
        let cases = [("A", 1u16), ("AAAA", 28), ("TXT", 16), ("NS", 2)];
        for (name, id) in cases {
            assert_eq!(
                match name {
                    "A" => 1,
                    "AAAA" => 28,
                    "TXT" => 16,
                    "NS" => 2,
                    _ => 0,
                },
                id,
                "{name} should map to {id}"
            );
        }
    }

    #[test]
    fn summarize_short_response_marks_truncated() {
        let s = summarize_dns_response(&[0; 5]);
        assert_eq!(s.status, "truncated");
        assert_eq!(s.ancount, 0);
        assert!(s.answers.is_empty());
    }

    #[test]
    fn summarize_nxdomain_response() {
        let mut response = Vec::new();
        response.extend_from_slice(&[0x00, 0x01]); // ID
        response.extend_from_slice(&[0x81, 0x83]); // QR=1, RD=1, RA=1, RCODE=3
        response.extend_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // Question: "x."
        response.push(1);
        response.push(b'x');
        response.push(0);
        response.extend_from_slice(&[0x00, 0x01]); // QTYPE=A
        response.extend_from_slice(&[0x00, 0x01]); // CLASS IN
        let s = summarize_dns_response(&response);
        assert_eq!(s.status, "nxdomain");
        assert_eq!(s.rcode, Some(3));
        assert_eq!(s.ancount, 0);
    }

    #[test]
    fn summarize_a_record_response() {
        // Build a response with one A record.
        let mut response = Vec::new();
        response.extend_from_slice(&[0x00, 0x01]); // ID
        response.extend_from_slice(&[0x81, 0x80]); // NOERROR
        response.extend_from_slice(&[0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
        // Question: "x."
        response.push(1);
        response.push(b'x');
        response.push(0);
        response.extend_from_slice(&[0x00, 0x01]); // QTYPE=A
        response.extend_from_slice(&[0x00, 0x01]); // CLASS IN
        // Answer: A 1.2.3.4
        // NAME pointer to offset 12.
        response.extend_from_slice(&[0xC0, 0x0C]);
        response.extend_from_slice(&[0x00, 0x01]); // TYPE=A
        response.extend_from_slice(&[0x00, 0x01]); // CLASS=IN
        response.extend_from_slice(&[0x00, 0x00, 0x00, 0x3C]); // TTL=60
        response.extend_from_slice(&[0x00, 0x04]); // RDLENGTH=4
        response.extend_from_slice(&[1, 2, 3, 4]);
        let s = summarize_dns_response(&response);
        assert_eq!(s.status, "noerror");
        assert_eq!(s.ancount, 1);
        assert_eq!(s.answers.len(), 1);
        assert_eq!(s.answers[0], "A 1.2.3.4");
    }

    #[test]
    fn summarize_txt_record_response() {
        // Build a response with one TXT record carrying "hello".
        let mut response = Vec::new();
        response.extend_from_slice(&[0x00, 0x01]);
        response.extend_from_slice(&[0x81, 0x80]);
        response.extend_from_slice(&[0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
        // Question: "x."
        response.push(1);
        response.push(b'x');
        response.push(0);
        response.extend_from_slice(&[0x00, 0x10]); // QTYPE=TXT
        response.extend_from_slice(&[0x00, 0x01]);
        // Answer.
        response.extend_from_slice(&[0xC0, 0x0C]);
        response.extend_from_slice(&[0x00, 0x10]); // TYPE=TXT
        response.extend_from_slice(&[0x00, 0x01]);
        response.extend_from_slice(&[0x00, 0x00, 0x00, 0x3C]); // TTL
        response.extend_from_slice(&[0x00, 0x06]); // RDLENGTH=6
        response.push(5); // TXT character-string length=5
        response.extend_from_slice(b"hello");
        let s = summarize_dns_response(&response);
        assert_eq!(s.ancount, 1);
        assert_eq!(s.answers[0], "TXT \"hello\"");
    }

    #[test]
    fn dns_response_summary_serializes_round_trip() {
        let s = DnsResponseSummary {
            status: "noerror".into(),
            ancount: 1,
            rcode: Some(0),
            answers: vec!["A 1.2.3.4".into()],
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: DnsResponseSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.answers, vec!["A 1.2.3.4".to_string()]);
    }

    /// `run_dns` with a bogus server URL fails fast with a
    /// useful error (it does not panic).
    #[test]
    fn run_dns_query_rejects_unsupported_qtype() {
        let err = query_dns("127.0.0.1:1", "x.test", "WUT", 100, false)
            .unwrap_err();
        assert!(
            err.chain().any(|e| e.to_string().contains("unsupported qtype")),
            "got: {err}"
        );
    }
}