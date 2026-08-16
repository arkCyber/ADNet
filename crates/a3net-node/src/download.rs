//! Download orchestration — locate peers + fetch a blob via transport or
//! mesh fallback.

use std::path::Path;
use std::sync::Arc;

use a3net_blobstore::BlobStore;
use a3net_mesh::{MeshFetchResult, fetch_from_mesh};
use a3net_transport::{Frame, TransportError, fetch_blob_over_transport};
use a3net_types::{BlobTicket, ContentHash, NodeAddr, RangeSpec};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Async job descriptor returned to the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadJob {
    pub hash: ContentHash,
    pub title: String,
    pub status: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// Per-chunk progress event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub hash: ContentHash,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// Try fetching `hash` from any of the given peers, preferring the QUIC
/// transport when configured and falling back to the mesh HTTP API.
///
/// `range` selects a sub-range of the blob (`RangeSpec::All` for whole
/// blob). Each peer's ticket may also carry its own range; we pass the
/// caller's `range` through.
pub async fn fetch_blob(
    store: Arc<BlobStore>,
    hash: &ContentHash,
    title: &str,
    peers: &[BlobTicket],
    dest: &Path,
    primary: Option<a3net_transport::SharedTransport>,
    range: RangeSpec,
) -> anyhow::Result<DownloadJob> {
    if peers.is_empty() {
        anyhow::bail!("no peers available for hash={}", hash);
    }
    // Try primary transport first.
    if let Some(transport) = primary {
        match try_transport(&transport, hash, dest, range.clone(), peers).await {
            Ok(n) => {
                return Ok(DownloadJob {
                    hash: hash.clone(),
                    title: title.to_string(),
                    status: "ok".into(),
                    bytes_done: n,
                    bytes_total: n,
                });
            }
            Err(e) => warn!("transport fetch failed: {e}; falling back to mesh"),
        }
    }
    // Mesh fallback. Each peer's ticket can override the caller-supplied
    // range; the caller's range wins on the first ticket to keep the API
    // predictable.
    let peer_bases: Vec<String> = peers.iter().filter_map(|p| p.http_base()).collect();
    let effective_range = match &peers[0].range {
        RangeSpec::All => range,
        r => r.clone(),
    };
    let res: MeshFetchResult = fetch_from_mesh(&store, hash, &peer_bases, dest, effective_range)
        .await
        .map_err(|e| anyhow::anyhow!("mesh fetch: {e}"))?;
    info!(
        "[a3net] mesh fetch ok from {} ({} bytes)",
        res.peer, res.bytes
    );
    Ok(DownloadJob {
        hash: hash.clone(),
        title: title.to_string(),
        status: "ok".into(),
        bytes_done: res.bytes,
        bytes_total: res.bytes,
    })
}

/// Best-effort transport fetch. We try each peer's ticket in order;
/// the first peer we successfully dial wins. Each ticket carries its
/// own `NodeAddr` (with a direct endpoint), so we use `dial_addr`
/// rather than the registry-based `dial`.
async fn try_transport(
    transport: &a3net_transport::SharedTransport,
    hash: &ContentHash,
    dest: &Path,
    range: RangeSpec,
    peers: &[BlobTicket],
) -> anyhow::Result<u64> {
    let mut last_err: Option<anyhow::Error> = None;
    for ticket in peers {
        match try_transport_one(transport, hash, dest, range.clone(), &ticket.endpoint).await {
            Ok(n) => return Ok(n),
            Err(e) => {
                warn!("transport fetch via {} failed: {e}", ticket.endpoint);
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        anyhow::anyhow!(
            "QUIC transport: no peer tickets carried a routable NodeAddr ({} peer(s) checked)",
            peers.len()
        )
    }))
}

async fn try_transport_one(
    transport: &a3net_transport::SharedTransport,
    hash: &ContentHash,
    dest: &Path,
    range: RangeSpec,
    addr: &NodeAddr,
) -> anyhow::Result<u64> {
    if addr.direct.is_none() {
        anyhow::bail!("ticket has no direct endpoint; QUIC dial needs host:port");
    }
    let mut conn = transport
        .dial_addr(addr.clone())
        .await
        .map_err(|e| anyhow::anyhow!("dial_addr: {e}"))?;
    let bytes = fetch_blob_over_transport(&mut conn, hash, &range, dest)
        .await
        .map_err(|e| anyhow::anyhow!("blob_over_quic: {e}"))?;
    Ok(bytes)
}

/// Strip a `Frame` to a single byte payload (used by future blob-over-QUIC
/// transfer — exposed for downstream callers that want to start writing
/// the protocol).
#[allow(dead_code)]
pub fn frame_payload(frame: &Frame) -> Result<&[u8], TransportError> {
    Ok(frame.as_bytes())
}
