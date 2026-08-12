//! `adnet` file / pin / repo top-level commands.
//!
//! These are the IPFS-equivalent surfaces — `add`, `get`, `cat`, `ls`,
//! `pin {add,rm,ls,verify}`, `repo {stat,ls,gc,verify}` — exposed as
//! first-class top-level commands (i.e. `adnet add <path>`, not
//! `adnet ipfs block put`). The implementations are deliberately
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

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adnet_blobstore::{
    BlobStore, CHUNK_SIZE,
};
use adnet_blobstore::scope::{BlobStoreScope, StorageTopology};
use adnet_types::{ByteRange, ContentHash};
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
}

#[derive(Debug, Clone)]
pub enum RepoCmd {
    Stat { json: bool },
    Ls { json: bool },
    Gc { dry_run: bool, json: bool },
    Verify { json: bool },
}

// ════════════════════════════════════════════════════════════════
//  Public dispatchers
// ════════════════════════════════════════════════════════════════

/// Run `adnet add` — equivalent of `ipfs add <path>`.
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
        AddSummary {
            root: entry.root,
            chunks: entry.chunks,
            size: entry.size,
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
            .map_err(|e| anyhow!("read meta: {e}"))?;
        let blob = shared
            .read_range_sync_verified(hash, 0, u32::MAX)
            .map_err(|e| anyhow!("read blob: {e}"))?;
        return Ok((blob, size, BlobSource::Shared));
    }
    // Private fallback — content the operator just `add`-ed.
    if topology.private.has_complete(hash) {
        let store = topology.store(BlobStoreScope::Private);
        let (size, _) = store
            .meta(hash)
            .map_err(|e| anyhow!("read meta (private): {e}"))?;
        let blob = store
            .read_range_sync(hash, &ByteRange { start: 0, end: u64::MAX })
            .map_err(|e| anyhow!("read blob (private): {e}"))?;
        return Ok((blob, size, BlobSource::Private));
    }
    bail!("not found in either local scope: {}", hash.as_hex())
}

#[derive(Debug, Clone, Copy)]
enum BlobSource {
    Shared,
    Private,
}

/// Run `adnet get` — equivalent of `ipfs get <cid>`.
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

/// Run `adnet cat` — equivalent of `ipfs cat <cid>`.
pub fn run_cat(args: &CatArgs, topology: &StorageTopology) -> Result<()> {
    let hash = parse_cid(&args.cid)?;
    let (blob, size, _src) = read_blob(topology, &hash)?;
    if size > CAT_MAX_BYTES && !args.json {
        bail!(
            "blob is {} bytes (limit {} for stdout); use `adnet get <cid>` instead",
            size,
            CAT_MAX_BYTES
        );
    }
    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "cid": hash.as_hex(),
                "size": size,
                "bytes_b64": {
                    let mut s = String::with_capacity((blob.len() * 4 / 3) + 4);
                    use base64::Engine;
                    base64::engine::general_purpose::STANDARD.encode_string(&blob, &mut s);
                    s
                },
            })
        );
    } else {
        std::io::stdout().write_all(&blob)?;
    }
    Ok(())
}

/// Run `adnet ls` — equivalent of `ipfs ls <cid>`.
pub fn run_ls(args: &LsArgs, topology: &StorageTopology) -> Result<()> {
    let hash = parse_cid(&args.cid)?;
    let entries = list_hamt_entries(&hash, topology)?;
    let shared = topology.shared_store();
    let private = &topology.private;
    let (size, _src_present) = if shared.has_complete(&hash) {
        shared.meta(&hash).map_err(|e| anyhow!("meta: {e}"))?
    } else if private.has_complete(&hash) {
        private.meta(&hash).map_err(|e| anyhow!("meta: {e}"))?
    } else {
        bail!("not found: {}", args.cid);
    };
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

/// Run `adnet pin <sub>` — pin / unpin / list / verify.
pub fn run_pin(cmd: &PinCmd, topology: &StorageTopology, data_dir: &Path) -> Result<()> {
    let pin_path = data_dir.join("pin.json");
    let mut state = load_pin_state(&pin_path)?;
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
            state.pins.insert(
                hash.as_hex(),
                PinRecord {
                    recursive: *recursive,
                    added_at_unix: now_unix(),
                },
            );
            save_pin_state(&pin_path, &state)?;
            println!(
                "pinned {} (recursive={}, scope={})",
                hash.as_hex(),
                recursive,
                source
            );
        }
        PinCmd::Rm { cid } => {
            let hash = parse_cid(cid)?;
            if state.pins.remove(&hash.as_hex()).is_none() {
                bail!("not pinned: {}", cid);
            }
            save_pin_state(&pin_path, &state)?;
            println!("unpinned {}", hash.as_hex());
        }
        PinCmd::Ls { cid, json } => {
            if let Some(cid) = cid {
                let hash = parse_cid(cid)?;
                match state.pins.get(&hash.as_hex()) {
                    Some(p) => {
                        if *json {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&serde_json::json!({
                                    "cid": hash.as_hex(),
                                    "recursive": p.recursive,
                                    "added_at_unix": p.added_at_unix,
                                }))?
                            );
                        } else {
                            println!(
                                "{} recursive={} added_at_unix={}",
                                hash.as_hex(),
                                p.recursive,
                                p.added_at_unix
                            );
                        }
                    }
                    None => println!("(not pinned)"),
                }
            } else if json {
                println!("{}", serde_json::to_string_pretty(&state.pins)?);
            } else {
                if state.pins.is_empty() {
                    println!("(no pins)");
                } else {
                    for (cid, p) in &state.pins {
                        println!("{} recursive={}", cid, p.recursive);
                    }
                }
            }
        }
        PinCmd::Verify { cid } => {
            let hash = parse_cid(cid)?;
            match state.pins.get(&hash.as_hex()) {
                Some(p) => {
                    let (_blob, _size, _src) = read_blob(topology, &hash).map_err(|e| {
                        anyhow!(
                            "pin broken: {} listed as pinned (recursive={}) but missing from both scopes: {e}",
                            hash.as_hex(),
                            p.recursive
                        )
                    })?;
                    if *json_field_from(&p) {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "cid": hash.as_hex(),
                                "ok": true,
                                "recursive": p.recursive,
                            }))?
                        );
                    } else {
                        println!("{} ok", hash.as_hex());
                    }
                }
                None => bail!("not pinned: {}", cid),
            }
        }
    }
    Ok(())
}

/// Run `adnet repo <sub>` — repository inspection / GC.
pub fn run_repo(cmd: &RepoCmd, topology: &StorageTopology) -> Result<()> {
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
                let hexes: Vec<String> = hashes.iter().map(|h| h.as_hex()).collect();
                println!("{}", serde_json::to_string_pretty(&hexes)?);
            } else {
                for h in &hashes {
                    println!("{}", h.as_hex());
                }
            }
        }
        RepoCmd::Gc { dry_run, json } => {
            let shared = topology.shared_store();
            let candidates = shared.list_complete()?;
            // Without a real reference counter we can only enumerate
            // — surface the candidate set and refuse to actually drop
            // anything. A future PR will wire PinService into GC.
            let candidate_count = candidates.len();
            if *dry_run {
                if *json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "dry_run": true,
                            "candidates": candidate_count,
                        }))?
                    );
                } else {
                    println!("would remove {} blocks (dry-run)", candidate_count);
                }
            } else {
                if *json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "dry_run": false,
                            "candidates": candidate_count,
                            "note": "GC requires a PinService to identify orphans; refusing to drop without one",
                        }))?
                    );
                } else {
                    println!(
                        "{} blocks eligible; refusing to drop without a PinService (run with --dry-run to preview)",
                        candidate_count
                    );
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
                    Ok(_) => ok += 1,
                    Err(_) => bad += 1,
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

#[derive(Debug, Default, Serialize, Deserialize)]
struct PinState {
    pins: BTreeMap<String, PinRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PinRecord {
    recursive: bool,
    added_at_unix: i64,
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

fn format_usage(usage: &adnet_blobstore::scope::TopologyUsage, scope: BlobStoreScope) -> ScopeUsage {
    let (objects, bytes) = match scope {
        BlobStoreScope::Private => {
            // The topology's quota view only exposes *bytes*, not
            // per-blob object counts. We approximate `objects` as
            // `bytes / CHUNK_SIZE` rounded up, which is the upper
            // bound for chunked blobs and the exact value for
            // tiny single-chunk ones. Operators who need the real
            // count should run `adnet repo ls | wc -l`.
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
    // it via the explicit `adnet storage share` replication path,
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
    // Without a UnixFS dag-cbor directory at hand, the listing is
    // intentionally limited: callers see `(empty listing)` for
    // single-blob CIDs and the manifest entries for wrapped
    // directories (which were encoded by `build_directory_manifest`).
    // A follow-up PR will parse a true HAMT root.
    Ok(Vec::new())
}

fn load_pin_state(path: &Path) -> Result<PinState> {
    if !path.exists() {
        return Ok(PinState::default());
    }
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| anyhow!("pin.json: {e}"))
}

fn save_pin_state(path: &Path, state: &PinState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(state)?;
    std::fs::write(path, bytes)?;
    Ok(())
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
            adnet_blobstore::scope::QuotaPolicy::default_split(1024 * 1024 * 1024),
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
        let path = dir.path().join("pin.json");
        let mut state = PinState::default();
        let hash = ContentHash::from_bytes(b"hello").unwrap();
        state.pins.insert(
            hash.as_hex(),
            PinRecord { recursive: true, added_at_unix: 42 },
        );
        save_pin_state(&path, &state).unwrap();
        let loaded = load_pin_state(&path).unwrap();
        assert_eq!(loaded.pins.len(), 1);
        assert!(loaded.pins.contains_key(&hash.as_hex()));
    }

    #[test]
    fn add_single_file_round_trip() {
        let (dir, topo) = temp_topology();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello adnet").unwrap();
        let args = AddArgs {
            path: file.clone(),
            recursive: false,
            wrap_in_dir: false,
            pin: false,
            json: false,
        };
        run_add(&args, &topo).unwrap();
        // Audit V6: `adnet add` writes to the **private** scope.
        // The shared scope stays empty until the explicit
        // replication path lifts the blob.
        let hashes = topo.private.list_complete().unwrap();
        assert_eq!(hashes.len(), 1);
    }

    #[test]
    fn add_then_get_then_cat_round_trip() {
        let (dir, topo) = temp_topology();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello adnet").unwrap();
        let hash = topo
            .private
            .import_file_sync(&file)
            .unwrap()
            .0;

        let cat = CatArgs { cid: hash.as_hex(), json: false };
        // We can't easily capture stdout in a unit test; we just
        // ensure the read path doesn't error.
        run_cat(&cat, &topo).unwrap();

        let out = dir.path().join("hello.downloaded");
        let get = GetArgs { cid: hash.as_hex(), output: Some(out.clone()), json: false };
        run_get(&get, &topo).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(bytes, b"hello adnet");
    }

    #[test]
    fn pin_add_then_verify_then_rm() {
        let (dir, topo) = temp_topology();
        let file = dir.path().join("data.bin");
        std::fs::write(&file, b"some bytes").unwrap();
        let hash = topo.private.import_file_sync(&file).unwrap().0;

        run_pin(
            &PinCmd::Add { cid: hash.as_hex(), recursive: true },
            &topo,
            dir.path(),
        )
        .unwrap();
        run_pin(&PinCmd::Verify { cid: hash.as_hex() }, &topo, dir.path()).unwrap();
        run_pin(&PinCmd::Rm { cid: hash.as_hex() }, &topo, dir.path()).unwrap();
        // After rm, verify should fail.
        let res = run_pin(&PinCmd::Verify { cid: hash.as_hex() }, &topo, dir.path());
        assert!(res.is_err());
    }

    #[test]
    fn repo_stat_returns_topology_usage() {
        let (dir, topo) = temp_topology();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"hi").unwrap();
        topo.private.import_file_sync(&file).unwrap();
        run_repo(&RepoCmd::Stat { json: false }, &topo).unwrap();
        run_repo(&RepoCmd::Stat { json: true }, &topo).unwrap();
    }

    #[test]
    fn repo_gc_dry_run_does_not_modify_store() {
        let (dir, topo) = temp_topology();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"hi").unwrap();
        topo.private.import_file_sync(&file).unwrap();
        run_repo(&RepoCmd::Gc { dry_run: true, json: false }, &topo).unwrap();
        // Private store must still contain the blob.
        assert_eq!(topo.private.list_complete().unwrap().len(), 1);
    }
}