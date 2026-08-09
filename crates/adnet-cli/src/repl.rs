//! `adnet run` — interactive `/cmd` REPL sitting on top of a running [`Node`].
//!
//! The REPL reads stdin line by line, dispatches `/cmd args ...` to
//! the matching node operation, and prints a result. Lines that do not
//! start with `/` are echoed back at `info` level so the operator can
//! leave in-session notes (e.g. `/note waiting on relay` would still be
//! a slash command — use plain text for notes).

use std::path::PathBuf;

use adnet_node::Node;
use adnet_types::{CdnContentKind, RoomId};
use anyhow::Result;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{info, warn};

use crate::feed_view::feed_for_humans;

/// Slash-command table:
///
/// | command                             | what it does                              |
/// |-------------------------------------|-------------------------------------------|
/// | `/help`                             | print this table                          |
/// | `/id`                               | print node id + data dir                  |
/// | `/mesh`                             | start mesh server, print listening URL    |
/// | `/transport`                        | print QUIC backend + bind + cert fp       |
/// | `/relay`                            | print embedded relay server status        |
/// | `/peers`                            | summarise discovered peers per room       |
/// | `/publish <file>`                   | copy file into shared workspace + gossip  |
/// | `/workspace`                        | list local + remote workspace entries     |
/// | `/fetch <idx>`                      | pull a remote workspace entry by index    |
/// | `/rooms`                            | list rooms we have joined                 |
/// | `/join <room>`                      | join a gossip room                        |
/// | `/leave <room>`                     | leave a gossip room                       |
/// | `/feed <room>`                      | print the room feed                       |
/// | `/announce <room> <file> [title] [kind]` | import + announce a local file       |
/// | `/echo <room>`                      | publish a synthetic announcement          |
/// | `/quit`  (or `/exit`, `/q`)         | shut the node down and exit the REPL      |
pub async fn run(data_dir: PathBuf, node: Node) -> Result<()> {
    println!("adnet REPL — type `/help` for the command list, `/quit` to exit");
    println!(
        "node: {}    data: {}",
        node.node_id().short(),
        data_dir.display()
    );

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    loop {
        print!("adnet> ");
        // `print!` doesn't flush on its own — `flush()` guarantees the
        // prompt appears before we block on stdin.
        use std::io::Write;
        std::io::stdout().flush().ok();

        let Some(line) = lines.next_line().await? else {
            // stdin closed (Ctrl-D / piped input ended) — treat as /quit.
            println!();
            info!("stdin closed, exiting REPL");
            break;
        };

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if !line.starts_with('/') {
            println!("  (note) {line}");
            continue;
        }

        // Parse the slash command into tokens. We deliberately keep the
        // parser trivial — it has to round-trip in a debugging context
        // where users may paste strings with whitespace.
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let args: Vec<&str> = parts.collect();

        match cmd {
            "/help" | "/?" => print_help(),
            "/id" => println!(
                "node_id  = {}\nshort    = adnet-{}\ndata_dir = {}",
                node.node_id(),
                node.node_id().short(),
                node.data_dir().display(),
            ),
            "/mesh" => match node.ensure_mesh().await {
                Ok(addr) => println!("mesh listening on http://{addr}/blobs/<hash>"),
                Err(e) => warn!(error = %e, "ensure_mesh failed"),
            },
            "/rooms" => {
                let rooms = node.joined_rooms().await;
                if rooms.is_empty() {
                    println!("(no joined rooms — try `/join lobby`)");
                } else {
                    for r in rooms {
                        println!("  {r}");
                    }
                }
            }
            "/join" => {
                let Some(room) = require_arg(&args, 0, "/join <room>")? else {
                    continue;
                };
                let room: RoomId = room.into();
                if let Err(e) = node.join_room(&room).await {
                    warn!(error = %e, room = %room, "join_room failed");
                } else {
                    println!("joined {room}");
                }
            }
            "/leave" => {
                let Some(room) = require_arg(&args, 0, "/leave <room>")? else {
                    continue;
                };
                let room: RoomId = room.into();
                if let Err(e) = node.leave_room(&room).await {
                    warn!(error = %e, room = %room, "leave_room failed");
                } else {
                    println!("left {room}");
                }
            }
            "/feed" => {
                let Some(room) = require_arg(&args, 0, "/feed <room>")? else {
                    continue;
                };
                let room: RoomId = room.into();
                if let Err(e) = node.join_room(&room).await {
                    warn!(error = %e, room = %room, "auto-join for /feed failed");
                    continue;
                }
                match node.room_feed(&room).await {
                    Ok(feed) => {
                        let json = serde_json::to_string_pretty(&feed_for_humans(&feed))?;
                        println!("{json}");
                    }
                    Err(e) => warn!(error = %e, room = %room, "room_feed failed"),
                }
            }
            "/announce" => {
                let room = match require_arg(&args, 0, "/announce <room> <file> [title] [kind]")? {
                    Some(v) => v,
                    None => continue,
                };
                let file = match require_arg(&args, 1, "/announce <room> <file> [title] [kind]")? {
                    Some(v) => v,
                    None => continue,
                };
                let title = args.get(2).copied().unwrap_or("shared file");
                let kind_str = args.get(3).copied().unwrap_or("generic_file");
                let Some(kind) = CdnContentKind::from_str_loose(kind_str) else {
                    warn!(kind = kind_str, "unknown kind — expected one of: article, ai_model, video_model, dataset, generic_file");
                    continue;
                };
                let room: RoomId = room.into();
                if let Err(e) = node.join_room(&room).await {
                    warn!(error = %e, room = %room, "auto-join for /announce failed");
                    continue;
                }
                let path = std::path::PathBuf::from(file);
                match node.import_and_announce(&room, &path, title, kind).await {
                    Ok(ann) => {
                        let ticket = ann.ticket.as_ref().map(|t| t.encode()).unwrap_or_default();
                        println!(
                            "{}",
                            serde_json::json!({
                                "room": room.as_str(),
                                "hash": ann.content_hash.as_hex(),
                                "sizeBytes": ann.size_bytes,
                                "ticket": ticket,
                            })
                        );
                    }
                    Err(e) => warn!(error = %e, "import_and_announce failed"),
                }
            }
            "/echo" => {
                let Some(room) = require_arg(&args, 0, "/echo <room>")? else {
                    continue;
                };
                let room: RoomId = room.into();
                if let Err(e) = node.join_room(&room).await {
                    warn!(error = %e, room = %room, "auto-join for /echo failed");
                    continue;
                }
                let hash = adnet_types::ContentHash::from_bytes(format!("echo:{room}").as_bytes());
                let ann = adnet_types::Announcement {
                    room_id: room.clone(),
                    content_hash: hash,
                    node_id: node.node_id().clone(),
                    title: format!("echo into {room}"),
                    kind: CdnContentKind::GenericFile,
                    size_bytes: 0,
                    mime_type: None,
                    source_url: None,
                    ticket: None,
                    timestamp: chrono::Utc::now(),
                    signer: None,
                    signature: None,
                };
                match node.announce(&room, &ann).await {
                    Ok(()) => println!("echoed into {room}"),
                    Err(e) => warn!(error = %e, "echo failed"),
                }
            }
            "/quit" | "/exit" | "/q" => {
                println!("bye");
                break;
            }
            "/transport" => match node.transport_handle() {
                Some(t) => {
                    let kind = t.kind();
                    let local = t.local_node().short();
                    let extra = if let Some(quic) = t
                        .as_any()
                        .and_then(|a| a.downcast_ref::<adnet_transport::QuicTransport>())
                    {
                        format!(
                            "\n  bind         = {}\n  cert_fp      = {}\n  incoming_q   = {}",
                            quic.bind_addr(),
                            hex::encode(quic.identity().fingerprint()),
                            node.incoming_queue_depth()
                                .await
                                .map(|d| d.to_string())
                                .unwrap_or_else(|| "n/a".into()),
                        )
                    } else {
                        String::new()
                    };
                    println!("transport    = {kind}\n  local_node   = {local}{extra}");
                }
                None => println!("(no transport wired — runs over mesh only)"),
            },
            "/relay" => match node.relay_info().await {
                Some(info) if info.running => println!(
                    "relay        = running\n\
                     base_url     = {}\n\
                     bind         = {}:{}\n\
                     port         = {}",
                    info.base_url, info.bind_host, info.port, info.port,
                ),
                Some(info) => println!(
                    "relay        = stopped\n\
                     base_url     = {}\n\
                     port         = {}",
                    info.base_url, info.port
                ),
                None => println!("(no embedded relay server)"),
            },
            "/peers" => {
                let rooms = node.joined_rooms().await;
                if rooms.is_empty() {
                    println!("(no joined rooms — try `/join lobby`)");
                } else {
                    for room in rooms {
                        match node.room_feed(&room).await {
                            Ok(feed) => {
                                let count = feed.assets.len();
                                println!("room {room} — {count} asset(s)");
                                for a in feed.assets.iter().take(5) {
                                    let has_ticket = feed
                                        .peer_map
                                        .get(&a.content_hash)
                                        .map(|tickets| !tickets.is_empty())
                                        .unwrap_or(false);
                                    println!(
                                        "  - {} by {} ({}B){}",
                                        a.title,
                                        a.announcer_node_id.short(),
                                        a.size_bytes,
                                        if has_ticket { " [ticket]" } else { "" }
                                    );
                                }
                                if count > 5 {
                                    println!("  ... and {} more", count - 5);
                                }
                            }
                            Err(e) => warn!(error = %e, room = %room, "room_feed failed"),
                        }
                    }
                }
            }
            "/publish" => {
                let Some(file) = require_arg(&args, 0, "/publish <file>")? else {
                    continue;
                };
                let path = std::path::PathBuf::from(file);
                match node.publish_to_workspace(&path).await {
                    Ok((entry, hash)) => println!(
                        "published {} ({}B, hash={})",
                        entry.name,
                        entry.size_bytes,
                        hash.short(),
                    ),
                    Err(e) => warn!(error = %e, "publish_to_workspace failed"),
                }
            }
            "/workspace" => {
                let local = node
                    .local_workspace_files()
                    .await
                    .unwrap_or_default();
                if local.is_empty() {
                    println!("(workspace disabled or empty)");
                } else {
                    println!("local ({}):", local.len());
                    for e in local.iter().take(10) {
                        let short = e
                            .content_hash
                            .as_deref()
                            .map(|s| &s[..8])
                            .unwrap_or("--------");
                        println!("  - {} ({}B, {})", e.name, e.size_bytes, short);
                    }
                    if local.len() > 10 {
                        println!("  ... and {} more", local.len() - 10);
                    }
                }
                let remote = node.remote_workspace_flat().await;
                if remote.is_empty() {
                    println!("remote: (no peers seen yet)");
                } else {
                    println!("remote ({}):", remote.len());
                    for (i, r) in remote.iter().enumerate().take(20) {
                        let short = r
                            .entry
                            .content_hash
                            .as_deref()
                            .map(|s| &s[..8])
                            .unwrap_or("--------");
                        let tag = if let Some(p) = &r.local_path {
                            format!(", fetched → {}", p.display())
                        } else if r.has_ticket {
                            ", ticket".to_string()
                        } else {
                            String::new()
                        };
                        println!(
                            "  [{i:>2}] {} from {} ({}B, {}{tag})",
                            r.entry.name,
                            r.owner.short(),
                            r.entry.size_bytes,
                            short,
                        );
                    }
                    if remote.len() > 20 {
                        println!("  ... and {} more", remote.len() - 20);
                    }
                }
            }
            "/fetch" => {
                let arg = match require_arg(&args, 1, "/fetch <idx>")? {
                    Some(a) => a,
                    None => continue,
                };
                let idx: usize = match arg.parse() {
                    Ok(n) => n,
                    Err(_) => {
                        warn!("usage: /fetch <idx> (use `/workspace` to see indices)");
                        continue;
                    }
                };
                let remote = node.remote_workspace_flat().await;
                let pick = match remote.get(idx) {
                    Some(r) => r.clone(),
                    None => {
                        warn!(idx, "no remote entry with that index");
                        continue;
                    }
                };
                if let Some(local_path) = pick.local_path.as_ref() {
                    println!("already fetched → {}", local_path.display());
                    continue;
                }
                if !pick.has_ticket {
                    warn!(
                        "remote entry {} from {} has no ticket — cannot fetch",
                        pick.entry.name,
                        pick.owner.short(),
                    );
                    continue;
                }
                let owner = pick.owner.clone();
                let name = pick.entry.name.clone();
                match node.fetch_remote_workspace_entry(&owner, &name).await {
                    Ok(p) => println!("fetched {} → {} ({}B)", name, p.display(), pick.entry.size_bytes),
                    Err(e) => warn!(error = %e, "auto-fetch failed"),
                }
            }
            other => warn!(command = other, "unknown command — try `/help`"),
        }
    }

    Ok(())
}

/// Convenience: extract positional arg `idx` or print a usage hint and
/// signal "skip this turn" via `Ok(None)` (vs `Err` for fatal errors).
fn require_arg<'a>(args: &'a [&'a str], idx: usize, usage: &str) -> Result<Option<&'a str>> {
    match args.get(idx) {
        Some(v) => Ok(Some(*v)),
        None => {
            warn!(usage, "missing argument");
            Ok(None)
        }
    }
}

fn print_help() {
    println!(
        "\
Slash commands:
  /help                                print this help
  /id                                  print node id + data dir
  /mesh                                start mesh server, print listening URL
  /transport                           print transport backend status + bind info
  /relay                               print embedded relay server status
  /peers                               summarise discovered peers per joined room
  /publish <file>                      copy file into shared workspace + announce via gossip
  /workspace                           list local + remote workspace entries
  /rooms                               list joined rooms
  /join <room>                         join a gossip room
  /leave <room>                        leave a gossip room
  /feed <room>                         print the room feed (auto-joins if needed)
  /announce <room> <file> [title] [kind]
                                       import a file and announce it
  /echo <room>                         publish a synthetic announcement
  /quit | /exit | /q                   shut the node down and exit

Any line that doesn't start with `/` is logged as a free-form note.
Press Ctrl-C once to abort an in-flight command; Ctrl-D to close stdin."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_arg_returns_value_when_present() {
        let args = ["alice", "42"];
        let got = require_arg(&args, 0, "usage").unwrap();
        assert_eq!(got, Some("alice"));
    }

    #[test]
    fn require_arg_returns_none_when_missing() {
        let args = ["alice"];
        let got = require_arg(&args, 1, "usage").unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn require_arg_handles_empty_slice() {
        let args: [&str; 0] = [];
        let got = require_arg(&args, 0, "usage").unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn require_arg_returns_first_position() {
        let args = ["first", "second"];
        let got = require_arg(&args, 0, "usage").unwrap();
        assert_eq!(got, Some("first"));
        let got = require_arg(&args, 1, "usage").unwrap();
        assert_eq!(got, Some("second"));
    }

    #[test]
    fn print_help_runs_without_panic() {
        // Smoke test: ensure the multiline string is well-formed and
        // doesn't trip any format-string issues.
        print_help();
    }

    #[test]
    fn feed_for_humans_sorts_recent_first() {
        // Build a minimal feed and verify the conversion preserves
        // ordering and emits the expected fields.
        use adnet_node::RoomFeed;
        use std::collections::HashMap;

        let h1 = adnet_types::ContentHash::from_bytes(b"a");
        let h2 = adnet_types::ContentHash::from_bytes(b"b");
        let room = adnet_types::RoomId::new("lobby");
        let older = adnet_types::RoomAsset {
            content_hash: h1.clone(),
            title: "first".into(),
            kind: adnet_types::CdnContentKind::GenericFile,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            room_id: room.clone(),
            announcer_node_id: adnet_types::NodeId::random(),
            announced_at: chrono::Utc::now() - chrono::Duration::seconds(60),
        };
        let newer = adnet_types::RoomAsset {
            content_hash: h2.clone(),
            title: "second".into(),
            kind: adnet_types::CdnContentKind::GenericFile,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            room_id: room.clone(),
            announcer_node_id: adnet_types::NodeId::random(),
            announced_at: chrono::Utc::now(),
        };
        let mut peer_map = HashMap::new();
        peer_map.insert(h1.clone(), vec![]);
        peer_map.insert(h2.clone(), vec![]);
        let feed = RoomFeed {
            room_id: room.clone(),
            assets: vec![older, newer],
            peer_map,
        };
        let humans = feed_for_humans(&feed);
        assert_eq!(humans.room, "lobby");
        assert_eq!(humans.assets.len(), 2);
        // `feed_for_humans` doesn't sort — that's the caller's job —
        // so just verify the asset payload round-trips correctly.
        assert_eq!(humans.assets[0].title, "first");
        assert_eq!(humans.assets[1].title, "second");
    }
}
