//! Realistic example: three users (alice, bob, dave) — alice
//! publishes a public post, bob reacts, dave comments, alice
//! deletes bob's comment, and the timeline + followers view stays
//! consistent. Demonstrates `create_post` / `react` / `comment_post`
//! / `unfollow` / `verify_post_integrity` in one run.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-socialfeed --example socialfeed_app
//! ```

use a3net_socialfeed::{
    SocialFeedService, SocialFeedServiceConfig, SocialFeedStorageConfig, TimelineQuery,
    TimelineScope,
};
use a3net_types::invariants::{ReactionTarget, ReactionType, Visibility};
use a3net_types::social_feed::{SocialComment, SocialPost, SocialReaction};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let svc = SocialFeedService::new(SocialFeedServiceConfig {
        storage: SocialFeedStorageConfig {
            storage_dir: dir.path().to_path_buf(),
            filename: "demo.db".into(),
        },
        gossip: None,
        local_node: None,
        validation_policy: a3net_ipc::validation::ValidationPolicy::Strict,
        gossip_transport: None,
    })?;

    // 1. Alice publishes a public post.
    let alice_post = svc
        .create_post(SocialPost {
            post_id: String::new(),
            author_id: "alice".into(),
            author_name: "Alice".into(),
            author_avatar: None,
            content: "A3Net moments — first post".into(),
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

    // 2. Bob reacts with a Like.
    let reacted = svc
        .react(SocialReaction {
            reaction_id: "r-bob-1".into(),
            target_id: alice_post.post_id.clone(),
            target_type: ReactionTarget::Post,
            user_id: "bob".into(),
            reaction_type: ReactionType::Like,
            created_at: 0,
        })
        .await?;
    println!("bob reacted: {reacted}");

    // 3. Dave comments.
    let dave_comment = svc
        .comment_post(SocialComment {
            comment_id: String::new(),
            post_id: alice_post.post_id.clone(),
            author_id: "dave".into(),
            author_name: "Dave".into(),
            author_avatar: None,
            content: "first!".into(),
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
    println!("dave commented: {}", dave_comment.comment_id);

    // 4. Follow graph: bob + dave both follow alice.
    svc.follow("bob", "alice")?;
    svc.follow("dave", "alice")?;
    let following_alice = svc.list_following("dave")?;
    println!("dave follows: {following_alice:?}");
    assert!(following_alice.contains(&"alice".to_string()));

    // 5. Bob unfollows alice.
    svc.unfollow("bob", "alice")?;
    let still_following = svc.is_following("bob", "alice")?;
    println!("bob follows alice after unfollow: {still_following}");
    assert!(!still_following);

    // 6. Timeline query for dave (still a follower).
    let page = svc.timeline(TimelineQuery {
        viewer_id: "dave".to_string(),
        scope: TimelineScope::ForViewer,
        limit: Some(10),
        before_cursor: None,
        before_ts: None,
        author_id: None,
    })?;
    println!(
        "dave's timeline: {} post(s), next_cursor={:?}",
        page.posts.len(),
        page.next_cursor
    );
    assert_eq!(page.posts.len(), 1);

    // 7. Verify integrity of the surfaced post.
    let ok = svc.verify_post_integrity(&page.posts[0])?;
    println!("verify_post_integrity: {ok}");
    assert!(ok);

    // 8. List reactions and confirm bob's like is still there.
    let reactions = svc.list_reactions(&alice_post.post_id)?;
    println!(
        "reactions on alice's post: {} (first by={})",
        reactions.len(),
        reactions.first().map(|r| r.user_id.as_str()).unwrap_or("?")
    );
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0].user_id, "bob");
    assert_eq!(reactions[0].reaction_type, ReactionType::Like);
    println!("ok");
    Ok(())
}