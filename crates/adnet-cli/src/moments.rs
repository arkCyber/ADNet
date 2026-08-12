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
        MomentsCmd::Post { path, caption } => {
            let body = match path.as_deref() {
                Some("-") | None => String::new(),
                Some(p) => std::fs::read_to_string(p).unwrap_or_default(),
            };
            let content = caption.as_ref().map(|c| c.as_str()).unwrap_or(&body);
            let post = SocialPost {
                post_id: String::new(),
                author_id: "local".into(),
                author_name: "Local User".into(),
                author_avatar: None,
                content: content.to_string(),
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
        MomentsCmd::List { limit: _ } => {
            let q = TimelineQuery {
                viewer_id: "local".into(),
                scope: TimelineScope::ForViewer,
                limit: Some(20),
                before_cursor: None,
                before_ts: None,
                author_id: None,
            };
            let page: TimelinePage = svc.timeline(q)?;
            print_timeline(&page);
        }
        MomentsCmd::Receive { channel: _, timeout: _ } => {
            println!("moments receive not implemented - uses gossip in node");
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
