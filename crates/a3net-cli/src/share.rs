//! `a3net share …` — P2P file & directory sharing CLI surface.
//!
//! Subcommands:
//! - `a3net share send <path>` — walk a local file or directory,
//!   ingest it into the local blob store, and print a printable
//!   `ShareTicket` that a peer can hand to `a3net share receive`.
//! - `a3net share receive <ticket>` — materialise a shared
//!   manifest into a local output directory, write a
//!   `resume.json` sidecar under `<data-dir>/incoming/<hash>/`
//!   so the transfer can be resumed / inspected / cleaned.
//! - `a3net share resume ls|clean|info|continue` — operate on
//!   the persistent resume state without booting the full
//!   node. The `ls` / `info` paths are pure read; `clean`
//!   deletes a terminal state; `continue` re-runs an
//!   interrupted receive from the sidecar's stored ticket.
//!
//! ## Dispatch model
//!
//! `share` is an **offline** command family in the same spirit as
//! `a3net config` / `a3net roster` / `a3net user` — the
//! `run_share_subcmd` dispatcher in `main.rs` fires **before**
//! the `Node` is constructed, so `a3net share send` works in
//! deployment scripts and CI smoke tests without spinning up
//! the iroh endpoint, the mesh server, or the gossip overlay.
//!
//! `share receive` uses a **two-tier strategy**:
//!   1. **Local-first**: if the manifest blob is already in the
//!      local blob store (same machine as sender, or cached),
//!      use the local `receive()` path — no network needed.
//!   2. **P2P pull**: if the manifest is not local, spin up a
//!      lightweight `iroh::Endpoint` and call `remote_fetch()`
//!      to pull bytes from the sender over Bao-verified QUIC.
//!      The endpoint is torn down when `remote_fetch` returns.
//!
//!      This means `share receive` works across machines with no
//!      pre-existing state — the sender only needs to have run
//!      `share send` to make their blobs available.
//!
//! ## Sync vs async
//!
//! `walk_import` is async (it spawns blocking work via
//! `tokio::task::spawn_blocking` + `buffer_unordered`), and
//! `receive` is async (it does `tokio::fs::*`). We drive both
//! via `futures::executor::block_on` at the boundary in
//! `main.rs::run_share_subcmd_blocking` (the CLI runs under a
//! `current_thread` tokio runtime).
//!
//! ## Persistence
//!
//! Send-side state lives in `<data-dir>/blobs/` (the existing
//! `BlobStore` directory). Receive-side state lives in
//! `<data-dir>/incoming/<hash_short>/`, sidecar'd by
//! `resume.json` + `manifest.bin` (the `a3net_share::resume`
//! module). Both sidecars survive across `a3net` invocations and
//! across machines (rsync-friendly).

use std::io::Read;
use std::path::{Path, PathBuf};

use a3net_blobstore::{BlobReader, BlobStore};
#[cfg(feature = "iroh")]
use a3net_share::{
    PutBytesFn, ReceiveOptions, RemoteFetchOptions, ShareTicket, WalkOptions,
    walk_import,
};
#[cfg(not(feature = "iroh"))]
use a3net_share::{
    PutBytesFn, ReceiveOptions, ShareTicket, WalkOptions,
    walk_import,
};
use a3net_types::ContentHash;
use anyhow::{Context, Result, anyhow, bail};
use tracing::{info, warn};

#[cfg(feature = "iroh")]
use {a3net_share::receive_p2p, a3net_blobstore::IrohBlobStore};

use crate::cli::{ShareCmd, ShareResumeCmd};

// ── top-level dispatcher ──────────────────────────────────────────────────

/// Apply `cmd` against the local blob store + `<data_dir>/incoming/`.
///
/// Mirrors `moments::run` / `roster::run`: this function is
/// `async` and is driven from `main.rs` via
/// `futures::executor::block_on` (cheap — the work is either
/// pure SQLite-ish metadata IO or a small handful of
/// `spawn_blocking` tasks).
pub async fn run(cmd: &ShareCmd, data_dir: &Path) -> Result<()> {
    match cmd {
        ShareCmd::Send {
            path,
            allow_symlinks,
            include_hidden,
            show_manifest,
        } => {
            run_send(
                data_dir,
                path,
                *allow_symlinks,
                *include_hidden,
                *show_manifest,
            )
            .await
        }
        ShareCmd::Receive {
            ticket,
            out_dir,
            overwrite,
        } => {
            run_receive(data_dir, ticket, out_dir.as_deref(), *overwrite).await
        }
        ShareCmd::Resume { sub } => run_resume(sub, data_dir),
    }
}

// ── send ───────────────────────────────────────────────────────────────────

async fn run_send(
    data_dir: &Path,
    raw_path: &str,
    allow_symlinks: bool,
    include_hidden: bool,
    show_manifest: bool,
) -> Result<()> {
    let path = PathBuf::from(raw_path);
    if !path.exists() {
        bail!("path does not exist: {}", path.display());
    }

    // Open (or create) the local blob store. The store records
    // into the global Prometheus registry via `blob_metrics()`,
    // so an operator with `metrics-http` enabled sees the bytes
    // flow through `a3net_blob_*` immediately.
    let blobs_dir = data_dir.join("blobs");
    std::fs::create_dir_all(&blobs_dir)
        .with_context(|| format!("creating blob store dir {}", blobs_dir.display()))?;
    let store = BlobStore::new(&blobs_dir)
        .with_context(|| format!("opening blob store at {}", blobs_dir.display()))?;
    let store = std::sync::Arc::new(store);

    // Build the sync `PutBytesFn` closure that `walk_import`
    // expects. `walk_import` runs the closure on the blocking
    // pool via `spawn_blocking`, so a sync `put_bytes` is
    // correct here — see PR1 notes in `walk.rs`. We discard
    // the size because the manifest records hashes, not
    // sizes.
    let store_for_put = std::sync::Arc::clone(&store);
    let put_bytes: PutBytesFn = std::sync::Arc::new(move |bytes: &[u8]| {
        let (hash, _size) = store_for_put
            .put_bytes_sync(bytes)
            .map_err(|e| a3net_share::ShareError::Backend(format!("blobstore: {e}")))?;
        Ok(hash)
    });

    let opts = WalkOptions {
        jobs: None, // auto: num_cpus
        allow_symlinks,
        skip_hidden: !include_hidden,
    };

    info!(
        path = %path.display(),
        allow_symlinks,
        include_hidden,
        "share send: walking"
    );
    let (manifest, manifest_hash, stats) =
        walk_import(&path, put_bytes, opts).await.map_err(|e| anyhow!("walk_import: {e}"))?;

    // The ticket needs a sender NodeId + NodeAddr. When iroh is available,
    // we spin up a brief endpoint to discover our own direct/relay addresses.
    // This makes the ticket usable for P2P fetching across machines.
    let node_id = load_or_create_local_node_id(data_dir)?;
    let endpoint = build_ticket_endpoint(&node_id).await;
    let total_size = stats.total_bytes;

    let ticket = ShareTicket::new(
        &node_id,
        &endpoint,
        &manifest_hash,
        &manifest,
        total_size,
    )
    .map_err(|e| anyhow!("build ticket: {e}"))?;

    // Store the manifest bytes themselves in the local blob
    // store. Without this step `a3net share receive` (on the
    // same machine) cannot reconstruct the manifest — `walk_import`
    // only stores per-file blobs, never the manifest blob. The
    // iroh path doesn't need this because the manifest comes
    // back as a regular `get(manifest_hash)` round-trip, but the
    // local path needs the bytes on disk.
    let manifest_bytes: Vec<u8> = match postcard::to_stdvec(&manifest) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                error = %e,
                "share send: could not serialise manifest for local-cache; \
                 a3net share receive will not find the manifest on this machine"
            );
            Vec::new()
        }
    };
    if !manifest_bytes.is_empty()
        && let Err(e) = store.put_bytes_sync(&manifest_bytes)
    {
        warn!(
            error = %e,
            "share send: could not cache manifest in local blob store"
        );
    }

    let encoded = ticket.encode();

    // Human-friendly output. The ticket is the only thing the
    // peer strictly needs; the surrounding lines make it easy to
    // inspect what was ingested.
    println!(
        "{}",
        serde_json::json!({
            "ticket": encoded,
            "sender_node_id": node_id.to_string(),
            "manifest_hash": manifest_hash.as_hex(),
            "files": stats.files_imported,
            "bytes": stats.total_bytes,
            "elapsed_ms": stats.elapsed_ms,
            "symlinks_skipped": stats.symlinks_skipped,
            "hidden_skipped": stats.hidden_skipped,
        })
    );
    if show_manifest {
        // Pretty manifest dump — handy for `diff`-ing two
        // imports of the same directory.
        let entries: Vec<serde_json::Value> = manifest
            .iter()
            .map(|(name, hash)| {
                serde_json::json!({
                    "name": name,
                    "hash": hash.as_hex(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "manifest_hash": manifest_hash.as_hex(),
                "entries": entries,
            }))?
        );
    }
    Ok(())
}

/// Load (or freshly generate) the local NodeId from
/// `<data_dir>/identity.key`. We deliberately use the same file
/// the rest of A3Net uses so a `share send` ticket carries the
/// operator's real NodeId — the same one `a3net diagnostics`
/// reports and the gossip / mesh layers sign with.
fn load_or_create_local_node_id(data_dir: &Path) -> Result<a3net_types::NodeId> {
    let path = data_dir.join("identity.key");
    if path.exists() {
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read identity file at {}", path.display()))?;
        // Accept either 32 raw bytes or 64 hex chars (legacy).
        if let Ok(s) = std::str::from_utf8(&bytes) {
            let trimmed: String = s.chars().filter(|c| !c.is_whitespace()).collect();
            if trimmed.len() == 64
                && trimmed.chars().all(|c| c.is_ascii_hexdigit())
                && let Ok(raw) = hex_decode(&trimmed)
                && let Ok(id) = a3net_types::NodeId::from_bytes(&raw)
            {
                return Ok(id);
            }
        }
        if bytes.len() == 32 {
            return a3net_types::NodeId::from_bytes(&bytes)
                .with_context(|| "32-byte identity blob is not a valid NodeId");
        }
        bail!(
            "identity file at {} has unexpected length {}",
            path.display(),
            bytes.len()
        );
    }
    // No identity yet: mint a fresh one and persist. We use
    // `NodeId::random()` to match the rest of the CLI; this is a
    // fresh identity, not a replacement for the operator's
    // existing one (the node hasn't started, so there's nothing
    // to replace yet).
    std::fs::create_dir_all(data_dir)?;
    let id = a3net_types::NodeId::random();
    std::fs::write(&path, id.as_bytes())
        .with_context(|| format!("writing identity file at {}", path.display()))?;
    warn!(
        path = %path.display(),
        "share send: minted a fresh local NodeId (no identity.key on disk)"
    );
    Ok(id)
}

/// Build a NodeAddr for the share ticket by discovering our current endpoints.
///
/// Spins up a minimal iroh endpoint briefly to discover our direct IP:port
/// and relay URL. The endpoint is closed immediately after discovery — it's
/// only used for address discovery, not for actual transfers.
async fn build_ticket_endpoint(node_id: &a3net_types::NodeId) -> a3net_types::NodeAddr {
    #[cfg(feature = "iroh")]
    {
        match a3net_share::discover_endpoints(node_id).await {
            Ok((mut addr, endpoint)) => {
                endpoint.close().await;

                if addr.direct.is_none() && addr.relay.is_none() {
                    warn!(
                        "share send: ticket has no direct/relay addresses; \
                         P2P will require the recipient to have a relay connection. \
                         Run `a3net serve` first for full P2P support."
                    );
                }

                addr
            }
            Err(e) => {
                warn!(error = %e, "share send: failed to discover endpoints");
                a3net_types::NodeAddr::new(node_id.clone())
            }
        }
    }

    #[cfg(not(feature = "iroh"))]
    {
        a3net_types::NodeAddr::new(node_id.clone())
    }
}

/// Tiny hex decoder for the legacy 64-byte identity files.
/// Mirrors the helper in `diagnostics.rs` but kept local so we
/// don't widen the public surface there.
fn hex_decode(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        bail!("hex string has odd length");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in s.as_bytes().chunks(2) {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => bail!("non-hex character: {b:#04x}"),
    }
}

// ── receive ────────────────────────────────────────────────────────────────

async fn run_receive(
    data_dir: &Path,
    raw_ticket: &str,
    out_dir: Option<&str>,
    overwrite: bool,
) -> Result<()> {
    let ticket_str = read_ticket_arg(raw_ticket)?;
    let ticket = ShareTicket::parse(&ticket_str).map_err(|e| anyhow!("invalid ticket: {e}"))?;

    let resolved_out_dir = match out_dir {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    std::fs::create_dir_all(&resolved_out_dir)?;

    #[cfg(feature = "iroh")]
    {
        run_receive_iroh(data_dir, &ticket, &resolved_out_dir, overwrite).await
    }

    #[cfg(not(feature = "iroh"))]
    {
        run_receive_local_only(data_dir, &ticket, &resolved_out_dir, overwrite).await
    }
}

/// Local-only receive path (no iroh feature).
/// Only works when the sender's blobs are already in this machine's blob store.
#[cfg(not(feature = "iroh"))]
async fn run_receive_local_only(
    data_dir: &Path,
    ticket: &ShareTicket,
    out_dir: &Path,
    overwrite: bool,
) -> Result<()> {
    let blobs_dir = data_dir.join("blobs");
    std::fs::create_dir_all(&blobs_dir)?;
    let store = BlobStore::new(&blobs_dir)?;
    let store = std::sync::Arc::new(store);

    let manifest_hash = ticket.manifest_hash.clone();
    let manifest = find_local_manifest(&store, &manifest_hash).await?;

    let incoming_root = a3net_share::resume_dir(data_dir, &manifest_hash);
    std::fs::create_dir_all(&incoming_root)?;
    let mut resume_state = a3net_share::ResumeState::new(ticket, manifest_hash.clone());
    resume_state.total_bytes = manifest_total_bytes(&manifest);
    a3net_share::save(&incoming_root, &resume_state)
        .map_err(|e| anyhow!("write resume.json: {e}"))?;

    let opts = ReceiveOptions {
        out_dir: Some(out_dir.to_path_buf()),
        overwrite,
    };

    let stats = a3net_share::receive(ticket, &manifest, store.as_ref(), opts)
        .await
        .map_err(|e| anyhow!("receive: {e}"))?;

    // Persist manifest for future resume.
    let manifest_bytes: Vec<u8> = postcard::to_stdvec(&manifest)
        .map_err(|e| anyhow!("serialize manifest: {e}"))?;
    std::fs::write(a3net_share::manifest_path(&incoming_root), &manifest_bytes)
        .map_err(|e| anyhow!("write manifest.bin: {e}"))?;

    // Update resume state.
    let mut total_done: u64 = 0;
    for (name, hash) in manifest.iter() {
        let size = BlobReader::size(store.as_ref(), hash).await.unwrap_or(0);
        resume_state.files.push(a3net_share::ResumeFileProgress {
            name: name.to_string(),
            hash: hash.clone(),
            total_bytes: size,
            bytes_done: size,
            complete: true,
        });
        total_done = total_done.saturating_add(size);
    }
    resume_state.total_bytes = total_done;
    resume_state.bytes_done = stats.bytes_written;
    resume_state.status = a3net_share::ResumeStatus::Completed;
    a3net_share::save(&incoming_root, &resume_state)
        .map_err(|e| anyhow!("write resume.json: {e}"))?;

    println!(
        "{}",
        serde_json::json!({
            "status": "completed",
            "manifest_hash": manifest_hash.as_hex(),
            "incoming_dir": incoming_root.display().to_string(),
            "out_dir": out_dir.display().to_string(),
            "files_written": stats.files_written,
            "bytes_written": stats.bytes_written,
            "elapsed_ms": stats.elapsed_ms,
            "mode": "local",
        })
    );
    Ok(())
}

/// P2P-capable receive path (with iroh feature).
/// Tries local-first, then falls back to remote_fetch over Bao-verified QUIC.
#[cfg(feature = "iroh")]
async fn run_receive_iroh(
    data_dir: &Path,
    ticket: &ShareTicket,
    out_dir: &Path,
    overwrite: bool,
) -> Result<()> {
    let blobs_dir = data_dir.join("blobs");
    std::fs::create_dir_all(&blobs_dir)?;

    // Open the iroh-backed store (compatible with the Bao QUIC protocol).
    let iroh_store = IrohBlobStore::open(&blobs_dir)
        .await
        .map_err(|e| anyhow!("open iroh blob store: {e}"))?;
    let iroh_store = std::sync::Arc::new(iroh_store);

    // Also open the legacy store for the local path.
    let legacy_store = BlobStore::new(&blobs_dir)?;
    let legacy_store = std::sync::Arc::new(legacy_store);

    let manifest_hash = ticket.manifest_hash.clone();
    let incoming_root = a3net_share::resume_dir(data_dir, &manifest_hash);
    std::fs::create_dir_all(&incoming_root)?;

    // Step 1: Try local manifest first (same-machine share or cached).
    if let Some(manifest) = try_local_manifest(&legacy_store, &manifest_hash).await {
        info!(hash = %manifest_hash.as_hex(), "using local manifest (local mode)");
        run_local_receive(data_dir, ticket, &manifest, out_dir, overwrite).await?;
        return Ok(());
    }

    // Step 2: Fall back to P2P remote_fetch.
    info!(hash = %manifest_hash.as_hex(), sender = %ticket.node_id, "manifest not local — initiating P2P fetch");

    let remote_opts = RemoteFetchOptions::default();

    let outcome = receive_p2p(ticket, &iroh_store, data_dir, remote_opts)
        .await
        .map_err(|e| anyhow!("p2p receive: {e}"))?;

    // Step 3: Lay out files from the iroh store into the output directory.
    let opts = ReceiveOptions {
        out_dir: Some(out_dir.to_path_buf()),
        overwrite,
    };

    let stats = a3net_share::receive(ticket, &outcome.manifest, iroh_store.as_ref(), opts)
        .await
        .map_err(|e| anyhow!("lay out files: {e}"))?;

    // Step 4: Update resume sidecar with completion state.
    update_resume_complete_from_remote(&outcome.resume, data_dir, iroh_store.as_ref(), &stats).await?;

    println!(
        "{}",
        serde_json::json!({
            "status": "completed",
            "manifest_hash": manifest_hash.as_hex(),
            "incoming_dir": incoming_root.display().to_string(),
            "out_dir": out_dir.display().to_string(),
            "files_written": stats.files_written,
            "bytes_written": stats.bytes_written,
            "elapsed_ms": stats.elapsed_ms,
            "mode": "p2p",
            "sender": ticket.node_id.to_string(),
        })
    );
    Ok(())
}

/// Run the local receive path after manifest is confirmed to be local.
#[cfg(feature = "iroh")]
async fn run_local_receive(
    data_dir: &Path,
    ticket: &ShareTicket,
    manifest: &a3net_share::Collection,
    out_dir: &Path,
    overwrite: bool,
) -> Result<()> {
    let blobs_dir = data_dir.join("blobs");
    let legacy_store = BlobStore::new(&blobs_dir)?;
    let legacy_store = std::sync::Arc::new(legacy_store);

    let manifest_hash = ticket.manifest_hash.clone();
    let incoming_root = a3net_share::resume_dir(data_dir, &manifest_hash);
    std::fs::create_dir_all(&incoming_root)?;
    let mut resume_state = a3net_share::ResumeState::new(ticket, manifest_hash.clone());
    resume_state.total_bytes = manifest_total_bytes(manifest);
    a3net_share::save(&incoming_root, &resume_state)
        .map_err(|e| anyhow!("write resume.json: {e}"))?;

    let opts = ReceiveOptions {
        out_dir: Some(out_dir.to_path_buf()),
        overwrite,
    };

    // Use legacy store for the receive call (local path, no network needed).
    let stats = a3net_share::receive(ticket, manifest, legacy_store.as_ref(), opts)
        .await
        .map_err(|e| anyhow!("receive: {e}"))?;

    // Persist manifest for future resume.
    let manifest_bytes = postcard::to_vec(manifest)
        .map_err(|e| anyhow!("serialize manifest: {e}"))?;
    std::fs::write(a3net_share::manifest_path(&incoming_root), &manifest_bytes)
        .map_err(|e| anyhow!("write manifest.bin: {e}"))?;

    // Update resume state using legacy store.
    let mut total_done: u64 = 0;
    for (name, hash) in manifest.iter() {
        let size = BlobReader::size(legacy_store.as_ref(), hash).await.unwrap_or(0);
        resume_state.files.push(a3net_share::ResumeFileProgress {
            name: name.to_string(),
            hash: hash.clone(),
            total_bytes: size,
            bytes_done: size,
            complete: true,
        });
        total_done = total_done.saturating_add(size);
    }
    resume_state.total_bytes = total_done;
    resume_state.bytes_done = stats.bytes_written;
    resume_state.status = a3net_share::ResumeStatus::Completed;
    a3net_share::save(&incoming_root, &resume_state)
        .map_err(|e| anyhow!("write resume.json: {e}"))?;

    println!(
        "{}",
        serde_json::json!({
            "status": "completed",
            "manifest_hash": manifest_hash.as_hex(),
            "incoming_dir": incoming_root.display().to_string(),
            "out_dir": out_dir.display().to_string(),
            "files_written": stats.files_written,
            "bytes_written": stats.bytes_written,
            "elapsed_ms": stats.elapsed_ms,
            "mode": "local",
        })
    );
    Ok(())
}

/// Update resume state after a completed remote fetch.
#[cfg(feature = "iroh")]
async fn update_resume_complete_from_remote(
    resume_state: &a3net_share::ResumeState,
    data_dir: &Path,
    store: &IrohBlobStore,
    stats: &a3net_share::ReceiveStats,
) -> Result<()> {
    let mut updated = resume_state.clone();
    updated.status = a3net_share::ResumeStatus::Completed;
    updated.bytes_done = stats.bytes_written;
    for file in &mut updated.files {
        if let Ok(size) = store.size(&file.hash).await {
            file.total_bytes = size;
            file.bytes_done = size;
            file.complete = true;
        }
    }
    // Write to the incoming directory.
    let incoming_root = a3net_share::resume_dir(data_dir, &updated.manifest_hash);
    a3net_share::save(&incoming_root, &updated)
        .map_err(|e| anyhow!("write resume.json: {e}"))?;
    Ok(())
}

/// Read the ticket from the argument or from stdin (`-`).
fn read_ticket_arg(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading ticket from stdin")?;
        let t = buf.trim();
        if t.is_empty() {
            bail!("expected ticket on stdin, got empty input");
        }
        return Ok(t.to_string());
    }
    Ok(trimmed.to_string())
}

/// Read the manifest blob from the local blob store. Returns `Ok(None)`
/// if the manifest is not cached locally (caller should use P2P fetch).
///
/// The manifest bytes are cached by `share send` (see the
/// `put_bytes_sync` call in `run_send`) so a same-machine
/// `share receive` round-trips with no extra wiring.
async fn try_local_manifest(
    store: &BlobStore,
    target: &ContentHash,
) -> Option<a3net_share::Collection> {
    if !store.has(target).await {
        return None;
    }
    let bytes = BlobReader::read_all(store, target).await.ok()?;
    let manifest: a3net_share::Collection = postcard::from_bytes(&bytes).ok()?;
    Some(manifest)
}

/// Read the manifest blob from the local blob store. The
/// manifest bytes were cached by `share send` (see the
/// `put_bytes_sync` call in `run_send`) so a same-machine
/// `share receive` round-trips with no extra wiring.
///
/// If the manifest is not in the local store (e.g. the ticket
/// came from a remote peer and `iroh` remote_fetch is not yet
/// wired into this CLI) we surface a clear error rather than
/// guess.
async fn find_local_manifest(
    store: &BlobStore,
    target: &ContentHash,
) -> Result<a3net_share::Collection> {
    try_local_manifest(store, target).await
        .ok_or_else(|| {
            anyhow!(
                "manifest {} is not in the local blob store; \
                 run `a3net share send <path>` on this machine first \
                 to materialise the blobs, or rebuild with the `iroh` \
                 feature for live P2P pull",
                target.as_hex()
            )
        })
}

fn manifest_total_bytes(_m: &a3net_share::Collection) -> u64 {
    // We don't have per-file sizes in the manifest itself; the
    // sidecar's `total_bytes` is best-effort and gets refined
    // when we read the on-disk blobs. We start with 0 and let
    // the reconcile pass fill it in.
    0
}

// ── resume ─────────────────────────────────────────────────────────────────

fn run_resume(sub: &ShareResumeCmd, data_dir: &Path) -> Result<()> {
    match sub {
        ShareResumeCmd::Ls { json } => resume_ls(data_dir, *json),
        ShareResumeCmd::Info { hash_short, json } => resume_info(data_dir, hash_short, *json),
        ShareResumeCmd::Clean { hash_short, yes } => resume_clean(data_dir, hash_short, *yes),
        ShareResumeCmd::Continue { hash_short, overwrite } => {
            // `continue` re-enters `run_receive` with the
            // ticket stored in the sidecar. We dispatch via the
            // async runtime by re-wrapping through `block_on`
            // — this keeps the resume surface uniform.
            let ticket = read_resume_ticket(data_dir, hash_short)?;
            let out_dir_flag = None; // user must use `--out-dir` on the original command
            futures::executor::block_on(run_receive(data_dir, &ticket, out_dir_flag, *overwrite))
        }
    }
}

fn resume_ls(data_dir: &Path, json: bool) -> Result<()> {
    let mut states = a3net_share::list(data_dir)?;
    // Newest first so an operator looking at `share resume ls`
    // sees their most recent receive at the top. We sort by
    // the millisecond timestamp so the ordering is total.
    states.sort_by_key(|s| std::cmp::Reverse(s.updated_at.timestamp_millis()));
    if json {
        println!("{}", serde_json::to_string_pretty(&states)?);
        return Ok(());
    }
    if states.is_empty() {
        println!("(no share receive state under {})", data_dir.display());
        return Ok(());
    }
    println!(
        "{:<18} {:<10} {:<8} {:<10} {:<10} sender",
        "hash_short", "status", "files", "bytes_done", "bytes_total"
    );
    for s in &states {
        let short = &s.manifest_hash.as_hex()[..a3net_share::HASH_SHORT_LEN];
        let files_done = s.files_done();
        let total_files = s.files.len();
        println!(
            "{:<18} {:<10} {:>3}/{:<4} {:>10} {:>10} {}",
            short,
            status_label(s.status),
            files_done,
            total_files,
            s.bytes_done,
            s.total_bytes,
            short_sender(&s.sender_node_id),
        );
    }
    Ok(())
}

fn resume_info(data_dir: &Path, hash_short: &str, json: bool) -> Result<()> {
    let state = load_state_by_short(data_dir, hash_short)?
        .ok_or_else(|| anyhow!("no receive state for short hash {hash_short:?}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&state)?);
    } else {
        println!("hash_short     : {}", &state.manifest_hash.as_hex()[..a3net_share::HASH_SHORT_LEN]);
        println!("manifest_hash  : {}", state.manifest_hash.as_hex());
        println!("status         : {}", status_label(state.status));
        println!("sender         : {}", state.sender_node_id);
        println!("started_at     : {}", state.started_at);
        println!("updated_at     : {}", state.updated_at);
        println!(
            "progress       : {} / {} bytes ({:.1}%)",
            state.bytes_done,
            state.total_bytes,
            state.percent_done()
        );
        println!(
            "files          : {} / {} complete",
            state.files_done(),
            state.files.len()
        );
        if let Some(err) = &state.error {
            println!("error          : {err}");
        }
        // Print the stored ticket verbatim so the operator can
        // re-paste it into another tool.
        println!("\nticket:\n{}", state.ticket);
    }
    Ok(())
}

fn resume_clean(data_dir: &Path, hash_short: &str, yes: bool) -> Result<()> {
    let hash = parse_hash_short(hash_short)?;
    if !yes {
        let dir = a3net_share::resume_dir(data_dir, &hash);
        eprintln!("a3net: about to delete {}", dir.display());
        eprintln!("hint: pass --yes to skip this prompt");
        eprintln!("continue? [y/N]");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            eprintln!("aborted");
            return Ok(());
        }
    }
    let removed = a3net_share::clean(data_dir, &hash)?;
    if removed {
        println!("removed receive state for {hash_short}");
    } else {
        println!("no receive state for {hash_short}");
    }
    Ok(())
}

/// Pull the stored ticket out of the sidecar. Used by
/// `resume continue`.
fn read_resume_ticket(data_dir: &Path, hash_short: &str) -> Result<String> {
    let state = load_state_by_short(data_dir, hash_short)?
        .ok_or_else(|| anyhow!("no receive state for short hash {hash_short:?}"))?;
    Ok(state.ticket)
}

/// Look up the resume state whose short hash matches `short`.
fn load_state_by_short(data_dir: &Path, short: &str) -> Result<Option<a3net_share::ResumeState>> {
    let states = a3net_share::list(data_dir)?;
    for s in states {
        if s.manifest_hash.as_hex().starts_with(short) {
            return Ok(Some(s));
        }
    }
    Ok(None)
}

fn parse_hash_short(s: &str) -> Result<ContentHash> {
    // We accept any well-formed 64-hex-char manifest hash and
    // match by prefix. `ContentHash::from_hex` is strict about
    // length, so we ask for a full hash here — `hash_short` is
    // just the directory-name shorthand the operator typed.
    if s.len() != a3net_share::HASH_SHORT_LEN {
        bail!(
            "hash_short must be exactly {} hex chars (got {})",
            a3net_share::HASH_SHORT_LEN,
            s.len()
        );
    }
    // Reconstruct a 64-char hash by padding the short form with
    // zeros. This is *only* used to compute the directory name;
    // `a3net_share::clean` calls `resume_dir(data_dir, &hash)`
    // which uses the first 16 hex chars, so padding with zeros
    // is fine.
    let mut padded = String::with_capacity(ContentHash::HEX_LEN);
    padded.push_str(s);
    for _ in 0..(ContentHash::HEX_LEN - s.len()) {
        padded.push('0');
    }
    ContentHash::from_hex(&padded).map_err(|e| anyhow!("invalid hash_short: {e}"))
}

fn status_label(s: a3net_share::ResumeStatus) -> &'static str {
    match s {
        a3net_share::ResumeStatus::InProgress => "in_progress",
        a3net_share::ResumeStatus::Completed => "completed",
        a3net_share::ResumeStatus::Interrupted => "interrupted",
        a3net_share::ResumeStatus::Failed => "failed",
    }
}

fn short_sender(node_id: &str) -> String {
    if node_id.len() > 12 {
        format!("{}…", &node_id[..12])
    } else {
        node_id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_decode_round_trip() {
        let raw = vec![0u8, 1, 2, 0xfe, 0xff];
        let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex_decode(&hex).unwrap(), raw);
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn hex_nibble_accepts_uppercase() {
        assert_eq!(hex_nibble(b'A').unwrap(), 10);
        assert_eq!(hex_nibble(b'F').unwrap(), 15);
    }

    #[test]
    fn parse_hash_short_rejects_wrong_length() {
        let err = parse_hash_short("abc").unwrap_err();
        assert!(err.to_string().contains("hex chars"));
    }

    #[test]
    fn parse_hash_short_accepts_exact_len() {
        let s = "abcdef0123456789";
        assert_eq!(s.len(), a3net_share::HASH_SHORT_LEN);
        let hash = parse_hash_short(s).unwrap();
        assert_eq!(&hash.as_hex()[..a3net_share::HASH_SHORT_LEN], s);
    }

    #[test]
    fn status_label_matches_variant() {
        assert_eq!(status_label(a3net_share::ResumeStatus::InProgress), "in_progress");
        assert_eq!(status_label(a3net_share::ResumeStatus::Completed), "completed");
        assert_eq!(status_label(a3net_share::ResumeStatus::Interrupted), "interrupted");
        assert_eq!(status_label(a3net_share::ResumeStatus::Failed), "failed");
    }

    #[test]
    fn short_sender_truncates_long() {
        let s = "0123456789abcdef0123456789abcdef";
        assert_eq!(short_sender(s), "0123456789ab…");
    }

    #[test]
    fn short_sender_passes_through_short() {
        assert_eq!(short_sender("abc"), "abc");
    }
}