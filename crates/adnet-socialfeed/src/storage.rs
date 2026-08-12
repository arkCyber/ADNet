//! SQLite persistence layer for the social feed.
//!
//! Mirrors the design of [`adnet_chatstore::storage::ChatStorage`]
//! so callers get the same operational semantics they are used
//! to:
//!
//! - one `SocialFeedStorage` per node, backed by a single SQLite
//!   file under `config.storage_dir`.
//! - `journal_mode=WAL`, `foreign_keys=ON`,
//!   `synchronous=NORMAL` configured at open time.
//! - DO-178C startup probe via `PRAGMA integrity_check`: a
//!   corrupt DB refuses to open instead of silently returning
//!   per-row errors.
//! - all writes validated *before* reaching SQLite, so a malformed
//!   record never lands on disk.
//!
//! # Tables
//!
//! - `posts`                — `SocialPost` payloads, keyed by
//!   `post_id`; presence-only, no user partitioning (timeline is
//!   global within a node).
//! - `post_attachments`     — `PostAttachment` rows attached to a
//!   post (1:N).
//! - `post_tags`            — tags vector (separate table to keep
//!   the row width small).
//! - `post_mentions`        — `mentions` vector.
//! - `user_posts`           — secondary index `author_id -> post_id`
//!   for "my posts" listings.
//! - `comments`             — `SocialComment` payloads, keyed by
//!   `comment_id`.
//! - `comment_mentions`     — mentions vector for comments.
//! - `post_comments`        — secondary index `post_id -> comment_id`.
//! - `reactions`            — `SocialReaction` records, unique on
//!   `(target_id, user_id, reaction_type)` so the same user cannot
//!   double-like.
//! - `follows`              — `FollowRelationship`, unique on
//!   `(follower_id, following_id)`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use tracing::{debug, info};

use crate::error::{Result, SocialFeedError};
use crate::storage_schema::{CREATE_STATEMENTS, SCHEMA_VERSION};

/// Configuration for [`SocialFeedStorage`].
#[derive(Debug, Clone)]
pub struct SocialFeedStorageConfig {
    /// Directory holding the SQLite file. Created if missing.
    pub storage_dir: PathBuf,
    /// Override the SQLite file name (mainly used by tests so
    /// parallel runs can't collide).
    pub filename: String,
}

impl SocialFeedStorageConfig {
    fn db_path(&self) -> PathBuf {
        self.storage_dir.join(&self.filename)
    }
}

impl Default for SocialFeedStorageConfig {
    fn default() -> Self {
        let mut storage_dir = std::env::temp_dir();
        storage_dir.push("adnet_social_feed");
        Self {
            storage_dir,
            filename: "adnet_social_feed.db".into(),
        }
    }
}

/// SQLite-backed social-feed persistence.
///
/// All operations take a single `std::sync::Mutex<Connection>`.
/// The lock guard is *not* held across `.await` points; the public
/// API is fully synchronous.
#[derive(Debug)]
pub struct SocialFeedStorage {
    config: SocialFeedStorageConfig,
    db: Arc<Mutex<Connection>>,
}

impl SocialFeedStorage {
    /// Open (or create) the database at the configured path.
    pub fn new(config: SocialFeedStorageConfig) -> Result<Self> {
        std::fs::create_dir_all(&config.storage_dir)?;
        let db_path = config.db_path();
        let conn = Connection::open(&db_path)?;
        configure_connection(&conn)?;
        apply_schema(&conn)?;

        // Fail-fast startup probe (DO-178C).
        let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(SocialFeedError::Database(format!(
                "integrity_check failed: {integrity}"
            )));
        }

        info!(path = %db_path.display(), "social feed storage opened");
        Ok(Self {
            config,
            db: Arc::new(Mutex::new(conn)),
        })
    }

    /// Configuration this storage was opened with (handy for
    /// builders / smoke tests).
    pub fn config(&self) -> &SocialFeedStorageConfig {
        &self.config
    }

    /// Storage path (file path of the SQLite database).
    pub fn db_path(&self) -> PathBuf {
        self.config.db_path()
    }

    /// Storage directory (parent of the SQLite file).
    pub fn storage_dir(&self) -> &Path {
        &self.config.storage_dir
    }

    /// Acquire the underlying mutex. Provided for callers that need
    /// to chain transactions (e.g. bridge tests). Public so the
    /// integration tests can craft stress scenarios without
    /// breaking encapsulation.
    pub fn handle(&self) -> std::sync::MutexGuard<'_, Connection> {
        // Recover from poisoned mutexes by swallowing the error —
        // the alternative is to panic on every subsequent write,
        // which would make a transient poison event cascade
        // permanently. This matches `adnet-chatstore`'s
        // recovery policy.
        self.db.lock().unwrap_or_else(|poison| {
            tracing::error!(error = %poison, "social feed storage mutex poisoned");
            poison.into_inner()
        })
    }

    // ─────────────────────────────────────────────────────────────────
    // Posts
    // ─────────────────────────────────────────────────────────────────

    /// Insert (or replace) a post and all its child rows.
    pub fn save_post(&self, post: &adnet_types::social_feed::SocialPost) -> Result<()> {
        post.validate()?;
        let mut p = post.clone();
        p.stamp_integrity_hash();
        let conn = self.handle();
        let tx = conn.unchecked_transaction()?;
        write_post(&tx, &p)?;
        tx.commit()?;
        debug!(post_id = %p.post_id, "save_post committed");
        Ok(())
    }

    /// Look up a post by id. `Ok(None)` if not present.
    pub fn get_post(&self, post_id: &str) -> Result<Option<adnet_types::social_feed::SocialPost>> {
        adnet_types::invariants::validate_id("post_id", post_id)?;
        let conn = self.handle();
        let row = conn
            .query_row(
                "SELECT post_json FROM posts WHERE post_id = ?1",
                params![post_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some(json) => {
                let p: adnet_types::social_feed::SocialPost = serde_json::from_str(&json)?;
                p.validate()?;
                Ok(Some(p))
            }
        }
    }

    /// All posts authored by `user_id`, newest first.
    pub fn list_user_posts(&self, user_id: &str) -> Result<Vec<adnet_types::social_feed::SocialPost>> {
        let _ = adnet_types::invariants::validate_id("user_id", user_id)?;
        let conn = self.handle();
        let mut stmt = conn.prepare_cached(
            "SELECT p.post_json FROM posts p
             JOIN user_posts up ON up.post_id = p.post_id
             WHERE up.user_id = ?1
             ORDER BY p.created_at DESC",
        )?;
        let rows = stmt.query_map(params![user_id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            let json = json?;
            let p: adnet_types::social_feed::SocialPost = serde_json::from_str(&json)?;
            p.validate()?;
            out.push(p);
        }
        Ok(out)
    }

    /// Delete a post (and its child rows). Idempotent: missing
    /// rows are treated as success.
    pub fn delete_post(&self, post_id: &str) -> Result<()> {
        adnet_types::invariants::validate_id("post_id", post_id)?;
        let conn = self.handle();
        let tx = conn.unchecked_transaction()?;

        // Tables that key child rows on `post_id`.
        for table in [
            "post_attachments",
            "post_tags",
            "post_mentions",
            "user_posts",
            "post_comments",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE post_id = ?1"),
                params![post_id],
            )?;
        }
        // `comment_mentions` keys on comment_id; we will cascade
        // after comments are deleted below.
        tx.execute(
            "DELETE FROM comment_mentions WHERE comment_id IN
             (SELECT comment_id FROM comments WHERE post_id = ?1)",
            params![post_id],
        )?;
        // Reactions keyed by `target_id` (may be post or comment) —
        // expand the comment set first so we cascade-reactions on
        // comments that live under this post.
        let mut reaction_stmt = tx.prepare_cached(
            "SELECT comment_id FROM comments WHERE post_id = ?1",
        )?;
        let rows = reaction_stmt
            .query_map(params![post_id], |row| row.get::<_, String>(0))?;
        let mut targets = vec![post_id.to_string()];
        for c in rows {
            targets.push(c?);
        }
        drop(reaction_stmt);
        // Delete in bulk.
        for t in &targets {
            tx.execute(
                "DELETE FROM reactions WHERE target_id = ?1",
                params![t],
            )?;
        }
        // Now drop comments and the post.
        tx.execute("DELETE FROM comments WHERE post_id = ?1", params![post_id])?;
        tx.execute("DELETE FROM posts WHERE post_id = ?1", params![post_id])?;
        tx.commit()?;
        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────
    // Comments
    // ─────────────────────────────────────────────────────────────────

    pub fn save_comment(&self, comment: &adnet_types::social_feed::SocialComment) -> Result<()> {
        comment.validate()?;
        let conn = self.handle();
        let tx = conn.unchecked_transaction()?;
        write_comment(&tx, comment)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_post_comments(
        &self,
        post_id: &str,
    ) -> Result<Vec<adnet_types::social_feed::SocialComment>> {
        let _ = adnet_types::invariants::validate_id("post_id", post_id)?;
        let conn = self.handle();
        let mut stmt = conn.prepare_cached(
            "SELECT c.comment_json FROM comments c
             JOIN post_comments pc ON pc.comment_id = c.comment_id
             WHERE pc.post_id = ?1
             ORDER BY c.created_at ASC",
        )?;
        let rows = stmt.query_map(params![post_id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            let json = json?;
            let c: adnet_types::social_feed::SocialComment = serde_json::from_str(&json)?;
            c.validate()?;
            out.push(c);
        }
        Ok(out)
    }

    // ─────────────────────────────────────────────────────────────────
    // Reactions
    // ─────────────────────────────────────────────────────────────────

    /// Insert a reaction. Returns `Ok(false)` if the same user
    /// already reacted with the same kind to the same target
    /// (idempotent no-op).
    pub fn save_reaction(
        &self,
        reaction: &adnet_types::social_feed::SocialReaction,
    ) -> Result<bool> {
        reaction.validate()?;
        let conn = self.handle();
        let tx = conn.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO reactions
             (reaction_id, target_id, target_type, user_id, reaction_type, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                reaction.reaction_id,
                reaction.target_id,
                reaction.target_type.as_str(),
                reaction.user_id,
                reaction.reaction_type.as_str(),
                serde_json::to_string(reaction)?,
                reaction.created_at as i64,
            ],
        )?;
        tx.commit()?;
        Ok(inserted > 0)
    }

    pub fn list_reactions(
        &self,
        target_id: &str,
    ) -> Result<Vec<adnet_types::social_feed::SocialReaction>> {
        let _ = adnet_types::invariants::validate_id("target_id", target_id)?;
        let conn = self.handle();
        let mut stmt = conn.prepare_cached(
            "SELECT payload_json FROM reactions WHERE target_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![target_id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            let json = json?;
            let r: adnet_types::social_feed::SocialReaction = serde_json::from_str(&json)?;
            r.validate()?;
            out.push(r);
        }
        Ok(out)
    }

    // ─────────────────────────────────────────────────────────────────
    // Follows
    // ─────────────────────────────────────────────────────────────────

    pub fn save_follow(
        &self,
        follow: &adnet_types::social_feed::FollowRelationship,
    ) -> Result<()> {
        follow.validate()?;
        let conn = self.handle();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO follows (follower_id, following_id, created_at)
             VALUES (?1, ?2, ?3)",
            params![follow.follower_id, follow.following_id, follow.created_at as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn is_following(&self, follower_id: &str, following_id: &str) -> Result<bool> {
        let _ = adnet_types::invariants::validate_id("follower_id", follower_id)?;
        let _ = adnet_types::invariants::validate_id("following_id", following_id)?;
        let conn = self.handle();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM follows WHERE follower_id = ?1 AND following_id = ?2",
            params![follower_id, following_id],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn list_following(&self, follower_id: &str) -> Result<Vec<String>> {
        let _ = adnet_types::invariants::validate_id("follower_id", follower_id)?;
        let conn = self.handle();
        let mut stmt = conn.prepare_cached(
            "SELECT following_id FROM follows WHERE follower_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![follower_id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for id in rows {
            out.push(id?);
        }
        Ok(out)
    }

    pub fn unfollow(&self, follower_id: &str, following_id: &str) -> Result<()> {
        let _ = adnet_types::invariants::validate_id("follower_id", follower_id)?;
        let _ = adnet_types::invariants::validate_id("following_id", following_id)?;
        let conn = self.handle();
        conn.execute(
            "DELETE FROM follows WHERE follower_id = ?1 AND following_id = ?2",
            params![follower_id, following_id],
        )?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────

fn write_post(
    tx: &Transaction<'_>,
    post: &adnet_types::social_feed::SocialPost,
) -> Result<()> {
    let json = serde_json::to_string(post)?;
    tx.execute(
        "INSERT OR REPLACE INTO posts
         (post_id, author_id, visibility, created_at, updated_at, sequence, post_json, integrity_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            post.post_id,
            post.author_id,
            post.visibility.as_str(),
            post.created_at as i64,
            post.updated_at as i64,
            post.sequence as i64,
            json,
            post.integrity_hash.as_deref().unwrap_or(""),
        ],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO user_posts (user_id, post_id, created_at)
         VALUES (?1, ?2, ?3)",
        params![post.author_id, post.post_id, post.created_at as i64],
    )?;
    tx.execute("DELETE FROM post_attachments WHERE post_id = ?1", params![post.post_id])?;
    for (i, a) in post.attachments.iter().enumerate() {
        tx.execute(
            "INSERT INTO post_attachments
             (post_id, ord, attachment_id, attachment_type, blob_hash, file_name, file_size, thumbnail_hash, caption)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                post.post_id,
                i as i64,
                a.attachment_id,
                a.attachment_type.as_str(),
                a.blob_hash,
                a.file_name,
                a.file_size as i64,
                a.thumbnail_hash,
                a.caption,
            ],
        )?;
    }
    tx.execute("DELETE FROM post_tags WHERE post_id = ?1", params![post.post_id])?;
    for tag in &post.tags {
        tx.execute(
            "INSERT INTO post_tags (post_id, tag) VALUES (?1, ?2)",
            params![post.post_id, tag],
        )?;
    }
    tx.execute("DELETE FROM post_mentions WHERE post_id = ?1", params![post.post_id])?;
    for m in &post.mentions {
        tx.execute(
            "INSERT INTO post_mentions (post_id, mention_id) VALUES (?1, ?2)",
            params![post.post_id, m],
        )?;
    }
    Ok(())
}

fn write_comment(
    tx: &Transaction<'_>,
    comment: &adnet_types::social_feed::SocialComment,
) -> Result<()> {
    let json = serde_json::to_string(comment)?;
    tx.execute(
        "INSERT OR REPLACE INTO comments
         (comment_id, post_id, author_id, parent_id, created_at, updated_at, comment_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            comment.comment_id,
            comment.post_id,
            comment.author_id,
            comment.parent_id,
            comment.created_at as i64,
            comment.updated_at as i64,
            json,
        ],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO post_comments (post_id, comment_id) VALUES (?1, ?2)",
        params![comment.post_id, comment.comment_id],
    )?;
    tx.execute("DELETE FROM comment_mentions WHERE comment_id = ?1", params![comment.comment_id])?;
    for m in &comment.mentions {
        tx.execute(
            "INSERT INTO comment_mentions (comment_id, mention_id) VALUES (?1, ?2)",
            params![comment.comment_id, m],
        )?;
    }
    Ok(())
}

fn configure_connection(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    Ok(())
}

fn apply_schema(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for stmt in CREATE_STATEMENTS {
        tx.execute(stmt, [])?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO schema_version (id, version) VALUES (1, ?1)",
        params![SCHEMA_VERSION as i64],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::invariants::{AttachmentKind, ReactionTarget, ReactionType, Visibility};
    use adnet_types::social_feed::{PostAttachment, SocialComment, SocialPost, SocialReaction};
    use adnet_types::ContentHash;
    use chrono::Utc;
    use tempfile::tempdir;

    fn make_post(id: &str, author: &str) -> SocialPost {
        let mut p = SocialPost {
            post_id: id.into(),
            author_id: author.into(),
            author_name: author.into(),
            author_avatar: None,
            content: format!("hello from {author}"),
            attachments: vec![],
            tags: vec!["friends".into()],
            visibility: Visibility::Public,
            location: None,
            mentions: vec![],
            created_at: Utc::now().timestamp_millis() as u64,
            updated_at: Utc::now().timestamp_millis() as u64,
            like_count: 0,
            comment_count: 0,
            share_count: 0,
            public_account_id: None,
            integrity_hash: None,
            sequence: 1,
            is_edited: false,
            edited_at: None,
        };
        p.stamp_integrity_hash();
        p
    }

    fn make_comment(id: &str, post: &SocialPost) -> SocialComment {
        let now = Utc::now().timestamp_millis() as u64;
        SocialComment {
            comment_id: id.into(),
            post_id: post.post_id.clone(),
            author_id: post.author_id.clone(),
            author_name: post.author_name.clone(),
            author_avatar: None,
            content: "first!".into(),
            parent_id: None,
            mentions: vec![],
            created_at: now,
            updated_at: now,
            like_count: 0,
            reply_count: 0,
            is_edited: false,
            edited_at: None,
        }
    }

    fn make_reaction(target: &str, user: &str, kind: ReactionType) -> SocialReaction {
        SocialReaction {
            reaction_id: format!("r-{target}-{user}"),
            target_id: target.into(),
            target_type: ReactionTarget::Post,
            user_id: user.into(),
            reaction_type: kind,
            created_at: Utc::now().timestamp_millis() as u64,
        }
    }

    fn open_temp_storage() -> (SocialFeedStorage, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let cfg = SocialFeedStorageConfig {
            storage_dir: dir.path().to_path_buf(),
            filename: "test.db".into(),
        };
        let store = SocialFeedStorage::new(cfg).unwrap();
        (store, dir)
    }

    #[test]
    fn open_creates_schema_and_passes_integrity() {
        let (_s, _d) = open_temp_storage();
    }

    #[test]
    fn save_and_get_post_roundtrip() {
        let (s, _d) = open_temp_storage();
        let p = make_post("p1", "alice");
        s.save_post(&p).unwrap();
        let got = s.get_post("p1").unwrap().unwrap();
        assert_eq!(got, p);
    }

    #[test]
    fn save_rejects_invalid_post() {
        let (s, _d) = open_temp_storage();
        let mut p = make_post("p1", "alice");
        p.post_id = "".into();
        assert!(s.save_post(&p).is_err());
    }

    #[test]
    fn list_user_posts_returns_descending() {
        let (s, _d) = open_temp_storage();
        let now = Utc::now().timestamp_millis() as u64;
        for (i, ts) in [(0u64, now + 1), (1, now + 2), (2, now + 3)] {
            let mut p = make_post(&format!("p{i}"), "alice");
            p.created_at = ts;
            p.updated_at = ts;
            p.stamp_integrity_hash();
            s.save_post(&p).unwrap();
        }
        let listed = s.list_user_posts("alice").unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].post_id, "p2");
        assert_eq!(listed[2].post_id, "p0");
    }

    #[test]
    fn save_post_with_attachment_then_query_returns_post() {
        let (s, _d) = open_temp_storage();
        let mut p = make_post("p1", "alice");
        p.attachments.push(PostAttachment {
            attachment_id: "a1".into(),
            attachment_type: AttachmentKind::Image,
            blob_hash: ContentHash::from_bytes(b"hello").as_hex().to_string(),
            file_name: "cat.png".into(),
            file_size: 1234,
            thumbnail_hash: None,
            caption: None,
        });
        p.stamp_integrity_hash();
        s.save_post(&p).unwrap();
        let got = s.get_post("p1").unwrap().unwrap();
        assert_eq!(got.attachments.len(), 1);
        assert_eq!(got.attachments[0].file_name, "cat.png");
    }

    #[test]
    fn save_comment_and_list() {
        let (s, _d) = open_temp_storage();
        let p = make_post("p1", "alice");
        s.save_post(&p).unwrap();
        let c = make_comment("c1", &p);
        s.save_comment(&c).unwrap();
        let listed = s.list_post_comments("p1").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].comment_id, "c1");
    }

    #[test]
    fn save_reaction_is_idempotent_per_user_kind() {
        let (s, _d) = open_temp_storage();
        let r1 = make_reaction("p1", "bob", ReactionType::Like);
        let r2 = make_reaction("p1", "bob", ReactionType::Like);
        assert!(s.save_reaction(&r1).unwrap());
        // Same user + same kind on same target is a no-op.
        assert!(!s.save_reaction(&r2).unwrap());
        let listed = s.list_reactions("p1").unwrap();
        assert_eq!(listed.len(), 1);
    }

    #[test]
    fn follows_roundtrip() {
        let (s, _d) = open_temp_storage();
        s.save_follow(&adnet_types::social_feed::FollowRelationship {
            follower_id: "bob".into(),
            following_id: "alice".into(),
            created_at: 1,
        })
        .unwrap();
        assert!(s.is_following("bob", "alice").unwrap());
        let following = s.list_following("bob").unwrap();
        assert_eq!(following, vec!["alice".to_string()]);
        s.unfollow("bob", "alice").unwrap();
        assert!(!s.is_following("bob", "alice").unwrap());
    }

    #[test]
    fn delete_post_drops_everything() {
        let (s, _d) = open_temp_storage();
        let p = make_post("p1", "alice");
        s.save_post(&p).unwrap();
        s.save_comment(&make_comment("c1", &p)).unwrap();
        s.save_reaction(&make_reaction("p1", "bob", ReactionType::Like))
            .unwrap();
        s.save_reaction(&make_reaction("c1", "bob", ReactionType::Like))
            .unwrap();
        s.delete_post("p1").unwrap();
        assert!(s.get_post("p1").unwrap().is_none());
        assert!(s.list_post_comments("p1").unwrap().is_empty());
        assert!(s.list_reactions("p1").unwrap().is_empty());
        assert!(s.list_reactions("c1").unwrap().is_empty());
    }
}
