//! Dispatcher for `a3net news …` subcommands.
//!
//! The `NewsService` lives on the `a3net-news` crate and is wired
//! into a `Node` through `a3net-node`'s `news` feature. The CLI
//! is a thin façade: it constructs a stand-alone `NewsService`
//! backed by an `InProcessGossip` transport so `a3net news …`
//! works without starting a full node runtime.
//!
//! [`run`]: self::run

use std::path::Path;

use a3net_gossip::{InProcessGossip, Topic};
use a3net_news::{BulletinEvent, NewsService, ValidationPolicy};
use a3net_types::{
    BulletinCategory, BulletinId, BulletinItem, BulletinKind, BulletinSeverity, NodeId, RoomId,
};
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;

use crate::cli::NewsCmd;

const NEWS_TOPIC_PREFIX: &str = "a3net-news-";

fn room_topic(room: &RoomId) -> Topic {
    let label = format!("{}{}", NEWS_TOPIC_PREFIX, room.as_str());
    Topic::from_label(&label)
}

/// Build a stand-alone `NewsService` rooted at `<data-dir>/news.db`.
/// The transport is `InProcessGossip` so the CLI works offline.
/// Signing is required under `Strict` policy; the CLI's `publish`
/// path attaches a deterministic, wallet-less placeholder signature
/// marked as audit-only — production callers should use the FFI to
/// hit a real wallet.
async fn open_service(data_dir: &Path) -> Result<NewsService> {
    use std::sync::Arc;
    let store_dir = data_dir.join("news");
    std::fs::create_dir_all(&store_dir)
        .with_context(|| format!("create news store dir {}", store_dir.display()))?;
    let cfg = a3net_news::NewsServiceConfig {
        store_dir,
        policy: ValidationPolicy::Audit,
        event_channel_capacity: 256,
    };
    let transport = Arc::new(InProcessGossip::new());
    let node = NodeId::random();
    NewsService::open(node, transport, cfg).context("open NewsService")
}

fn parse_kind(s: &str) -> Result<BulletinKind> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "announcement" => BulletinKind::Announcement,
        "advisory" => BulletinKind::Advisory,
        "alert" | "news" | "news_article" | "newsarticle" | "article" => {
            BulletinKind::NewsArticle
        }
        "correction" => BulletinKind::Correction,
        "retraction" => BulletinKind::Retraction,
        other => bail!("unknown bulletin kind: {other}"),
    })
}

fn parse_severity(s: &str) -> Result<BulletinSeverity> {
    use a3net_types::BulletinSeverity as B;
    Ok(match s.to_ascii_lowercase().as_str() {
        "info" => B::Info,
        "notice" | "notable" => B::Notable,
        "warning" | "important" => B::Important,
        "critical" => B::Critical,
        "emergency" => {
            bail!("`emergency` is not a severity tier in this build; use `critical`");
        }
        other => bail!("unknown severity: {other}"),
    })
}

fn parse_category(s: &str) -> Result<BulletinCategory> {
    use a3net_types::BulletinCategory as C;
    Ok(match s.to_ascii_lowercase().as_str() {
        "general" => C::General,
        "security" => C::Security,
        "ops" | "outage" => C::Outage,
        "weather" => C::Weather,
        "health" => C::Health,
        "safety" => C::Safety,
        "traffic" => C::Traffic,
        "politics" => C::Politics,
        "economy" => C::Economy,
        "tech" => C::Tech,
        "community" => C::Community,
        "sports" => C::Sports,
        "culture" => C::Culture,
        other => bail!("unknown category: {other}"),
    })
}

fn severity_rank(s: BulletinSeverity) -> u8 {
    use BulletinSeverity as S;
    match s {
        S::Info => 0,
        S::Notable => 1,
        S::Important => 2,
        S::Critical => 3,
    }
}

fn kind_severity_floor(kind: BulletinKind) -> u8 {
    use BulletinKind::*;
    // Update / withdraw records carry no minimum severity — the
    // point is to *replace* an earlier bulletin, not escalate it.
    match kind {
        Announcement | Advisory | NewsArticle | Correction | Retraction => 0,
    }
}

/// Entry point — dispatch the parsed CLI request.
pub async fn run(sub: &NewsCmd, data_dir: &Path) -> Result<()> {
    match sub {
        NewsCmd::Post {
            content,
            tags,
        } => {
            let svc = open_service(data_dir).await?;
            let tags_vec: Vec<String> = tags
                .as_ref()
                .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
                .unwrap_or_default();
            publish(
                &svc,
                "general",
                "news_article",
                "info",
                "general",
                "News Update",
                "",
                content,
                false,
            )
            .await
        }
        NewsCmd::List { limit } => {
            let svc = open_service(data_dir).await?;
            let lim = limit.unwrap_or(20) as usize;
            timeline(&svc, "general", None, lim, false)
        }
        NewsCmd::Subscribe { channel } => {
            let svc = open_service(data_dir).await?;
            subscribe(&svc, channel).await
        }
        NewsCmd::Receive { channel, timeout } => {
            let svc = open_service(data_dir).await?;
            tokio::time::sleep(tokio::time::Duration::from_secs(*timeout)).await;
            subscribe(&svc, channel).await
        }
    }
}

async fn publish(
    svc: &NewsService,
    room: &str,
    kind: &str,
    severity: &str,
    category: &str,
    title: &str,
    summary: &str,
    body: &str,
    json: bool,
) -> Result<()> {
    let room_id = RoomId::new(room);
    let kind = parse_kind(kind)?;
    let sev = parse_severity(severity)?;
    let cat = parse_category(category)?;
    if severity_rank(sev) < kind_severity_floor(kind) {
        bail!(
            "severity `{severity}` is below the floor for kind `{}`",
            kind.as_str()
        );
    }
    // Read from stdin when body == "-".
    let body = if body == "-" {
        use tokio::io::AsyncReadExt;
        let mut buf = String::new();
        tokio::io::stdin()
            .read_to_string(&mut buf)
            .await
            .context("read body from stdin")?;
        buf
    } else {
        body.to_string()
    };
    // Derive a per-call nonce so repeated CLI invocations don't
    // collide on the canonical id.
    let nonce = format!("cli-{}", Utc::now().timestamp_nanos_opt().unwrap_or(0));
    let item = BulletinItem::new(
        kind,
        cat,
        sev,
        room_id,
        svc.local_node().clone(),
        title,
        summary,
        &body,
        nonce.as_bytes(),
        None,
    )
    .map_err(|e| anyhow!("build bulletin: {e}"))?;
    let stored = svc
        .publish(item)
        .await
        .map_err(|e| anyhow!("publish: {e}"))?;
    if json {
        let summary = serde_json::json!({
            "bulletin_id": stored.bulletin_id.to_string(),
            "room": stored.room_id.as_str(),
            "sequence": stored.sequence,
            "kind": stored.kind.as_str(),
            "category": stored.category.as_str(),
            "severity": stored.severity.as_str(),
            "created_at": stored.created_at.timestamp(),
            "expires_at": stored.expires_at.timestamp(),
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "published {} into room `{}` (seq={}, kind={}, severity={}, category={})",
            stored.bulletin_id,
            stored.room_id.as_str(),
            stored.sequence,
            stored.kind.as_str(),
            stored.severity.as_str(),
            stored.category.as_str(),
        );
    }
    Ok(())
}

fn timeline(
    svc: &NewsService,
    room: &str,
    before_seq: Option<u32>,
    limit: usize,
    json: bool,
) -> Result<()> {
    let entries = svc
        .timeline(&RoomId::new(room), before_seq, limit)
        .map_err(|e| anyhow!("timeline: {e}"))?;
    if json {
        let out: Vec<_> = entries
            .into_iter()
            .map(|b| {
                serde_json::json!({
                    "bulletin_id": b.item.bulletin_id.to_string(),
                    "room": b.item.room_id.as_str(),
                    "author": b.item.author_id.to_string(),
                    "sequence": b.item.sequence,
                    "kind": b.item.kind.as_str(),
                    "category": b.item.category.as_str(),
                    "severity": b.item.severity.as_str(),
                    "title": b.item.title,
                    "summary": b.item.summary,
                    "created_at": b.item.created_at.timestamp(),
                    "expires_at": b.item.expires_at.timestamp(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        if entries.is_empty() {
            println!("(no bulletins in room `{room}`)");
        } else {
            println!(
                "{:<72} {:>5}  {:<13} {:<10}  TITLE",
                "BULLETIN_ID", "SEQ", "KIND", "SEVERITY"
            );
            for entry in entries {
                let row = format!(
                    "{:<72} {:>5}  {:<13} {:<10}  {}",
                    entry.item.bulletin_id.to_string(),
                    entry.item.sequence,
                    entry.item.kind.as_str(),
                    entry.item.severity.as_str(),
                    entry.item.title,
                );
                println!("{row}");
            }
        }
    }
    Ok(())
}

fn get(svc: &NewsService, room: &str, id: &str, json: bool) -> Result<()> {
    let id = a3net_types::BulletinId::from_hex(id)
        .map_err(|e| anyhow!("invalid bulletin id `{id}`: {e}"))?;
    let stored = svc
        .get(&RoomId::new(room), &id)
        .map_err(|e| anyhow!("get: {e}"))?;
    match stored {
        None => {
            if json {
                println!("null");
            } else {
                println!("no bulletin `{id}` in room `{room}`");
            }
        }
        Some(b) => {
            let value = serde_json::json!({
                "bulletin_id": b.item.bulletin_id.to_string(),
                "room": b.item.room_id.as_str(),
                "author": b.item.author_id.to_string(),
                "sequence": b.item.sequence,
                "kind": b.item.kind.as_str(),
                "category": b.item.category.as_str(),
                "severity": b.item.severity.as_str(),
                "title": b.item.title,
                "summary": b.item.summary,
                "body": b.item.body,
                "created_at": b.item.created_at.timestamp(),
                "expires_at": b.item.expires_at.timestamp(),
                "received_at": b.received_at.timestamp(),
                "signer": b.item.signer.as_ref().map(|s| s.to_string()),
                "supersedes": b.item.supersedes.as_ref().map(|s| s.to_string()),
            });
            if json {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!("{value:#?}");
            }
        }
    }
    Ok(())
}

fn mark_read(svc: &NewsService, room: &str, id: &str) -> Result<()> {
    let id = a3net_types::BulletinId::from_hex(id)
        .map_err(|e| anyhow!("invalid bulletin id `{id}`: {e}"))?;
    svc.mark_read(&RoomId::new(room), &id)
        .map_err(|e| anyhow!("mark read: {e}"))?;
    println!("marked {id} read in room `{room}`");
    Ok(())
}

async fn subscribe(svc: &NewsService, room: &str) -> Result<()> {
    svc.join_room(&RoomId::new(room))
        .await
        .map_err(|e| anyhow!("join room: {e}"))?;
    let _ = room_topic(&RoomId::new(room)); // ensure the label compiles
    let mut rx: broadcast::Receiver<BulletinEvent> = svc.subscribe();
    // Drain the initial replay events so live events only show
    // up here.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    while rx.try_recv().is_ok() {}
    println!(
        "{{\"event\":\"subscribed\",\"room\":\"{}\",\"node\":\"{}\"}}",
        room,
        svc.local_node()
    );
    loop {
        match rx.recv().await {
            Ok(BulletinEvent::Insert(item)) => {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "insert",
                        "bulletin_id": item.bulletin_id.to_string(),
                        "room": item.room_id.as_str(),
                        "kind": item.kind.as_str(),
                        "severity": item.severity.as_str(),
                        "title": item.title,
                    })
                );
            }
            Ok(BulletinEvent::Correction {
                superseded_id,
                corrected,
            }) => {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "correction",
                        "superseded_id": superseded_id.to_string(),
                        "corrected_id": corrected.bulletin_id.to_string(),
                    })
                );
            }
            Ok(BulletinEvent::Retraction {
                superseded_id,
                retraction,
            }) => {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "retraction",
                        "superseded_id": superseded_id.to_string(),
                        "retraction_id": retraction.bulletin_id.to_string(),
                    })
                );
            }
            Ok(BulletinEvent::ReplayComplete { room, replayed }) => {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "replay_complete",
                        "room": room.as_str(),
                        "replayed": replayed,
                    })
                );
            }
            Err(_) => break,
        }
    }
    Ok(())
}

// ── Helpers exposed for tests / unit checks ────────────────────
#[doc(hidden)]
pub fn parse_kind_pub(s: &str) -> Result<BulletinKind> {
    parse_kind(s)
}
#[doc(hidden)]
pub fn parse_severity_pub(s: &str) -> Result<BulletinSeverity> {
    parse_severity(s)
}
#[doc(hidden)]
pub fn parse_category_pub(s: &str) -> Result<BulletinCategory> {
    parse_category(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_known_kinds() {
        for k in [
            "announcement",
            "advisory",
            "news",
            "news_article",
            "correction",
            "retraction",
        ] {
            parse_kind(k).unwrap();
        }
        assert!(parse_kind("nope").is_err());
    }

    #[test]
    fn parses_all_known_severities() {
        for s in ["info", "notice", "notable", "warning", "important", "critical"] {
            parse_severity(s).unwrap();
        }
        assert!(parse_severity("boom").is_err());
    }

    #[test]
    fn parses_all_known_categories() {
        for c in [
            "general",
            "security",
            "ops",
            "outage",
            "weather",
            "health",
            "safety",
            "traffic",
            "politics",
            "economy",
            "tech",
            "community",
            "sports",
            "culture",
        ] {
            parse_category(c).unwrap();
        }
        assert!(parse_category("misc").is_err());
    }

    #[test]
    fn severity_floor_matches_kind() {
        assert_eq!(kind_severity_floor(parse_kind("alert").unwrap()), 0);
        assert_eq!(kind_severity_floor(parse_kind("retraction").unwrap()), 0);
        assert!(severity_rank(parse_severity("info").unwrap()) >= kind_severity_floor(parse_kind("alert").unwrap()));
        assert!(severity_rank(parse_severity("notable").unwrap()) >= kind_severity_floor(parse_kind("retraction").unwrap()));
    }
}
