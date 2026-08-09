//! Parallel chunk-aware fetcher for the mesh HTTP API.
//!
//! Mirrors what an iroh-blobs `Provide`-flavored peer would expose over HTTP.

use std::path::Path;

use adnet_blobstore::BlobStore;
use adnet_types::{ContentHash, RangeSpec};
use futures::future::join_all;
use reqwest::Client;

/// Result of a mesh fetch — includes which peer succeeded and bytes returned.
#[derive(Debug)]
pub struct MeshFetchResult {
    pub bytes: u64,
    pub peer: String,
}

/// Fetch a blob (or sub-range) from the first reachable peer.
///
/// `range` is applied as an HTTP `Range:` header. For [`RangeSpec::All`]
/// the server returns the full blob in `200 OK`; for sub-ranges we expect
/// `206 Partial Content`. Tickets come from
/// [`BlobTicket::http_base`](adnet_types::BlobTicket::http_base).
pub async fn fetch_from_mesh(
    store: &BlobStore,
    hash: &ContentHash,
    peer_bases: &[String],
    dest: &Path,
    range: RangeSpec,
) -> Result<MeshFetchResult, String> {
    if peer_bases.is_empty() {
        return Err("No peers available for mesh fetch".into());
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()
        .map_err(|e| e.to_string())?;

    let mut last_err = String::new();
    for base in peer_bases {
        match fetch_from_one(&client, store, hash, base, dest, &range).await {
            Ok(n) => {
                return Ok(MeshFetchResult {
                    bytes: n,
                    peer: base.clone(),
                })
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!("All mesh peers failed: {last_err}"))
}

async fn fetch_from_one(
    client: &Client,
    _store: &BlobStore,
    hash: &ContentHash,
    base: &str,
    dest: &Path,
    range: &RangeSpec,
) -> Result<u64, String> {
    let meta_url = format!("{base}/blobs/{}/meta", hash);
    let meta: serde_json::Value = client
        .get(&meta_url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let chunk_count = meta.get("chunkCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let total_size = meta.get("sizeBytes").and_then(|v| v.as_u64()).unwrap_or(0);

    // If the caller wants a sub-range, use the Range: header path. Skip the
    // chunked fan-out: a single HTTP request is simpler and uses fewer
    // connections.
    if !matches!(range, RangeSpec::All) {
        return fetch_with_range(client, base, hash, dest, range, total_size).await;
    }

    if chunk_count > 1 {
        return fetch_chunks_parallel(client.clone(), base, hash, dest, chunk_count).await;
    }

    let url = format!("{base}/blobs/{}", hash);
    let bytes = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(dest, &bytes).map_err(|e| e.to_string())?;

    let actual = ContentHash::from_bytes(&bytes);
    if &actual != hash {
        // Delete the corrupted file so callers can't accidentally treat it
        // as a valid local copy.
        let _ = std::fs::remove_file(dest);
        return Err(format!(
            "blob hash mismatch: expected={} got={}",
            hash, actual
        ));
    }
    Ok(bytes.len() as u64)
}

async fn fetch_with_range(
    client: &Client,
    base: &str,
    hash: &ContentHash,
    dest: &Path,
    range: &RangeSpec,
    total_size: u64,
) -> Result<u64, String> {
    let url = format!("{base}/blobs/{}", hash);
    let header_value = match range.to_http_header() {
        Some(h) => h,
        None => return Err("RangeSpec::All handled before fetch_with_range".into()),
    };
    let mut req = client.get(&url);
    // Tell reqwest it's OK to receive 206.
    req = req.header(reqwest::header::RANGE, header_value);
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("range fetch returned HTTP {status}"));
    }
    // Detect multipart/byteranges and strip the framing so the destination
    // file contains exactly the requested bytes.
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = if ctype.starts_with("multipart/byteranges") {
        // Extract boundary.
        let boundary = ctype
            .split(';')
            .find_map(|p| p.trim().strip_prefix("boundary="))
            .ok_or_else(|| "missing multipart boundary".to_string())?
            .trim_matches('"');
        let raw = resp.bytes().await.map_err(|e| e.to_string())?;
        extract_multipart_bodies(&raw, boundary)
    } else {
        resp.bytes().await.map_err(|e| e.to_string())?.to_vec()
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(dest, &bytes).map_err(|e| e.to_string())?;
    // Verify the requested size against `total_size` returned in meta.
    let expected = match range {
        RangeSpec::All => total_size,
        RangeSpec::Single(r) => r.end.saturating_sub(r.start),
        RangeSpec::Multi(rs) => rs.iter().map(|r| r.end.saturating_sub(r.start)).sum(),
    };
    if bytes.len() as u64 != expected {
        return Err(format!(
            "range fetch size mismatch: expected {expected}, got {}",
            bytes.len()
        ));
    }
    Ok(bytes.len() as u64)
}

/// Concatenate the body sections of a `multipart/byteranges` response.
fn extract_multipart_bodies(raw: &[u8], boundary: &str) -> Vec<u8> {
    let delim = format!("--{boundary}");
    let mut out = Vec::new();
    // Split on the boundary delimiter; each chunk contains a part header
    // block followed by `\r\n\r\n<body>\r\n`.
    let mut rest = raw;
    while let Some(idx) = find_subslice(rest, delim.as_bytes()) {
        // Skip the delimiter line + CRLF
        let after_delim = &rest[idx + delim.len()..];
        let after_crlf = after_delim.strip_prefix(b"\r\n").unwrap_or(after_delim);
        // Find the blank line that ends the part headers.
        let body_start = match find_subslice(after_crlf, b"\r\n\r\n") {
            Some(p) => p + 4,
            None => break,
        };
        let from_body = &after_crlf[body_start..];
        // Body ends at the next CRLF preceding the next boundary (or end).
        let body_end = from_body
            .windows(2)
            .position(|w| w == b"\r\n")
            .unwrap_or(from_body.len());
        out.extend_from_slice(&from_body[..body_end]);
        // Advance to the next delimiter.
        let consumed = idx + delim.len() + 2 /* \r\n after delim */
            + body_start
            + body_end;
        if consumed >= rest.len() {
            break;
        }
        rest = &rest[consumed..];
        if rest.starts_with(b"--") {
            // closing boundary "--<boundary>--"
            break;
        }
    }
    out
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

async fn fetch_chunks_parallel(
    client: Client,
    base: &str,
    hash: &ContentHash,
    dest: &Path,
    chunk_count: u32,
) -> Result<u64, String> {
    let fetches: Vec<_> = (0..chunk_count)
        .map(|index| {
            let client = client.clone();
            let url = format!("{base}/blobs/{}/chunks/{index:06}", hash);
            async move {
                client
                    .get(&url)
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .map_err(|e| e.to_string())?
                    .bytes()
                    .await
                    .map(|b| b.to_vec())
                    .map_err(|e| e.to_string())
            }
        })
        .collect();

    let parts = join_all(fetches).await;
    for part in &parts {
        if let Err(e) = part {
            return Err(e.clone());
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut total = 0u64;
    for part in parts {
        let chunk = part?;
        use std::io::Write;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        total += chunk.len() as u64;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::MeshServer;
    use adnet_blobstore::chunked::CHUNK_SIZE;
    use std::io::Write;
    use std::sync::Arc;

    #[tokio::test]
    async fn parallel_chunk_fetch_from_mesh() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let src = dir.path().join("big.bin");
        let payload: Vec<u8> = (0..50_000).map(|i| (i % 256) as u8).collect();
        {
            let mut f = std::fs::File::create(&src).unwrap();
            f.write_all(&payload).unwrap();
        }
        let (hash, _) = store.import_file_sync(&src).unwrap();

        let mesh = MeshServer::start(Arc::clone(&store)).await.unwrap();
        let base = format!("http://127.0.0.1:{}", mesh.port);
        let dest = dir.path().join("fetched.bin");
        let res = fetch_from_mesh(&store, &hash, &[base], &dest, adnet_types::RangeSpec::All)
            .await
            .unwrap();
        assert_eq!(res.bytes, payload.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        mesh.shutdown();
    }

    #[tokio::test]
    async fn rejects_empty_peers() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let dest = dir.path().join("x.bin");
        let err = fetch_from_mesh(
            &store,
            &ContentHash::from_bytes(b"x"),
            &[],
            &dest,
            adnet_types::RangeSpec::All,
        )
        .await
        .unwrap_err();
        assert!(err.contains("No peers"));
    }

    #[tokio::test]
    async fn range_fetch_single() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let src = dir.path().join("data.bin");
        let payload: Vec<u8> = (0..(CHUNK_SIZE * 2 + 200))
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&src, &payload).unwrap();
        let (hash, _) = store.import_file_sync(&src).unwrap();

        let mesh = MeshServer::start(Arc::clone(&store)).await.unwrap();
        let base = format!("http://127.0.0.1:{}", mesh.port);
        let dest = dir.path().join("partial.bin");
        let res = fetch_from_mesh(
            &store,
            &hash,
            std::slice::from_ref(&base),
            &dest,
            RangeSpec::single(100, 500).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(res.bytes, 400);
        let bytes = std::fs::read(&dest).unwrap();
        assert_eq!(bytes, &payload[100..500]);

        // Also verify multi-range on the client side works.
        let dest2 = dir.path().join("multi.bin");
        let res = fetch_from_mesh(
            &store,
            &hash,
            &[base],
            &dest2,
            RangeSpec::Multi(vec![
                adnet_types::ByteRange::new(0, 50).unwrap(),
                adnet_types::ByteRange::new(payload.len() as u64 - 50, payload.len() as u64)
                    .unwrap(),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(res.bytes, 100);
        let body = std::fs::read(&dest2).unwrap();
        assert!(body.starts_with(&payload[..50]));
        assert!(body.ends_with(&payload[payload.len() - 50..]));

        mesh.shutdown();
    }
}
