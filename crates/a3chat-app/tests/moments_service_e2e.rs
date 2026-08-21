//! End-to-end integration tests for the **Moments / 朋友圈** (F-05)
//! stack:
//!
//! 1. [`MomentsService`] is wired under a fresh tempdir SQLite
//!    `moments.db`.
//! 2. [`moments_service::dispatch`] exercises every `a3chat.moments.*`
//!    method the same way the HTTP RPC layer would — JSON in / JSON
//!    out, no private field access.
//! 3. The [`NotificationBus`] emits the expected
//!    `MomentsPostCreated` / `MomentsCommentAdded` /
//!    `MomentsReactionToggled` / `MomentsPostDeleted` events.
//! 4. Follow / unfollow + list / check round-trips through the same
//!    dispatch path.
//! 5. The integrity-hash verifies for every post/comment/reaction
//!    that the service has stamped.

use std::sync::Arc;
use std::time::Duration;

use a3chat_app::moments_service::{
    self, MomentsConfig, MomentsService,
};
use a3chat_app::notification_bus::NotificationReceiver;
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::UserId;
use a3net_types::content::ContentHash;
use a3net_types::invariants::{
    AttachmentKind, ReactionTarget, ReactionType, Visibility,
};
use a3net_types::social_feed::{
    attachment_from_hash, SocialComment, SocialPost, SocialReaction,
};
use serde_json::json;
use tempfile::TempDir;

fn alice() -> UserId {
    UserId::from("alice-node")
}
fn bob() -> UserId {
    UserId::from("bob-node")
}
fn carol() -> UserId {
    UserId::from("carol-node")
}

/// Boot a fresh [`MomentsService`] on disk + a clone for the
/// dispatcher (mirroring how `A3chatApp` holds an `Arc` per service).
async fn boot() -> (TempDir, Arc<MomentsService>, Arc<MomentsService>) {
    let dir = TempDir::new().expect("tempdir");
    let cfg = MomentsConfig {
        data_dir: dir.path().to_path_buf(),
    };
    let svc = Arc::new(MomentsService::open(&cfg).expect("MomentsService::open"));
    let dup = Arc::clone(&svc);
    (dir, svc, dup)
}

/// Subscribe to the bus so tests can assert that the right
/// `A3chatEvent` was published. When a `filter_for` is provided,
/// events not addressed to that user are dropped — matching the
/// production SSE handler in `a3chat-rpc`.
fn subscribe(svc: &MomentsService, filter_for: Option<&UserId>) -> NotificationReceiver {
    match filter_for {
        Some(uid) => svc.bus().subscribe_for(uid.clone()),
        None => svc.bus().subscribe(),
    }
}

fn post_json(content: &str) -> serde_json::Value {
    json!({
        "post": {
            "post_id": "",
            "author_id": "",
            "author_name": "",
            "author_avatar": null,
            "content": content,
            "attachments": [],
            "tags": [],
            "visibility": "public",
            "location": null,
            "mentions": [],
            "created_at": 0,
            "updated_at": 0,
            "like_count": 0,
            "comment_count": 0,
            "share_count": 0,
            "public_account_id": null,
            "integrity_hash": null,
            "sequence": 1,
            "is_edited": false,
            "edited_at": null,
        }
    })
}

#[tokio::test]
async fn node_info_round_trip() {
    let (_dir, svc, dup) = boot().await;
    let v = moments_service::dispatch(
        dup,
        "a3chat.moments.node_info",
        &alice(),
        serde_json::json!({}),
    )
    .await
    .expect("node_info");
    let info: moments_service::NodeInfo =
        serde_json::from_value(v).expect("node_info payload");
    assert!(!info.node_id.is_empty());
    assert!(info.ts > 0);
    // SCHEMA_VERSION is a `const u32` in a3net-socialfeed; we don't
    // hard-code the value here to avoid drift, only that it is
    // non-zero.
    assert!(info.schema_version > 0);
    // Confirm the local-node id matches the dispatcher view.
    assert_eq!(info.node_id, svc.local_node().to_string());
}

#[tokio::test]
async fn post_create_persists_stamps_integrity_and_emits_event() {
    let (_dir, svc, dup) = boot().await;
    let mut rx = subscribe(&svc, None);

    let params = post_json("first post \u{1F44D}");
    let out = moments_service::dispatch(
        dup.clone(),
        "a3chat.moments.post.create",
        &alice(),
        params,
    )
    .await
    .expect("create");
    let stored: SocialPost = serde_json::from_value(out).expect("stored post");

    // The facade stamps author_id / timestamps / sequence.
    assert_eq!(stored.author_id, alice().to_string());
    assert!(stored.created_at > 0);
    assert!(stored.integrity_hash.is_some());
    // integrity-hash must round-trip through the verify RPC.
    let verify = moments_service::dispatch(
        dup.clone(),
        "a3chat.moments.verify.post",
        &alice(),
        json!({ "post": stored.clone() }),
    )
    .await
    .expect("verify");
    assert_eq!(verify["valid"], json!(true));

    // Bus event matches the post id and the chat owner.
    let evt = tokio::time::timeout(Duration::from_millis(150), rx.recv())
        .await
        .expect("event timeout")
        .expect("event recv");
    match evt {
        A3chatEvent::MomentsPostCreated {
            user_id,
            post_id,
            author_id,
            visibility,
        } => {
            assert_eq!(user_id, alice());
            assert_eq!(post_id, stored.post_id);
            assert_eq!(author_id, alice().to_string());
            assert_eq!(visibility, "public");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn post_update_marks_edited_and_stamps_again() {
    let (_dir, _svc, dup) = boot().await;

    let mut p: SocialPost = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.post.create",
            &alice(),
            post_json("draft v1"),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    p.content = "draft v2 (edited)".into();
    let updated: SocialPost = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.post.update",
            &alice(),
            json!({ "post": p.clone() }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(updated.is_edited);
    assert!(updated.edited_at.is_some());
    assert!(updated.integrity_hash.is_some());

    // The post can now be re-fetched and verified.
    let env: moments_service::PostEnvelope = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.post.get",
            &alice(),
            json!({ "post_id": p.post_id }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let fetched = env.post.expect("post present");
    assert!(fetched.is_edited);
    assert_eq!(fetched.content, "draft v2 (edited)");
}

#[tokio::test]
async fn post_get_and_list_by_user() {
    let (_dir, _svc, dup) = boot().await;
    for body in ["alpha", "beta", "gamma"] {
        let _: serde_json::Value = moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.post.create",
            &alice(),
            post_json(body),
        )
        .await
        .unwrap();
    }
    // One post by bob so the by-user list only includes alice.
    let _: serde_json::Value = moments_service::dispatch(
        dup.clone(),
        "a3chat.moments.post.create",
        &bob(),
        post_json("bob shouldn't show up"),
    )
    .await
    .unwrap();

    let list: moments_service::PostsResult = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.posts.by_user",
            &alice(),
            json!({ "user_id": alice().to_string() }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(list.posts.len(), 3);
    for p in &list.posts {
        assert_eq!(p.author_id, alice().to_string());
    }
}

#[tokio::test]
async fn post_delete_emits_event() {
    let (_dir, svc, dup) = boot().await;
    let mut rx = subscribe(&svc, None);

    let stored: SocialPost = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.post.create",
            &alice(),
            post_json("to be deleted"),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let post_id = stored.post_id.clone();

    let ack: serde_json::Value = moments_service::dispatch(
        dup.clone(),
        "a3chat.moments.post.delete",
        &alice(),
        json!({ "post_id": post_id }),
    )
    .await
    .expect("delete");
    assert_eq!(ack["ok"], json!(true));

    // Drain the create event first; the delete event arrives after.
    let _ = tokio::time::timeout(Duration::from_millis(150), rx.recv())
        .await
        .expect("create event");
    let evt = tokio::time::timeout(Duration::from_millis(150), rx.recv())
        .await
        .expect("delete event timeout")
        .expect("delete event recv");
    match evt {
        A3chatEvent::MomentsPostDeleted {
            user_id, post_id, ..
        } => {
            assert_eq!(user_id, alice());
            assert_eq!(post_id, stored.post_id);
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // Subsequent get returns `post: null`.
    let env: moments_service::PostEnvelope = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.post.get",
            &alice(),
            json!({ "post_id": post_id }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(env.post.is_none());
}

#[tokio::test]
async fn comment_add_persists_lists_and_emits_event() {
    let (_dir, svc, dup) = boot().await;
    let mut rx = subscribe(&svc, None);

    let post: SocialPost = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.post.create",
            &alice(),
            post_json("photo of the day"),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let _ = rx.rx.try_recv(); // discard PostCreated

    let comment = SocialComment {
        comment_id: String::new(),
        post_id: post.post_id.clone(),
        author_id: String::new(),
        author_name: String::new(),
        author_avatar: None,
        content: "nice one!".into(),
        parent_id: None,
        mentions: vec![],
        created_at: 0,
        updated_at: 0,
        like_count: 0,
        reply_count: 0,
        is_edited: false,
        edited_at: None,
    };
    let stored: moments_service::CommentEnvelope = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.comment.add",
            &bob(),
            json!({ "comment": comment }),
        )
        .await
        .expect("comment"),
    )
    .unwrap();
    assert_eq!(stored.comment.author_id, bob().to_string());
    assert_eq!(stored.comment.post_id, post.post_id);
    assert!(stored.comment.created_at > 0);

    // Event matches.
    let evt = tokio::time::timeout(Duration::from_millis(150), rx.recv())
        .await
        .expect("event timeout")
        .expect("event recv");
    match evt {
        A3chatEvent::MomentsCommentAdded {
            user_id,
            post_id,
            comment_id,
            author_id,
        } => {
            assert_eq!(user_id, bob());
            assert_eq!(post_id, post.post_id);
            assert_eq!(comment_id, stored.comment.comment_id);
            assert_eq!(author_id, bob().to_string());
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // list comments returns the one we just added.
    let list: moments_service::CommentsResult = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.comments.list",
            &alice(),
            json!({ "post_id": post.post_id }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(list.comments.len(), 1);
    assert_eq!(list.comments[0].comment_id, stored.comment.comment_id);

    // Verify the integrity hash.
    let verify = moments_service::dispatch(
        dup.clone(),
        "a3chat.moments.verify.comment",
        &alice(),
        json!({ "comment": stored.comment }),
    )
    .await
    .unwrap();
    assert_eq!(verify["valid"], json!(true));
}

#[tokio::test]
async fn reaction_toggle_is_idempotent_and_emits_event_only_on_first_insert() {
    let (_dir, svc, dup) = boot().await;
    let mut rx = subscribe(&svc, None);

    let post: SocialPost = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.post.create",
            &alice(),
            post_json("please react"),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let _ = rx.rx.try_recv(); // discard PostCreated

    let reaction = || SocialReaction {
        reaction_id: String::new(),
        target_id: post.post_id.clone(),
        target_type: ReactionTarget::Post,
        user_id: String::new(),
        reaction_type: ReactionType::Like,
        created_at: 0,
    };

    // First like → `inserted = true` and a bus event fires.
    let first: moments_service::ReactResult = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.react",
            &bob(),
            json!({ "reaction": reaction() }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(first.inserted);
    let evt = tokio::time::timeout(Duration::from_millis(150), rx.recv())
        .await
        .expect("event timeout")
        .expect("event recv");
    match evt {
        A3chatEvent::MomentsReactionToggled {
            user_id,
            target_id,
            actor_id,
            reaction_type,
            is_added,
        } => {
            assert_eq!(user_id, bob());
            assert_eq!(target_id, post.post_id);
            assert_eq!(actor_id, bob().to_string());
            assert_eq!(reaction_type, "like");
            assert!(is_added);
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // Second like from the same actor → `inserted = false`, no event.
    let second: moments_service::ReactResult = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.react",
            &bob(),
            json!({ "reaction": reaction() }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(!second.inserted);
    // Give the bus a moment: nothing should arrive.
    let none = tokio::time::timeout(Duration::from_millis(80), rx.recv()).await;
    assert!(none.is_err(), "second like should not publish");

    // Reaction list returns exactly one row.
    let list: moments_service::ReactionsResult = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.reactions.list",
            &alice(),
            json!({ "target_id": post.post_id }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(list.reactions.len(), 1);
    assert_eq!(list.reactions[0].user_id, bob().to_string());
    assert_eq!(list.reactions[0].reaction_type, ReactionType::Like);

    // Verify reaction integrity hash.
    let verify = moments_service::dispatch(
        dup.clone(),
        "a3chat.moments.verify.reaction",
        &alice(),
        json!({ "reaction": list.reactions[0].clone() }),
    )
    .await
    .unwrap();
    assert_eq!(verify["valid"], json!(true));
}

#[tokio::test]
async fn follow_unfollow_list_and_check_round_trip() {
    let (_dir, _svc, dup) = boot().await;

    // alice follows bob & carol.
    for target in [bob(), carol()] {
        let ack: serde_json::Value = moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.follow",
            &alice(),
            json!({ "following_id": target.to_string() }),
        )
        .await
        .unwrap();
        assert_eq!(ack["ok"], json!(true));
    }

    // list following (defaults to caller).
    let list: moments_service::FollowingResult = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.following.list",
            &alice(),
            json!({}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(list.following_ids.len(), 2);
    assert!(list.following_ids.contains(&bob().to_string()));
    assert!(list.following_ids.contains(&carol().to_string()));

    // alice → bob yes, alice → carol yes, carol → bob no.
    for (caller, target, expect) in [
        (alice(), bob(), true),
        (alice(), carol(), true),
        (carol(), bob(), false),
    ] {
        let r: moments_service::FollowingCheckResult = serde_json::from_value(
            moments_service::dispatch(
                dup.clone(),
                "a3chat.moments.following.check",
                &caller,
                json!({
                    "follower_id": caller.to_string(),
                    "following_id": target.to_string(),
                }),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(r.following, expect, "{caller} -> {target}");
    }

    // alice unfollows carol.
    let ack: serde_json::Value = moments_service::dispatch(
        dup.clone(),
        "a3chat.moments.unfollow",
        &alice(),
        json!({ "following_id": carol().to_string() }),
    )
    .await
    .unwrap();
    assert_eq!(ack["ok"], json!(true));

    let after: moments_service::FollowingResult = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.following.list",
            &alice(),
            json!({}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(after.following_ids, vec![bob().to_string()]);
}

#[tokio::test]
async fn timeline_returns_only_viewable_posts() {
    let (_dir, _svc, dup) = boot().await;

    // alice publishes 2 public + 1 private + 1 friends-only post.
    let mut bodies = vec![
        ("hello public 1", Visibility::Public),
        ("hello public 2", Visibility::Public),
        ("secret diary", Visibility::Private),
        ("friends only", Visibility::Friends),
    ];
    for (body, _vis) in bodies.drain(..) {
        let mut p: SocialPost = serde_json::from_value({
            let mut v = post_json(body);
            v["post"].take()
        })
        .unwrap();
        // default visibility in `post_json` is public; we patch
        // inline below to avoid duplicating the full struct.
        if body == "secret diary" {
            p.visibility = Visibility::Private;
        } else if body == "friends only" {
            p.visibility = Visibility::Friends;
        }
        let _: serde_json::Value = moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.post.create",
            &alice(),
            json!({ "post": p }),
        )
        .await
        .unwrap();
    }
    // Bob publishes one public post — carol's "ForViewer" timeline
    // does not include bob's since carol doesn't follow him.
    let _: serde_json::Value = moments_service::dispatch(
        dup.clone(),
        "a3chat.moments.post.create",
        &bob(),
        post_json("bob here"),
    )
    .await
    .unwrap();
    // Carol follows alice.
    let _: serde_json::Value = moments_service::dispatch(
        dup.clone(),
        "a3chat.moments.follow",
        &carol(),
        json!({ "following_id": alice().to_string() }),
    )
    .await
    .unwrap();

    let page: moments_service::TimelineResult = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.timeline",
            &carol(),
            json!({}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    // Carol sees: 2 public + 1 friends from alice, no private.
    // Bob's public post is also visible to carol because public
    // posts are visible to everyone (the `is_visible_to` rule).
    // Bob's post is not from a followed author, but `ForViewer`
    // does not gate on the follow relationship for public posts.
    assert_eq!(page.posts.len(), 4);
    for p in &page.posts {
        assert_ne!(p.visibility, Visibility::Private);
    }
    // Alice's private post must never appear here.
    assert!(page.posts.iter().all(|p| p.content != "secret diary"));

    // Explicit `ForViewer` scope via params.
    let scoped: moments_service::TimelineResult = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.timeline",
            &carol(),
            json!({ "scope": "for_viewer" }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(scoped.posts.len(), 4);

    // `by_user` scope with `author_id` returns that user's posts.
    let alice_page: moments_service::TimelineResult = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.timeline",
            &carol(),
            json!({
                "scope": "by_user",
                "author_id": alice().to_string(),
            }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(alice_page.posts.len(), 4);
    for p in &alice_page.posts {
        assert_eq!(p.author_id, alice().to_string());
    }
    // Alice's 2 public posts must be visible; the friends-only post
    // is visible too because carol is a follower of alice.
    let public_in_alice = alice_page
        .posts
        .iter()
        .filter(|p| p.visibility == Visibility::Public)
        .count();
    assert_eq!(public_in_alice, 2);
}

#[tokio::test]
async fn timeline_pagination_round_trip() {
    let (_dir, _svc, dup) = boot().await;
    for i in 0..6 {
        let _: serde_json::Value = moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.post.create",
            &alice(),
            post_json(&format!("post #{i}")),
        )
        .await
        .unwrap();
        // 1 ms gap so created_at differs deterministically.
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    // First page.
    let p1: moments_service::TimelineResult = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.timeline",
            &alice(),
            json!({ "limit": 3 }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(p1.posts.len(), 3);
    let cursor = p1.next_cursor.expect("cursor present");

    // Second page using the cursor we received.
    let p2: moments_service::TimelineResult = serde_json::from_value(
        moments_service::dispatch(
            dup,
            "a3chat.moments.timeline",
            &alice(),
            json!({
                "limit": 3,
                "before_cursor": cursor,
            }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(p2.posts.len(), 3);
    // No overlap between the two pages.
    for a in &p1.posts {
        for b in &p2.posts {
            assert_ne!(a.post_id, b.post_id);
        }
    }
}

#[tokio::test]
async fn unknown_method_returns_invalid_input() {
    let (_dir, _svc, dup) = boot().await;
    let err = moments_service::dispatch(
        dup,
        "a3chat.moments.bogus",
        &alice(),
        json!({}),
    )
    .await
    .expect_err("must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("unknown moments method"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
async fn post_with_attachment_round_trips_through_dispatch() {
    let (_dir, _svc, dup) = boot().await;

    let blob_hash_obj = ContentHash::from_bytes(b"binary-image-bytes");
    let attachment = attachment_from_hash(
        format!("att-{}", uuid::Uuid::new_v4()),
        AttachmentKind::Image,
        &blob_hash_obj,
        "photo.jpg".to_string(),
        4096,
    );

    let mut p: SocialPost = serde_json::from_value({
        let mut v = post_json("with attachment");
        v["post"].take()
    })
    .unwrap();
    p.attachments.push(attachment);
    let stored: SocialPost = serde_json::from_value(
        moments_service::dispatch(
            dup.clone(),
            "a3chat.moments.post.create",
            &alice(),
            json!({ "post": p }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(stored.attachments.len(), 1);
    assert_eq!(stored.attachments[0].blob_hash, blob_hash_obj.as_hex().to_string());
}

#[tokio::test]
async fn moderation_blocks_post_when_attached() {
    use a3chat_app::moderation_service::{ModerationConfig, ModerationService};
    use a3net_types::content::ContentHash;

    let dir = TempDir::new().expect("tempdir");
    let mod_cfg = ModerationConfig::under_base(dir.path());
    let mod_svc = ModerationService::open(&mod_cfg).expect("ModerationService::open");

    // Pre-block the exact hash of the post body so `check_content`
    // denies the write before it hits SQLite.
    let body = "this content has been pre-blocked by moderation";
    let digest = blake3::hash(body.as_bytes()).to_hex().to_string();
    let hash = ContentHash::from_hex(&digest).expect("blake3 hex");
    mod_svc
        .block_hash(&hash, "test fixture")
        .expect("block_hash");

    let moments = MomentsService::open(&MomentsConfig {
        data_dir: dir.path().join("moments"),
    })
    .expect("moments open")
    .with_moderation(mod_svc);
    let dup = Arc::new(moments);

    let err = moments_service::dispatch(
        dup,
        "a3chat.moments.post.create",
        &alice(),
        post_json(body),
    )
    .await
    .expect_err("moderation must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("moderation denied"),
        "unexpected error: {msg}"
    );
}