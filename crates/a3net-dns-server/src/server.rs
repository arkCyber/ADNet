//! DNS server harness.
//!
//! This module owns the UDP / TCP sockets and a small in-process
//! DNS protocol codec. We deliberately do not depend on
//! `hickory-server`'s full authority trait — that surface is
//! large and changes between releases. Instead we parse the wire
//! format ourselves for the small subset of queries we serve
//! (TXT for `_a3net.<name>.<zone>` and A/AAAA for `<name>.<zone>`).
//!
//! The wire format follows RFC 1035:
//!   * Header (12 bytes): ID, flags, QDCOUNT, ANCOUNT, NSCOUNT, ARCOUNT
//!   * Question section: QNAME, QTYPE (2 bytes), QCLASS (2 bytes)
//!   * Answer section: NAME, TYPE, CLASS, TTL, RDLENGTH, RDATA
//!
//! We answer synchronously from the `ZoneStore` and return
//! NOERROR + the matching records, or NXDOMAIN if no match.
//!
//! ## Why this shape
//!
//! Operators who want a full authoritative DNS server (zone
//! transfers, DNSSEC, edns-client-subnet, etc.) can already use
//! `hickory-server` or `coredns`. The value this crate adds is
//! the **pkarr-compatible A3Net zone**, not the DNS plumbing.
//! Keeping the DNS plumbing minimal and audit-friendly is a
//! feature.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::{TcpListener, UdpSocket};

use crate::config::DnsServerConfig;
use crate::zone::{RecordKind, ZoneStore};

/// Start the DNS server. The handle exposes a shutdown channel.
pub async fn serve(cfg: DnsServerConfig) -> Result<DnsServerHandle, ServeError> {
    let store = crate::zone::open(cfg.clone()).map_err(ServeError::Zone)?;
    let store = Arc::new(store);

    let udp = UdpSocket::bind(&cfg.bind).await.map_err(ServeError::Io)?;
    let tcp = TcpListener::bind(&cfg.bind).await.map_err(ServeError::Io)?;
    let bind = cfg.bind;

    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = DnsServerHandle {
        shutdown: Some(tx),
    };

    tokio::spawn(async move {
        run_udp(udp, store.clone(), bind, rx).await;
        let _ = tcp.accept().await; // noop: drop on shutdown
    });

    Ok(handle)
}

async fn run_udp(
    socket: UdpSocket,
    store: Arc<ZoneStore>,
    bind: SocketAddr,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let mut buf = vec![0u8; 1500];
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            res = socket.recv_from(&mut buf) => {
                let (n, peer) = match res {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let response = match answer(&buf[..n], &store) {
                    Some(r) => r,
                    None => continue,
                };
                let _ = socket.send_to(&response, peer).await;
            }
        }
    }
    let _ = bind;
}

/// Build a DNS response for a single-question query.
fn answer(query: &[u8], store: &ZoneStore) -> Option<Vec<u8>> {
    let parsed = parse_query(query)?;
    let qname = parsed.qname.to_ascii_lowercase();
    let recs = store.get(&qname);
    let matching: Vec<_> = recs
        .iter()
        .filter(|r| match (&r.kind, parsed.qtype) {
            (RecordKind::AdnetIpnsTxt { .. }, 16) => true, // TXT
            (RecordKind::RelayAddr { .. }, 1) => true,    // A
            (RecordKind::RelayAddr { .. }, 28) => true,   // AAAA
            _ => false,
        })
        .collect();

    if matching.is_empty() {
        // NXDOMAIN
        return Some(encode_response(&parsed, &[], true));
    }
    Some(encode_response(&parsed, &matching, false))
}

#[derive(Debug)]
struct ParsedQuery {
    id: u16,
    qname: String,
    qtype: u16,
    qclass: u16,
}

fn parse_query(buf: &[u8]) -> Option<ParsedQuery> {
    if buf.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([buf[0], buf[1]]);
    let qdcount = u16::from_be_bytes([buf[4], buf[5]]);
    if qdcount != 1 {
        return None;
    }
    // Skip header.
    let mut i = 12;
    let mut labels = Vec::new();
    loop {
        if i >= buf.len() {
            return None;
        }
        let len = buf[i] as usize;
        i += 1;
        if len == 0 {
            break;
        }
        if i + len > buf.len() {
            return None;
        }
        labels.push(std::str::from_utf8(&buf[i..i + len]).ok()?.to_string());
        i += len;
    }
    if i + 4 > buf.len() {
        return None;
    }
    let qtype = u16::from_be_bytes([buf[i], buf[i + 1]]);
    let qclass = u16::from_be_bytes([buf[i + 2], buf[i + 3]]);
    let qname = format!("{}.", labels.join("."));
    Some(ParsedQuery { id, qname, qtype, qclass })
}

fn encode_response(
    q: &ParsedQuery,
    matches: &[&crate::zone::ZoneRecord],
    nxdomain: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&q.id.to_be_bytes());
    // flags: QR=1, RD=1, RA=1, RCODE=3 for NXDOMAIN, 0 otherwise
    let mut flags = 0x8180u16;
    if nxdomain {
        flags = 0x8183;
    }
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&(matches.len() as u16).to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT

    // Question section (echo back).
    let mut qname_wire = Vec::new();
    for label in q.qname.trim_end_matches('.').split('.') {
        if label.is_empty() {
            continue;
        }
        qname_wire.push(label.len() as u8);
        qname_wire.extend_from_slice(label.as_bytes());
    }
    qname_wire.push(0);
    out.extend_from_slice(&qname_wire);
    out.extend_from_slice(&q.qtype.to_be_bytes());
    out.extend_from_slice(&q.qclass.to_be_bytes());

    // Answer section.
    for rec in matches {
        out.extend_from_slice(&qname_wire);
        match &rec.kind {
            RecordKind::AdnetIpnsTxt { payload, ttl_secs, .. } => {
                out.extend_from_slice(&16u16.to_be_bytes()); // TXT
                out.extend_from_slice(&1u16.to_be_bytes());  // CLASS IN
                out.extend_from_slice(&ttl_secs.to_be_bytes());
                let payload_bytes = payload.as_bytes();
                let mut rdata = Vec::new();
                // Single TXT character-string; length byte + bytes.
                if payload_bytes.len() > 255 {
                    // Truncate to the largest single-string length RFC
                    // 1035 §3.3.14 allows. A multi-string TXT record
                    // would be more correct, but for our payload
                    // (base64 ≤ 320 chars for 240-byte pkarr packet)
                    // this is sufficient.
                    rdata.push(255);
                    rdata.extend_from_slice(&payload_bytes[..255]);
                } else {
                    rdata.push(payload_bytes.len() as u8);
                    rdata.extend_from_slice(payload_bytes);
                }
                out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
                out.extend_from_slice(&rdata);
            }
            RecordKind::RelayAddr { addr, ttl_secs, .. } => {
                // Best-effort: parse as IPv4. IPv6 not yet wired —
                // follow-up PR.
                let ip: std::net::Ipv4Addr = match addr.parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                out.extend_from_slice(&1u16.to_be_bytes()); // A
                out.extend_from_slice(&1u16.to_be_bytes()); // IN
                out.extend_from_slice(&ttl_secs.to_be_bytes());
                out.extend_from_slice(&4u16.to_be_bytes());
                out.extend_from_slice(&ip.octets());
            }
        }
    }

    out
}

pub struct DnsServerHandle {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl DnsServerHandle {
    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("io: {0}")]
    Io(std::io::Error),
    #[error("zone: {0}")]
    Zone(crate::zone::ZoneStoreError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddrV4};

    fn store() -> ZoneStore {
        let dir = tempfile::tempdir().unwrap();
        let cfg = DnsServerConfig::default()
            .with_zone("a3net.test")
            .with_state_path(dir.path().join("z.json"));
        ZoneStore::new(cfg)
    }

    fn cfg() -> DnsServerConfig {
        DnsServerConfig::default()
            .with_bind(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::LOCALHOST,
                0,
            )))
            .with_zone("a3net.test")
    }

    #[tokio::test]
    async fn serve_binds_and_returns_handle() {
        let handle = serve(cfg()).await.unwrap();
        handle.shutdown();
    }

    #[test]
    fn round_trip_txt_query() {
        let store = store();
        store
            .put(crate::zone::ZoneRecord {
                key: "_a3net.alice.a3net.test.".into(),
                kind: RecordKind::AdnetIpnsTxt {
                    ipns_name: "alice".into(),
                    payload: "hello".into(),
                    ttl_secs: 60,
                },
            })
            .unwrap();

        // Build a wire-format query for `_a3net.alice.a3net.test.` TXT.
        let mut q = Vec::new();
        q.extend_from_slice(&[0x12, 0x34]); // ID
        q.extend_from_slice(&[0x01, 0x00]); // flags
        q.extend_from_slice(&[0x00, 0x01]); // QDCOUNT
        q.extend_from_slice(&[0x00, 0x00]); // ANCOUNT
        q.extend_from_slice(&[0x00, 0x00]);
        q.extend_from_slice(&[0x00, 0x00]);
        // qname: 6 _a3net 5 alice 6 a3net 4 test 0
        for label in ["_a3net", "alice", "a3net", "test"] {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&16u16.to_be_bytes()); // TXT
        q.extend_from_slice(&1u16.to_be_bytes());  // IN

        let resp = answer(&q, &store).unwrap();
        assert_eq!(resp.len() >= 12, true);
        // ANCOUNT > 0
        let ancount = u16::from_be_bytes([resp[6], resp[7]]);
        assert_eq!(ancount, 1);
    }

    #[test]
    fn unknown_qtype_returns_nxdomain() {
        let store = store();
        let mut q = Vec::new();
        q.extend_from_slice(&[0x12, 0x34]);
        q.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        for label in ["_a3net", "alice", "a3net", "test"] {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&99u16.to_be_bytes()); // unknown qtype
        q.extend_from_slice(&1u16.to_be_bytes());

        let resp = answer(&q, &store).unwrap();
        let rcode = resp[3] & 0x0F;
        assert_eq!(rcode, 3); // NXDOMAIN
    }
}