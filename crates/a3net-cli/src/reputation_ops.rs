//! `a3net reputation <sub>` operator-facing commands.
//!
//! Commands for inspecting and managing the global peer reputation
//! table. The table tracks bitswap delivery, gossip validity,
//! pairing ceremonies, and chat trust signals in a unified score
//! per peer.
//!
//! ## Subcommands
//!
//! | Sub | Purpose |
//! |-----|---------|
//! | `show` | print all tracked peers and their scores |
//! | `list` | alias for `show` |
//! | `get <peer>` | print score for a specific peer |
//! | `adjust <peer> <delta>` | apply a manual score delta |
//! | `reset <peer>` | remove a peer's score entry |
//! | `stats` | print aggregate statistics |

use std::path::Path;

use a3net_reputation::{
    ReputationStore, ReputationStoreConfig, ReputationEvent,
};
use a3net_types::NodeId;

/// Reputation store path under the data directory.
const REPUTATION_DIR: &str = "reputation";

/// Run a reputation subcommand.
pub fn run_reputation(
    sub: &super::cli::ReputationCmd,
    data_dir: &Path,
) -> anyhow::Result<()> {
    let rep_dir = data_dir.join(REPUTATION_DIR);
    let cfg = ReputationStoreConfig {
        path: rep_dir.clone(),
        ..Default::default()
    };
    let store = ReputationStore::open(cfg).map_err(|e| {
        anyhow::anyhow!("opening reputation store under {}: {e}", rep_dir.display())
    })?;

    match sub {
        super::cli::ReputationCmd::Show { json } => cmd_show(&store, *json),
        super::cli::ReputationCmd::List { json } => cmd_show(&store, *json),
        super::cli::ReputationCmd::Get { peer } => cmd_get(&store, peer),
        super::cli::ReputationCmd::Adjust { peer, delta } => cmd_adjust(&store, peer, *delta),
        super::cli::ReputationCmd::Reset { peer } => cmd_reset(&store, peer),
        super::cli::ReputationCmd::Stats => cmd_stats(&store),
    }
}

// ───────────────────────────────────────────────────────────────────────
// subcommand handlers
// ───────────────────────────────────────────────────────────────────────

fn cmd_show(store: &ReputationStore, json: bool) -> anyhow::Result<()> {
    let snap = store.table().snapshot();
    if json {
        let peers: Vec<_> = snap
            .scores
            .iter()
            .map(|(node, score)| {
                serde_json::json!({
                    "peer": node.as_hex(),
                    "score": score,
                })
            })
            .collect();
        let body = serde_json::json!({
            "peers": peers,
            "total": peers.len(),
            "unix_now": snap.unix_now,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        if snap.scores.is_empty() {
            println!("reputation: no peers tracked");
            return Ok(());
        }
        println!("{:64}  {:>8}", "Peer", "Score");
        println!("{}", "-".repeat(74));
        for (node, score) in &snap.scores {
            println!("{}  {:>8.2}", node.as_hex(), score);
        }
        println!("{}", "-".repeat(74));
        println!("total: {} peers tracked", snap.scores.len());
    }
    Ok(())
}

fn cmd_get(store: &ReputationStore, peer_hex: &str) -> anyhow::Result<()> {
    let node = NodeId::from_hex(peer_hex)
        .map_err(|e| anyhow::anyhow!("invalid peer NodeId: {e}"))?;
    match store.table().score(&node) {
        Some(score) => {
            println!("{}  {:>8.2}", node.as_hex(), score);
        }
        None => {
            println!("{}: no score recorded (peer not tracked)", node.as_hex());
        }
    }
    Ok(())
}

fn cmd_adjust(store: &ReputationStore, peer_hex: &str, delta: f64) -> anyhow::Result<()> {
    let node = NodeId::from_hex(peer_hex)
        .map_err(|e| anyhow::anyhow!("invalid peer NodeId: {e}"))?;
    let event = ReputationEvent::ManualAdjust {
        peer: node.clone(),
        delta,
        reason: format!("CLI adjustment of {}", delta),
    };
    let result = store.apply(event)?;
    let new_score = store.table().score(&node).unwrap_or(0.0);
    println!(
        "{}: {} -> {:.2} (delta {:.2})",
        node.as_hex(),
        result.score_before,
        new_score,
        result.delta
    );
    Ok(())
}

fn cmd_reset(store: &ReputationStore, peer_hex: &str) -> anyhow::Result<()> {
    let node = NodeId::from_hex(peer_hex)
        .map_err(|e| anyhow::anyhow!("invalid peer NodeId: {e}"))?;
    if store.table().reset(&node) {
        store.flush()?;
        println!("{}: score entry removed", node.as_hex());
    } else {
        println!("{}: not tracked", node.as_hex());
    }
    Ok(())
}

fn cmd_stats(store: &ReputationStore) -> anyhow::Result<()> {
    let snap = store.table().snapshot();
    let total = snap.scores.len() as u64;
    if total == 0 {
        println!("reputation stats: no peers tracked");
        return Ok(());
    }

    let mut sum = 0.0;
    let mut positive = 0u64;
    let mut negative = 0u64;
    let mut graylist = 0u64; // score < 0
    let mut blocklist = 0u64; // score <= -10

    for (_, score) in &snap.scores {
        sum += score;
        if *score > 0.0 {
            positive += 1;
        } else if *score < 0.0 {
            negative += 1;
            if *score <= -10.0 {
                blocklist += 1;
            }
        }
        if *score < 0.0 {
            graylist += 1;
        }
    }

    let avg = sum / (total as f64);
    println!("reputation stats:");
    println!("  total tracked:   {}", total);
    println!("  positive:       {} ({:.1}%)", positive, 100.0 * (positive as f64) / (total as f64));
    println!("  neutral:        {} ({:.1}%)", total.saturating_sub(positive + negative), 100.0 * ((total.saturating_sub(positive + negative)) as f64) / (total as f64));
    println!("  negative:       {} ({:.1}%)", negative, 100.0 * (negative as f64) / (total as f64));
    println!("  graylisted:     {} (score < 0)", graylist);
    println!("  blocklisted:    {} (score <= -10)", blocklist);
    println!("  average score:  {:.2}", avg);
    println!("  refusal threshold: {:.1}", a3net_reputation::MIN_SCORE);
    println!("  graylist threshold: 0.0");

    Ok(())
}
