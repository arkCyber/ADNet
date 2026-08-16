//! `a3net dht` — DHT command implementations.
//!
//! These handlers use the real `a3net_node::Node` DHT API
//! surface (`dht_provide`, `dht_find_providers`, `dht_find_node`,
//! `dht_num_peers`, `dht_stats`). When `a3net-node` is built
//! without the `dht` feature the methods are not compiled in,
//! so the handlers return a clear error message that points
//! the operator at the missing build flag.
//!
//! Commands:
//! - `a3net dht provide <cid>` — announce that we provide a CID
//! - `a3net dht findprovs <cid>` — find providers for a CID
//! - `a3net dht findpeer <peer-id>` — closest peers (Kademlia
//!   iterative FIND_NODE)
//! - `a3net dht peers` — list routing table peers
//! - `a3net dht stats` — show DHT statistics

use anyhow::{anyhow, Result};
use a3net_types::ContentHash;

/// Parse a CID (hex string, possibly with `/ipfs/` prefix).
fn parse_cid(s: &str) -> Result<ContentHash> {
    let s = s.trim_start_matches("/ipfs/");
    a3net_types::ContentHash::from_hex(s)
        .map_err(|_| anyhow!("invalid CID: {s}"))
}

/// Parse a peer ID / NodeId.
fn parse_node_id(s: &str) -> Result<a3net_types::NodeId> {
    let cleaned = s.trim_start_matches("a3net-").trim_start_matches("/p2p/");
    a3net_types::NodeId::from_hex(cleaned)
        .map_err(|_| anyhow!("invalid PeerID: {s}"))
}

/// Run `a3net dht provide <cid>`.
#[cfg(feature = "dht")]
pub async fn run_dht_provide(
    cid: &str,
    node: &a3net_node::Node,
) -> Result<()> {
    let hash = parse_cid(cid)?;
    node.dht_provide(&hash).await
}

/// Run `a3net dht provide <cid>` when the `dht` feature is off.
#[cfg(not(feature = "dht"))]
pub async fn run_dht_provide(
    cid: &str,
    _node: &a3net_node::Node,
) -> Result<()> {
    let _ = parse_cid(cid)?;
    anyhow::bail!(
        "DHT not enabled. Rebuild a3net-cli with `--features dht` to use `a3net dht provide`."
    )
}

/// Run `a3net dht findprovs <cid>`.
#[cfg(feature = "dht")]
pub async fn run_dht_findprovs(
    cid: &str,
    _num: Option<u32>,
    node: &a3net_node::Node,
) -> Result<()> {
    let hash = parse_cid(cid)?;
    let providers = node.dht_find_providers(&hash).await?;
    println!("found {} provider(s) for {}", providers.len(), hash.as_hex());
    for p in &providers {
        println!("  - {}", p.provider_id);
    }
    Ok(())
}

/// Run `a3net dht findprovs <cid>` when the `dht` feature is off.
#[cfg(not(feature = "dht"))]
pub async fn run_dht_findprovs(
    cid: &str,
    _num: Option<u32>,
    _node: &a3net_node::Node,
) -> Result<()> {
    let _ = parse_cid(cid)?;
    anyhow::bail!(
        "DHT not enabled. Rebuild a3net-cli with `--features dht` to use `a3net dht findprovs`."
    )
}

/// Run `a3net dht findpeer <peer-id>` — iterative FIND_NODE
/// query against the local routing table. The full network
/// reach is wired in a follow-up PR; today the result is the
/// closest `KBUCKET_SIZE` contacts the local node has cached.
#[cfg(feature = "dht")]
pub async fn run_dht_findpeer(
    peer_id: &str,
    json: bool,
    node: &a3net_node::Node,
) -> Result<()> {
    let target = parse_node_id(peer_id)?;
    let result = node.dht_find_node(&target).await?;

    if json {
        let peer_ids: Vec<String> = result.peers.iter().map(|p| p.id.to_string()).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "peer_id": peer_id,
                "closest_peers": peer_ids.len(),
                "peer_ids": peer_ids,
                "query_id": result.query_id,
                "duration_ms": result.duration_ms,
                "timed_out": result.timed_out,
            }))?
        );
    } else {
        println!(
            "iterative FIND_NODE for {} → {} closest peer(s) (query_id={}, duration_ms={}, timed_out={})",
            peer_id,
            result.peers.len(),
            result.query_id,
            result.duration_ms,
            result.timed_out
        );
        for p in &result.peers {
            println!("  - {}", p.id);
        }
    }
    Ok(())
}

/// Run `a3net dht findpeer <peer-id>` when the `dht` feature is off.
#[cfg(not(feature = "dht"))]
pub async fn run_dht_findpeer(
    peer_id: &str,
    _json: bool,
    _node: &a3net_node::Node,
) -> Result<()> {
    let _ = parse_node_id(peer_id)?;
    anyhow::bail!(
        "DHT not enabled. Rebuild a3net-cli with `--features dht` to use `a3net dht findpeer`."
    )
}

/// Run `a3net dht peers`.
#[cfg(feature = "dht")]
pub async fn run_dht_peers(
    json: bool,
    node: &a3net_node::Node,
) -> Result<()> {
    let num = node.dht_num_peers().await?;
    // `dht_num_peers` returns the count; we do not have a
    // direct accessor for the full id list. The CLI prints
    // the count and offers `findpeer` for per-node lookups.
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "num_peers": num,
            }))?
        );
    } else {
        println!("routing table: {num} peer(s)");
        println!("(use `a3net dht findpeer <id>` to enumerate closest peers)");
    }
    Ok(())
}

/// Run `a3net dht peers` when the `dht` feature is off.
#[cfg(not(feature = "dht"))]
pub async fn run_dht_peers(
    _json: bool,
    _node: &a3net_node::Node,
) -> Result<()> {
    anyhow::bail!(
        "DHT not enabled. Rebuild a3net-cli with `--features dht` to use `a3net dht peers`."
    )
}

/// Run `a3net dht stats`.
#[cfg(feature = "dht")]
pub async fn run_dht_stats(
    json: bool,
    node: &a3net_node::Node,
) -> Result<()> {
    let stats = node.dht_stats().await;
    if json {
        let value = match stats {
            Some(s) => serde_json::to_value(&s)?,
            None => serde_json::json!({ "error": "DHT not initialized" }),
        };
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("DHT Statistics:");
        if let Some(s) = stats {
            println!("  providers_total     : {}", s.provides_total);
            println!("  finds_total         : {}", s.find_total);
            println!("  find_records_total  : {}", s.find_records_total);
            println!("  find_misses_total   : {}", s.find_misses_total);
            println!("  last_find_latency_us: {}", s.last_find_latency_us);
        } else {
            println!("  (DHT not initialized)");
        }
    }
    Ok(())
}

/// Run `a3net dht stats` when the `dht` feature is off.
#[cfg(not(feature = "dht"))]
pub async fn run_dht_stats(
    _json: bool,
    _node: &a3net_node::Node,
) -> Result<()> {
    anyhow::bail!(
        "DHT not enabled. Rebuild a3net-cli with `--features dht` to use `a3net dht stats`."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cid_accepts_hex() {
        let hex = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(parse_cid(hex).is_ok());
        assert!(parse_cid(&format!("/ipfs/{hex}")).is_ok());
        assert!(parse_cid("not-a-cid").is_err());
    }

    #[test]
    fn parse_node_id_accepts_short_and_full() {
        let full = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(parse_node_id(full).is_ok());
        assert!(parse_node_id(&format!("a3net-{full}")).is_ok());
        assert!(parse_node_id("garbage").is_err());
    }
}