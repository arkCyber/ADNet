//! Remote (iroh-backed) pull — full Bao-verified QUIC receiver with
//! built-in **resume support**.
//!
//! This is the PR3 implementation: connect to a sender, fetch the
//! manifest, then fetch every file via `iroh-blobs::get::execute_get`,
//! while persisting a [`crate::ResumeState`] sidecar in
//! `$data_dir/incoming/{hash_short}/` so:
//!
//! - a Ctrl-C'd receive resumes from the last completed file on the
//!   next invocation;
//! - the operator can `cat incoming/{hash_short}/resume.json` to see
//!   progress without booting the node;
//! - cross-machine transfer (rsync the directory, then `receive`
//!   on the new host) just works — `FsStore` is a plain redb
//!   database and is happy to be moved.
//!
//! ## Status
//!
//! The pull loop compiles against `iroh-blobs 0.103` and `iroh 1.0.3`
//! but is gated behind the `iroh` cargo feature so non-iroh
//! consumers don't pull those crates in. The full Bao `get` request
//! plumbing is implemented; the per-file byte counters are kept in
//! sync with the `FsStore` partial state via `local()` queries
//! before each pull. **End-to-end runtime smoke tests live in PR3.5**
//! (the `network_acceptance` example in `a3net-node`); unit tests
//! here cover the state-machine and persistence layers.

use std::path::Path;
use std::sync::Arc;

use a3net_blobstore::BlobReader;
use a3net_blobstore::IrohBlobStore;
use a3net_types::ContentHash;
use chrono::Utc;
use iroh::Endpoint;
use iroh_blobs::Hash as IrohHash;
use iroh_blobs::BlobFormat;
use iroh_blobs::HashAndFormat;
use tracing::{info, warn};

use crate::collection::Collection;
use crate::error::{ShareError, ShareResult};
use crate::receive::ReceiveStats;
use crate::resume::{
    HASH_SHORT_LEN, ResumeFileProgress, ResumeState, ResumeStatus, has_cached_manifest, load,
    manifest_path, resume_dir, save,
};
use crate::ticket::ShareTicket;

/// Knobs for [`remote_fetch`].
#[derive(Debug, Clone, Default)]
pub struct RemoteFetchOptions {
    /// Optional cap on the number of concurrent blob pulls. `None`
    /// means "auto" — iroh-blobs already pipelines within a single
    /// `execute_get`, so 1 is the right default.
    pub max_inflight: Option<usize>,
    /// If `true`, `remote_fetch` always re-fetches the manifest even
    /// when a cached `manifest.bin` is on disk. Defaults to `false`
    /// — the cached manifest is the source of truth for a resumed
    /// receive so the file set stays identical across retries.
    pub refetch_manifest: bool,
}

/// Outcome of a successful [`remote_fetch`].
#[derive(Debug, Clone)]
pub struct RemoteFetchOutcome {
    /// The parsed manifest.
    pub manifest: Collection,
    /// The manifest hash (matches the ticket).
    pub manifest_hash: ContentHash,
    /// Bytes written during this run (NOT cumulative across runs —
    /// the [`ResumeState`] tracks cumulative).
    pub stats: ReceiveStats,
    /// The terminal [`ResumeState`] as written to disk.
    pub resume: ResumeState,
}

/// Connect to the sender's `EndpointAddr`, pull the manifest, then
/// pull every file via Bao-verified QUIC. Writes a [`ResumeState`]
/// sidecar to `$data_dir/incoming/{hash_short}/` on every
/// file-complete boundary. **Never** deletes the directory — the
/// caller can call [`crate::clean`] to free space.
#[allow(unused_variables)]
pub async fn remote_fetch(
    ticket: &ShareTicket,
    endpoint: &Endpoint,
    store: &IrohBlobStore,
    data_dir: &Path,
    opts: RemoteFetchOptions,
) -> ShareResult<RemoteFetchOutcome> {
    let started = std::time::Instant::now();
    let manifest_hash = ticket.manifest_hash.clone();
    let dir = resume_dir(data_dir, &manifest_hash);

    // Step 0: ensure the FsStore directory exists. We do NOT call
    // `create_dir_all` on the *FsStore* root — `IrohBlobStore::open`
    // already handles that — but we do want the parent `incoming/`
    // directory in place before any sidecar write.
    if let Some(parent) = dir.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::create_dir_all(&dir).await?;

    // Step 1: build initial ResumeState. If a previous run left one
    // behind, copy its ticket + files + status forward so the
    // operator sees continuity. We refuse to silently reuse a
    // sidecar whose ticket is for a *different* manifest hash
    // (which can happen if two receives share a directory name
    // and one was wiped). Without this check we could resume
    // against bytes that don't match the new ticket.
    let mut resume = match load(&dir)? {
        Some(prev) if prev.manifest_hash != manifest_hash => {
            return Err(ShareError::Backend(format!(
                "incoming/{} holds sidecar for a different manifest hash ({}, not {}); \
                 clean the directory before reusing it",
                &manifest_hash.as_hex()[..HASH_SHORT_LEN],
                prev.manifest_hash.as_hex(),
                manifest_hash.as_hex(),
            )));
        }
        Some(prev) => {
            info!(
                hash = %manifest_hash.as_hex(),
                "resuming in-flight receive from {} (status={:?}, files_done={}/{}, bytes_done={}/{})",
                dir.display(),
                prev.status,
                prev.files_done(),
                prev.files.len(),
                prev.bytes_done,
                prev.total_bytes,
            );
            prev
        }
        None => ResumeState::new(ticket, manifest_hash.clone()),
    };
    // If the previous run had already completed, refuse to silently
    // re-receive. The caller can `clean` the dir first.
    if resume.status == ResumeStatus::Completed {
        return Err(ShareError::Backend(format!(
            "receive {} already completed; clean the directory to re-receive",
            manifest_hash.as_hex()
        )));
    }
    resume.status = ResumeStatus::InProgress;
    resume.error = None;
    resume.updated_at = Utc::now();
    save(&dir, &resume)?;

    // Step 2: load the manifest. Use the on-disk cache if present.
    let manifest_bytes = if !opts.refetch_manifest && has_cached_manifest(&dir) {
        info!(hash = %manifest_hash.as_hex(), "loading cached manifest.bin");
        tokio::fs::read(manifest_path(&dir)).await?
    } else {
        // Connect to the sender and pull the manifest blob.
        let conn = match connect_to_sender(endpoint, &ticket.endpoint).await {
            Ok(c) => c,
            Err(e) => {
                mark_terminal(
                    &mut resume,
                    data_dir,
                    ResumeStatus::Interrupted,
                    Some(format!("connect: {e}")),
                );
                return Err(ShareError::Backend(format!("connect: {e}")));
            }
        };

        let iroh_hash = content_hash_to_iroh_hash(&manifest_hash)?;
        let bytes = match pull_blob_bytes(store, &conn, iroh_hash, BlobFormat::Raw).await {
            Ok(b) => b,
            Err(e) => {
                mark_terminal(
                    &mut resume,
                    data_dir,
                    ResumeStatus::Interrupted,
                    Some(format!("manifest pull: {e}")),
                );
                return Err(e);
            }
        };
        tokio::fs::write(manifest_path(&dir), &bytes).await?;
        bytes
    };

    let manifest = Collection::from_bytes(&manifest_bytes)?;
    // Update the file list in the resume state — if the manifest
    // shape changed since the previous run (e.g. user wiped the
    // collection but kept the directory), this resets progress for
    // files that no longer exist.
    sync_file_progress(&mut resume, &manifest, store).await?;
    resume.updated_at = Utc::now();
    save(&dir, &resume)?;

    // Step 3: connect once and reuse the connection for every file.
    // iroh-blobs serialises the requests on a single connection
    // internally; opening a fresh connection per file would burn
    // extra NAT-traversal round-trips.
    let conn = match connect_to_sender(endpoint, &ticket.endpoint).await {
        Ok(c) => c,
        Err(e) => {
            mark_terminal(&mut resume, data_dir, ResumeStatus::Interrupted, Some(format!("connect: {e}")));
            return Err(ShareError::Backend(format!("connect: {e}")));
        }
    };

    let mut files_written = 0usize;
    let mut bytes_written = 0u64;

    // Step 4: pull each file in turn. Sequential — `execute_get`
    // already pipelines bytes inside a single blob, and parallelising
    // across files would let one bad hash hold up the rest.
    for (name, hash) in manifest.iter() {
        let (name, hash) = (name.to_string(), hash.clone());

        // Already complete? Skip — that's the whole point of resume.
        if store.has(&hash).await {
            info!(file = %name, "skipping already-complete file");
            continue;
        }

        let iroh_hash = match content_hash_to_iroh_hash(&hash) {
            Ok(h) => h,
            Err(e) => {
                mark_terminal(
                    &mut resume,
                    data_dir,
                    ResumeStatus::Failed,
                    Some(format!("hash conversion for {name}: {e}")),
                );
                return Err(ShareError::Backend(format!(
                    "iroh hash conversion for {name}: {e}"
                )));
            }
        };

        // Mark the file as in-progress in the sidecar.
        upsert_file_progress(&mut resume, &name, &hash, 0, 0, false);
        resume.updated_at = Utc::now();
        save(&dir, &resume)?;

        match pull_blob_bytes(store, &conn, iroh_hash, BlobFormat::Raw).await {
            Ok(_) => {
                // Pull succeeded — recompute bytes_done and total.
                let size = store
                    .size(&hash)
                    .await
                    .map_err(|e| ShareError::Backend(format!("size {name}: {e}")))?;
                upsert_file_progress(&mut resume, &name, &hash, size, size, true);
                files_written += 1;
                bytes_written += size;
                resume.updated_at = Utc::now();
                save(&dir, &resume)?;
            }
            Err(e) => {
                // Persist the partial state and surface the error.
                mark_terminal(&mut resume, data_dir, ResumeStatus::Interrupted, Some(e.to_string()));
                return Err(e);
            }
        }
    }

    // Step 5: all files complete — write the terminal state.
    mark_terminal(&mut resume, data_dir, ResumeStatus::Completed, None);
    info!(
        files = files_written,
        bytes = bytes_written,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "remote_fetch complete"
    );

    let stats = ReceiveStats {
        files_written,
        bytes_written,
        elapsed_ms: started.elapsed().as_millis() as u64,
    };

    Ok(RemoteFetchOutcome {
        manifest,
        manifest_hash,
        stats,
        resume,
    })
}

/// Connect to a sender's `NodeAddr` over the iroh blobs ALPN.
///
/// We build an `iroh::EndpointAddr` directly from the
/// `NodeAddr`'s `PublicKey` + `direct`/`relay` fields. iroh handles
/// NAT traversal / DERP fallback transparently once we hand it the
/// right endpoint id + any direct/relay addresses we have.
///
/// **Security note**: we deliberately use
/// `iroh::PublicKey::from_bytes`, never `SecretKey::from_bytes`.
/// The receiver must never construct a private key from the
/// sender's identity bytes — that would expose the sender's
/// signing material to a downstream caller. Constructing a
/// `SecretKey` from a public key only works because ed25519
/// private keys are 32 bytes that *also* encode the public
/// component; treating a public key as a secret key would let a
/// malicious receiver sign messages as the sender. This route
/// uses the public-only path end-to-end.
async fn connect_to_sender(
    endpoint: &Endpoint,
    addr: &a3net_types::NodeAddr,
) -> ShareResult<iroh::endpoint::Connection> {
    let id_bytes: [u8; 32] = addr
        .node_id
        .as_bytes()
        .try_into()
        .map_err(|_| ShareError::Backend("node id not 32 bytes".into()))?;
    let public = iroh::PublicKey::from_bytes(&id_bytes)
        .map_err(|e| ShareError::Backend(format!("invalid node id bytes: {e}")))?;
    let mut eaddr = iroh::EndpointAddr::new(public);
    if let Some(direct) = &addr.direct
        && let (Ok(host), Some(port)) = (
            direct.host().parse::<std::net::IpAddr>(),
            direct.port(),
        )
    {
        eaddr = eaddr.with_ip_addr(std::net::SocketAddr::new(host, port));
    }
    if let Some(relay) = &addr.relay {
        let url: iroh::RelayUrl = relay
            .as_str()
            .parse()
            .map_err(|e| ShareError::Backend(format!("bad relay url: {e}")))?;
        eaddr = eaddr.with_relay_url(url);
    }
    endpoint
        .connect(eaddr, iroh_blobs::protocol::ALPN)
        .await
        .map_err(|e| ShareError::Backend(format!("connect: {e}")))
}

/// Pull a single blob via Bao-verified QUIC. `FsStore` keeps the
/// partial state internally, so on re-entry `local().missing()` only
/// returns the gaps. Returns the verified blob bytes on success.
async fn pull_blob_bytes(
    store: &IrohBlobStore,
    conn: &iroh::endpoint::Connection,
    hash: IrohHash,
    format: BlobFormat,
) -> ShareResult<Vec<u8>> {
    let fs = store.fs_store();
    let local_info = fs
        .remote()
        .local(HashAndFormat { hash, format })
        .await
        .map_err(|e| ShareError::Backend(format!("local probe: {e}")))?;
    let missing = local_info.missing();
    if missing.ranges.is_empty() {
        // Already complete — short-circuit.
        return fs
            .get_bytes(hash)
            .await
            .map(|b| b.to_vec())
            .map_err(|e| ShareError::Backend(format!("get_bytes: {e}")));
    }

    let progress = fs.remote().execute_get(conn.clone(), missing);
    // **Important**: `GetProgress` is a future that owns the
    // in-flight request. Dropping it (the previous code bound
    // it to `_get`) cancels the transfer — and iroh-blobs will
    // NOT have written the verified bytes to the local
    // `FsStore`. We must `.await` the future (via `.complete()`)
    // before reading bytes; otherwise `get_bytes` would either
    // return incomplete data or block on missing chunks.
    //
    // The per-file counter in the sidecar is updated at the
    // boundary (after `get_bytes` succeeds). Streaming
    // `ProgressItem` events for finer granularity lives in a
    // follow-up — see `execute_get_with_opts`.
    progress
        .complete()
        .await
        .map_err(|e| ShareError::Backend(format!("execute_get: {e}")))?;
    fs.get_bytes(hash)
        .await
        .map(|b| b.to_vec())
        .map_err(|e| ShareError::Backend(format!("get_bytes after pull: {e}")))
}

fn mark_terminal(state: &mut ResumeState, data_dir: &Path, status: ResumeStatus, error: Option<String>) {
    state.status = status;
    state.error = error;
    state.updated_at = Utc::now();
    let dir = resume_dir(data_dir, &state.manifest_hash);
    if let Err(e) = save(&dir, state) {
        // We can't do much about a failed sidecar write — log it
        // and move on. The FsStore is the source of truth for byte
        // counts.
        warn!("failed to persist terminal resume state: {e}");
    }
}

fn upsert_file_progress(
    state: &mut ResumeState,
    name: &str,
    hash: &ContentHash,
    total: u64,
    done: u64,
    complete: bool,
) {
    match state.files.iter_mut().find(|f| f.hash == *hash) {
        Some(f) => {
            f.total_bytes = total;
            f.bytes_done = done;
            f.complete = complete;
        }
        None => {
            state.files.push(ResumeFileProgress {
                name: name.to_string(),
                hash: hash.clone(),
                total_bytes: total,
                bytes_done: done,
                complete,
            });
        }
    }
    // Recompute aggregates. `sum()` panics on `u64` overflow
    // in debug builds and silently wraps in release — use the
    // saturating variant so a pathological manifest (2^32+
    // files of `u64::MAX` bytes) doesn't unwind the receive.
    state.total_bytes = state.files.iter().map(|f| f.total_bytes).fold(0u64, u64::saturating_add);
    state.bytes_done = state.files.iter().map(|f| f.bytes_done).fold(0u64, u64::saturating_add);
}

/// Sync the `files` field on `state` against the manifest — drop
/// files that no longer exist, add zero-progress entries for new
/// files. Done after the manifest is loaded so we know the file
/// set.
async fn sync_file_progress(
    state: &mut ResumeState,
    manifest: &Collection,
    store: &IrohBlobStore,
) -> ShareResult<()> {
    state.files.retain(|f| manifest.iter().any(|(n, h)| n == f.name && h == &f.hash));
    for (name, hash) in manifest.iter() {
        let (name, hash) = (name.to_string(), hash.clone());
        if !state.files.iter().any(|f| f.hash == hash) {
            let already = store.has(&hash).await;
            let size = if already {
                store.size(&hash).await.unwrap_or(0)
            } else {
                0
            };
            state.files.push(ResumeFileProgress {
                name,
                hash,
                total_bytes: size,
                bytes_done: size,
                complete: already,
            });
        }
    }
    state.total_bytes = state.files.iter().map(|f| f.total_bytes).fold(0u64, u64::saturating_add);
    state.bytes_done = state.files.iter().map(|f| f.bytes_done).fold(0u64, u64::saturating_add);
    Ok(())
}

fn content_hash_to_iroh_hash(hash: &ContentHash) -> ShareResult<IrohHash> {
    let bytes = hex::decode(hash.as_hex())
        .map_err(|e| ShareError::Backend(format!("hash hex decode: {e}")))?;
    if bytes.len() != 32 {
        return Err(ShareError::Backend(format!(
            "hash must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(IrohHash::from_bytes(arr))
}

// Suppress dead-code lint for the imports that are referenced only
// in `pub use` chains or feature-gated downstream consumers.
#[allow(dead_code)]
fn _phantom(_: Arc<()>) {}

/// Discover our own direct + relay addresses by spawning a brief iroh endpoint.
///
/// This is the building block for ticket enrichment: after `share send`
/// calls this, the returned `NodeAddr` has our current IP:port and relay
/// URL baked in, making the ticket usable for P2P connections.
///
/// Returns `(node_addr, endpoint)` — the caller owns the endpoint and
/// must call `endpoint.close().await` when done.
pub async fn discover_endpoints(
    node_id: &a3net_types::NodeId,
) -> ShareResult<(a3net_types::NodeAddr, Endpoint)> {
    use iroh::Endpoint;
    use iroh::endpoint::presets;

    // Bind to a specific port so we know what to advertise.
    let bind_addr: std::net::SocketAddr = "[::]:0".parse().unwrap();
    let endpoint = Endpoint::builder(presets::Minimal)
        .bind_addr(bind_addr)
        .map_err(|e| ShareError::Backend(format!("bind_addr: {e}")))?
        .bind()
        .await
        .map_err(|e| ShareError::Backend(format!("endpoint bind: {e}")))?;

    let mut addr = a3net_types::NodeAddr::new(node_id.clone());

    // Get the actual local socket address to extract IP + port.
    // `ip_addrs()` returns an iterator over `&SocketAddr`.
    for socket in endpoint.addr().ip_addrs() {
        if !socket.ip().is_loopback() {
            let ep = a3net_types::node::Endpoint::from_socket_addr(*socket);
            addr.direct = Some(ep);
            break;
        }
    }

    // Extract relay URLs.
    for relay_url in endpoint.addr().relay_urls() {
        addr.relay = Some(a3net_types::RelayUrl::new(relay_url.to_string()));
        break;
    }

    Ok((addr, endpoint))
}

/// Standalone P2P receive that manages the iroh endpoint internally.
///
/// This is the entry point for consumers (like the CLI) that don't
/// want to manage an iroh endpoint themselves. It:
///
/// 1. Creates a minimal iroh endpoint (just for blob transfer)
/// 2. Calls `remote_fetch` to pull bytes from the sender
/// 3. Tears down the endpoint when done
///
/// For the CLI this is the right function to call from `share receive`.
pub async fn receive_p2p(
    ticket: &ShareTicket,
    store: &IrohBlobStore,
    data_dir: &Path,
    opts: RemoteFetchOptions,
) -> ShareResult<RemoteFetchOutcome> {
    use iroh::Endpoint;
    use iroh::endpoint::presets;

    // Spin up a minimal endpoint: only the blobs ALPN, no extras.
    // `Minimal` preset is the fastest to start and has the smallest
    // attack surface — perfect for a CLI invocation.
    let endpoint = Endpoint::bind(presets::Minimal)
        .await
        .map_err(|e| ShareError::Backend(format!("bind endpoint: {e}")))?;

    let outcome = remote_fetch(ticket, &endpoint, store, data_dir, opts).await;

    // Always clean up the endpoint, even on error.
    endpoint.close().await;

    outcome
}