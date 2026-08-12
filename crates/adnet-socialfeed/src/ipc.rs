//! JSON-RPC-over-Unix-socket service for the social feed.
//!
//! This is the ADNet port of the original
//! `Exodus@src-backup/src-tauri/src/microservice/social_feed_service.rs`
//! + `social_feed_commands.rs`. The original code lives behind a
//! Tauri command layer that talks to a single-machine Unix socket
//! service; the port re-shapes that into a stand-alone service
//! that:
//!
//! - hangs off the same `adnet-ipc` JSON-RPC machinery as the
//!   other ADNet services (typed `params` / `result`, fail-closed
//!   `Validate` gates);
//! - persists records in [`SocialFeedStorage`] (SQLite) by
//!   default. A pure in-memory backend is also exposed via
//!   [`SocialFeedIpcService::with_in_memory`] for unit tests that
//!   don't want to touch SQLite;
//! - exposes DO-178C-grade invariants via the shared
//!   [`ValidationPolicy`] (Strict by default).
//!
//! # Methods
//!
//! | method                       | params                                     | result            |
//! |------------------------------|--------------------------------------------|-------------------|
//! | `node_info`                  | `{}`                                       | `{node_id, ts}`   |
//! | `create_post`                | `{post: SocialPost}`                       | `{post}`          |
//! | `update_post`                | `{post: SocialPost}`                       | `{post}`          |
//! | `delete_post`                | `{post_id}`                                | `{ok}`            |
//! | `get_post`                   | `{post_id}`                                | `{post | null}`   |
//! | `list_user_posts`            | `{user_id}`                                | `{posts}`         |
//! | `timeline_for`               | `{viewer_id, limit?, before_ts?}`          | `{posts}`         |
//! | `comment_post`               | `{comment: SocialComment}`                 | `{comment}`       |
//! | `list_post_comments`         | `{post_id}`                                | `{comments}`      |
//! | `react`                      | `{reaction: SocialReaction}`               | `{inserted}`      |
//! | `list_reactions`             | `{target_id}`                              | `{reactions}`     |
//! | `follow`                     | `{follower_id, following_id}`              | `{ok}`            |
//! | `unfollow`                   | `{follower_id, following_id}`              | `{ok}`            |
//! | `list_following`             | `{follower_id}`                            | `{following_ids}` |
//! | `is_following`               | `{follower_id, following_id}`              | `{following}`     |
//! | `verify_post_integrity`      | `{post}`                                   | `{valid}`         |
//! | `verify_comment_integrity`   | `{comment}`                                | `{valid}`         |
//! | `verify_reaction_integrity`  | `{reaction}`                               | `{valid}`         |
//!
//! All records on the wire are the typed records from
//! [`adnet_types::social_feed`], serialised via `serde_json`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use adnet_ipc::server::{JsonRpcServer, JsonRpcServerHandle, RpcHandler};
use adnet_ipc::validation::{ValidationOutcome, ValidationPolicy};
use adnet_types::invariants::ReactionType;
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

use adnet_types::NodeId;
use adnet_types::social_feed::{
    FollowRelationship, SocialComment, SocialPost, SocialReaction,
};

use crate::error::{Result, SocialFeedError};
use crate::storage::{SocialFeedStorage, SocialFeedStorageConfig};

/// Re-export so downstream callers can configure the same
/// strictness matrix as the chat IPC service.
pub use adnet_ipc::validation::ValidationPolicy as SocialFeedValidationPolicy;

/// Configuration for [`SocialFeedIpcService`].
#[derive(Debug, Clone)]
pub struct SocialFeedIpcConfig {
    /// Unix-domain socket path the server binds to. Defaults to
    /// `$TMPDIR/adnet_social_feed.sock`.
    pub socket_path: PathBuf,
    /// Node identity of the local service instance. Reported by
    /// `node_info`.
    pub node_id: NodeId,
    /// Validation policy applied at every IPC entry point. Defaults
    /// to [`ValidationPolicy::Strict`].
    pub policy: ValidationPolicy,
    /// Storage config used by the SQLite backend (ignored when the
    /// service is built via [`SocialFeedIpcService::with_in_memory`]).
    pub storage: SocialFeedStorageConfig,
}

impl Default for SocialFeedIpcConfig {
    fn default() -> Self {
        Self {
            socket_path: std::env::temp_dir().join("adnet_social_feed.sock"),
            node_id: NodeId::random(),
            policy: ValidationPolicy::Strict,
            storage: SocialFeedStorageConfig::default(),
        }
    }
}

/// Backing store for the IPC service. The default implementation
/// persists everything in SQLite via [`SocialFeedStorage`]; an
/// in-memory variant lives in [`InMemoryBackend`] for unit tests
/// that want to bypass SQLite.
#[derive(Debug, Clone)]
pub(crate) enum BackingStore {
    Sqlite(Arc<SocialFeedStorage>),
    Memory(Arc<InMemoryBackend>),
}

/// In-memory equivalents of the SQLite-backed data. Used by
/// integration tests to keep the suite hermetic.
#[derive(Debug, Default)]
pub struct InMemoryBackend {
    /// `post_id -> post`
    pub posts: Mutex<HashMap<String, SocialPost>>,
    /// `post_id -> comments in insertion order`
    pub comments: Mutex<HashMap<String, Vec<SocialComment>>>,
    /// `(target_id, user_id, reaction_type) -> reaction`
    pub reactions: Mutex<HashMap<(String, String, ReactionType), SocialReaction>>,
    /// `follower_id -> following_ids`
    pub follows: Mutex<HashMap<String, Vec<String>>>,
}

impl InMemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Social feed service that can be served over JSON-RPC.
#[derive(Clone)]
pub struct SocialFeedIpcService {
    cfg: SocialFeedIpcConfig,
    store: BackingStore,
}

impl std::fmt::Debug for SocialFeedIpcService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocialFeedIpcService")
            .field("socket_path", &self.cfg.socket_path)
            .field("node_id", &self.cfg.node_id)
            .field("policy", &self.cfg.policy)
            .finish()
    }
}

impl SocialFeedIpcService {
    /// Build a service backed by SQLite (the production path).
    pub fn new(cfg: SocialFeedIpcConfig) -> Result<Self> {
        let storage = SocialFeedStorage::new(cfg.storage.clone())
            .map_err(|e| SocialFeedError::database(e))?;
        Ok(Self {
            cfg,
            store: BackingStore::Sqlite(Arc::new(storage)),
        })
    }

    /// Build a service backed by an in-memory hash map. Intended for
    /// unit tests that don't want to touch SQLite.
    pub fn with_in_memory(cfg: SocialFeedIpcConfig) -> Self {
        Self {
            cfg,
            store: BackingStore::Memory(Arc::new(InMemoryBackend::new())),
        }
    }

    /// Borrow the underlying SQLite storage handle (None when
    /// running in-memory).
    pub fn storage(&self) -> Option<&SocialFeedStorage> {
        match &self.store {
            BackingStore::Sqlite(s) => Some(s.as_ref()),
            BackingStore::Memory(_) => None,
        }
    }

    /// Borrow the underlying in-memory backend (None when running
    /// on SQLite).
    pub fn memory(&self) -> Option<&InMemoryBackend> {
        match &self.store {
            BackingStore::Memory(m) => Some(m.as_ref()),
            BackingStore::Sqlite(_) => None,
        }
    }

    /// Start the Unix socket server. Returns the handle so callers
    /// can shut it down on drop.
    pub async fn serve(self: Arc<Self>) -> std::result::Result<JsonRpcServerHandle, String> {
        JsonRpcServer::start(self.cfg.socket_path.clone(), self).await
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.cfg.socket_path
    }

    pub fn node_id(&self) -> &NodeId {
        &self.cfg.node_id
    }

    pub fn policy(&self) -> ValidationPolicy {
        self.cfg.policy
    }

    // ── DO-178C gates ────────────────────────────────────────────────────

    /// Apply the configured validation policy to `value` and
    /// accumulate warnings. Under [`ValidationPolicy::Strict`] the
    /// first failure becomes the return error.
    fn check<T: adnet_ipc::validation::Validate>(&self, value: &T, what: &str) -> ValidationOutcome {
        let mut out = ValidationOutcome::default();
        if let Err(e) = value.validate() {
            match self.cfg.policy {
                ValidationPolicy::Strict => {
                    out.error = Some(format!("{what}: {e}"));
                }
                ValidationPolicy::Audit => {
                    out.warnings.push(format!("{what}: {e}"));
                }
                ValidationPolicy::Lenient => {}
            }
        }
        out
    }

    fn gate<T: adnet_ipc::validation::Validate>(
        &self,
        value: &T,
        what: &str,
    ) -> std::result::Result<Vec<String>, String> {
        let outcome = self.check(value, what);
        if let Some(e) = outcome.error {
            return Err(e);
        }
        Ok(outcome.warnings)
    }

    fn require<T: DeserializeOwned>(
        value: &Value,
        field: &str,
    ) -> std::result::Result<T, String> {
        serde_json::from_value::<T>(
            value
                .get(field)
                .cloned()
                .ok_or_else(|| format!("missing field: {field}"))?,
        )
        .map_err(|e| format!("decode {field}: {e}"))
    }

    /// Server-side timestamp generator. Returns wall-clock
    /// **nanoseconds since the UNIX epoch** — nanosecond
    /// resolution gives 7 orders of magnitude headroom over
    /// millisecond resolution and prevents distinct events from
    /// collapsing onto the same `created_at` value when they
    /// happen in the same millisecond (which is common in
    /// tight-loop integration tests and back-fill scenarios).
    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }

    // ── High-level API (also used from CLI/FFI/IPC-consumer tests) ──────

    /// Create a new post. Auto-fills `post_id` (when empty) and
    /// stamps the integrity hash. Returns the stored record.
    pub fn create_post(&self, mut post: SocialPost) -> std::result::Result<SocialPost, String> {
        if post.post_id.is_empty() {
            post.post_id = format!("post-{}", Uuid::new_v4());
        }
        let now = Self::now();
        if post.created_at == 0 {
            post.created_at = now;
        }
        // Only bump `updated_at` from server time when the caller
        // didn't provide one. Otherwise a caller that wants to
        // back-fill historical records (e.g. a sync that imports
        // gossip-replayed posts) would hit "updated_at < created_at".
        if post.updated_at == 0 {
            post.updated_at = post.created_at;
        }
        // Re-check ordering: if the caller supplied a `created_at`
        // in the future and `updated_at` is also in the future at
        // server time, normalise `updated_at` to at least
        // `created_at`.
        if post.updated_at < post.created_at {
            post.updated_at = post.created_at;
        }
        self.gate(&post, "post")?;
        post.stamp_integrity_hash();

        match &self.store {
            BackingStore::Sqlite(s) => {
                s.save_post(&post).map_err(|e| e.to_string())?;
            }
            BackingStore::Memory(m) => {
                m.posts
                    .lock()
                    .map_err(|e| format!("lock: {e}"))?
                    .insert(post.post_id.clone(), post.clone());
            }
        }
        Ok(post)
    }

    /// Update an existing post. The record is expected to already
    /// exist; otherwise the storage layer returns `NotFound`.
    pub fn update_post(&self, mut post: SocialPost) -> std::result::Result<SocialPost, String> {
        if post.post_id.is_empty() {
            return Err("post_id is required".into());
        }
        let now = Self::now();
        post.updated_at = now;
        self.gate(&post, "post")?;
        post.stamp_integrity_hash();

        match &self.store {
            BackingStore::Sqlite(s) => {
                if s.get_post(&post.post_id)
                    .map_err(|e| e.to_string())?
                    .is_none()
                {
                    return Err(format!("post not found: {}", post.post_id));
                }
                s.save_post(&post).map_err(|e| e.to_string())?;
            }
            BackingStore::Memory(m) => {
                let mut guard = m
                    .posts
                    .lock()
                    .map_err(|e| format!("lock: {e}"))?;
                if !guard.contains_key(&post.post_id) {
                    return Err(format!("post not found: {}", post.post_id));
                }
                guard.insert(post.post_id.clone(), post.clone());
            }
        }
        Ok(post)
    }

    pub fn delete_post(&self, post_id: &str) -> std::result::Result<(), String> {
        match &self.store {
            BackingStore::Sqlite(s) => s.delete_post(post_id).map_err(|e| e.to_string()),
            BackingStore::Memory(m) => {
                m.posts
                    .lock()
                    .map_err(|e| format!("lock: {e}"))?
                    .remove(post_id)
                    .ok_or_else(|| format!("post not found: {post_id}"))?;
                Ok(())
            }
        }
    }

    pub fn get_post(&self, post_id: &str) -> std::result::Result<Option<SocialPost>, String> {
        match &self.store {
            BackingStore::Sqlite(s) => s.get_post(post_id).map_err(|e| e.to_string()),
            BackingStore::Memory(m) => Ok(m
                .posts
                .lock()
                .map_err(|e| format!("lock: {e}"))?
                .get(post_id)
                .cloned()),
        }
    }

    pub fn list_user_posts(&self, user_id: &str) -> std::result::Result<Vec<SocialPost>, String> {
        match &self.store {
            BackingStore::Sqlite(s) => s.list_user_posts(user_id).map_err(|e| e.to_string()),
            BackingStore::Memory(m) => {
                let mut out: Vec<SocialPost> = m
                    .posts
                    .lock()
                    .map_err(|e| format!("lock: {e}"))?
                    .values()
                    .filter(|p| p.author_id == user_id)
                    .cloned()
                    .collect();
                out.sort_by_key(|p| std::cmp::Reverse(p.created_at));
                Ok(out)
            }
        }
    }

/// Resolve the timeline visible to `viewer_id`. Visibility
    /// semantics:
    /// - `Public`           — everyone.
/// - `Friends`          — author + viewers that follow the author.
/// - `Private`          — author only.
///
/// Note: pagination (`limit` / `before_ts`) is handled by the
    /// higher-level `service::timeline` facade, not here. This
    /// keeps the SQLite and memory backends symmetric — both
    /// return *every* matching post, in `created_at DESC` order.
    /// The service layer then paginates against the full set so
    /// callers don't have to depend on internal slicing.
    pub fn timeline_for(
        &self,
        viewer_id: &str,
        _limit: Option<usize>,
        before_ts: Option<u64>,
    ) -> std::result::Result<Vec<SocialPost>, String> {
        let all: Vec<SocialPost> = match &self.store {
            BackingStore::Sqlite(_) => {
                let mut out = Vec::new();
                let authors = self.known_authors_sqlite()?;
                for a in authors {
                    let mut ps = self.list_user_posts(&a)?;
                    out.append(&mut ps);
                }
                out
            }
            BackingStore::Memory(m) => m
                .posts
                .lock()
                .map_err(|e| format!("lock: {e}"))?
                .values()
                .cloned()
                .collect(),
        };

        let following = self.list_following(viewer_id).unwrap_or_default();
        let mut out = Vec::new();
        for p in all.into_iter() {
            if let Some(b) = before_ts {
                if p.created_at >= b {
                    continue;
                }
            }
            if p.is_visible_to(viewer_id, &following) {
                out.push(p);
            }
        }
        out.sort_by_key(|p| std::cmp::Reverse(p.created_at));
        Ok(out)
    }

    fn known_authors_sqlite(&self) -> std::result::Result<Vec<String>, String> {
        let storage = self.storage().ok_or_else(|| "no sqlite backend".to_string())?;
        let conn = storage.handle();
        let mut stmt = conn
            .prepare_cached("SELECT DISTINCT author_id FROM posts ORDER BY author_id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for id in rows {
            out.push(id.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn comment_post(&self, mut comment: SocialComment) -> std::result::Result<SocialComment, String> {
        if comment.comment_id.is_empty() {
            comment.comment_id = format!("comment-{}", Uuid::new_v4());
        }
        let now = Self::now();
        if comment.created_at == 0 {
            comment.created_at = now;
        }
        // Mirror the `create_post` policy: respect a caller-supplied
        // `updated_at`, never let server time write an `updated_at`
        // earlier than `created_at`. Without this guard a caller
        // back-filling historical records would trip
        // `validate()`'s temporal ordering gate.
        if comment.updated_at == 0 || comment.updated_at < comment.created_at {
            comment.updated_at = comment.created_at;
        }
        self.gate(&comment, "comment")?;

        match &self.store {
            BackingStore::Sqlite(s) => {
                if s.get_post(&comment.post_id)
                    .map_err(|e| e.to_string())?
                    .is_none()
                {
                    return Err(format!("post not found: {}", comment.post_id));
                }
                s.save_comment(&comment).map_err(|e| e.to_string())?;
            }
            BackingStore::Memory(m) => {
                let posts = m.posts.lock().map_err(|e| format!("lock: {e}"))?;
                if !posts.contains_key(&comment.post_id) {
                    return Err(format!("post not found: {}", comment.post_id));
                }
                drop(posts);
                m.comments
                    .lock()
                    .map_err(|e| format!("lock: {e}"))?
                    .entry(comment.post_id.clone())
                    .or_default()
                    .push(comment.clone());
            }
        }
        Ok(comment)
    }

    pub fn list_post_comments(&self, post_id: &str) -> std::result::Result<Vec<SocialComment>, String> {
        match &self.store {
            BackingStore::Sqlite(s) => s.list_post_comments(post_id).map_err(|e| e.to_string()),
            BackingStore::Memory(m) => Ok(m
                .comments
                .lock()
                .map_err(|e| format!("lock: {e}"))?
                .get(post_id)
                .cloned()
                .unwrap_or_default()),
        }
    }

    pub fn react(&self, reaction: SocialReaction) -> std::result::Result<bool, String> {
        self.gate(&reaction, "reaction")?;
        match &self.store {
            BackingStore::Sqlite(s) => s.save_reaction(&reaction).map_err(|e| e.to_string()),
            BackingStore::Memory(m) => {
                let mut guard = m
                    .reactions
                    .lock()
                    .map_err(|e| format!("lock: {e}"))?;
                let key = (
                    reaction.target_id.clone(),
                    reaction.user_id.clone(),
                    reaction.reaction_type,
                );
                // Idempotent: never overwrite an existing
                // reaction. Two callers racing to insert the
                // same `(target, user, kind)` triple both
                // observe `inserted = true` exactly once.
                if guard.contains_key(&key) {
                    return Ok(false);
                }
                guard.insert(key, reaction);
                Ok(true)
            }
        }
    }

    pub fn list_reactions(&self, target_id: &str) -> std::result::Result<Vec<SocialReaction>, String> {
        match &self.store {
            BackingStore::Sqlite(s) => s.list_reactions(target_id).map_err(|e| e.to_string()),
            BackingStore::Memory(m) => {
                let guard = m
                    .reactions
                    .lock()
                    .map_err(|e| format!("lock: {e}"))?;
                Ok(guard
                    .values()
                    .filter(|r| r.target_id == target_id)
                    .cloned()
                    .collect())
            }
        }
    }

    pub fn follow(&self, follower_id: &str, following_id: &str) -> std::result::Result<(), String> {
        let f = FollowRelationship {
            follower_id: follower_id.into(),
            following_id: following_id.into(),
            created_at: Self::now(),
        };
        f.validate().map_err(|e| e.to_string())?;
        match &self.store {
            BackingStore::Sqlite(s) => s.save_follow(&f).map_err(|e| e.to_string()),
            BackingStore::Memory(m) => {
                let mut guard = m.follows.lock().map_err(|e| format!("lock: {e}"))?;
                let list = guard.entry(follower_id.into()).or_default();
                if !list.contains(&following_id.to_string()) {
                    list.push(following_id.to_string());
                }
                Ok(())
            }
        }
    }

    pub fn unfollow(&self, follower_id: &str, following_id: &str) -> std::result::Result<(), String> {
        match &self.store {
            BackingStore::Sqlite(s) => s.unfollow(follower_id, following_id).map_err(|e| e.to_string()),
            BackingStore::Memory(m) => {
                let mut guard = m.follows.lock().map_err(|e| format!("lock: {e}"))?;
                if let Some(list) = guard.get_mut(follower_id) {
                    list.retain(|x| x != following_id);
                }
                Ok(())
            }
        }
    }

    pub fn list_following(&self, follower_id: &str) -> std::result::Result<Vec<String>, String> {
        match &self.store {
            BackingStore::Sqlite(s) => s.list_following(follower_id).map_err(|e| e.to_string()),
            BackingStore::Memory(m) => Ok(m
                .follows
                .lock()
                .map_err(|e| format!("lock: {e}"))?
                .get(follower_id)
                .cloned()
                .unwrap_or_default()),
        }
    }

    pub fn is_following(&self, follower_id: &str, following_id: &str) -> std::result::Result<bool, String> {
        self.list_following(follower_id)
            .map(|v| v.iter().any(|x| x == following_id))
    }

    pub fn verify_post_integrity(&self, post: &SocialPost) -> bool {
        self.gate(post, "post").is_ok() && post.verify_integrity()
    }

    /// Validate a `SocialComment` end-to-end: typed invariants
    /// plus, when present, the integrity hash. Mirrors
    /// [`Self::verify_post_integrity`].
    pub fn verify_comment_integrity(&self, comment: &SocialComment) -> bool {
        self.gate(comment, "comment").is_ok()
            && comment.validate().is_ok()
            // `SocialComment` doesn't carry an integrity hash; the
            // check is therefore a passed validity gate.
            && true
    }

    /// Same for reactions.
    pub fn verify_reaction_integrity(&self, reaction: &SocialReaction) -> bool {
        self.gate(reaction, "reaction").is_ok()
            && reaction.validate().is_ok()
    }
}

#[async_trait]
impl RpcHandler for SocialFeedIpcService {
    async fn handle(&self, method: &str, params: Value) -> std::result::Result<Value, String> {
        match method {
            "node_info" => Ok(json!({
                "node_id": self.cfg.node_id,
                "timestamp": Self::now(),
            })),
            "create_post" => {
                let post: SocialPost = Self::require(&params, "post")?;
                let stored = self.create_post(post)?;
                Ok(json!({ "post": stored }))
            }
            "update_post" => {
                let post: SocialPost = Self::require(&params, "post")?;
                let stored = self.update_post(post)?;
                Ok(json!({ "post": stored }))
            }
            "delete_post" => {
                let post_id: String = Self::require(&params, "post_id")?;
                self.delete_post(&post_id)?;
                Ok(json!({ "ok": true }))
            }
            "get_post" => {
                let post_id: String = Self::require(&params, "post_id")?;
                let post = self.get_post(&post_id)?;
                Ok(json!({ "post": post }))
            }
            "list_user_posts" => {
                let user_id: String = Self::require(&params, "user_id")?;
                let posts = self.list_user_posts(&user_id)?;
                Ok(json!({ "posts": posts }))
            }
            "timeline_for" => {
                let viewer_id: String = Self::require(&params, "viewer_id")?;
                let limit: Option<usize> = params
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);
                let before_ts: Option<u64> = params.get("before_ts").and_then(|v| v.as_u64());
                let posts = self.timeline_for(&viewer_id, limit, before_ts)?;
                Ok(json!({ "posts": posts }))
            }
            "comment_post" => {
                let comment: SocialComment = Self::require(&params, "comment")?;
                let stored = self.comment_post(comment)?;
                Ok(json!({ "comment": stored }))
            }
            "list_post_comments" => {
                let post_id: String = Self::require(&params, "post_id")?;
                let comments = self.list_post_comments(&post_id)?;
                Ok(json!({ "comments": comments }))
            }
            "react" => {
                let reaction: SocialReaction = Self::require(&params, "reaction")?;
                let inserted = self.react(reaction)?;
                Ok(json!({ "inserted": inserted }))
            }
            "list_reactions" => {
                let target_id: String = Self::require(&params, "target_id")?;
                let reactions = self.list_reactions(&target_id)?;
                Ok(json!({ "reactions": reactions }))
            }
            "follow" => {
                let follower_id: String = Self::require(&params, "follower_id")?;
                let following_id: String = Self::require(&params, "following_id")?;
                self.follow(&follower_id, &following_id)?;
                Ok(json!({ "ok": true }))
            }
            "unfollow" => {
                let follower_id: String = Self::require(&params, "follower_id")?;
                let following_id: String = Self::require(&params, "following_id")?;
                self.unfollow(&follower_id, &following_id)?;
                Ok(json!({ "ok": true }))
            }
            "list_following" => {
                let follower_id: String = Self::require(&params, "follower_id")?;
                Ok(json!({ "following_ids": self.list_following(&follower_id)? }))
            }
            "is_following" => {
                let follower_id: String = Self::require(&params, "follower_id")?;
                let following_id: String = Self::require(&params, "following_id")?;
                Ok(json!({ "following": self.is_following(&follower_id, &following_id)? }))
            }
            "verify_post_integrity" => {
                let post: SocialPost = Self::require(&params, "post")?;
                Ok(json!({ "valid": self.verify_post_integrity(&post) }))
            }
            "verify_comment_integrity" => {
                let comment: SocialComment = Self::require(&params, "comment")?;
                Ok(json!({ "valid": self.verify_comment_integrity(&comment) }))
            }
            "verify_reaction_integrity" => {
                let reaction: SocialReaction = Self::require(&params, "reaction")?;
                Ok(json!({ "valid": self.verify_reaction_integrity(&reaction) }))
            }
            other => Err(format!("unknown method: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_ipc::validation::ValidationPolicy;
    use adnet_types::invariants::{ReactionTarget, ReactionType, Visibility};
    use adnet_types::social_feed::SocialPost;
    use chrono::Utc;

    fn sample_post(author: &str) -> SocialPost {
        SocialPost {
            post_id: String::new(),
            author_id: author.into(),
            author_name: author.into(),
            author_avatar: None,
            content: format!("hello from {author}"),
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

    fn memory_cfg() -> (tempfile::TempDir, SocialFeedIpcConfig) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SocialFeedIpcConfig {
            socket_path: dir.path().join("sf.sock"),
            node_id: NodeId::random(),
            policy: ValidationPolicy::Strict,
            storage: SocialFeedStorageConfig {
                storage_dir: dir.path().to_path_buf(),
                filename: "ignored".into(),
            },
        };
        (dir, cfg)
    }

    #[test]
    fn create_post_in_memory_roundtrip() {
        let (_dir, cfg) = memory_cfg();
        let svc = SocialFeedIpcService::with_in_memory(cfg);
        let stored = svc.create_post(sample_post("alice")).unwrap();
        assert!(!stored.post_id.is_empty());
        assert_eq!(stored.sequence, 1);
        let fetched = svc.get_post(&stored.post_id).unwrap().unwrap();
        assert_eq!(fetched.author_id, "alice");
    }

    #[test]
    fn timeline_visibility_filtering_works() {
        let (_dir, cfg) = memory_cfg();
        let svc = SocialFeedIpcService::with_in_memory(cfg);

        let now = Utc::now().timestamp_millis() as u64;
        for (i, who) in ["alice", "bob"].iter().enumerate() {
            let mut p = sample_post(who);
            p.content = format!("public post {i}");
            p.created_at = now + i as u64;
            p.updated_at = p.created_at;
            svc.create_post(p).unwrap();
        }
        let mut priv_p = sample_post("alice");
        priv_p.visibility = Visibility::Friends;
        priv_p.content = "friends-only".into();
        svc.create_post(priv_p).unwrap();

        let viewer_pub = svc.timeline_for("eve", None, None).unwrap();
        assert_eq!(viewer_pub.len(), 2);

        svc.follow("bob", "alice").unwrap();
        let viewer_friends = svc.timeline_for("bob", None, None).unwrap();
        assert_eq!(viewer_friends.len(), 3);
    }

    #[test]
    fn react_idempotent_per_user_kind() {
        let (_dir, cfg) = memory_cfg();
        let svc = SocialFeedIpcService::with_in_memory(cfg);
        let stored = svc.create_post(sample_post("alice")).unwrap();

        let r1 = SocialReaction {
            reaction_id: "r1".into(),
            target_id: stored.post_id.clone(),
            target_type: ReactionTarget::Post,
            user_id: "bob".into(),
            reaction_type: ReactionType::Like,
            created_at: 1,
        };
        let r2 = r1.clone();
        assert!(svc.react(r1.clone()).unwrap());
        assert!(!svc.react(r2).unwrap());

        let mut r3 = r1;
        r3.reaction_type = ReactionType::Love;
        r3.reaction_id = "r2".into();
        assert!(svc.react(r3).unwrap());
    }

    #[test]
    fn delete_post_removes_followup_rows_in_sqlite_mode() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SocialFeedIpcConfig {
            socket_path: dir.path().join("sf.sock"),
            node_id: NodeId::random(),
            policy: ValidationPolicy::Strict,
            storage: SocialFeedStorageConfig {
                storage_dir: dir.path().to_path_buf(),
                filename: "t.db".into(),
            },
        };
        let svc = SocialFeedIpcService::new(cfg).unwrap();
        let stored = svc.create_post(sample_post("alice")).unwrap();
        let c = SocialComment {
            comment_id: String::new(),
            post_id: stored.post_id.clone(),
            author_id: "bob".into(),
            author_name: "Bob".into(),
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
        };
        let c = svc.comment_post(c).unwrap();
        svc.react(SocialReaction {
            reaction_id: "r1".into(),
            target_id: stored.post_id.clone(),
            target_type: ReactionTarget::Post,
            user_id: "bob".into(),
            reaction_type: ReactionType::Like,
            created_at: 1,
        })
        .unwrap();
        svc.react(SocialReaction {
            reaction_id: "r2".into(),
            target_id: c.comment_id.clone(),
            target_type: ReactionTarget::Comment,
            user_id: "bob".into(),
            reaction_type: ReactionType::Like,
            created_at: 1,
        })
        .unwrap();
        svc.delete_post(&stored.post_id).unwrap();
        assert!(svc.get_post(&stored.post_id).unwrap().is_none());
        assert!(svc.list_post_comments(&stored.post_id).unwrap().is_empty());
        assert!(svc.list_reactions(&stored.post_id).unwrap().is_empty());
        assert!(svc.list_reactions(&c.comment_id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn end_to_end_json_rpc_round_trip() {
        use adnet_ipc::client::json_rpc_call;
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sf.sock");
        let cfg = SocialFeedIpcConfig {
            socket_path: sock.clone(),
            node_id: NodeId::random(),
            policy: ValidationPolicy::Strict,
            storage: SocialFeedStorageConfig {
                storage_dir: dir.path().to_path_buf(),
                filename: "rpc.db".into(),
            },
        };
        let svc = std::sync::Arc::new(SocialFeedIpcService::with_in_memory(cfg));
        let handle = svc.clone().serve().await.unwrap();

        // node_info
        let r: serde_json::Value = json_rpc_call(&sock, "sf", "node_info", json!({})).await.unwrap();
        assert!(r["node_id"].is_string());

        // create_post
        let mut p = sample_post("alice");
        p.content = "via rpc".into();
        let r = json_rpc_call(&sock, "sf", "create_post", json!({"post": p}))
            .await
            .unwrap();
        let post_id = r["post"]["post_id"].as_str().unwrap().to_string();

        // timeline_for
        let r = json_rpc_call(&sock, "sf", "timeline_for", json!({"viewer_id": "eve"}))
            .await
            .unwrap();
        let arr = r["posts"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["post_id"], post_id);

        handle.shutdown();
    }
}
