//! `socialfeed_basic` — 单用户创建 post + 自己的时间线
//!
//! 比 `two_users_moments` 更基础:不开 gossip,不开 IPC,只演示
//! `SocialFeedService::create_post` + `timeline`。
//!
//! 运行:`cargo run -p adnet-socialfeed --example socialfeed_basic`

use adnet_socialfeed::{SocialFeedService, SocialFeedServiceConfig, TimelineQuery, TimelineScope};
use adnet_types::invariants::Visibility;
use adnet_types::social_feed::SocialPost;
use adnet_types::NodeId;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let svc = SocialFeedService::new(SocialFeedServiceConfig {
        storage: adnet_socialfeed::SocialFeedStorageConfig {
            storage_dir: dir.path().to_path_buf(),
            filename: "feed.db".into(),
        },
        gossip: None,
        local_node: Some(NodeId::random()),
        validation_policy: adnet_ipc::validation::ValidationPolicy::Strict,
        gossip_transport: None,
    })?;

    // 1. 创建一条公开 post。
    let post = svc
        .create_post(SocialPost {
            post_id: String::new(),
            author_id: "alice".into(),
            author_name: "Alice".into(),
            author_avatar: None,
            content: "first moment".into(),
            attachments: vec![],
            tags: vec!["intro".into()],
            visibility: Visibility::Public,
            location: None,
            mentions: vec![],
            created_at: 0,
            updated_at: 0,
            like_count: 0,
            comment_count: 0,
            share_count: 0,
            public_account_id: None,
            integrity_hash: None,
            sequence: 1,
            is_edited: false,
            edited_at: None,
        })
        .await?;
    println!("created post {} (hash={:?})", post.post_id, post.integrity_hash);

    // 2. 拉时间线。
    let page = svc.timeline(TimelineQuery {
        viewer_id: "alice".to_string(),
        scope: TimelineScope::ForViewer,
        limit: Some(10),
        before_cursor: None,
        before_ts: None,
        author_id: None,
    })?;
    println!("timeline size = {}", page.posts.len());

    // 3. 完整性校验。
    assert!(svc.verify_post_integrity(&page.posts[0])?);
    println!("integrity ✔");
    Ok(())
}