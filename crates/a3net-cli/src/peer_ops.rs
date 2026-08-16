//! `a3net peer <list|connect|disconnect|status|heartbeat|tick|config>` —
//! peer table management.
//!
//! Thin wrapper around the `peer_list`, `peer_connect`,
//! `peer_disconnect`, `peer_status`, `peer_heartbeat`, `peer_tick`,
//! and `peer_config` RPC methods exposed by `NodeRpc`. The wrappers
//! add a friendlier text-mode rendering on top of the raw JSON.

use anyhow::Result;

use crate::cli::PeerCmd;
use crate::ipc_client::IpcClient;

/// Top-level dispatch — `a3net peer <sub>`.
pub async fn run_peer(sub: &PeerCmd, client: &IpcClient) -> Result<()> {
    match sub {
        PeerCmd::List { json } => {
            let raw = client.call_raw("peer_list", serde_json::json!({})).await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&raw)?);
                return Ok(());
            }
            // The daemon returns a structured peer table with
            // `peers` (array of {node_id, status, last_seen_at,
            // ...}) plus summary counters. Render a
            // human-readable table.
            let capacity = raw.get("capacity").and_then(|v| v.as_u64()).unwrap_or(0);
            let alive = raw.get("alive_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let dead = raw.get("dead_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let connecting = raw
                .get("connecting_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            println!(
                "capacity: {capacity}  alive: {alive}  dead: {dead}  connecting: {connecting}"
            );
            if let Some(arr) = raw.get("peers").and_then(|p| p.as_array()) {
                if arr.is_empty() {
                    println!("(no peers)");
                } else {
                    println!(
                        "{:<14} {:<11} {:<10} {:<24} {}",
                        "NODE", "STATUS", "FAILS", "LAST HEARTBEAT", "ALIAS"
                    );
                    for p in arr {
                        let id = p
                            .get("node_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let short = if id.len() >= 12 { &id[..12] } else { id };
                        let state = p
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let fails = p
                            .get("heartbeat_failures")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let last_hb = p
                            .get("last_heartbeat_at")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let alias = p
                            .get("alias")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        println!(
                            "{:<14} {:<11} {:<10} {:<24} {}",
                            short,
                            state,
                            fails,
                            last_hb,
                            alias
                        );
                    }
                }
            } else {
                // Fallback: legacy daemon that didn't return a
                // structured table — print the raw response.
                println!("{}", serde_json::to_string_pretty(&raw)?);
            }
        }
        PeerCmd::Connect { addr, json } => {
            let raw = client
                .call_raw("peer_connect", serde_json::json!({"addr": addr}))
                .await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&raw)?);
            } else {
                println!("connect: {}", serde_json::to_string(&raw)?);
            }
        }
        PeerCmd::Disconnect { peer_id, json } => {
            let raw = client
                .call_raw(
                    "peer_disconnect",
                    serde_json::json!({"peer_id": peer_id}),
                )
                .await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&raw)?);
            } else {
                println!("disconnect: {}", serde_json::to_string(&raw)?);
            }
        }
        PeerCmd::Status { peer_id, json } => {
            let raw = client
                .call_raw("peer_status", serde_json::json!({"peer_id": peer_id}))
                .await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&raw)?);
                return Ok(());
            }
            match raw.as_object() {
                Some(obj) if obj.contains_key("node_id") => {
                    println!("peer: {}", obj.get("node_id").and_then(|v| v.as_str()).unwrap_or("?"));
                    println!("status: {}", obj.get("status").and_then(|v| v.as_str()).unwrap_or("?"));
                    println!(
                        "is_alive: {}",
                        obj.get("is_alive").and_then(|v| v.as_bool()).unwrap_or(false)
                    );
                    println!(
                        "connected_at: {}",
                        obj.get("connected_at").and_then(|v| v.as_str()).unwrap_or("")
                    );
                    println!(
                        "last_seen_at: {}",
                        obj.get("last_seen_at").and_then(|v| v.as_str()).unwrap_or("")
                    );
                    println!(
                        "last_heartbeat_at: {}",
                        obj.get("last_heartbeat_at").and_then(|v| v.as_str()).unwrap_or("")
                    );
                    println!(
                        "heartbeat_failures: {}",
                        obj.get("heartbeat_failures").and_then(|v| v.as_u64()).unwrap_or(0)
                    );
                    if let Some(a) = obj.get("alias").and_then(|v| v.as_str()) {
                        println!("alias: {a}");
                    }
                }
                _ => {
                    println!("(peer {peer_id} not in connection table)");
                }
            }
        }
        PeerCmd::Heartbeat { peer_id, json } => {
            let raw = client
                .call_raw("peer_heartbeat", serde_json::json!({"peer_id": peer_id}))
                .await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&raw)?);
            } else {
                println!("heartbeat recorded for {peer_id}");
                if let Some(e) = raw.get("entry") {
                    println!(
                        "  status: {}",
                        e.get("status").and_then(|v| v.as_str()).unwrap_or("?")
                    );
                }
            }
        }
        PeerCmd::Tick { json } => {
            let raw = client.call_raw("peer_tick", serde_json::json!({})).await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&raw)?);
            } else {
                let pings = raw.get("pings_sent").and_then(|v| v.as_u64()).unwrap_or(0);
                let dead = raw.get("newly_dead").and_then(|v| v.as_u64()).unwrap_or(0);
                let suspect = raw.get("became_suspect").and_then(|v| v.as_u64()).unwrap_or(0);
                let recovered = raw.get("recovered").and_then(|v| v.as_u64()).unwrap_or(0);
                println!(
                    "heartbeat tick: pings_sent={pings} newly_dead={dead} became_suspect={suspect} recovered={recovered}"
                );
            }
        }
        PeerCmd::Config { json } => {
            let raw = client.call_raw("peer_config", serde_json::json!({})).await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&raw)?);
            } else {
                let max = raw.get("max_peers").and_then(|v| v.as_u64()).unwrap_or(0);
                let interval = raw
                    .get("heartbeat_interval_seconds")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let timeout = raw
                    .get("heartbeat_timeout_seconds")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                println!("max_peers: {max}");
                println!("heartbeat_interval: {interval}s");
                println!("heartbeat_timeout: {timeout}s");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::PeerCmd;

    // Most behaviour requires a running daemon; the wiring is
    // validated by main.rs integration. Pure unit coverage is
    // already covered by ipc_client tests.
    #[test]
    fn enum_variants_exist() {
        // Just make sure PeerCmd keeps its variants.
        let _ = PeerCmd::List { json: false };
        let _ = PeerCmd::Connect {
            addr: "/ip4/1.2.3.4/udp/1234/quic-v1".into(),
            json: false,
        };
        let _ = PeerCmd::Disconnect {
            peer_id: "abcd1234".into(),
            json: false,
        };
        let _ = PeerCmd::Status {
            peer_id: "abcd1234".into(),
            json: false,
        };
        let _ = PeerCmd::Heartbeat {
            peer_id: "abcd1234".into(),
            json: false,
        };
        let _ = PeerCmd::Tick { json: false };
        let _ = PeerCmd::Config { json: false };
    }
}
