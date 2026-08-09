//! `MeshServer` — a minimal `tokio`-based HTTP server exposing a
//! [`BlobStore`](adnet_blobstore::BlobStore).
//!
//! Supports byte-range requests via the standard HTTP `Range:` header,
//! matching what iroh-blobs' HTTP mesh fallback would offer. Routes:
//!
//! ```text
//! GET /health
//! GET /blobs/<hash>                — full blob (or 200 with all bytes)
//! GET /blobs/<hash>/meta           — JSON { hash, sizeBytes, chunkCount }
//! GET /blobs/<hash>/chunks/<index> — raw 16 KiB chunk
//! ```

use std::sync::Arc;

use adnet_blobstore::BlobStore;
use adnet_types::{ByteRange, ContentHash, RangeSpec};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{info, warn};

/// Running server handle. Drop to stop.
pub struct MeshServerHandle {
    pub port: u16,
    pub host: String,
    shutdown_tx: watch::Sender<bool>,
}

impl MeshServerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Builder / starter for the mesh server.
pub struct MeshServer;

impl MeshServer {
    /// Bind `0.0.0.0:0` and serve `store`'s blobs over HTTP.
    pub async fn start(store: Arc<BlobStore>) -> Result<MeshServerHandle, String> {
        let listener = TcpListener::bind("0.0.0.0:0")
            .await
            .map_err(|e| format!("mesh bind failed: {e}"))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let port = addr.port();
        let host = local_lan_ip().unwrap_or_else(|| "127.0.0.1".to_string());

        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let store_for_task = Arc::clone(&store);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accept = listener.accept() => {
                        match accept {
                            Ok((mut stream, _)) => {
                                let store = Arc::clone(&store_for_task);
                                tokio::spawn(async move {
                                    let _ = handle_connection(&mut stream, &store).await;
                                });
                            }
                            Err(e) => warn!("mesh accept error: {e}"),
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() { break; }
                    }
                }
            }
        });

        info!("ADNet mesh listening on {host}:{port}");
        Ok(MeshServerHandle {
            port,
            host,
            shutdown_tx,
        })
    }
}

/// Parsed minimal HTTP request.
struct ParsedRequest {
    method: String,
    path: String,
    range_header: Option<String>,
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Result<ParsedRequest, String> {
    // Read header bytes (up to ~8KiB) — enough for our small requests.
    let mut buf = vec![0u8; 8192];
    let mut total = 0;
    loop {
        let n = stream
            .read(&mut buf[total..])
            .await
            .map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n;
        if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if total >= buf.len() {
            // Fall through and parse what we have; oversized headers will be rejected.
            break;
        }
    }
    let header_block = String::from_utf8_lossy(&buf[..total]);
    let mut lines = header_block.lines();
    let request_line = lines.next().unwrap_or("");
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err("malformed request line".into());
    }
    let mut range_header = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Range:") {
            range_header = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("range:") {
            range_header = Some(rest.trim().to_string());
        }
    }
    Ok(ParsedRequest {
        method: parts[0].to_string(),
        path: parts[1].to_string(),
        range_header,
    })
}

async fn handle_connection(
    stream: &mut tokio::net::TcpStream,
    store: &BlobStore,
) -> Result<(), String> {
    let req = match read_request(stream).await {
        Ok(r) => r,
        Err(_) => {
            let _ = write_response(stream, 400, "text/plain", b"bad request").await;
            return Ok(());
        }
    };
    // Reject anything that isn't GET/HEAD. This prevents POST/PUT
    // writes to the store via the mesh interface and matches what
    // iroh-blobs's HTTP fallback expects.
    if !matches!(req.method.as_str(), "GET" | "HEAD") {
        let _ = write_response(stream, 405, "text/plain", b"method not allowed").await;
        return Ok(());
    }

    if req.path == "/health" {
        return write_response(stream, 200, "text/plain", b"ok").await;
    }

    if let Some(rest) = req.path.strip_prefix("/blobs/") {
        let rest = rest.split('?').next().unwrap_or(rest);
        if let Some((hash, tail)) = rest.split_once("/chunks/")
            && let Ok(index) = tail.parse::<u32>()
        {
            return serve_chunk(stream, store, hash, index).await;
        }
        if let Some(hash) = rest.strip_suffix("/meta") {
            return serve_meta(stream, store, hash).await;
        }
        return serve_full_blob(stream, store, rest, req.range_header.as_deref()).await;
    }

    write_response(stream, 404, "text/plain", b"not found").await
}

async fn serve_meta(
    stream: &mut tokio::net::TcpStream,
    store: &BlobStore,
    hash_hex: &str,
) -> Result<(), String> {
    let hash = match ContentHash::from_hex(hash_hex) {
        Ok(h) => h,
        Err(_) => return write_response(stream, 400, "text/plain", b"bad hash").await,
    };
    let (size, chunks) = match store.meta(&hash) {
        Ok(m) => m,
        Err(_) => return write_response(stream, 404, "text/plain", b"blob not found").await,
    };
    let body = serde_json::json!({
        "hash": hash.as_hex(),
        "sizeBytes": size,
        "chunkCount": chunks,
    });
    let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
    write_response(stream, 200, "application/json", &bytes).await
}

async fn serve_chunk(
    stream: &mut tokio::net::TcpStream,
    store: &BlobStore,
    hash_hex: &str,
    index: u32,
) -> Result<(), String> {
    let hash = match ContentHash::from_hex(hash_hex) {
        Ok(h) => h,
        Err(_) => return write_response(stream, 400, "text/plain", b"bad hash").await,
    };
    match store.read_chunk_sync(&hash, index) {
        Ok(bytes) => write_response(stream, 200, "application/octet-stream", &bytes).await,
        Err(_) => write_response(stream, 404, "text/plain", b"chunk not found").await,
    }
}

async fn serve_full_blob(
    stream: &mut tokio::net::TcpStream,
    store: &BlobStore,
    hash_hex: &str,
    range_header: Option<&str>,
) -> Result<(), String> {
    let hash = match ContentHash::from_hex(hash_hex) {
        Ok(h) => h,
        Err(_) => return write_response(stream, 400, "text/plain", b"bad hash").await,
    };
    if !store.has_complete(&hash) {
        return write_response(stream, 404, "text/plain", b"blob not found").await;
    }
    let (size, _count) = store.meta(&hash).map_err(|e| e.to_string())?;

    // No Range header → 200 with whole body.
    let Some(header) = range_header else {
        let body = store
            .read_range_sync(&hash, &ByteRange::new(0, size).unwrap())
            .map_err(|e| e.to_string())?;
        return write_response_with_extra(stream, 200, "application/octet-stream", &body, &[])
            .await;
    };

    // With Range header → 206 Partial Content (multi or single).
    let spec = match RangeSpec::from_http_header(header, size) {
        Ok(s) => s,
        Err(_) => return write_response(stream, 416, "text/plain", b"range not satisfiable").await,
    };
    match spec {
        RangeSpec::Single(r) => {
            let body = store
                .read_range_sync(&hash, &r)
                .map_err(|e| e.to_string())?;
            let extra = [format!(
                "Content-Range: bytes {}-{}/{}",
                r.start,
                r.end - 1,
                size
            )];
            write_response_with_extra(stream, 206, "application/octet-stream", &body, &extra).await
        }
        RangeSpec::Multi(rs) => {
            // For multi-range we emit multipart/byteranges, mirroring nginx/curl behavior.
            let boundary = format!("----ADNET-{x}", x = rand::random::<u64>());
            let mut out: Vec<u8> = Vec::new();
            for r in &rs {
                let head = format!(
                    "--{b}\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes {s}-{e}/{t}\r\n\r\n",
                    b = boundary,
                    s = r.start,
                    e = r.end - 1,
                    t = size,
                );
                out.extend_from_slice(head.as_bytes());
                let body = store.read_range_sync(&hash, r).map_err(|e| e.to_string())?;
                out.extend_from_slice(&body);
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            let ctype = format!("multipart/byteranges; boundary={boundary}");
            let extra = [format!("Content-Length: {}", out.len())];
            write_response_with_extra(stream, 206, &ctype, &out, &extra).await
        }
        RangeSpec::All => unreachable!(),
    }
}

async fn write_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), String> {
    write_response_with_extra(stream, status, content_type, body, &[]).await
}

async fn write_response_with_extra(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra_headers: &[String],
) -> Result<(), String> {
    let status_text = match status {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        _ => "Error",
    };
    let extras = if extra_headers.is_empty() {
        String::new()
    } else {
        format!("{}\r\n", extra_headers.join("\r\n"))
    };
    let header = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extras}Connection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(body).await.map_err(|e| e.to_string())?;
    let _ = stream.shutdown().await;
    Ok(())
}

/// Best-effort LAN address for peer tickets. Falls back to `127.0.0.1`.
pub fn local_lan_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let local = socket.local_addr().ok()?;
    Some(local.ip().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_blobstore::chunked::CHUNK_SIZE;
    use std::io::Write;

    #[tokio::test]
    async fn mesh_serves_health_meta_and_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let src = dir.path().join("data.bin");
        {
            let mut f = std::fs::File::create(&src).unwrap();
            f.write_all(b"mesh-payload").unwrap();
        }
        let (hash, _) = store.import_file_sync(&src).unwrap();
        let mesh = MeshServer::start(Arc::clone(&store)).await.unwrap();

        let client = reqwest::Client::new();
        let health = client
            .get(format!("http://127.0.0.1:{}/health", mesh.port))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(health, "ok");

        let meta_url = format!("http://127.0.0.1:{}/blobs/{}/meta", mesh.port, hash);
        let meta: serde_json::Value = client
            .get(&meta_url)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(meta["hash"].as_str().unwrap(), hash.as_hex());

        let blob_url = format!("http://127.0.0.1:{}/blobs/{}", mesh.port, hash);
        let bytes = client
            .get(&blob_url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), b"mesh-payload");

        mesh.shutdown();
    }

    #[tokio::test]
    async fn mesh_serves_range_requests() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let src = dir.path().join("data.bin");
        let payload: Vec<u8> = (0..(CHUNK_SIZE * 2 + 200))
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&src, &payload).unwrap();
        let (hash, _) = store.import_file_sync(&src).unwrap();
        let mesh = MeshServer::start(Arc::clone(&store)).await.unwrap();

        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/blobs/{}", mesh.port, hash);

        // Single cross-chunk range
        let resp = client
            .get(&url)
            .header("Range", "bytes=100-300")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 206);
        let bytes = resp.bytes().await.unwrap();
        assert_eq!(bytes.len(), 201);
        assert_eq!(bytes.as_ref(), &payload[100..301]);

        // Multi-range
        let resp = client
            .get(&url)
            .header("Range", "bytes=0-49,100-149")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 206);
        let ctype = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ctype.starts_with("multipart/byteranges"));
        let body = resp.bytes().await.unwrap();
        // Body is multipart, verify the ctype looks right and both sub-ranges appear.
        assert!(body.windows(50).any(|w| *w == payload[0..50]));
        assert!(body.windows(50).any(|w| *w == payload[100..150]));

        // Suffix range
        let resp = client
            .get(&url)
            .header("Range", "bytes=-50")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 206);
        let bytes = resp.bytes().await.unwrap();
        assert_eq!(bytes.len(), 50);
        assert_eq!(bytes.as_ref(), &payload[payload.len() - 50..]);

        mesh.shutdown();
    }

    /// POST / PUT must be rejected with 405 — the mesh is read-only.
    /// We verify this end-to-end through `reqwest` so the response
    /// status line and the status code both flow through.
    #[tokio::test]
    async fn mesh_rejects_non_get_methods() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let mesh = MeshServer::start(Arc::clone(&store)).await.unwrap();
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/health", mesh.port);
        let resp = client.post(&url).body("not allowed").send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 405);
        let resp = client
            .request(reqwest::Method::DELETE, &url)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 405);
        mesh.shutdown();
    }
}
