//! HTTP admin API for the self-hosted DNS server.
//!
//! Three endpoints:
//!
//!   * `PUT /zones/<zone>/ipns/<ipns_name>`  — publish a record.
//!   * `GET  /zones/<zone>/ipns/<ipns_name>`  — fetch a record payload.
//!   * `GET  /zones/<zone>/records`           — list all records.
//!
//! The HTTP server runs on the same `bind` as the DNS listener by
//! default; in production it should bind a private address and use
//! TLS terminated by a reverse proxy. We provide a single binary
//! later that exposes both.

use std::net::SocketAddr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::config::DnsServerConfig;
use crate::zone::{RecordKind, ZoneRecord, ZoneStore, ZoneStoreError};

/// HTTP service bound to a `ZoneStore`.
#[derive(Clone)]
pub struct HttpApi {
    store: ZoneStore,
}

impl HttpApi {
    pub fn new(store: ZoneStore) -> Self {
        Self { store }
    }

    /// Construct from the binary's config.
    pub fn from_config(cfg: DnsServerConfig) -> Result<Self, ZoneStoreError> {
        let store = crate::zone::open(cfg)?;
        Ok(Self::new(store))
    }

    pub fn store(&self) -> &ZoneStore {
        &self.store
    }

    /// Handle `PUT /zones/<zone>/ipns/<ipns_name>`.
    pub fn publish(
        &self,
        ipns_name: &str,
        body: PublishBody,
    ) -> Result<ZoneRecord, ZoneStoreError> {
        let kind = RecordKind::AdnetIpnsTxt {
            ipns_name: ipns_name.to_string(),
            payload: body.payload,
            ttl_secs: body.ttl_secs.unwrap_or(3600),
        };
        let rec = ZoneRecord {
            key: self.store.ipns_txt_key(ipns_name),
            kind,
        };
        self.store.put(rec.clone())?;
        Ok(rec)
    }

    /// Handle `GET /zones/<zone>/ipns/<ipns_name>`.
    pub fn fetch(&self, ipns_name: &str) -> Option<ZoneRecord> {
        let key = self.store.ipns_txt_key(ipns_name);
        self.store.get(&key).into_iter().next()
    }

    /// Handle `GET /zones/<zone>/records`.
    pub fn list(&self) -> Vec<ZoneRecord> {
        self.store.all()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishBody {
    /// Base64-encoded pkarr packet body (matches the value at
    /// `_a3net.<ipns_name>.<zone>`).
    pub payload: String,
    pub ttl_secs: Option<u32>,
}

/// Bind a TCP listener and serve the HTTP admin API. Runs forever.
pub async fn serve_http(bind: SocketAddr, api: Arc<HttpApi>) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    loop {
        let (stream, _peer) = listener.accept().await?;
        let api = api.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_request(stream, api).await {
                tracing::debug!(error = %e, "http request failed");
            }
        });
    }
}

async fn handle_request(mut stream: TcpStream, api: Arc<HttpApi>) -> std::io::Result<()> {
    let (read_half, mut write_half) = stream.split();
    let mut reader = BufReader::new(read_half);

    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    // Skip headers.
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");
    let mut segs = path.split('/');
    // Drop leading empty segment from the leading slash.
    segs.next();

    let response = match (method, segs.next(), segs.next(), segs.next(), segs.next()) {
        ("GET", Some("zones"), _zone, Some("ipns"), Some(name)) => {
            match api.fetch(name) {
                Some(rec) => http_response(200, "application/json", &serde_json::to_string(&rec).unwrap_or_default()),
                None => http_response(404, "text/plain", "not found"),
            }
        }
        ("GET", Some("zones"), _zone, Some("records"), _) => {
            http_response(200, "application/json", &serde_json::to_string(&api.list()).unwrap_or_default())
        }
        ("PUT", Some("zones"), _zone, Some("ipns"), Some(name)) => {
            // Read body (rest of the buffer).
            let mut body = String::new();
            let _ = reader.read_to_string(&mut body).await;
            let parsed: PublishBody = match serde_json::from_str(body.trim()) {
                Ok(p) => p,
                Err(e) => {
                    return write_half
                        .write_all(http_response(400, "text/plain", &e.to_string()).as_bytes())
                        .await;
                }
            };
            match api.publish(name, parsed) {
                Ok(rec) => {
                    let body = serde_json::to_string(&rec).unwrap_or_default();
                    return write_half
                        .write_all(http_response(201, "application/json", &body).as_bytes())
                        .await;
                }
                Err(e) => {
                    return write_half
                        .write_all(http_response(500, "text/plain", &e.to_string()).as_bytes())
                        .await;
                }
            }
        }
        ("GET", Some("health"), _, _, _) => http_response(200, "text/plain", "ok"),
        _ => http_response(404, "text/plain", "not found"),
    };

    write_half.write_all(response.as_bytes()).await
}

use tokio::io::AsyncReadExt;

fn http_response(status: u16, content_type: &str, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let mut out = String::new();
    out.push_str(&format!("HTTP/1.1 {status} {reason}\r\n"));
    out.push_str(&format!("Content-Type: {content_type}\r\n"));
    out.push_str(&format!("Content-Length: {}\r\n", body.len()));
    out.push_str("Connection: close\r\n");
    out.push_str("\r\n");
    out.push_str(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api() -> (tempfile::TempDir, HttpApi) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = DnsServerConfig::default()
            .with_state_path(dir.path().join("zone.json"))
            .with_zone("a3net.test");
        (dir, HttpApi::from_config(cfg).unwrap())
    }

    #[test]
    fn publish_then_fetch_round_trip() {
        let (_dir, api) = api();
        let rec = api
            .publish(
                "alice",
                PublishBody {
                    payload: "AAECAwQFBg==".into(),
                    ttl_secs: Some(120),
                },
            )
            .unwrap();
        assert_eq!(
            rec.kind,
            RecordKind::AdnetIpnsTxt {
                ipns_name: "alice".into(),
                payload: "AAECAwQFBg==".into(),
                ttl_secs: 120,
            }
        );
        let got = api.fetch("alice").unwrap();
        assert_eq!(got, rec);
    }

    #[test]
    fn list_returns_all_records() {
        let (_dir, api) = api();
        api.publish("a", PublishBody { payload: "AAA=".into(), ttl_secs: None }).unwrap();
        api.publish("b", PublishBody { payload: "BBB=".into(), ttl_secs: None }).unwrap();
        assert_eq!(api.list().len(), 2);
    }

    #[test]
    fn fetch_unknown_returns_none() {
        let (_dir, api) = api();
        assert!(api.fetch("nobody").is_none());
    }

    #[test]
    fn http_response_emits_headers() {
        let r = http_response(200, "text/plain", "hi");
        assert!(r.starts_with("HTTP/1.1 200 OK"));
        assert!(r.contains("Content-Type: text/plain"));
        assert!(r.ends_with("hi"));
    }
}