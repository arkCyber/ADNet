//! Canonical SQLite schema for the social feed storage.
//!
//! Mirrors `adnet-chatstore`'s `schema.rs` philosophy: a single
//! immutable list of `CREATE TABLE IF NOT EXISTS` /
//! `CREATE INDEX IF NOT EXISTS` statements behind an idempotent
//! `apply_schema()`. Schema versioning is stamped in
//! `schema_version`; the migration ladder is a future concern
//! (today there's only version 1).
//!
//! # Tables
//!
//! - `posts`              — `SocialPost` payloads (JSON
//!   serialisation; integrity hash stored separately for
//!   fast lookup).
//! - `post_attachments`   — `PostAttachment` 1:N child rows.
//! - `post_tags` / `post_mentions` / `comment_mentions` —
//!   vector fields extracted for indexing.
//! - `user_posts`         — `author_id -> post_id` index.
//! - `comments`           — `SocialComment` payloads.
//! - `post_comments`      — `post_id -> comment_id` index.
//! - `reactions`          — `SocialReaction` payloads (uniqueness
//!   on `(target_id, user_id, reaction_type)` so a user can't
//!   double-like).
//! - `follows`            — `FollowRelationship` rows.
//! - `schema_version`     — version stamp.

/// Current schema version. Bump on every change and add a
/// migration step in `apply_schema` (or a future
/// `migrate_to`).
pub const SCHEMA_VERSION: u32 = 1;

pub(super) const CREATE_STATEMENTS: &[&str] = &[
    // Versioning
    "CREATE TABLE IF NOT EXISTS schema_version (
        id      INTEGER PRIMARY KEY CHECK (id = 1),
        version INTEGER NOT NULL
    )",
    // Posts
    "CREATE TABLE IF NOT EXISTS posts (
        post_id        TEXT PRIMARY KEY,
        author_id      TEXT NOT NULL,
        visibility     TEXT NOT NULL,
        created_at     INTEGER NOT NULL,
        updated_at     INTEGER NOT NULL,
        sequence       INTEGER NOT NULL,
        post_json      TEXT NOT NULL,
        integrity_hash TEXT NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_posts_author_created
        ON posts (author_id, created_at DESC)",
    "CREATE INDEX IF NOT EXISTS idx_posts_created
        ON posts (created_at DESC)",
    // Post child rows
    "CREATE TABLE IF NOT EXISTS post_attachments (
        post_id         TEXT NOT NULL,
        ord             INTEGER NOT NULL,
        attachment_id   TEXT NOT NULL,
        attachment_type TEXT NOT NULL,
        blob_hash       TEXT NOT NULL,
        file_name       TEXT NOT NULL,
        file_size       INTEGER NOT NULL,
        thumbnail_hash  TEXT,
        caption         TEXT,
        PRIMARY KEY (post_id, ord)
    )",
    "CREATE TABLE IF NOT EXISTS post_tags (
        post_id TEXT NOT NULL,
        tag     TEXT NOT NULL,
        PRIMARY KEY (post_id, tag)
    )",
    "CREATE TABLE IF NOT EXISTS post_mentions (
        post_id    TEXT NOT NULL,
        mention_id TEXT NOT NULL,
        PRIMARY KEY (post_id, mention_id)
    )",
    "CREATE TABLE IF NOT EXISTS user_posts (
        user_id    TEXT NOT NULL,
        post_id    TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (user_id, post_id)
    )",
    // Comments
    "CREATE TABLE IF NOT EXISTS comments (
        comment_id   TEXT PRIMARY KEY,
        post_id      TEXT NOT NULL,
        author_id    TEXT NOT NULL,
        parent_id    TEXT,
        created_at   INTEGER NOT NULL,
        updated_at   INTEGER NOT NULL,
        comment_json TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS post_comments (
        post_id    TEXT NOT NULL,
        comment_id TEXT NOT NULL,
        PRIMARY KEY (post_id, comment_id)
    )",
    "CREATE TABLE IF NOT EXISTS comment_mentions (
        comment_id TEXT NOT NULL,
        mention_id TEXT NOT NULL,
        PRIMARY KEY (comment_id, mention_id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_comments_post_created
        ON comments (post_id, created_at ASC)",
    // Reactions
    "CREATE TABLE IF NOT EXISTS reactions (
        reaction_id   TEXT PRIMARY KEY,
        target_id     TEXT NOT NULL,
        target_type   TEXT NOT NULL,
        user_id       TEXT NOT NULL,
        reaction_type TEXT NOT NULL,
        payload_json  TEXT NOT NULL,
        created_at    INTEGER NOT NULL,
        UNIQUE (target_id, user_id, reaction_type)
    )",
    "CREATE INDEX IF NOT EXISTS idx_reactions_target
        ON reactions (target_id)",
    // Follows
    "CREATE TABLE IF NOT EXISTS follows (
        follower_id  TEXT NOT NULL,
        following_id TEXT NOT NULL,
        created_at   INTEGER NOT NULL,
        PRIMARY KEY (follower_id, following_id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_follows_following
        ON follows (following_id)",
];
