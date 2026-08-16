//! `a3net stats <sub>` — runtime statistics across subsystems.
//!
//! Mirrors iroh's `iroh stats {bw, ifaces}` and a subset of
//! Kubo's `ipfs stats {bw, repo, dht, bitswap, network}`. Each
//! sub-command accepts `--json` for CI-friendly output.
//!
//! ## Design choice — what we count vs. what we expose
//!
//! The Node has `metrics()` (peer count, blob count, gossip
//! topics, uptime) and `info()` (full NodeInfo with mesh +
//! relay endpoints + joined rooms). For subsystems where the
//! underlying stat is feature-gated (dht, bitswap, iroh
//! transport) we look up the subsystem handle and fall back to
//! a clear `not compiled in` JSON value. We do NOT fabricate
//! numbers — an operator reading `a3net stats network` needs
//! to know whether `peer_count: 0` means "really zero" or
//! "transport was built out".

use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::cli::StatsCmd;

/// Top-level dispatch — `a3net stats <sub>`.
pub async fn run_stats(sub: &StatsCmd, data_dir: &Path, node: &a3net_node::Node) -> Result<()> {
    match sub {
        StatsCmd::Bw { per_conn: _, json } => bandwidth_stats(node, *json).await,
        StatsCmd::Repo { json } => repo_stats(data_dir, *json),
        StatsCmd::Ifaces { json } => ifaces_stats(node, *json).await,
        StatsCmd::Dht { json } => dht_stats(node, *json).await,
        StatsCmd::Bitswap { json } => bitswap_stats(node, *json).await,
        StatsCmd::Network { json } => network_stats(node, *json).await,
    }
}

async fn bandwidth_stats(node: &a3net_node::Node, json_out: bool) -> Result<()> {
    let m = node.metrics();
    let info = node.info().await;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "node_id": node.node_id().as_hex(),
                "uptime_secs": m.uptime_secs,
                "peer_count": m.peer_count,
                "started_at": info.started_at,
                "note": "rate-in/out requires the iroh runtime; rebuild with `--features iroh` to enable",
            }))?
        );
    } else {
        println!("Node {} (uptime {}s)", node.node_id().short(), m.uptime_secs);
        println!("Connected peers: {}", m.peer_count);
        println!(
            "(per-second rate counters require the iroh runtime; rebuild with `--features iroh` to enable)"
        );
    }
    Ok(())
}

fn repo_stats(data_dir: &Path, json_out: bool) -> Result<()> {
    // `StorageTopology::shared_store()` exposes a `SharedStoreHandle`
    // but the handle doesn't yet surface a `total_bytes` accessor;
    // count blobs via the filesystem (each `.bao` under
    // `{data_dir}/shared/blobs/3/<hash-prefix>`) and sum their
    // sizes. Cheap because we only stat, not read.
    let (shared_blobs, shared_bytes) = count_blobs(&data_dir.join("shared"));
    let (private_blobs, private_bytes) = count_blobs(&data_dir.join("private"));
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "shared": {"blobs": shared_blobs, "bytes": shared_bytes},
                "private": {"blobs": private_blobs, "bytes": private_bytes},
            }))?
        );
    } else {
        println!("Shared scope : {:>10} B across {} blobs", shared_bytes, shared_blobs);
        println!("Private scope: {:>10} B across {} blobs", private_bytes, private_blobs);
    }
    Ok(())
}

/// Walk `{data_dir}/{scope}/blobs/` and return (blob_count, total_bytes).
///
/// Recursive dir walk because the scope stores blobs as
/// `<prefix>/<rest>` shards. We stat every file but don't
/// read its contents; this is cheap even on a 50 GB store.
fn count_blobs(scope_dir: &Path) -> (usize, u64) {
    let mut count = 0usize;
    let mut bytes = 0u64;
    if !scope_dir.exists() {
        return (0, 0);
    }
    let blobs = scope_dir.join("blobs");
    if !blobs.exists() {
        return (0, 0);
    }
    walk_dir(&blobs, &mut |path| {
        if path.is_file() {
            count += 1;
            bytes += path.metadata().map(|m| m.len()).unwrap_or(0);
        }
    });
    (count, bytes)
}

fn walk_dir(dir: &Path, cb: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        cb(&p);
        if p.is_dir() {
            walk_dir(&p, cb);
        }
    }
}

async fn ifaces_stats(node: &a3net_node::Node, json_out: bool) -> Result<()> {
    let info = node.info().await;
    let mesh = info.mesh.as_ref().map(|m| format!("{}:{}", m.host, m.port));
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "mesh": mesh,
                "relay": info.relay,
            }))?
        );
    } else {
        match mesh {
            Some(addr) => println!("Mesh endpoint: {}", addr),
            None => println!("(no mesh endpoint — start `a3net serve` first)"),
        }
        if let Some(r) = info.relay {
            println!("Relay endpoint: {}:{}", r.base_url, r.port);
        }
    }
    Ok(())
}

async fn dht_stats(node: &a3net_node::Node, json_out: bool) -> Result<()> {
    #[cfg(feature = "dht")]
    {
        if let Some(handle) = node.dht_handle().await {
            let stats = handle.stats();
            if json_out {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                println!("DHT local id           : {}", stats.local_id);
                println!("DHT external addr      : {:?}", stats.external_addr);
                println!("DHT provides_total     : {}", stats.metrics.provides_total);
                println!("DHT find_total         : {}", stats.metrics.find_total);
                println!("DHT find_records_total : {}", stats.metrics.find_records_total);
                println!("DHT find_misses_total  : {}", stats.metrics.find_misses_total);
                println!("DHT last_find_us       : {}", stats.metrics.last_find_latency_us);
            }
            return Ok(());
        }
    }
    let payload = json!({
        "compiled_in": cfg!(feature = "dht"),
        "initialized": false,
        "hint": "Rebuild with `--features dht` and run `a3net init --dht` to enable DHT stats.",
    });
    if json_out {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("DHT not available (rebuild with `--features dht`).");
    }
    Ok(())
}

async fn bitswap_stats(node: &a3net_node::Node, json_out: bool) -> Result<()> {
    #[cfg(feature = "bitswap")]
    {
        if let Some(handle) = node.bitswap_handle() {
            let stats = handle.stats();
            if json_out {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                println!("Bitswap connected peers  : {}", stats.connected_peers);
                println!("Bitswap local content    : {}", stats.local_content);
                println!("Bitswap pending wants    : {}", stats.pending_wants);
            }
            return Ok(());
        }
    }
    let payload = json!({
        "compiled_in": cfg!(feature = "bitswap"),
        "initialized": false,
        "hint": "Rebuild with `--features bitswap` to enable Bitswap stats.",
    });
    if json_out {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("Bitswap not available (rebuild with `--features bitswap`).");
    }
    Ok(())
}

async fn network_stats(node: &a3net_node::Node, json_out: bool) -> Result<()> {
    let m = node.metrics();
    let info = node.info().await;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "peer_count": m.peer_count,
                "gossip_topics": m.gossip_topics,
                "uptime_secs": m.uptime_secs,
                "joined_rooms": info.joined_rooms.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            }))?
        );
    } else {
        println!("Connected peers   : {}", m.peer_count);
        println!("Gossip topics     : {}", m.gossip_topics);
        println!("Uptime            : {} s", m.uptime_secs);
        if !info.joined_rooms.is_empty() {
            println!(
                "Joined rooms      : {}",
                info.joined_rooms
                    .iter()
                    .map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(())
}

use crate::storage;