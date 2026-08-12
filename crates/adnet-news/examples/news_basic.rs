//! Minimal example: stand up a `NewsService` on the in-process
//! gossip transport, publish a single bulletin via the typed
//! `BulletinItem::new` constructor, then read it back from the
//! store and confirm the assigned sequence is strictly monotonic.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-news --example news_basic
//! ```

use adnet_gossip::InProcessGossip;
use adnet_news::{NewsService, NewsServiceConfig, ValidationPolicy};
use adnet_types::{BulletinCategory, BulletinKind, BulletinSeverity, NodeId, RoomId};
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Stand up the service on a tempdir-backed store + in-process gossip.
    let transport = Arc::new(InProcessGossip::default());
    let local = NodeId::random();
    let dir = tempfile::tempdir()?;
    let svc = NewsService::open(
        local.clone(),
        transport,
        NewsServiceConfig {
            store_dir: dir.path().to_path_buf(),
            policy: ValidationPolicy::Lenient,
            event_channel_capacity: 16,
        },
    )?;
    println!("service local node: {}", local.short());

    // 2. Build a BulletinItem via the typed constructor.
    let mut item = adnet_types::BulletinItem::new(
        BulletinKind::Announcement,
        BulletinCategory::General,
        BulletinSeverity::Info,
        RoomId::new("lobby"),
        local.clone(),
        "Welcome to ADNet news",
        "First bulletin",
        "Hello world from adnet-news.",
        &[0xAA; 16],
        None,
    )?;
    item.author_name = "alice".into();
    item.lang = "en".into();

    let stored = svc.publish(item).await?;
    println!(
        "published: id={} seq={} title={:?}",
        stored.bulletin_id, stored.sequence, stored.title
    );
    assert_eq!(stored.sequence, 1);

    // 3. Read it back from the store.
    let list = svc.store().list_replay(&RoomId::new("lobby"))?;
    println!("store returned {} bulletin(s)", list.len());
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].item.bulletin_id, stored.bulletin_id);
    println!("ok");
    Ok(())
}