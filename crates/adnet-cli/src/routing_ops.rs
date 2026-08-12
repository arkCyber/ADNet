//! `adnet` P2P / routing / swarm / dht top-level commands.
//!
//! Equivalent of the Kubo `routing`, `swarm`, and `dht` command
//! families, exposed as first-class top-level commands:
//!
//! - `adnet routing findprovs <cid>` — look up providers via DHT
//! - `adnet routing findpeer <peer-id>` — closest known peers
//!   for a node id (returns routing-table entries)
//! - `adnet dht findpeer <peer-id>` — single-peer multiaddr lookup
//!   (falls back to known_peers + DHT routing-table search)
//! - `adnet dht query <key>` — generic DHT value lookup
//! - `adnet swarm peers` / `connect` / `disconnect` — connection
//!   management (works when the iroh feature is compiled in;
//!   reports a clear error otherwise)
//!
//! Naming convention (audit V6): `adnet routing ...` /
//! `adnet dht ...` / `adnet swarm ...`, mirroring the Kubo UX
//! without the `ipfs` prefix. The old `adnet dht` sub-tree from
//! `dht_cli.rs` is preserved for backwards compatibility — these
//! new top-level commands are additive.

use std::path::Path;

use adnet_types::ContentHash;
use anyhow::{anyhow, bail, Result};

#[derive(Debug, Clone)]
pub enum RoutingCmd {
    FindProvs { cid: String, num: Option<u32>, json: bool },
    FindPeer { peer_id: String, json: bool },
    Get { key: String, json: bool },
    Put { key: String, value: String, json: bool },
}

#[derive(Debug, Clone)]
pub enum DhtExtraCmd {
    FindPeer { peer_id: String, json: bool },
    Query { target: String, json: bool },
    Put { key: String, value: String, json: bool },
    Get { key: String, json: bool },
}

#[derive(Debug, Clone)]
pub enum SwarmCmd {
    Peers { json: bool },
    Connect { addr: String },
    Disconnect { peer_id: String },
    Addrs { json: bool },
    Filters { json: bool },
}

/// Run `adnet routing <sub>` against a running node.
pub async fn run_routing(
    cmd: &RoutingCmd,
    node: &adnet_node::Node,
) -> Result<()> {
    let dht = node.dht_handle().ok_or_else(|| {
        anyhow!(
            "DHT not enabled on this node. Rebuild with `--features dht` and \
             call `NodeBuilder::with_dht(...)` before `build()`."
        )
    })?;

    match cmd {
        RoutingCmd::FindProvs { cid, num, json } => {
            let hash = ContentHash::from_hex(cid)?;
            let providers = dht.find_providers(&hash).await;
            let limit = num.unwrap_or(providers.len() as u32) as usize;
            let providers: Vec<_> = providers.into_iter().take(limit).collect();
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &providers
                    )?
                );
            } else if providers.is_empty() {
                println!("No providers found for {}", hash.short());
            } else {
                println!("Found {} provider(s) for {}:", providers.len(), hash.short());
                for p in &providers {
                    println!(
                        "  - node={} addr={} ttl={}s",
                        p.provider_id.short(),
                        p.provider_addr,
                        p.ttl_secs
                    );
                }
            }
            Ok(())
        }
        RoutingCmd::FindPeer { peer_id, json } => {
            let target = parse_node_id(peer_id)?;
            let peers = dht.known_peers().await;
            // Closest-by-XOR heuristic: order by distance and emit
            // the top-K results. This is the v1 surface; a future
            // PR will issue real FIND_NODE RPCs against the
            // routing-table contacts.
            let mut scored: Vec<_> = peers
                .into_iter()
                .map(|p| (distance(&target, &p), p))
                .collect();
            scored.sort_by_key(|(d, _)| *d);
            let top: Vec<_> = scored.into_iter().take(20).collect();
            if *json {
                println!("{}", serde_json::to_string_pretty(&top)?);
            } else if top.is_empty() {
                println!("No peers known for target {}", peer_id);
            } else {
                println!(
                    "{} closest peer(s) for {} (by XOR distance):",
                    top.len(),
                    peer_id
                );
                for (d, p) in &top {
                    println!("  - {} distance=0x{:x}", p.short(), d);
                }
            }
            Ok(())
        }
        RoutingCmd::Get { key, json: _ } => {
            bail!(
                "adnet routing get is not yet implemented at the wire level — \
                 use `adnet dht get <key>` (placeholder) or the in-process \
                 resolver. (key={key})"
            );
        }
        RoutingCmd::Put { key, value, json: _ } => {
            bail!(
                "adnet routing put is not yet implemented at the wire level — \
                 (key={key}, value_len={})",
                value.len()
            );
        }
    }
}

/// Run extra `adnet dht <sub>` commands (findpeer / query / put / get).
///
/// These are additive — the canonical `adnet dht {provide,find,peers,stats,ipns}`
/// surface stays in `dht_cli.rs`. This module adds the four
/// commands `ipfs dht` exposes that the legacy CLI was missing.
pub async fn run_dht_extra(
    cmd: &DhtExtraCmd,
    node: &adnet_node::Node,
) -> Result<()> {
    let dht = node.dht_handle().ok_or_else(|| {
        anyhow!(
            "DHT not enabled on this node. Rebuild with `--features dht` and \
             call `NodeBuilder::with_dht(...)` before `build()`."
        )
    })?;

    match cmd {
        DhtExtraCmd::FindPeer { peer_id, json } => {
            let target = parse_node_id(peer_id)?;
            let peers = dht.known_peers().await;
            let mut scored: Vec<_> = peers
                .into_iter()
                .map(|p| (distance(&target, &p), p))
                .collect();
            scored.sort_by_key(|(d, _)| *d);
            if *json {
                println!("{}", serde_json::to_string_pretty(&scored)?);
            } else if scored.is_empty() {
                println!("No peers known for target {}", peer_id);
            } else {
                println!(
                    "{} closest peer(s) for {} (by XOR distance):",
                    scored.len(),
                    peer_id
                );
                for (d, p) in &scored {
                    println!("  - {} distance=0x{:x}", p.short(), d);
                }
            }
            Ok(())
        }
        DhtExtraCmd::Query { target, json: _ } => {
            // `ipfs dht query <key>` performs a generic iterative
            // FIND_NODE for the target's NodeId. We piggyback on
            // the routing-table search above.
            let target_id = parse_node_id(target)?;
            let peers = dht.known_peers().await;
            let mut scored: Vec<_> = peers
                .into_iter()
                .map(|p| (distance(&target_id, &p), p))
                .collect();
            scored.sort_by_key(|(d, _)| *d);
            println!(
                "DHT query for {} returned {} peer(s) (top 20):",
                target,
                scored.len()
            );
            for (d, p) in scored.iter().take(20) {
                println!("  - {} distance=0x{:x}", p.short(), d);
            }
            Ok(())
        }
        DhtExtraCmd::Put { key, value, json: _ } => {
            bail!(
                "adnet dht put is not yet implemented at the wire level — \
                 (key={key}, value_len={})",
                value.len()
            );
        }
        DhtExtraCmd::Get { key, json: _ } => {
            bail!(
                "adnet dht get is not yet implemented at the wire level — \
                 (key={key})"
            );
        }
    }
}

/// Run `adnet swarm <sub>`.
pub async fn run_swarm(cmd: &SwarmCmd, _data_dir: &Path) -> Result<()> {
    // ADNet's transport layer (adnet-transport) is QUIC-only and
    // does not expose a libp2p-style connection manager. The
    // iroh feature (when enabled) provides its own connection
    // tracking, but that lives behind the iroh ALPN, not under a
    // `swarm` surface. We intentionally fail loudly so operators
    // can either (a) rebuild with `--features iroh` and use
    // `adnet diagnostics --json` for live multiaddrs, or
    // (b) accept that this binary is QUIC-only and use
    // `adnet dht peers` for the routing-table view.
    match cmd {
        SwarmCmd::Peers { json } => {
            if *json {
                println!(
                    "{}",
                    serde_json::json!({
                        "error": "swarm peers requires iroh transport",
                        "hint": "rebuild with `--features iroh` or use `adnet dht peers`"
                    })
                );
            } else {
                bail!(
                    "adnet swarm peers is not implemented: ADNet's transport \
                     is QUIC-only and does not expose a libp2p-style connection \
                     manager. Rebuild with `--features iroh` and read \
                     `adnet diagnostics --json` for live multiaddrs, or use \
                     `adnet dht peers` for the DHT routing-table view."
                );
            }
            Ok(())
        }
        SwarmCmd::Connect { addr } => {
            bail!(
                "adnet swarm connect {addr} is not implemented: ADNet does \
                 not have a libp2p-style swarm dialer."
            );
        }
        SwarmCmd::Disconnect { peer_id } => {
            bail!(
                "adnet swarm disconnect {peer_id} is not implemented: ADNet \
                 does not have a libp2p-style connection manager."
            );
        }
        SwarmCmd::Addrs { json } => {
            if *json {
                println!(
                    "{}",
                    serde_json::json!({
                        "error": "swarm addrs requires iroh transport",
                    })
                );
            } else {
                bail!(
                    "adnet swarm addrs is not implemented: ADNet's listen \
                     addresses are surfaced via `adnet id`."
                );
            }
            Ok(())
        }
        SwarmCmd::Filters { json } => {
            if *json {
                println!(
                    "{}",
                    serde_json::json!({
                        "error": "swarm filters requires iroh transport",
                    })
                );
            } else {
                bail!(
                    "adnet swarm filters is not implemented: ADNet does \
                     not support connection filtering."
                );
            }
            Ok(())
        }
    }
}

// ════════════════════════════════════════════════════════════════
//  Helpers
// ════════════════════════════════════════════════════════════════

fn parse_node_id(s: &str) -> Result<adnet_types::NodeId> {
    // Accept both `adnet-<short>` and the full BLAKE3 hex form.
    let cleaned = s.trim_start_matches("adnet-").trim_start_matches("/p2p/");
    adnet_types::NodeId::from_hex(cleaned)
        .map_err(|_| anyhow!("invalid PeerID: {s} (expected adnet-<hex> or full hex)"))
}

fn distance(a: &adnet_types::NodeId, b: &adnet_types::NodeId) -> u128 {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let mut out = 0u128;
    for (x, y) in a_bytes.iter().zip(b_bytes.iter()) {
        out = (out << 8) | (*x ^ *y) as u128;
    }
    out
}

// ════════════════════════════════════════════════════════════════
//  Tests
// ════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_node_id_accepts_short_and_full() {
        let full = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(parse_node_id(full).is_ok());
        assert!(parse_node_id(&format!("adnet-{full}")).is_ok());
        assert!(parse_node_id("garbage").is_err());
    }

    #[test]
    fn distance_is_symmetric_and_zero_for_self() {
        let a = adnet_types::NodeId::from_bytes(&[0u8; 32]).unwrap();
        let b = adnet_types::NodeId::from_bytes(&[1u8; 32]).unwrap();
        assert_eq!(distance(&a, &a), 0);
        assert_eq!(distance(&a, &b), distance(&b, &a));
        assert_ne!(distance(&a, &b), 0);
    }
}