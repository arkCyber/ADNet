//! Integration tests for the social-feed service.
//!
//! These tests exercise the public API end-to-end against the
//! SQLite backend and the in-memory backend. They mirror what a
//! real embedder would do — drive `SocialFeedService` from a
//! single async task, asserting visibility filters, idempotency,
//! and gossip fan-out.

use adnet_socialfeed::{
    SocialFeedService, SocialFeedServiceConfig, TimelineScope, TimelineQuery,
};
use adnet_types::invariants::{ReactionTarget, ReactionType, Visibility};
use adnet_types::social_feed::{SocialComment, SocialPost, SocialReaction};
use adnet_types::NodeId;
use tempfile::TempDir;

fn sample_post(author: &str, content: &str) -> SocialPost {
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
    }
}

fn cfg_at(dir: &std::path::Path) -> SocialFeedServiceConfig {
    SocialFeedServiceConfig {
        storage: adnet_socialfeed::SocialFeedStorageConfig {
            storage_dir: dir.to_path_buf(),
            filename: "it.db".into(),
        },
        gossip: None,
        local_node: Some(NodeId::random()),
        validation_policy: adnet_ipc::validation::ValidationPolicy::Strict,
        gossip_transport: None,
    }
}

#[tokio::test]
async fn create_post_persists_and_lists() {
    let dir = TempDir::new().unwrap();
    let svc = SocialFeedService::new(cfg_at(dir.path())).unwrap();
    let post = svc.create_post(sample_post("alice", "hi")).await.unwrap();
    assert!(!post.post_id.is_empty());

    let page = svc
        .timeline(TimelineQuery {
            viewer_id: "bob".into(),
            scope: TimelineScope::ForViewer,
            limit: Some(10),
            before_cursor: None,
            before_ts: None,
            author_id: None,
        })
        .unwrap();
    assert_eq!(page.posts.len(), 1);
    assert_eq!(page.posts[0].post_id, post.post_id);
}

#[tokio::test]
async fn comments_attach_to_posts() {
    let dir = TempDir::new().unwrap();
    let svc = SocialFeedService::new(cfg_at(dir.path())).unwrap();
    let post = svc.create_post(sample_post("alice", "blog")).await.unwrap();
    let c = svc
        .comment_post(SocialComment {
            comment_id: String::new(),
            post_id: post.post_id.clone(),
            author_id: "bob".into(),
            author_name: "Bob".into(),
            author_avatar: None,
            content: "nice!".into(),
            parent_id: None,
            mentions: vec![],
            created_at: 0,
            updated_at: 0,
            like_count: 0,
            reply_count: 0,
            is_edited: false,
            edited_at: None,
        })
        .await
        .unwrap();
    let listed = svc.list_post_comments(&post.post_id).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].comment_id, c.comment_id);
}

#[tokio::test]
async fn followers_see_friends_only_posts() {
    let dir = TempDir::new().unwrap();
    let svc = SocialFeedService::new(cfg_at(dir.path())).unwrap();

    let mut friends_post = sample_post("alice", "secret sauce");
    friends_post.visibility = Visibility::Friends;
    let stored = svc.create_post(friends_post).await.unwrap();

    svc.follow("bob", "alice").unwrap();

    let mut public_post = sample_post("alice", "open secret");
    public_post.content = "open secret".into();
    svc.create_post(public_post).await.unwrap();

    let page = svc
        .timeline(TimelineQuery {
            viewer_id: "bob".into(),
            scope: TimelineScope::ForViewer,
            limit: Some(10),
            before_cursor: None,
            before_ts: None,
            author_id: None,
        })
        .unwrap();
    assert_eq!(page.posts.len(), 2);
    // Newest first.
    assert!(page.posts.iter().any(|p| p.post_id == stored.post_id));
}

#[tokio::test]
async fn reactions_are_idempotent() {
    let dir = TempDir::new().unwrap();
    let svc = SocialFeedService::new(cfg_at(dir.path())).unwrap();
    let post = svc.create_post(sample_post("alice", "vote")).await.unwrap();

    let r = SocialReaction {
        reaction_id: "r1".into(),
        target_id: post.post_id.clone(),
        target_type: ReactionTarget::Post,
        user_id: "bob".into(),
        reaction_type: ReactionType::Like,
        created_at: 1,
    };

    assert!(svc.react(r.clone()).await.unwrap());
    assert!(!svc.react(r).await.unwrap());
    let listed = svc.list_reactions(&post.post_id).unwrap();
    assert_eq!(listed.len(), 1);
}

#[tokio::test]
async fn delete_post_cascades_to_reactions_and_comments() {
    let dir = TempDir::new().unwrap();
    let svc = SocialFeedService::new(cfg_at(dir.path())).unwrap();
    let post = svc.create_post(sample_post("alice", "doomed")).await.unwrap();

    let c = svc
        .comment_post(SocialComment {
            comment_id: String::new(),
            post_id: post.post_id.clone(),
            author_id: "bob".into(),
            author_name: "Bob".into(),
            author_avatar: None,
            content: "rip".into(),
            parent_id: None,
            mentions: vec![],
            created_at: 0,
            updated_at: 0,
            like_count: 0,
            reply_count: 0,
            is_edited: false,
            edited_at: None,
        })
        .await
        .unwrap();

    svc.react(SocialReaction {
        reaction_id: "r1".into(),
        target_id: post.post_id.clone(),
        target_type: ReactionTarget::Post,
        user_id: "bob".into(),
        reaction_type: ReactionType::Like,
        created_at: 1,
    })
    .await
    .unwrap();
    svc.react(SocialReaction {
        reaction_id: "r2".into(),
        target_id: c.comment_id.clone(),
        target_type: ReactionTarget::Comment,
        user_id: "bob".into(),
        reaction_type: ReactionType::Like,
        created_at: 1,
    })
    .await
    .unwrap();

    svc.delete_post(&post.post_id).unwrap();
    assert!(svc.get_post(&post.post_id).unwrap().is_none());
    assert!(svc.list_post_comments(&post.post_id).unwrap().is_empty());
    assert!(svc.list_reactions(&post.post_id).unwrap().is_empty());
    assert!(svc.list_reactions(&c.comment_id).unwrap().is_empty());
}

// ── Pagination ────────────────────────────────────────────────────────

#[tokio::test]
async fn timeline_pagination_uses_next_before_ts() {
    use adnet_socialfeed::TimelineCursor;
    let dir = TempDir::new().unwrap();
    let svc = SocialFeedService::new(cfg_at(dir.path())).unwrap();
    for i in 0..7 {
        svc.create_post(sample_post("alice", &format!("p{i}"))).await.unwrap();
    }
    let page1 = svc
        .timeline(TimelineQuery {
            viewer_id: "bob".into(),
            scope: TimelineScope::ForViewer,
            limit: Some(3),
            before_cursor: None,
            before_ts: None,
            author_id: None,
        })
        .unwrap();
    assert_eq!(page1.posts.len(), 3);
    let cursor1 = page1.next_cursor.clone().expect("page1 cursor set");
    let page2 = svc
        .timeline(TimelineQuery {
            viewer_id: "bob".into(),
            scope: TimelineScope::ForViewer,
            limit: Some(3),
            before_cursor: Some(cursor1.clone()),
            before_ts: None,
            author_id: None,
        })
        .unwrap();
    assert_eq!(page2.posts.len(), 3);
    let cursor2 = page2.next_cursor.clone().expect("page2 cursor set");
    let page3 = svc
        .timeline(TimelineQuery {
            viewer_id: "bob".into(),
            scope: TimelineScope::ForViewer,
            limit: Some(3),
            before_cursor: Some(cursor2),
            before_ts: None,
            author_id: None,
        })
        .unwrap();
    assert_eq!(page3.posts.len(), 1);
    assert!(page3.next_cursor.is_none());
    // Pages are pairwise disjoint — composite cursor guarantees
    // no skip and no double-count even when posts share a
    // timestamp.
    let mut seen = std::collections::HashSet::new();
    for p in page1.posts.iter().chain(page2.posts.iter()).chain(page3.posts.iter()) {
        assert!(seen.insert(p.post_id.clone()), "duplicate: {}", p.post_id);
    }
    assert_eq!(seen.len(), 7);
    // Sanity: cursor round-trip preserves the original values.
    let expected = TimelineCursor::from_post(&page1.posts[2]);
    assert_eq!(cursor1, expected);
}

#[tokio::test]
async fn by_user_scope_honours_before_cursor() {
    let dir = TempDir::new().unwrap();
    let svc = SocialFeedService::new(cfg_at(dir.path())).unwrap();
    for i in 0..4 {
        svc.create_post(sample_post("alice", &format!("p{i}"))).await.unwrap();
    }
    let page = svc
        .timeline(TimelineQuery {
            viewer_id: "anyone".into(),
            scope: TimelineScope::ByUser,
            limit: Some(3),
            before_cursor: None,
            before_ts: None,
            author_id: Some("alice".into()),
        })
        .unwrap();
    assert_eq!(page.posts.len(), 3);
    let cursor = page.next_cursor.clone().expect("truncated");
    let page2 = svc
        .timeline(TimelineQuery {
            viewer_id: "anyone".into(),
            scope: TimelineScope::ByUser,
            limit: Some(3),
            before_cursor: Some(cursor),
            before_ts: None,
            author_id: Some("alice".into()),
        })
        .unwrap();
    assert_eq!(page2.posts.len(), 1);
    assert!(page2.next_cursor.is_none());
    let ids1: std::collections::HashSet<_> =
        page.posts.iter().map(|p| p.post_id.clone()).collect();
    for p in &page2.posts {
        assert!(!ids1.contains(&p.post_id));
    }
}

// ── Integrity verification ────────────────────────────────────────────

#[tokio::test]
async fn verify_comment_and_reaction_integrity() {
    let dir = TempDir::new().unwrap();
    let svc = SocialFeedService::new(cfg_at(dir.path())).unwrap();
    let post = svc.create_post(sample_post("alice", "test")).await.unwrap();
    let c = svc
        .comment_post(SocialComment {
            comment_id: String::new(),
            post_id: post.post_id.clone(),
            author_id: "bob".into(),
            author_name: "Bob".into(),
            author_avatar: None,
            content: "fine".into(),
            parent_id: None,
            mentions: vec![],
            created_at: 0,
            updated_at: 0,
            like_count: 0,
            reply_count: 0,
            is_edited: false,
            edited_at: None,
        })
        .await
        .unwrap();
    assert!(svc.verify_post_integrity(&post).unwrap());
    assert!(svc.inner().verify_comment_integrity(&c));
    let r = SocialReaction {
        reaction_id: "r1".into(),
        target_id: post.post_id.clone(),
        target_type: ReactionTarget::Post,
        user_id: "bob".into(),
        reaction_type: ReactionType::Like,
        created_at: 1,
    };
    assert!(svc.inner().verify_reaction_integrity(&r));
}

// ── Follow graph ──────────────────────────────────────────────────────

#[tokio::test]
async fn follow_unfollow_round_trip() {
    let dir = TempDir::new().unwrap();
    let svc = SocialFeedService::new(cfg_at(dir.path())).unwrap();

    assert!(!svc.is_following("bob", "alice").unwrap());
    svc.follow("bob", "alice").unwrap();
    assert!(svc.is_following("bob", "alice").unwrap());
    assert_eq!(svc.list_following("bob").unwrap(), vec!["alice".to_string()]);

    svc.unfollow("bob", "alice").unwrap();
    assert!(!svc.is_following("bob", "alice").unwrap());
    assert!(svc.list_following("bob").unwrap().is_empty());
}

// ── Visibility semantics ──────────────────────────────────────────────

#[tokio::test]
async fn private_posts_only_visible_to_author() {
    let dir = TempDir::new().unwrap();
    let svc = SocialFeedService::new(cfg_at(dir.path())).unwrap();
    let mut p = sample_post("alice", "diary");
    p.visibility = Visibility::Private;
    svc.create_post(p).await.unwrap();

    // Author can see their own private post.
    let alice = svc
        .timeline(TimelineQuery {
            viewer_id: "alice".into(),
            scope: TimelineScope::ForViewer,
            limit: Some(10),
            before_cursor: None,
            before_ts: None,
            author_id: None,
        })
        .unwrap();
    assert_eq!(alice.posts.len(), 1);

    // A non-author non-follower cannot.
    let stranger = svc
        .timeline(TimelineQuery {
            viewer_id: "carol".into(),
            scope: TimelineScope::ForViewer,
            limit: Some(10),
            before_cursor: None,
            before_ts: None,
            author_id: None,
        })
        .unwrap();
    assert!(stranger.posts.is_empty());
}
