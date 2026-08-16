//! Realistic example: publish three bulletins in order (announce →
//! correction → retraction), demonstrating how a single bulletin
//! can supersede previous ones via `supersedes`, and how the
//! service emits typed `Insert` / `Correction` / `Retraction`
//! events on the broadcast channel.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-news --example news_app
//! ```

use a3net_gossip::{GossipTransport, InProcessGossip};
use a3net_news::{BulletinEvent, BulletinItem, NewsService, NewsServiceConfig, ValidationPolicy};
use a3net_types::{
    BulletinCategory, BulletinKind, BulletinSeverity, NodeId, RoomId,
};
use std::sync::Arc;
use tokio::sync::broadcast::error::TryRecvError;

fn make_item(
    local: &NodeId,
    kind: BulletinKind,
    title: &str,
    body: &str,
    supersedes: Option<a3net_types::BulletinId>,
    nonce: &[u8],
) -> Result<BulletinItem, Box<dyn std::error::Error>> {
    let mut item = BulletinItem::new(
        kind,
        BulletinCategory::Tech,
        BulletinSeverity::Info,
        RoomId::new("ops"),
        local.clone(),
        title,
        "",
        body,
        nonce,
        supersedes,
    )?;
    item.author_name = "ops-bot".into();
    item.lang = "en".into();
    Ok(item)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let transport: Arc<dyn GossipTransport> = Arc::new(InProcessGossip::new());
    let local = NodeId::random();
    let dir = tempfile::tempdir()?;
    let svc = NewsService::open(
        local.clone(),
        transport,
        NewsServiceConfig {
            store_dir: dir.path().to_path_buf(),
            policy: ValidationPolicy::Lenient,
            event_channel_capacity: 32,
        },
    )?;

    // Subscribe BEFORE publishing so we capture every event.
    let mut rx = svc.subscribe();

    // 1. Original announcement.
    let announce = svc
        .publish(make_item(
            &local,
            BulletinKind::Announcement,
            "Maintenance at 10:00",
            "5 minute downtime scheduled.",
            None,
            &[0x01; 16],
        )?)
        .await?;
    println!("announce id={} seq={}", announce.bulletin_id, announce.sequence);

    // 2. Correction that supersedes the announcement.
    let corr = svc
        .publish(make_item(
            &local,
            BulletinKind::Correction,
            "Maintenance at 11:30",
            "Postponed by 90 minutes.",
            Some(announce.bulletin_id.clone()),
            &[0x02; 16],
        )?)
        .await?;
    println!("correction id={} seq={}", corr.bulletin_id, corr.sequence);

    // 3. Retraction (e.g. maintenance cancelled outright).
    let retr = svc
        .publish(make_item(
            &local,
            BulletinKind::Retraction,
            "Maintenance cancelled",
            "Rolling restart already covered it.",
            Some(corr.bulletin_id.clone()),
            &[0x03; 16],
        )?)
        .await?;
    println!("retraction id={} seq={}", retr.bulletin_id, retr.sequence);

    // 4. Drain the broadcast channel and verify the three events.
    let mut insert_count = 0;
    let mut corr_count = 0;
    let mut retr_count = 0;
    loop {
        match rx.try_recv() {
            Ok(BulletinEvent::Insert(_)) => insert_count += 1,
            Ok(BulletinEvent::Correction { .. }) => corr_count += 1,
            Ok(BulletinEvent::Retraction { .. }) => retr_count += 1,
            Ok(BulletinEvent::ReplayComplete { .. }) => {}
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Lagged(n)) => println!("(skipped {n} lagged events)"),
            Err(TryRecvError::Closed) => break,
        }
    }
    println!("events: insert={insert_count} correction={corr_count} retraction={retr_count}");
    assert_eq!(insert_count, 1);
    assert_eq!(corr_count, 1);
    assert_eq!(retr_count, 1);

    // 5. Sequence numbers must be strictly monotonic across the room.
    let list = svc.store().list_replay(&RoomId::new("ops"))?;
    let seqs: Vec<u32> = list.iter().map(|b| b.item.sequence).collect();
    println!("sequences: {seqs:?}");
    assert_eq!(seqs, vec![1, 2, 3]);
    println!("ok");
    Ok(())
}