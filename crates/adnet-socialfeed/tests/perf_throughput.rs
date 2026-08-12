//! Throughput smoke test for the social feed storage.
//!
//! Run with `cargo test --release -p adnet-socialfeed --test perf_throughput
//! -- --ignored --nocapture`.
//!
//! Goal: catch quadratic regressions in `list_user_posts`,
//! `timeline_for`, and `list_post_comments`. Not a hard SLO —
//! numbers will vary by host — but useful to surface `O(n²)`
//! regressions early.

use adnet_socialfeed::{
    SocialFeedService, SocialFeedServiceConfig, TimelineQuery, TimelineScope,
};
use adnet_types::invariants::{ReactionTarget, ReactionType, Visibility};
use adnet_types::social_feed::{SocialComment, SocialPost, SocialReaction};
use adnet_types::NodeId;
use std::time::Instant;
use tempfile::TempDir;

fn sample(author: &str, content: &str, ts: u64) -> SocialPost {
    SocialPost {
        post_id: String::new(),
        author_id: author.into(),
        author_name: author.into(),
        author_avatar: None,
        content: content.into(),
        attachments: vec![],
        tags: vec![],
        visibility: Visibility::Public,
        location: None,
        mentions: vec![],
        created_at: ts,
        updated_at: ts,
        like_count: 0,
        comment_count: 0,
        share_count: 0,
        public_account_id: None,
        integrity_hash: None,
        sequence: 1,
        is_edited: false,
        edited_at: None,
    }
}

fn cfg_at(dir: &std::path::Path) -> SocialFeedServiceConfig {
    SocialFeedServiceConfig {
        storage: adnet_socialfeed::SocialFeedStorageConfig {
            storage_dir: dir.to_path_buf(),
            filename: "perf.db".into(),
        },
        gossip: None,
        local_node: Some(NodeId::random()),
        validation_policy: adnet_ipc::validation::ValidationPolicy::Strict,
        gossip_transport: None,
    }
}

#[tokio::test]
#[ignore = "throughput smoke"]
async fn perf_5000_posts_then_timeline() {
    let dir = TempDir::new().unwrap();
    let svc = SocialFeedService::new(cfg_at(dir.path())).unwrap();

    let authors = ["alice", "bob", "carol", "dave", "eve"];
    let n = 5000usize;
    let t0 = Instant::now();
    let mut last_id = String::new();
    for i in 0..n {
        let who = authors[i % authors.len()];
        let p = svc
            .create_post(sample(who, &format!("post #{i}"), i as u64))
            .await
            .unwrap();
        last_id = p.post_id;
    }
    let write_dt = t0.elapsed();
    eprintln!("wrote {n} posts in {write_dt:?} ({:.1} ops/s)",
        n as f64 / write_dt.as_secs_f64());

    let t1 = Instant::now();
    let page = svc
        .timeline(TimelineQuery {
            viewer_id: "stranger".into(),
            scope: TimelineScope::ForViewer,
            limit: Some(50),
            before_cursor: None,
            before_ts: None,
            author_id: None,
        })
        .unwrap();
    let dt = t1.elapsed();
    assert_eq!(page.posts.len(), 50);
    eprintln!("timeline_for(50) over {n} posts: {dt:?}");
    assert!(dt.as_secs_f64() < 1.0, "slow timeline: {dt:?}");

    let t2 = Instant::now();
    let listed = svc.list_user_posts("alice").unwrap();
    let dt = t2.elapsed();
    eprintln!("list_user_posts('alice') returned {} in {dt:?}", listed.len());
    assert_eq!(listed.len(), n / authors.len());
    assert!(dt.as_secs_f64() < 1.0, "slow listing: {dt:?}");

    // And one big fanout of comments on `last_id`.
    let t3 = Instant::now();
    for i in 0..100 {
        let c = SocialComment {
            comment_id: String::new(),
            post_id: last_id.clone(),
            author_id: format!("u{i}"),
            author_name: format!("u{i}"),
            author_avatar: None,
            content: format!("comment #{i}"),
            parent_id: None,
            mentions: vec![],
            created_at: i as u64,
            updated_at: i as u64,
            like_count: 0,
            reply_count: 0,
            is_edited: false,
            edited_at: None,
        };
        svc.comment_post(c).await.unwrap();
    }
    let insert_dt = t3.elapsed();
    let t4 = Instant::now();
    let comments = svc.list_post_comments(&last_id).unwrap();
    let list_dt = t4.elapsed();
    eprintln!("100 comments: insert {insert_dt:?}, list {list_dt:?}");
    assert_eq!(comments.len(), 100);

    // Reaction fanout.
    let t5 = Instant::now();
    for i in 0..100 {
        svc.react(SocialReaction {
            reaction_id: format!("r-{i}"),
            target_id: last_id.clone(),
            target_type: ReactionTarget::Post,
            user_id: format!("u{i}"),
            reaction_type: if i % 2 == 0 {
                ReactionType::Like
            } else {
                ReactionType::Love
            },
            created_at: i as u64,
        })
        .await
        .unwrap();
    }
    let react_insert_dt = t5.elapsed();
    let t6 = Instant::now();
    let reactions = svc.list_reactions(&last_id).unwrap();
    let react_dt = t6.elapsed();
    eprintln!("100 reactions: insert {react_insert_dt:?}, list {react_dt:?}");
    assert_eq!(reactions.len(), 100);
}
