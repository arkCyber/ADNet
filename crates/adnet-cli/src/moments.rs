//! `adnet moments …` — Social-feed (朋友圈) CLI surface.
//!
//! Mirrors the original Tauri command layer from
//! `Exodus@src-backup/src-tauri/src/microservice/social_feed_commands.rs`.
//! Subcommands:
//!
//! - `adnet moments post <file|->` — publish a post. `-` reads the
//!   post body from stdin.
//! - `adnet moments timeline <viewer>` — print the visible
//!   timeline for a viewer, oldest-first to newest-last.
//! - `adnet moments comment <post_id> <file|->` — add a comment.
//! - `adnet moments react <target_id> <reaction_type>` — add /
//!   remove a reaction.
//! - `adnet moments follow <follower_id> <following_id>` — follow.
//! - `adnet moments unfollow <follower_id> <following_id>` —
//!   unfollow.
//!
//! All subcommands are offline: they only touch
//! `<data_dir>/adnet_social_feed.db` (SQLite) and never spin up
//! the node. The gossip fan-out layer is gated by config; today we
//! stay in pure-storage mode so the CLI is hermetic.

use std::path::Path;

use adnet_socialfeed::{
    SocialFeedService, SocialFeedServiceConfig, TimelinePage, TimelineQuery, TimelineScope,
};
use adnet_types::invariants::{
    ReactionTarget, ReactionType, Visibility,
};
use adnet_types::social_feed::{
    SocialComment, SocialPost, SocialReaction,
};
use adnet_types::NodeId;
use chrono::Utc;

use crate::cli::MomentsCmd;

/// Top-level dispatcher. Mirrors the `roster::run` /
/// `userstore::run` pattern so `main.rs` can route
/// `Cmd::Moments { sub }` through this function before the Node
/// is constructed.
pub async fn run(sub: &MomentsCmd, data_dir: &Path) -> anyhow::Result<()> {
    run_async(sub, data_dir).await
}

async fn run_async(sub: &MomentsCmd, data_dir: &Path) -> anyhow::Result<()> {
    let svc = make_service(data_dir)?;
    match sub {
        MomentsCmd::Post { file, visibility, author, content } => {
            let body = read_text(file)?;
            let post = SocialPost {
                post_id: String::new(),
                author_id: author.into(),
                author_name: author.into(),
                author_avatar: None,
                content: if !content.is_empty() { content.clone() } else { body },
                attachments: vec![],
                tags: vec![],
                visibility: parse_visibility(visibility)?,
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
            };
            let stored = svc.create_post(post).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "post_id": stored.post_id,
                    "author_id": stored.author_id,
                    "created_at": stored.created_at,
                    "visibility": stored.visibility.as_str(),
                    "integrity_hash": stored.integrity_hash,
                }))?
            );
        }
        MomentsCmd::Timeline { viewer, limit } => {
            let q = TimelineQuery {
                viewer_id: viewer.clone(),
                scope: TimelineScope::ForViewer,
                limit: *limit,
                before_cursor: None,
                before_ts: None,
                author_id: None,
            };
            let page: TimelinePage = svc.timeline(q)?;
            print_timeline(&page);
        }
        MomentsCmd::Comment { post_id, author, file } => {
            let body = read_text(file)?;
            let c = SocialComment {
                comment_id: String::new(),
                post_id: post_id.clone(),
                author_id: author.into(),
                author_name: author.into(),
                author_avatar: None,
                content: body,
                parent_id: None,
                mentions: vec![],
                created_at: 0,
                updated_at: 0,
                like_count: 0,
                reply_count: 0,
                is_edited: false,
                edited_at: None,
            };
            let stored = svc.comment_post(c).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "comment_id": stored.comment_id,
                    "post_id": stored.post_id,
                    "author_id": stored.author_id,
                    "created_at": stored.created_at,
                }))?
            );
        }
        MomentsCmd::React {
            target_id,
            target_type,
            user_id,
            reaction,
        } => {
            let kind = parse_reaction_kind(reaction)?;
            let target_kind = parse_target_kind(target_type)?;
            let mut r = SocialReaction {
                reaction_id: format!("r-{}", simple_uuid()),
                target_id: target_id.clone(),
                target_type: target_kind,
                user_id: user_id.into(),
                reaction_type: kind,
                created_at: Utc::now().timestamp_millis() as u64,
            };
            let inserted = svc.react(r.clone()).await?;
            // Re-fire the same reaction later → idempotent.
            r.reaction_id = format!("r-{}", simple_uuid());
            let reinserted = svc.react(r).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "inserted": inserted,
                    "idempotent_no_op_on_second_call": !reinserted,
                }))?
            );
        }
        MomentsCmd::Follow { follower, following } => {
            svc.follow(follower, following)?;
            println!("ok: {follower} now follows {following}");
        }
        MomentsCmd::Unfollow { follower, following } => {
            svc.unfollow(follower, following)?;
            println!("ok: {follower} no longer follows {following}");
        }
    }
    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────────

fn make_service(data_dir: &Path) -> anyhow::Result<SocialFeedService> {
    let dir = data_dir.to_path_buf();
    std::fs::create_dir_all(&dir)?;
    let cfg = SocialFeedServiceConfig {
        storage: adnet_socialfeed::SocialFeedStorageConfig {
            storage_dir: dir,
            filename: "adnet_social_feed.db".into(),
        },
        gossip: None,
        local_node: Some(NodeId::random()),
        validation_policy: adnet_ipc::validation::ValidationPolicy::Strict,
        gossip_transport: None,
    };
    Ok(SocialFeedService::new(cfg)?)
}

fn read_text(file: &str) -> anyhow::Result<String> {
    if file == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
        return Ok(buf.trim_end().to_string());
    }
    let s = std::fs::read_to_string(file)?;
    Ok(s.trim_end().to_string())
}

fn parse_visibility(s: &str) -> anyhow::Result<Visibility> {
    match s {
        "public" => Ok(Visibility::Public),
        "friends" => Ok(Visibility::Friends),
        "private" => Ok(Visibility::Private),
        other => anyhow::bail!(
            "unknown visibility: {other:?} (expected public | friends | private)"
        ),
    }
}

fn parse_reaction_kind(s: &str) -> anyhow::Result<ReactionType> {
    match s {
        "like" => Ok(ReactionType::Like),
        "love" => Ok(ReactionType::Love),
        "laugh" => Ok(ReactionType::Laugh),
        "wow" => Ok(ReactionType::Wow),
        "sad" => Ok(ReactionType::Sad),
        "angry" => Ok(ReactionType::Angry),
        other => anyhow::bail!(
            "unknown reaction: {other:?} (expected like | love | laugh | wow | sad | angry)"
        ),
    }
}

fn parse_target_kind(s: &str) -> anyhow::Result<ReactionTarget> {
    match s {
        "post" => Ok(ReactionTarget::Post),
        "comment" => Ok(ReactionTarget::Comment),
        other => anyhow::bail!(
            "unknown target type: {other:?} (expected post | comment)"
        ),
    }
}

fn print_timeline(page: &TimelinePage) {
    println!("{} post(s):", page.posts.len());
    for p in &page.posts {
        println!(
            "  [{}] {} — {}",
            p.created_at,
            p.author_id,
            one_line(&p.content)
        );
        if let Some(h) = &p.integrity_hash {
            println!("       integrity: {h}");
        }
    }
    if let Some(next) = page.next_cursor.as_ref() {
        println!("next: --before-ts {} --before-id {}", next.created_at, next.post_id);
    }
}

fn one_line(s: &str) -> String {
    s.replace('\n', " ").chars().take(80).collect()
}

/// Lightweight unique-id helper. Avoids pulling in the `uuid`
/// crate (the CLI is already heavy). The string is unique per
/// process invocation.
fn simple_uuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{ts}-{n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_parses_public() {
        assert!(parse_visibility("public").is_ok());
    }
    #[test]
    fn visibility_rejects_unknown() {
        assert!(parse_visibility("publicity").is_err());
    }
    #[test]
    fn reaction_parses_like() {
        assert!(parse_reaction_kind("like").is_ok());
    }
    #[test]
    fn target_kind_parses_comment() {
        assert!(parse_target_kind("comment").is_ok());
    }
}
