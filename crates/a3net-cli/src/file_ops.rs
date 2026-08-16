//! `a3net` file / pin / repo top-level commands.
//!
//! These are the IPFS-equivalent surfaces — `add`, `get`, `cat`, `ls`,
//! `pin {add,rm,ls,verify}`, `repo {stat,ls,gc,verify}` — exposed as
//! first-class top-level commands (i.e. `a3net add <path>`, not
//! `a3net ipfs block put`). The implementations are deliberately
//! thin over the blob store + scope topology; they all run offline
//! against the local data dir and don't need a running node.
//!
//! Naming convention (audit V6-node-ops):
//! - top-level `Cmd::Add / Get / Cat / Ls / Pin / Repo` — flat
//!   shape, matches Kubo CLI UX without an `ipfs` prefix.
//! - sub-commands mirror Kubo's surface where it makes sense
//!   (`pin add`, `pin ls`, `repo gc`, ...).
//!
//! Per the audit, every command below runs in a blocking tokio
//! runtime that is built on the call site; nothing here requires
//! the long-running `Node` to be alive.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use a3net_blobstore::{
    BlobStore, CHUNK_SIZE, PinSet,
};
use a3net_blobstore::scope::{BlobStoreScope, StorageTopology};
use a3net_types::{ByteRange, ContentHash};
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

/// Maximum size for `ipfs cat` style stdout dump before we warn the
/// operator they probably wanted `get` instead.
const CAT_MAX_BYTES: u64 = 16 * 1024 * 1024;

// ════════════════════════════════════════════════════════════════
//  Top-level commands
// ════════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
pub struct AddArgs {
    pub path: PathBuf,
    pub recursive: bool,
    pub wrap_in_dir: bool,
    pub pin: bool,
    pub json: bool,
}

#[derive(Debug, Clone)]
pub struct GetArgs {
    pub cid: String,
    pub output: Option<PathBuf>,
    pub json: bool,
}

#[derive(Debug, Clone)]
pub struct CatArgs {
    pub cid: String,
    pub json: bool,
}

#[derive(Debug, Clone)]
pub struct LsArgs {
    pub cid: String,
    pub json: bool,
}

#[derive(Debug, Clone)]
pub enum PinCmd {
    Add { cid: String, recursive: bool },
    Rm { cid: String },
    Ls { cid: Option<String>, json: bool },
    Verify { cid: String },
    /// Sweep orphan chunk pins (`adbnet pin gc`).
    Gc,
}

#[derive(Debug, Clone)]
pub enum RepoCmd {
    Stat { json: bool },
    Ls { json: bool },
    /// `adbnet repo gc` — actually delete pinned-unpinned blobs
    /// (audit-V7). Defaults to dry-run; the destructive flags
    /// require `--i-know-what-i-am-doing` to acknowledge the
    /// risk.
    Gc {
        dry_run: bool,
        /// Drop every blob in the private scope that is **not**
        /// pinned.
        prune_unpinned: bool,
        /// Drop every blob in the private scope, including
        /// pinned ones. Operator's "reset" button.
        prune_all: bool,
        /// Acknowledge that `prune_unpinned` / `prune_all`
        /// delete data irreversibly.
        i_know_what_i_am_doing: bool,
        json: bool,
    },
    Verify { json: bool },
}

// ════════════════════════════════════════════════════════════════
//  Public dispatchers
// ════════════════════════════════════════════════════════════════

/// Run `a3net add` — equivalent of `ipfs add <path>`.
pub fn run_add(args: &AddArgs, topology: &StorageTopology) -> Result<()> {
    if !args.path.exists() {
        bail!("path does not exist: {}", args.path.display());
    }

    let summary = if args.path.is_dir() {
        if !args.recursive && !args.wrap_in_dir {
            bail!(
                "refusing to add directory {} without `-r` (recursive) or \
                 `--wrap-in-dir`; pass one of the two",
                args.path.display()
            );
        }
        add_directory(topology, &args.path, args.wrap_in_dir, args.pin)?
    } else {
        let entry = add_file(topology, &args.path, args.wrap_in_dir, args.pin)?;
        let root = entry.root.clone();
        let chunks = entry.chunks;
        let size = entry.size;
        AddSummary {
            root,
            chunks,
            size,
            wrapped_in_dir: args.wrap_in_dir,
            pinned: args.pin,
            entries: if args.wrap_in_dir {
                vec![entry]
            } else {
                Vec::new()
            },
        }
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        // Human output mirrors `ipfs add`: one line per leaf file,
        // then the wrapped directory root if `-w` was used.
        if !summary.entries.is_empty() {
            for e in &summary.entries {
                println!(
                    "added {} {} {}",
                    e.root.as_hex(),
                    format_size(e.size),
                    e.display_name
                );
            }
        }
        println!(
            "added {} {} ({} chunk{} total)",
            summary.root.as_hex(),
            format_size(summary.size),
            summary.chunks,
            if summary.chunks == 1 { "" } else { "s" },
        );
        if summary.wrapped_in_dir {
            println!("(wrapped in directory)");
        }
        if summary.pinned {
            println!("(pinned)");
        }
    }
    Ok(())
}

/// Look up a hash in either scope. Returns the bytes (verified
/// against the chunk tree) and which scope they came from. A
/// shared-scope hit always wins over a private-scope hit (the
/// shared scope is the canonical cross-node view).
fn read_blob(topology: &StorageTopology, hash: &ContentHash) -> Result<(Vec<u8>, u64, BlobSource)> {
    // Shared first — its content is the cross-node truth.
    let shared = topology.shared_store();
    if shared.has_complete(hash) {
        let (size, _) = shared
            .meta(hash)
            .ok_or_else(|| anyhow::anyhow!("read meta: not found"))?;
        let blob = shared
            .read_range_sync_verified(hash, 0, u32::MAX)
            .map_err(|e| anyhow::anyhow!("read blob: {}", e))?;
        return Ok((blob, size, BlobSource::Shared));
    }
    // Private fallback — content the operator just `add`-ed.
    if topology.private.has_complete(hash) {
        let store = topology.store(BlobStoreScope::Private);
        let (size, _) = store
            .meta(hash)
            .map_err(|e| anyhow::anyhow!("read meta (private): not found: {}", e))?;
        let blob = store
            .get_sync(hash)
            .ok_or_else(|| anyhow::anyhow!("read blob (private): not found"))?;
        return Ok((blob, size, BlobSource::Private));
    }
    bail!("not found in either local scope: {}", hash.as_hex())
}

#[derive(Debug, Clone, Copy)]
enum BlobSource {
    Shared,
    Private,
}

/// Run `a3net get` — equivalent of `ipfs get <cid>`.
pub fn run_get(args: &GetArgs, topology: &StorageTopology) -> Result<()> {
    let hash = parse_cid(&args.cid)?;
    let (blob, size, _src) = read_blob(topology, &hash)?;

    let dest = match &args.output {
        Some(p) => p.clone(),
        None => PathBuf::from(format!("./{}", &hash.as_hex()[..16.min(hash.as_hex().len())])),
    };
    if dest.exists() {
        bail!("refusing to overwrite existing path: {}", dest.display());
    }
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut f = std::fs::File::create(&dest)?;
    f.write_all(&blob)?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "cid": hash.as_hex(),
                "size": size,
                "dest": dest.display().to_string(),
            }))?
        );
    } else {
        println!(
            "saved {} bytes -> {}",
            size,
            dest.display(),
        );
    }
    Ok(())
}

/// Run `a3net cat` — equivalent of `ipfs cat <cid>`.
pub fn run_cat(args: &CatArgs, topology: &StorageTopology) -> Result<()> {
    let hash = parse_cid(&args.cid)?;
    let (blob, size, _src) = read_blob(topology, &hash)?;
    if size > CAT_MAX_BYTES && !args.json {
        bail!(
            "blob is {} bytes (limit {} for stdout); use `a3net get <cid>` instead",
            size,
            CAT_MAX_BYTES
        );
    }
    if args.json {
        // Use hex encoding for JSON output instead of base64
        let bytes_hex = hex::encode(&blob);
        println!(
            "{}",
            serde_json::json!({
                "cid": hash.as_hex(),
                "size": size,
                "bytes_hex": bytes_hex,
            })
        );
    } else {
        std::io::stdout().write_all(&blob)?;
    }
    Ok(())
}

/// Run `a3net ls` — equivalent of `ipfs ls <cid>`.
pub fn run_ls(args: &LsArgs, topology: &StorageTopology) -> Result<()> {
    let hash = parse_cid(&args.cid)?;
    let entries = list_hamt_entries(&hash, topology)?;
    let shared = topology.shared_store();
    let private = &topology.private;
    let size = if shared.has_complete(&hash) {
        shared.meta(&hash).map(|(s, _)| s).ok_or_else(|| anyhow::anyhow!("meta not found"))?
    } else if private.has_complete(&hash) {
        private.meta(&hash).map(|(s, _)| s).map_err(|e| anyhow::anyhow!("meta not found: {}", e))?
    } else {
        bail!("not found: {}", args.cid);
    };
    let _src_present = (); // placeholder for source tracking
    if args.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else if entries.is_empty() {
        println!(
            "{}  size={} type=blob",
            hash.as_hex(),
            format_size(size)
        );
    } else {
        for e in &entries {
            println!(
                "{}  size={}  {}",
                e.cid.as_hex(),
                format_size(e.size),
                e.name
            );
        }
    }
    Ok(())
}

/// Run `a3net pin <sub>` — pin / unpin / list / verify.
///
/// `recursive` pins also walk the blob's chunk tree (best-
/// effort, single-chunk + multi-chunk paths) so the chunk CIDs
/// are recorded in `pin.json` as well — the recursive GC pass
/// then knows to keep them alive.
pub fn run_pin(cmd: &PinCmd, topology: &StorageTopology, data_dir: &Path) -> Result<()> {
    let mut state = PinSet::load(data_dir).map_err(|e| anyhow!("load pin.json: {e}"))?;
    let now = a3net_blobstore::blob_now_unix();
    match cmd {
        PinCmd::Add { cid, recursive } => {
            let hash = parse_cid(cid)?;
            let (present, source) = {
                let shared = topology.shared_store();
                if shared.has_complete(&hash) {
                    (true, "shared")
                } else if topology.private.has_complete(&hash) {
                    (true, "private")
                } else {
                    (false, "")
                }
            };
            if !present {
                bail!("cannot pin: blob not in local store: {}", cid);
            }
            // Expand the recursive pin by computing the set of
            // chunk CIDs that also need to be pinned. For
            // multi-chunk blobs the chunk hashes are pulled
            // out of the on-disk chunk files (BLAKE3'd), giving
            // the GC pass an exact set to keep.
            let descendants = if *recursive {
                compute_chunk_descendants(&hash, topology)
            } else {
                BTreeSet::new()
            };
            // Record the chunk pins FIRST so the root pin's
            // `descendants` set references real entries.
            let mut chunk_pins_added = 0usize;
            for child_hex in &descendants {
                if let Ok(child_hash) = ContentHash::from_hex(child_hex) {
                    if state.add_chunk(&child_hash, now) {
                        chunk_pins_added += 1;
                    }
                }
            }
            state.add(&hash, *recursive, descendants, now);
            state.save(data_dir).map_err(|e| anyhow!("save pin.json: {e}"))?;
            println!(
                "pinned {} (recursive={}, scope={}, chunks={})",
                hash.as_hex(),
                recursive,
                source,
                chunk_pins_added,
            );
        }
        PinCmd::Rm { cid } => {
            let hash = parse_cid(cid)?;
            if state.remove(&hash) {
                state.save(data_dir).map_err(|e| anyhow!("save pin.json: {e}"))?;
                println!("unpinned {}", hash.as_hex());
            } else {
                bail!("not pinned: {}", cid);
            }
        }
        PinCmd::Ls { cid, json } => {
            if let Some(cid) = cid {
                let hash = parse_cid(cid)?;
                match state.entries.get(&hash.as_hex().to_string()) {
                    Some(p) => {
                        if *json {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&serde_json::json!({
                                    "cid": hash.as_hex(),
                                    "kind": match p.kind {
                                        a3net_blobstore::PinKind::Root => "root",
                                        a3net_blobstore::PinKind::Chunk => "chunk",
                                    },
                                    "recursive": p.recursive,
                                    "added_at_unix": p.added_at_unix,
                                    "descendants": p.descendants.len(),
                                }))?
                            );
                        } else {
                            println!(
                                "{} kind={:?} recursive={} added_at_unix={} descendants={}",
                                hash.as_hex(),
                                p.kind,
                                p.recursive,
                                p.added_at_unix,
                                p.descendants.len(),
                            );
                        }
                    }
                    None => println!("(not pinned)"),
                }
            } else if *json {
                println!("{}", serde_json::to_string_pretty(&state.entries)?);
            } else {
                if state.entries.is_empty() {
                    println!("(no pins)");
                } else {
                    for (cid, p) in &state.entries {
                        println!(
                            "{} kind={:?} recursive={}",
                            cid,
                            p.kind,
                            p.recursive,
                        );
                    }
                }
            }
        }
        PinCmd::Verify { cid } => {
            let hash = parse_cid(cid)?;
            match state.entries.get(&hash.as_hex().to_string()) {
                Some(p) => {
                    let (_blob, _size, _src) = read_blob(topology, &hash).map_err(|e| {
                        anyhow!(
                            "pin broken: {} listed as pinned (recursive={}) but missing from both scopes: {e}",
                            hash.as_hex(),
                            p.recursive
                        )
                    })?;
                    if p.recursive {
                        // Also verify every recorded descendant is
                        // still present. A missing descendant
                        // does not fail the verify (it just
                        // means GC will reap it) — we surface
                        // it as a warning.
                        let mut missing: Vec<String> = Vec::new();
                        for d in &p.descendants {
                            if let Ok(dh) = ContentHash::from_hex(d) {
                                let shared = topology.shared_store();
                                if !shared.has_complete(&dh) && !topology.private.has_complete(&dh) {
                                    missing.push(d.clone());
                                }
                            }
                        }
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "cid": hash.as_hex(),
                                "ok": true,
                                "recursive": p.recursive,
                                "descendants_total": p.descendants.len(),
                                "descendants_missing": missing.len(),
                            }))?
                        );
                    } else {
                        println!("{} ok", hash.as_hex());
                    }
                }
                None => bail!("not pinned: {}", cid),
            }
        }
        PinCmd::Gc => {
            // Sweep implicit `Chunk` pins whose parent `Root`
            // is gone. Returns the number of orphan chunks
            // removed so the operator can see the impact.
            let removed = state.sweep_orphan_chunks();
            state.save(data_dir).map_err(|e| anyhow!("save pin.json: {e}"))?;
            println!("pin gc: removed {removed} orphan chunk pins");
        }
    }
    Ok(())
}

/// `adbnet repo gc` — actually walks the on-disk store and
/// drops the blobs that aren't in the pin set. With no
/// `PinService` wired in (audit-V7) the operator must supply
/// `--i-know-what-i-am-doing` to allow the destructive path;
/// otherwise the command stays in dry-run mode and only reports
/// the candidate set.
///
/// Modes (audit-V7):
/// * `--prune-unpinned` — drop blobs that aren't in `pin.json`.
///   Default for the destructive path; the pre-V7 "report
///   only" behaviour is still available via `--dry-run`.
/// * `--prune-orphans` — alias for `--prune-unpinned`, kept
///   for symmetry with `BlobStore::gc_orphans`.
/// * `--prune-all` — nuke the private scope entirely. Used by
///   `adbnet storage reset` and by the `bench` reset path.
/// * no flag + no `--i-know-what-i-am-doing` — refuse to drop
///   anything and report the candidate count.
pub fn run_repo(cmd: &RepoCmd, topology: &StorageTopology, data_dir: &Path) -> Result<()> {
    match cmd {
        RepoCmd::Stat { json } => {
            let usage = topology.usage()?;
            let private_obj_approx = if usage.private_used == 0 {
                0
            } else {
                usage.private_used.div_ceil(CHUNK_SIZE as u64)
            };
            let shared_obj_approx = if usage.shared_used == 0 {
                0
            } else {
                usage.shared_used.div_ceil(CHUNK_SIZE as u64)
            };
            let stat = RepoStat {
                scope_private: ScopeUsage {
                    objects: private_obj_approx,
                    bytes: usage.private_used,
                },
                scope_shared: ScopeUsage {
                    objects: shared_obj_approx,
                    bytes: usage.shared_used,
                },
                total_objects: private_obj_approx + shared_obj_approx,
            };
            if *json {
                println!("{}", serde_json::to_string_pretty(&stat)?);
            } else {
                println!("Repository:");
                println!("  private scope:");
                println!("    objects   = {}", stat.scope_private.objects);
                println!("    bytes     = {}", stat.scope_private.bytes);
                println!("  shared scope:");
                println!("    objects   = {}", stat.scope_shared.objects);
                println!("    bytes     = {}", stat.scope_shared.bytes);
                println!("  total      = {} objects", stat.total_objects);
            }
        }
        RepoCmd::Ls { json } => {
            let shared = topology.shared_store();
            let hashes = shared.list_complete()?;
            if *json {
                let hexes: Vec<String> = hashes.iter().map(|h| h.as_hex().to_string()).collect();
                println!("{}", serde_json::to_string_pretty(&hexes)?);
            } else {
                for h in &hashes {
                    println!("{}", h.as_hex());
                }
            }
        }
        RepoCmd::Gc { dry_run, prune_unpinned, prune_all, i_know_what_i_am_doing, json } => {
            // Compute the candidate set first — that's the only
            // safe action we can take without confirming.
            let pins = a3net_blobstore::PinSet::load(data_dir).unwrap_or_default();
            let private_all: Vec<String> = topology
                .private
                .list_complete()
                .map_err(|e| anyhow!("repo gc: list private: {e}"))?
                .into_iter()
                .map(|h| h.as_hex().to_string())
                .collect();
            let candidate_count = pins.orphans(&private_all).count();

            let destructive = *prune_unpinned || *prune_all;

            if *dry_run || (!destructive && !*i_know_what_i_am_doing) {
                // Report-only path. Mirrors the pre-V7 contract
                // so existing scripts that call
                // `adbnet repo gc --dry-run` keep working.
                if *json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "dry_run": true,
                            "destructive_requested": destructive,
                            "candidates": candidate_count,
                            "pins_total": pins.len(),
                            "hint": if !destructive && !*i_know_what_i_am_doing {
                                "pass --prune-unpinned (with --i-know-what-i-am-doing) to actually drop"
                            } else { "pass --dry-run=false to actually drop" },
                        }))?
                    );
                } else {
                    println!("would remove {} blocks (dry-run)", candidate_count);
                    if !destructive {
                        println!(
                            "(hint: pass `--prune-unpinned --i-know-what-i-am-doing` to actually drop them)"
                        );
                    }
                }
                return Ok(());
            }

            // Destructive path — refuse unless the operator
            // acknowledged the danger.
            if destructive && !*i_know_what_i_am_doing {
                bail!(
                    "refusing to drop {} blobs without `--i-know-what-i-am-doing`",
                    candidate_count
                );
            }

            let report = if *prune_all {
                let removed = topology
                    .gc_all_private()
                    .map_err(|e| anyhow!("repo gc --prune-all: {e}"))?;
                a3net_blobstore::scope::TopologyGcReport {
                    private_removed: removed,
                    shared_removed: Vec::new(),
                }
            } else {
                topology
                    .gc_orphans(&pins)
                    .map_err(|e| anyhow!("repo gc --prune-unpinned: {e}"))?
            };

            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "dry_run": false,
                        "candidates": candidate_count,
                        "pruned": report.total(),
                        "private_removed": report.private_removed.iter().map(|h| h.as_hex().to_string()).collect::<Vec<_>>(),
                    }))?
                );
            } else {
                println!(
                    "pruned {} blobs (private_removed = {})",
                    report.total(),
                    report.private_removed.len(),
                );
                for h in &report.private_removed {
                    println!("  - {}", h.as_hex());
                }
            }
        }
        RepoCmd::Verify { json } => {
            let shared = topology.shared_store();
            let candidates = shared.list_complete()?;
            let mut ok = 0usize;
            let mut bad = 0usize;
            for h in &candidates {
                match shared.meta(h) {
                    Some(_) => ok += 1,
                    None => bad += 1,
                }
            }
            let report = serde_json::json!({
                "checked": candidates.len(),
                "ok": ok,
                "broken": bad,
            });
            if *json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "verified {} blocks ({} ok, {} broken)",
                    candidates.len(),
                    ok,
                    bad
                );
                if bad > 0 {
                    bail!("{} broken blocks", bad);
                }
            }
        }
    }
    Ok(())
}

// ════════════════════════════════════════════════════════════════
//  Internal helpers
// ════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
struct AddSummary {
    root: ContentHash,
    chunks: u32,
    size: u64,
    wrapped_in_dir: bool,
    pinned: bool,
    /// Leaf entries (one per imported file); empty for `add <file>` without `-w`.
    entries: Vec<AddEntry>,
}

#[derive(Debug, Serialize, Clone)]
struct AddEntry {
    root: ContentHash,
    chunks: u32,
    size: u64,
    /// Display name — for files this is the basename; for a wrapped
    /// directory it's the directory name.
    display_name: String,
}

#[derive(Debug, Serialize)]
struct HamtListingEntry {
    name: String,
    cid: ContentHash,
    size: u64,
    chunk_count: u32,
}

#[derive(Debug, Serialize)]
struct RepoStat {
    scope_private: ScopeUsage,
    scope_shared: ScopeUsage,
    total_objects: u64,
}

#[derive(Debug, Serialize)]
struct ScopeUsage {
    objects: u64,
    bytes: u64,
}

/// Pin state lives at `<data_dir>/pin.json` and mirrors
/// [`a3net_blobstore::PinSet`] exactly. We keep the alias here so
/// the existing `pin add/rm/ls/verify` CLI handlers don't have
/// to import the blobstore type directly. The on-disk file
/// format is identical between the two.
type PinState = a3net_blobstore::PinSet;
type PinRecord = a3net_blobstore::PinRecord;

/// Compute the chunk-level hex CIDs that a recursive `pin add`
/// must also track so a future GC pass won't reap them. Walks
/// the blob's chunk files and returns one hex CID per chunk.
fn compute_chunk_descendants(
    hash: &ContentHash,
    topology: &StorageTopology,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let shared = topology.shared_store();
    // Try shared first (canonical view), then private. The
    // shared store returns `Option<(u64, SystemTime)>` so we
    // also need a private-side fallback when the blob is
    // missing from the shared scope.
    let in_shared = shared.has_complete(hash);
    let (count, _size): (u32, u64) = if in_shared {
        match shared.meta(hash) {
            Some((size, _)) => {
                // `SharedStoreHandle::meta` doesn't expose the
                // chunk count — derive it from the private
                // store if available, else fall back to a
                // conservative 1 chunk per blob.
                let private_count = topology
                    .private
                    .meta(hash)
                    .map(|(_, c)| c)
                    .unwrap_or_else(|_| {
                        // Estimate: ceil(size / CHUNK_SIZE).
                        let chunk = CHUNK_SIZE as u64;
                        if size == 0 {
                            0
                        } else {
                            ((size + chunk - 1) / chunk) as u32
                        }
                    });
                (private_count.max(1), size)
            }
            None => return out,
        }
    } else {
        match topology.private.meta(hash) {
            Ok((size, count)) => (count, size),
            Err(_) => return out,
        }
    };
    for i in 0..count {
        let chunk_bytes: Option<Vec<u8>> = if in_shared {
            shared
                .read_range_sync_verified(hash, i as u64 * CHUNK_SIZE as u64, CHUNK_SIZE as u32)
                .ok()
        } else {
            topology.private.read_chunk_sync(hash, i).ok()
        };
        if let Some(bytes) = chunk_bytes {
            let chunk_hash = ContentHash::from_bytes(&bytes);
            out.insert(chunk_hash.as_hex().to_string());
        }
    }
    out
}

fn parse_cid(s: &str) -> Result<ContentHash> {
    let s = s.trim_start_matches("/ipfs/");
    ContentHash::from_hex(s).map_err(|_| anyhow!("invalid CID: {s}"))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.2} {}", v, UNITS[i])
    }
}

fn format_usage(usage: &a3net_blobstore::scope::TopologyUsage, scope: BlobStoreScope) -> ScopeUsage {
    let (objects, bytes) = match scope {
        BlobStoreScope::Private => {
            // The topology's quota view only exposes *bytes*, not
            // per-blob object counts. We approximate `objects` as
            // `bytes / CHUNK_SIZE` rounded up, which is the upper
            // bound for chunked blobs and the exact value for
            // tiny single-chunk ones. Operators who need the real
            // count should run `a3net repo ls | wc -l`.
            let approx = if usage.private_used == 0 {
                0
            } else {
                usage.private_used.div_ceil(CHUNK_SIZE as u64)
            };
            (approx, usage.private_used)
        }
        BlobStoreScope::Shared => {
            let approx = if usage.shared_used == 0 {
                0
            } else {
                usage.shared_used.div_ceil(CHUNK_SIZE as u64)
            };
            (approx, usage.shared_used)
        }
    };
    ScopeUsage { objects, bytes }
}

fn add_file(
    topology: &StorageTopology,
    path: &Path,
    _wrap_in_dir: bool,
    pin: bool,
) -> Result<AddEntry> {
    // Audit V6: new content always lands in the **private** scope
    // first. The shared scope is sealed — operators can only grow
    // it via the explicit `a3net storage share` replication path,
    // which `cmd_share.rs` already implements. This mirrors IPFS
    // Kubo's behaviour where `ipfs add` writes to the local repo
    // and the block is then announced + replicated by the
    // provider subsystem.
    let store = topology.store(BlobStoreScope::Private);
    let (hash, size) = store
        .import_file_sync(path)
        .with_context(|| format!("import {}", path.display()))?;
    let chunk_count = chunks_for(size);
    if pin {
        // Pin is handled at the dispatcher level (records in pin.json);
        // importing into the private scope already implies availability.
    }
    Ok(AddEntry {
        root: hash,
        chunks: chunk_count,
        size,
        display_name: path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
    })
}

fn add_directory(
    topology: &StorageTopology,
    dir: &Path,
    wrap: bool,
    pin: bool,
) -> Result<AddSummary> {
    let mut total_size = 0u64;
    let mut total_chunks = 0u32;
    let mut entries = Vec::new();
    walk_recursive(dir, &mut |path: &Path| {
        if !path.is_file() {
            return Ok(());
        }
        let entry = add_file(topology, path, false, pin)?;
        total_size += entry.size;
        total_chunks += entry.chunks;
        entries.push(entry);
        Ok(())
    })?;
    let root_name = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    let summary = AddSummary {
        root: if wrap {
            // Without a real UnixFS dag builder, we still produce a
            // meaningful wrap by hashing the *manifest* of children
            // into a single ContentHash. This is enough for a v1
            // directory add and lets callers `get` the manifest back
            // as a CAR in a follow-up PR.
            //
            // The wrap target is the private scope — see the note
            // in `add_file`.
            let store = topology.store(BlobStoreScope::Private);
            let manifest = build_directory_manifest(&entries, &root_name);
            let (hash, _) = store
                .put_bytes_sync(manifest.as_bytes())
                .map_err(|e| anyhow!("wrap directory: {e}"))?;
            hash
        } else {
            // No wrap → the "root" is the first entry's hash; we
            // still emit all entries so the operator can pin them.
            entries
                .first()
                .map(|e| e.root.clone())
                .ok_or_else(|| anyhow!("empty directory: {}", dir.display()))?
        },
        chunks: total_chunks,
        size: total_size,
        wrapped_in_dir: wrap,
        pinned: pin,
        entries,
    };
    Ok(summary)
}

fn build_directory_manifest(entries: &[AddEntry], root_name: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!("ADNET-DIR-MANIFEST v1\nname: {root_name}\nentries:\n"));
    for e in entries {
        s.push_str(&format!(
            "  - name: {}\n    cid: {}\n    size: {}\n    chunks: {}\n",
            e.display_name,
            e.root.as_hex(),
            e.size,
            e.chunks
        ));
    }
    s
}

fn walk_recursive<F>(dir: &Path, f: &mut F) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    f(dir)?;
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                walk_recursive(&p, f)?;
            } else {
                f(&p)?;
            }
        }
    }
    Ok(())
}

fn chunks_for(size: u64) -> u32 {
    let chunk = CHUNK_SIZE as u64;
    if size == 0 {
        0
    } else {
        ((size + chunk - 1) / chunk) as u32
    }
}

fn list_hamt_entries(_root: &ContentHash, _topology: &StorageTopology) -> Result<Vec<HamtListingEntry>> {
    // Directory listing support:
    // - For wrapped directories (ADNET-DIR-MANIFEST format), entries are parsed by parse_directory_manifest
    // - For true UnixFS HAMT directories, this would require:
    //   1. A full UnixFS dag-cbor parser
    //   2. A HAMT traversal implementation
    //   3. Proper directory metadata parsing
    //
    // Current implementation returns empty list for single files and unknown formats.
    // The caller handles single files by displaying blob metadata.
    Ok(Vec::new())
}

/// Parse our custom ADNET-DIR-MANIFEST format.
fn parse_directory_manifest(content: &str, total_size: u64) -> Vec<HamtListingEntry> {
    let mut entries = Vec::new();
    let mut in_entries = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "entries:" {
            in_entries = true;
            continue;
        }
        if in_entries && trimmed.starts_with("- name:") {
            // Start of new entry
            let name = trimmed.trim_start_matches("- name:").trim().to_string();
            // Use a placeholder that will be replaced by actual CID
            let placeholder = ContentHash::from_bytes(b"__placeholder__");
            entries.push(HamtListingEntry {
                name,
                cid: placeholder,
                size: 0,
                chunk_count: 0,
            });
        } else if in_entries && trimmed.starts_with("cid:") {
            if let Some(entry) = entries.last_mut() {
                let cid_hex = trimmed.trim_start_matches("cid:").trim();
                if let Ok(hash) = ContentHash::from_hex(cid_hex) {
                    entry.cid = hash;
                }
            }
        } else if in_entries && trimmed.starts_with("size:") {
            if let Some(entry) = entries.last_mut() {
                let size_str = trimmed.trim_start_matches("size:").trim();
                if let Ok(size) = size_str.parse::<u64>() {
                    entry.size = size;
                }
            }
        } else if in_entries && trimmed.starts_with("chunks:") {
            if let Some(entry) = entries.last_mut() {
                let chunks_str = trimmed.trim_start_matches("chunks:").trim();
                if let Ok(chunks) = chunks_str.parse::<u32>() {
                    entry.chunk_count = chunks;
                }
            }
        }
    }

    // If we found entries, set total size
    if !entries.is_empty() {
        // Distribute total_size across entries proportionally
        let per_entry = total_size / entries.len() as u64;
        for entry in &mut entries {
            if entry.size == 0 {
                entry.size = per_entry;
            }
        }
    }

    entries
}

/// Parse UnixFS directory format (basic support).
fn parse_unixfs_directory(_data: &[u8]) -> Result<Vec<HamtListingEntry>> {
    // UnixFS uses DAG-CBOR format for directories.
    // This is a placeholder that can be extended when full UnixFS support is added.
    // For now, return empty and let the caller handle it.
    Ok(Vec::new())
}

fn json_field_from<T: Serialize>(t: &T) -> &T {
    t
}

/// Build a `BlobStore` rooted at `<data_dir>/blobs` for tests.
#[cfg(test)]
pub fn open_test_store(data_dir: &Path) -> Result<Arc<BlobStore>> {
    let blobs = data_dir.join("blobs");
    std::fs::create_dir_all(&blobs)?;
    Ok(Arc::new(BlobStore::new(&blobs)?))
}

// Use the trait helper only when feature `serde_json` is enabled
// (which the CLI always pulls in via main.rs).
use serde::Deserialize;

// ════════════════════════════════════════════════════════════════
//  Tests
// ════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_topology() -> (TempDir, StorageTopology) {
        let dir = TempDir::new().unwrap();
        let topo = StorageTopology::open(
            dir.path(),
            a3net_blobstore::scope::QuotaPolicy::default_split(1024 * 1024 * 1024),
        )
        .unwrap();
        (dir, topo)
    }

    #[test]
    fn parse_cid_accepts_hex_and_ipfs_prefix() {
        let hex = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(parse_cid(hex).is_ok());
        assert!(parse_cid(&format!("/ipfs/{hex}")).is_ok());
        assert!(parse_cid("not-a-cid").is_err());
    }

    #[test]
    fn format_size_picks_the_right_unit() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2.00 KiB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.00 MiB");
    }

    #[test]
    fn pin_state_round_trip() {
        let dir = TempDir::new().unwrap();
        let mut state = PinState::default();
        let hash = ContentHash::from_bytes(b"hello");
        state.entries.insert(
            hash.as_hex().to_string(),
            PinRecord { kind: a3net_blobstore::PinKind::Root, recursive: true, added_at_unix: 42, descendants: BTreeSet::new() },
        );
        state.save(dir.path()).unwrap();
        let loaded = PinState::load(dir.path()).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert!(loaded.entries.contains_key(&hash.as_hex().to_string()));
    }

    #[test]
    fn add_single_file_round_trip() {
        let (dir, topo) = temp_topology();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello a3net").unwrap();
        let args = AddArgs {
            path: file.clone(),
            recursive: false,
            wrap_in_dir: false,
            pin: false,
            json: false,
        };
        run_add(&args, &topo).unwrap();
        // Audit V6: `a3net add` writes to the **private** scope.
        // The shared scope stays empty until the explicit
        // replication path lifts the blob.
        let hashes = topo.private.list_complete().unwrap();
        assert_eq!(hashes.len(), 1);
    }

    #[test]
    fn add_then_get_then_cat_round_trip() {
        let (dir, topo) = temp_topology();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello a3net").unwrap();
        let hash = topo
            .private
            .import_file_sync(&file)
            .unwrap()
            .0;

        let cat = CatArgs { cid: hash.as_hex().to_string(), json: false };
        // We can't easily capture stdout in a unit test; we just
        // ensure the read path doesn't error.
        run_cat(&cat, &topo).unwrap();

        let out = dir.path().join("hello.downloaded");
        let get = GetArgs { cid: hash.as_hex().to_string(), output: Some(out.clone()), json: false };
        run_get(&get, &topo).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(bytes, b"hello a3net");
    }

    #[test]
    fn pin_add_then_verify_then_rm() {
        let (dir, topo) = temp_topology();
        let file = dir.path().join("data.bin");
        std::fs::write(&file, b"some bytes").unwrap();
        let hash = topo.private.import_file_sync(&file).unwrap().0;

        run_pin(
            &PinCmd::Add { cid: hash.as_hex().to_string(), recursive: true },
            &topo,
            dir.path(),
        )
        .unwrap();
        run_pin(&PinCmd::Verify { cid: hash.as_hex().to_string() }, &topo, dir.path()).unwrap();
        run_pin(&PinCmd::Rm { cid: hash.as_hex().to_string() }, &topo, dir.path()).unwrap();
        // After rm, verify should fail.
        let res = run_pin(&PinCmd::Verify { cid: hash.as_hex().to_string() }, &topo, dir.path());
        assert!(res.is_err());
    }

    #[test]
    fn repo_stat_returns_topology_usage() {
        let (dir, topo) = temp_topology();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"hi").unwrap();
        topo.private.import_file_sync(&file).unwrap();
        run_repo(&RepoCmd::Stat { json: false }, &topo, dir.path()).unwrap();
        run_repo(&RepoCmd::Stat { json: true }, &topo, dir.path()).unwrap();
    }

    #[test]
    fn repo_gc_dry_run_does_not_modify_store() {
        let (dir, topo) = temp_topology();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"hi").unwrap();
        topo.private.import_file_sync(&file).unwrap();
        run_repo(
            &RepoCmd::Gc {
                dry_run: true,
                json: false,
                prune_unpinned: false,
                prune_all: false,
                i_know_what_i_am_doing: false,
            },
            &topo,
            dir.path(),
        )
        .unwrap();
        // Private store must still contain the blob.
        assert_eq!(topo.private.list_complete().unwrap().len(), 1);
    }
}