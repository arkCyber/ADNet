//! `a3net room <sub>` — manage joined gossip rooms.
//!
//! Lifts the REPL-only `/rooms /join /leave /peers /feed` slash
//! commands to the top-level CLI so headless operators don't
//! have to drop into the interactive REPL just to inspect the
//! gossip rooms the node is currently subscribed to.
//!
//! All sub-commands map onto existing `a3net_node::Node` methods
//! — `joined_rooms()`, `join_room()`, `leave_room()`, and the
//! per-room `room_feed()` snapshot. Nothing here is new state;
//! this module is purely a CLI surface lift.

use std::collections::BTreeSet;

use anyhow::{bail, Result};
use serde_json::json;

use a3net_node::Node;
use a3net_types::RoomId;

use crate::cli::RoomCmd;

/// Top-level dispatch — `a3net room <sub>`.
pub async fn run_room(sub: &RoomCmd, node: &Node) -> Result<()> {
    match sub {
        RoomCmd::Ls { json } => list_rooms(node, *json).await,
        RoomCmd::Join { room, json } => join_room(node, room, *json).await,
        RoomCmd::Leave { room, json } => leave_room(node, room, *json).await,
        RoomCmd::Peers { room, json } => peers(node, room, *json).await,
        RoomCmd::Feed { room, limit, json } => feed(node, room, *limit, *json).await,
    }
}

async fn list_rooms(node: &Node, json_out: bool) -> Result<()> {
    let rooms = node.joined_rooms().await;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "rooms": rooms.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
                "count": rooms.len(),
            }))?
        );
    } else if rooms.is_empty() {
        println!("(no joined rooms — `a3net room join <room>` to subscribe)");
    } else {
        println!("Joined rooms ({}):", rooms.len());
        for r in &rooms {
            println!("  {}", r.as_str());
        }
    }
    Ok(())
}

async fn join_room(node: &Node, room: &str, json_out: bool) -> Result<()> {
    let id = RoomId::new(room.to_string());
    node.join_room(&id).await?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "ok": true,
                "room": id.as_str(),
                "action": "joined",
            }))?
        );
    } else {
        println!("✓ joined room `{}`", id.as_str());
    }
    Ok(())
}

async fn leave_room(node: &Node, room: &str, json_out: bool) -> Result<()> {
    let id = RoomId::new(room.to_string());
    if !node.joined_rooms().await.iter().any(|r| r == &id) {
        bail!("not currently subscribed to room `{}`", id.as_str());
    }
    node.leave_room(&id).await?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "ok": true,
                "room": id.as_str(),
                "action": "left",
            }))?
        );
    } else {
        println!("✓ left room `{}`", id.as_str());
    }
    Ok(())
}

async fn peers(node: &Node, room: &str, json_out: bool) -> Result<()> {
    let id = RoomId::new(room.to_string());
    let feed = node.room_feed(&id).await?;
    // Deduplicate peer node-ids across every blob ticket in
    // the room's peer_map. The map is keyed by content hash;
    // we don't care about the hash here, only the peers.
    let mut peers = BTreeSet::new();
    for tickets in feed.peer_map.values() {
        for t in tickets {
            peers.insert(t.node_id.as_hex());
        }
    }
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "room": id.as_str(),
                "peers": peers.iter().cloned().collect::<Vec<_>>(),
                "count": peers.len(),
            }))?
        );
    } else if peers.is_empty() {
        println!("(no peers in `{}`)", id.as_str());
    } else {
        println!("Peers in `{}` ({}):", id.as_str(), peers.len());
        for p in &peers {
            println!("  a3net-{}", &p.chars().take(8).collect::<String>());
        }
    }
    Ok(())
}

async fn feed(node: &Node, room: &str, limit: usize, json_out: bool) -> Result<()> {
    let id = RoomId::new(room.to_string());
    let feed = node.room_feed(&id).await?;
    // Newest entries first — the feed stores them in insertion
    // order, so `rev().take(limit)` keeps the most recent.
    let slice: Vec<_> = feed.assets.iter().rev().take(limit).collect();
    if json_out {
        let payload: Vec<serde_json::Value> = slice
            .iter()
            .map(|e| {
                json!({
                    "hash": e.content_hash.as_hex(),
                    "title": e.title,
                    "node_id": e.announcer_node_id.as_hex(),
                    "timestamp": e.announced_at,
                    "kind": format!("{:?}", e.kind),
                    "size_bytes": e.size_bytes,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "room": id.as_str(),
                "count": payload.len(),
                "entries": payload,
            }))?
        );
    } else if slice.is_empty() {
        println!("(no announcements in `{}`)", id.as_str());
    } else {
        println!("Latest {} announcements in `{}`:", slice.len(), id.as_str());
        for e in slice.iter().rev() {
            println!(
                "  {} | {:>8} B | {} | {}",
                e.content_hash.as_hex().chars().take(8).collect::<String>(),
                e.size_bytes,
                format!("{:?}", e.kind),
                e.title
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_id_round_trip() {
        let id = RoomId::new("test-room".to_string());
        assert_eq!(id.as_str(), "test-room");
    }
}