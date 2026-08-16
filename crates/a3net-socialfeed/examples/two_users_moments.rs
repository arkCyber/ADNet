//! End-to-end example: two users exchanging posts and reactions on
//! a temporary SQLite database.
//!
//! Run with `cargo run -p a3net-socialfeed --example two_users_moments`.

use a3net_socialfeed::{
    SocialFeedService, SocialFeedServiceConfig, TimelineQuery, TimelineScope,
};
use a3net_types::invariants::{ReactionTarget, ReactionType, Visibility};
use a3net_types::social_feed::{SocialComment, SocialPost, SocialReaction};
use a3net_types::NodeId;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let svc = SocialFeedService::new(SocialFeedServiceConfig {
        storage: a3net_socialfeed::SocialFeedStorageConfig {
            storage_dir: dir.path().to_path_buf(),
            filename: "demo.db".into(),
        },
        gossip: None,
        local_node: Some(NodeId::random()),
        validation_policy: a3net_ipc::validation::ValidationPolicy::Strict,
        gossip_transport: None,
    })?;

    // Alice creates a public post.
    let alice_post = svc
        .create_post(SocialPost {
            post_id: String::new(),
            author_id: "alice".into(),
            author_name: "Alice".into(),
            author_avatar: None,
            content: "hello world — first moment on A3Net".into(),
            attachments: vec![],
            tags: vec!["intro".into()],
            visibility: Visibility::Public,
            location: Some("Earth".into()),
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
    println!(
        "alice posted {} (integrity={:?})",
        alice_post.post_id, alice_post.integrity_hash
    );

    // Bob comments + reacts.
    svc.comment_post(SocialComment {
        comment_id: String::new(),
        post_id: alice_post.post_id.clone(),
        author_id: "bob".into(),
        author_name: "Bob".into(),
        author_avatar: None,
        content: "welcome!".into(),
        parent_id: None,
        mentions: vec![],
        created_at: 0,
        updated_at: 0,
        like_count: 0,
        reply_count: 0,
        is_edited: false,
        edited_at: None,
    })
    .await?;

    svc.react(SocialReaction {
        reaction_id: "demo-r1".into(),
        target_id: alice_post.post_id.clone(),
        target_type: ReactionTarget::Post,
        user_id: "bob".into(),
        reaction_type: ReactionType::Like,
        created_at: 0,
    })
    .await?;

    // Carol arrives, follows alice, sees the moment.
    svc.follow("carol", "alice")?;
    let page = svc.timeline(TimelineQuery {
        viewer_id: "carol".to_string(),
        scope: TimelineScope::ForViewer,
        limit: Some(10),
        before_cursor: None,
        before_ts: None,
        author_id: None,
    })?;
    println!("carol's timeline has {} moment(s)", page.posts.len());
    assert_eq!(page.posts.len(), 1);
    assert!(svc.verify_post_integrity(&page.posts[0])?);
    println!("\n--- demo complete ---");
    Ok(())
}
