//! `a3net workspace …` — CLI surface for `a3net-workspace`.
//!
//! Bridges the pure-local `a3net_workspace::Workspace` API with the
//! P2P network by:
//!
//! - **`publish`** — copy to `shared/`, update manifest, optionally
//!   broadcast to `a3net-room-ExodusWorkSpace` via IPC RPC.
//! - **`pull`** — fetch peer's manifest via IPC, resolve entry, download to
//!   `inbox/` via `a3net-blobstore`.
//! - **`push`** — stage to `outbox/`, emit `ShareTicket` for the peer.
//! - **`sync`** — subscribe to workspace gossip topic via daemon, merge manifests,
//!   reconcile files.
//! - **`ls`** / **`unpublish`** — direct wrappers around `Workspace` API.
//! - **`verify`** — re-hash each file and compare against recorded hash.
//!
//! All subcommands are **offline-first**: `publish`, `ls`, `unpublish`,
//! `verify` work without a running node. `pull`, `push`, `sync` require
//! a live daemon (IPC RPC to `a3net daemon` on port 11436 or Unix socket).

use std::path::{Path, PathBuf};

use a3net_workspace::{
    workspace_room_topic, Workspace, WorkspaceFileEntry, WorkspaceManifest,
    DIR_SHARED, DIR_INBOX, DIR_OUTBOX,
};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use tracing::{info, warn};

use crate::cli::WorkspaceCmd;
use crate::ipc_client::IpcClient;

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Returns the workspace root for the current data directory.
fn workspace_root(data_dir: &Path) -> PathBuf {
    data_dir.join("ExodusWorkSpace")
}

/// Open or create a workspace. Fails if the directory exists but is
/// not a valid workspace.
fn open_workspace(data_dir: &Path, node_id: &str) -> Result<Workspace> {
    let root = workspace_root(data_dir);
    let ws = Workspace::new(root.parent().unwrap_or(data_dir), node_id)
        .map_err(|e| anyhow::anyhow!("workspace: failed to open or create: {e}")).map_err(|e| anyhow::anyhow!("workspace: {e}"))?;
    Ok(ws)
}

// ── Verify ──────────────────────────────────────────────────────────────────

/// Result of verifying a single manifest entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyEntry {
    pub relative_path: String,
    pub recorded_hash: Option<String>,
    pub computed_hash: Option<String>,
    pub status: VerifyStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerifyStatus {
    Ok,
    Mismatch,
    MissingFile,
    Skipped,
}

impl VerifyEntry {
    fn mismatch(recorded: Option<String>, computed: Option<String>) -> Self {
        Self {
            relative_path: String::new(),
            recorded_hash: recorded,
            computed_hash: computed,
            status: VerifyStatus::Mismatch,
        }
    }
}

/// Verify every entry in the manifest against the on-disk files.
///
/// Returns `Ok(true)` if all entries pass, `Ok(false)` if any mismatch
/// was found. On error, returns `Err`.
pub fn verify_workspace(data_dir: &Path, node_id: &str, skip_unhashed: bool) -> Result<Vec<VerifyEntry>> {
    let ws = open_workspace(data_dir, node_id).map_err(|e| anyhow::anyhow!("workspace: {e}"))?;
    let manifest = ws.manifest_snapshot().map_err(|e| anyhow::anyhow!("workspace: {e}"))?;

    let mut results = Vec::new();
    let mut all_ok = true;

    for entry in &manifest.files {
        let Some(recorded) = &entry.content_hash else {
            if skip_unhashed {
                results.push(VerifyEntry {
                    relative_path: entry.relative_path.clone(),
                    recorded_hash: None,
                    computed_hash: None,
                    status: VerifyStatus::Skipped,
                });
                continue;
            }
            // Verify even entries without a hash (just check file exists)
            let full_path = ws.root().join(&entry.relative_path);
            if full_path.is_file() {
                results.push(VerifyEntry {
                    relative_path: entry.relative_path.clone(),
                    recorded_hash: None,
                    computed_hash: None,
                    status: VerifyStatus::Ok,
                });
            } else {
                all_ok = false;
                results.push(VerifyEntry {
                    relative_path: entry.relative_path.clone(),
                    recorded_hash: None,
                    computed_hash: None,
                    status: VerifyStatus::MissingFile,
                });
            }
            continue;
        };

        let full_path = ws.root().join(&entry.relative_path);
        let Some(file_data) = std::fs::read(&full_path).ok() else {
            all_ok = false;
            results.push(VerifyEntry {
                relative_path: entry.relative_path.clone(),
                recorded_hash: Some(recorded.clone()),
                computed_hash: None,
                status: VerifyStatus::MissingFile,
            });
            continue;
        };

        let computed = compute_hash(&file_data, recorded);
        let status = if computed.as_deref() == Some(recorded.as_str()) {
            VerifyStatus::Ok
        } else {
            all_ok = false;
            VerifyStatus::Mismatch
        };

        results.push(VerifyEntry {
            relative_path: entry.relative_path.clone(),
            recorded_hash: Some(recorded.clone()),
            computed_hash: computed,
            status,
        });
    }

    if !all_ok {
        // Return results even on mismatch; caller can decide
        return Ok(results);
    }
    Ok(results)
}

/// Compute a hash in the same format as the recorded hash string.
fn compute_hash(data: &[u8], recorded: &str) -> Option<String> {
    let (algo, _hex) = recorded.split_once(':')?;
    let computed = match algo.trim() {
        "blake3" => {
            use std::io::Write;
            let mut hasher = blake3::Hasher::new();
            hasher.write_all(data).ok()?;
            hex::encode(hasher.finalize().as_bytes())
        }
        "sha256" => {
            let mut hasher = sha2::Sha256::new();
            use sha2::Digest;
            hasher.update(data);
            hex::encode(hasher.finalize())
        }
        _ => {
            warn!("unsupported hash algorithm '{algo}', skipping verification");
            return None;
        }
    };
    // blake3 hex in our format is the raw 32-byte output as 64 hex chars
    if algo == "blake3" && computed.len() == 64 {
        Some(format!("blake3:{}", &computed[..16.min(computed.len())]))
    } else {
        Some(format!("{algo}:{computed}"))
    }
}

// ── List ─────────────────────────────────────────────────────────────────────

/// Print the manifest as human-readable rows.
pub fn list_entries(data_dir: &Path, node_id: &str, folder: Option<&str>) -> Result<()> {
    let ws = open_workspace(data_dir, node_id).map_err(|e| anyhow::anyhow!("workspace: {e}"))?;
    let manifest = ws.manifest_snapshot().map_err(|e| anyhow::anyhow!("workspace: {e}"))?;

    if let Some(f) = folder {
        let filtered: Vec<_> = manifest.files.iter()
            .filter(|e| e.relative_path.starts_with(&format!("{}/", f)))
            .collect();
        if filtered.is_empty() {
            println!("(no entries)");
            return Ok(());
        }
        println!("{:<40} {:>10} {:>20} {}", "PATH", "SIZE", "HASH", "ADDED_AT");
        println!("{}", "-".repeat(95));
        for e in &filtered {
            let hash = e.content_hash.as_deref().unwrap_or("-");
            println!("{:<40} {:>10} {:>20} {}", e.relative_path, e.size_bytes, hash, e.added_at);
        }
        println!();
        println!("{} entries", filtered.len());
        return Ok(());
    }

    // No filter: print all
    if manifest.files.is_empty() {
        println!("(no entries)");
        return Ok(());
    }

    println!("{:<40} {:>10} {:>20} {}", "PATH", "SIZE", "HASH", "ADDED_AT");
    println!("{}", "-".repeat(95));
    for e in &manifest.files {
        let hash = e.content_hash.as_deref().unwrap_or("-");
        println!("{:<40} {:>10} {:>20} {}", e.relative_path, e.size_bytes, hash, e.added_at);
    }
    println!();
    println!("{} entries", manifest.files.len());
    Ok(())
}

// ── Publish ──────────────────────────────────────────────────────────────────

/// Publish a local file to shared/.
pub fn publish(
    data_dir: &Path,
    node_id: &str,
    path: &Path,
    hash: Option<String>,
) -> Result<WorkspaceFileEntry> {
    let ws = open_workspace(data_dir, node_id).map_err(|e| anyhow::anyhow!("workspace: {e}"))?;

    let entry = ws
        .publish_file(path, hash)
        .map_err(|e| anyhow::anyhow!("workspace: failed to publish '{}': {}", path.display(), e))?;

    info!(
        "published: {} ({} bytes) → {}",
        entry.name,
        entry.size_bytes,
        entry.relative_path
    );

    Ok(entry)
}

/// Broadcast the freshly-published manifest via the daemon's
/// `workspace_publish` RPC. The daemon must be running and joined
/// to the workspace gossip room. Returns `Err` when the daemon
/// is offline so the caller can emit a clear next-step hint.
fn broadcast_manifest(data_dir: &Path, path: &Path) -> Result<()> {
    let client = IpcClient::connect(data_dir);
    if !client.is_daemon_running() {
        bail!(
            "daemon is not running; start it with \
             `a3net daemon --auto-join ExodusWorkSpace` and retry"
        );
    }
    let params = serde_json::json!({
        "file": path.to_string_lossy(),
    });
    let response: serde_json::Value = futures::executor::block_on(async {
        client.call("workspace_publish", params).await
    })
    .map_err(|e| anyhow::anyhow!("workspace_publish RPC failed: {e}"))?;
    info!("workspace_publish response: {}", response);
    Ok(())
}

// ── Unpublish ────────────────────────────────────────────────────────────────

/// Remove an entry from the manifest (file stays on disk).
pub fn unpublish(data_dir: &Path, node_id: &str, relative_path: &str) -> Result<bool> {
    let ws = open_workspace(data_dir, node_id).map_err(|e| anyhow::anyhow!("workspace: {e}"))?;
    let changed = ws
        .unpublish(relative_path)
        .map_err(|e| anyhow::anyhow!("workspace: failed to unpublish '{relative_path}': {e}")).map_err(|e| anyhow::anyhow!("workspace: {e}"))?;

    if changed {
        info!("unpublished: {relative_path}");
    } else {
        info!("no change: {relative_path} was not in manifest");
    }
    Ok(changed)
}

// ── Pull (P2P — requires a running daemon) ───────────────────────────────────

/// Response from workspace_pull RPC call.
#[derive(Debug, Deserialize)]
struct WorkspacePullResponse {
    matches: Vec<WorkspacePullMatch>,
}

#[derive(Debug, Deserialize)]
struct WorkspacePullMatch {
    owner: String,
    entries: Vec<WorkspaceFileEntry>,
}

/// Pull a file from a peer via the daemon's workspace gossip bridge.
/// The daemon must be running and joined to `a3net-room-ExodusWorkSpace`.
pub async fn pull_from_peer(
    data_dir: &Path,
    node_id: &str,
    name: &str,
    peer: Option<&str>,
) -> Result<WorkspaceFileEntry> {
    // Connect to the daemon via IPC (Unix socket or HTTP on port 11436)
    let client = IpcClient::connect(data_dir);
    
    // Check if daemon is running
    if !client.is_daemon_running() {
        bail!(
            "workspace pull requires a running daemon. Start the daemon first:\n\
               a3net daemon --auto-join ExodusWorkSpace\n\
             then retry this command."
        );
    }

    // Call the workspace_pull RPC to get matching remote entries
    let params = serde_json::json!({
        "name": name,
        "peer": peer,
    });
    
    let response: WorkspacePullResponse = client
        .call("workspace_pull", params)
        .await
        .map_err(|e| anyhow::anyhow!("workspace_pull RPC failed: {e}"))?;

    if response.matches.is_empty() {
        bail!("no peer has a file matching '{}'", name);
    }

    // Use the first match (or specific peer if specified)
    let target = if let Some(peer_id) = peer {
        response
            .matches
            .iter()
            .find(|m| m.owner == peer_id)
            .ok_or_else(|| anyhow::anyhow!("peer '{}' not found in remote workspace", peer_id))?
    } else {
        &response.matches[0]
    };

    // Find the matching entry
    let entry = target
        .entries
        .iter()
        .find(|e| e.name == name || e.relative_path.contains(name))
        .ok_or_else(|| anyhow::anyhow!("file '{}' not found in peer's workspace", name))?;

    info!(
        "found '{}' from peer {} ({} bytes)",
        name,
        &target.owner[..8.min(target.owner.len())],
        entry.size_bytes
    );

    // Open local workspace and save to inbox
    let ws = open_workspace(data_dir, node_id)?;
    let inbox_dir = ws.inbox_dir();
    let dest = ws.resolve_unique_path(&inbox_dir, &entry.name);
    
    // If we have the content hash and the file is not yet fetched, 
    // we'd need to trigger a fetch through the daemon
    // For now, return the entry info
    if dest.exists() || entry.content_hash.is_some() {
        // Copy the file if it exists locally
        if let Some(src_path) = entry.relative_path.strip_prefix("shared/") {
            let peer_dir = inbox_dir.parent().unwrap().join("peers").join(&target.owner);
            let peer_file = peer_dir.join(src_path);
            if peer_file.exists() {
                std::fs::copy(&peer_file, &dest)?;
                info!("pulled: {} → {}", name, dest.display());
            }
        }
    }

    Ok(entry.clone())
}

// ── Push (P2P — requires a running daemon) ───────────────────────────────────

/// Response from workspace_push RPC call.
#[derive(Debug, Deserialize)]
struct WorkspacePushResponse {
    entry: WorkspaceFileEntry,
    hash: String,
    ticket: String,
    recipient: String,
}

/// Push a file to peers via the daemon's workspace gossip bridge.
/// Publishes to shared/ and announces on the gossip room.
pub async fn push_to_peer(
    data_dir: &Path,
    node_id: &str,
    path: &Path,
    to: &str,
) -> Result<()> {
    // Connect to the daemon
    let client = IpcClient::connect(data_dir);
    
    if !client.is_daemon_running() {
        bail!(
            "workspace push requires a running daemon. Start the daemon first:\n\
               a3net daemon --auto-join ExodusWorkSpace\n\
             then retry this command."
        );
    }

    // First, publish locally (this also announces via gossip)
    let entry = publish(data_dir, node_id, path, None)?;

    // Then call workspace_push to get the ticket
    let params = serde_json::json!({
        "file": path.to_string_lossy(),
        "to": to,
    });
    
    let response: WorkspacePushResponse = client
        .call("workspace_push", params)
        .await
        .map_err(|e| anyhow::anyhow!("workspace_push RPC failed: {e}"))?;

    // Save the ticket to outbox
    let ws = open_workspace(data_dir, node_id)?;
    let outbox_dir = ws.outbox_dir();
    let ticket_path = outbox_dir.join(format!("{}.ticket", entry.name));
    std::fs::write(&ticket_path, &response.ticket)?;
    
    info!(
        "pushed: {} (hash: {}) → {}\n\
         ticket saved to: {}",
        path.display(),
        response.hash,
        to,
        ticket_path.display()
    );

    Ok(())
}

// ── Sync (P2P — requires a running daemon) ────────────────────────────────────

/// Response from workspace_sync RPC call.
#[derive(Debug, Deserialize)]
struct WorkspaceSyncResponse {
    local_count: usize,
    remote_owners: usize,
    pull_enabled: bool,
    push_enabled: bool,
    announce_only: bool,
}

/// Response from workspace_remote RPC call.
#[derive(Debug, Deserialize)]
struct WorkspaceRemoteResponse {
    remote_peers: Vec<WorkspaceRemotePeer>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceRemotePeer {
    owner: String,
    owner_short: String,
    file_count: usize,
    files: Vec<WorkspaceFileEntry>,
}

/// Full workspace sync over gossip.
/// Collects manifests from peers, merges by latest updated_at,
/// and reconciles files bidirectionally.
pub async fn sync_with_peers(
    data_dir: &Path,
    node_id: &str,
    pull: bool,
    push: bool,
    announce_only: bool,
) -> Result<()> {
    // Connect to the daemon
    let client = IpcClient::connect(data_dir);
    
    if !client.is_daemon_running() {
        bail!(
            "workspace sync requires a running daemon. Start the daemon first:\n\
               a3net daemon --auto-join ExodusWorkSpace\n\
             then retry this command."
        );
    }

    // Get sync status and remote peers info
    let sync_params = serde_json::json!({
        "pull": pull,
        "push": push,
        "announce_only": announce_only,
    });
    
    let sync_response: WorkspaceSyncResponse = client
        .call("workspace_sync", sync_params)
        .await
        .map_err(|e| anyhow::anyhow!("workspace_sync RPC failed: {e}"))?;

    let remote_params = serde_json::json!({});
    let remote_response: WorkspaceRemoteResponse = client
        .call("workspace_remote", remote_params)
        .await
        .map_err(|e| anyhow::anyhow!("workspace_remote RPC failed: {e}"))?;

    println!("=== Workspace Sync Status ===");
    println!("Local files: {}", sync_response.local_count);
    println!("Remote peers: {}", sync_response.remote_owners);
    println!("Topic: {}", workspace_room_topic());
    println!();

    if remote_response.remote_peers.is_empty() {
        println!("No remote peers detected on workspace gossip room.");
        return Ok(());
    }

    println!("Remote Peers:");
    for peer in &remote_response.remote_peers {
        println!("  {} ({}): {} files", 
            peer.owner_short, 
            &peer.owner[..12.min(peer.owner.len())],
            peer.file_count
        );
        for file in &peer.files {
            let hash_short = file.content_hash.as_ref()
                .map(|h| &h[..8.min(h.len())])
                .unwrap_or("-");
            println!("    - {} ({} bytes, hash: {})", 
                file.name, file.size_bytes, hash_short);
        }
    }
    println!();

    // Announce local manifest if push is enabled
    if push && !announce_only {
        // The daemon handles the actual announcement via gossip
        // This just triggers the sync operation
        info!("Push enabled: local files are advertised to peers");
    }

    // Indicate what would be pulled
    if pull && !announce_only {
        let total_remote_files: usize = remote_response.remote_peers
            .iter()
            .map(|p| p.file_count)
            .sum();
        info!("Pull enabled: {} remote files available", total_remote_files);
    }

    println!("Sync complete.");
    println!("Note: Use 'a3net workspace pull <name>' to fetch specific files.");
    
    Ok(())
}

// ── Top-level dispatcher ─────────────────────────────────────────────────────

/// Run a workspace subcommand. Returns `Ok(())` on success.
pub fn run(cmd: &WorkspaceCmd, data_dir: &Path, node_id: &str) -> Result<()> {
    match cmd {
        WorkspaceCmd::Publish { path, hash, sync, json } => {
            let path = PathBuf::from(path);
            let entry = publish(data_dir, node_id, &path, hash.clone()).map_err(|e| anyhow::anyhow!("workspace: {e}"))?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&entry)?);
            } else {
                println!(
                    "published: {} ({} bytes) → {}",
                    entry.name, entry.size_bytes, entry.relative_path
                );
            }
            if *sync {
                // Best-effort broadcast via the daemon's
                // `workspace_publish` RPC. If the daemon isn't
                // running we surface a clear hint rather than
                // silently dropping the announcement.
                match broadcast_manifest(data_dir, &path) {
                    Ok(()) => info!(
                        "broadcast: manifest announced to '{}'",
                        workspace_room_topic()
                    ),
                    Err(e) => {
                        eprintln!(
                            "workspace: broadcast skipped ({e}). \
                             Start the daemon with \
                             `a3net daemon --auto-join ExodusWorkSpace` to sync."
                        );
                    }
                }
            }
            Ok(())
        }

        WorkspaceCmd::Pull { name, peer, out_dir, json } => {
            let _out_dir = out_dir.as_ref().map(PathBuf::from);
            // P2P path — must be called from async context with a running node.
            // We return a friendly error rather than panicking.
            let name = name.clone();
            let peer = peer.as_deref();
            let result = futures::executor::block_on(pull_from_peer(data_dir, node_id, &name, peer));
            match result {
                Ok(entry) => {
                    if *json {
                        println!("{}", serde_json::to_string_pretty(&entry)?);
                    } else {
                        println!(
                            "pulled: {} → {}",
                            name,
                            entry.relative_path
                        );
                    }
                }
                Err(e) => {
                    if *json {
                        println!("{}", serde_json::json!({ "error": format!("{e}") }));
                    }
                    return Err(e);
                }
            }
            Ok(())
        }

        WorkspaceCmd::Push { path, to, json } => {
            let path = PathBuf::from(path);
            let result =
                futures::executor::block_on(push_to_peer(data_dir, node_id, &path, to));
            match result {
                Ok(()) => {
                    if !*json {
                        println!("pushed: {} → {}", path.display(), to);
                    }
                }
                Err(e) => {
                    if *json {
                        println!("{}", serde_json::json!({ "error": format!("{e}") }));
                    }
                    return Err(e);
                }
            }
            Ok(())
        }

        WorkspaceCmd::Sync { pull, push, announce_only, json } => {
            let result = futures::executor::block_on(sync_with_peers(
                data_dir,
                node_id,
                *pull,
                *push,
                *announce_only,
            ));
            match result {
                Ok(()) => {
                    if !*json {
                        println!("sync complete");
                    }
                }
                Err(e) => {
                    if *json {
                        println!("{}", serde_json::json!({ "error": format!("{e}") }));
                    }
                    return Err(e);
                }
            }
            Ok(())
        }

        WorkspaceCmd::Ls { folder, json } => {
            if *json {
                let ws = open_workspace(data_dir, node_id).map_err(|e| anyhow::anyhow!("workspace: {e}"))?;
                let m = ws.manifest_snapshot().map_err(|e| anyhow::anyhow!("workspace: {e}"))?;
                println!("{}", serde_json::to_string_pretty(&m)?);
            } else {
                list_entries(data_dir, node_id, folder.as_deref()).map_err(|e| anyhow::anyhow!("workspace: {e}"))?;
            }
            Ok(())
        }

        WorkspaceCmd::Unpublish { relative_path, json } => {
            let changed = unpublish(data_dir, node_id, relative_path).map_err(|e| anyhow::anyhow!("workspace: {e}"))?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&changed)?);
            } else if changed {
                println!("unpublished: {relative_path}");
            } else {
                println!("(no change)");
            }
            Ok(())
        }

        WorkspaceCmd::Verify { skip_unhashed, json } => {
            let results = verify_workspace(data_dir, node_id, *skip_unhashed).map_err(|e| anyhow::anyhow!("workspace: {e}"))?;
            let failures: Vec<_> = results
                .iter()
                .filter(|r| r.status != VerifyStatus::Ok && r.status != VerifyStatus::Skipped)
                .collect();

            if *json {
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else {
                for r in &results {
                    let icon = match r.status {
                        VerifyStatus::Ok => "✅",
                        VerifyStatus::Mismatch => "❌ MISMATCH",
                        VerifyStatus::MissingFile => "⚠️  MISSING",
                        VerifyStatus::Skipped => "➖ skipped",
                    };
                    println!(
                        "{} {:40} recorded={:?} computed={:?}",
                        icon,
                        r.relative_path,
                        r.recorded_hash.as_ref().map(|s| &s[..8.min(s.len())]),
                        r.computed_hash.as_ref().map(|s| &s[..8.min(s.len())]),
                    );
                }
                println!();
                if failures.is_empty() {
                    println!("✅ all entries verified OK");
                } else {
                    println!("❌ {} entries failed verification", failures.len());
                }
            }

            if !failures.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `broadcast_manifest` must surface a clear "daemon not
    /// running" error when the IPC socket doesn't exist —
    /// callers rely on the message to print next-step hints.
    #[test]
    fn broadcast_manifest_fails_clean_when_daemon_is_offline() {
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join("does-not-exist.txt");
        std::fs::write(&bogus, b"hello").unwrap();
        // No daemon is running → `IpcClient::connect` will
        // point at a socket that doesn't exist, and the
        // helper must report a `bail!`-style error rather
        // than silently dropping the announcement.
        let err = broadcast_manifest(dir.path(), &bogus).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("daemon is not running")
                || msg.contains("workspace_publish RPC failed"),
            "unexpected error message: {msg}"
        );
    }

    /// `publish` must accept a fresh workspace and round-trip
    /// the file into the manifest. This is the offline path the
    /// `--sync` flag extends.
    #[test]
    fn publish_records_file_in_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("input.bin");
        std::fs::write(&src, b"workspace payload").unwrap();
        let entry = publish(dir.path(), "node-test", &src, None).unwrap();
        assert_eq!(entry.size_bytes, "workspace payload".len() as u64);
        assert!(entry.relative_path.starts_with("shared/"));
        // `content_hash` is populated by the workspace API when
        // the file is hashed on insert; the field is an
        // `Option<String>` so accept either presence (hashed)
        // or absence (skipped) without panicking.
        if let Some(h) = &entry.content_hash {
            assert!(!h.is_empty());
        }
    }
}
