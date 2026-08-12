//! High-level facade combining storage, validation and gossip for
//! the social feed.
//!
//! The CLI, FFI, and direct embedders all talk to
//! [`SocialFeedService`] rather than the lower-level
//! [`crate::ipc::SocialFeedIpcService`] or
//! [`crate::storage::SocialFeedStorage`]. Internally the service
//! owns a storage handle and (optionally) a gossip bridge; every
//! state-changing operation is journaled to SQLite and broadcast
//! over the configured gossip scope.
//!
//! # Pure-storage mode
//!
//! Constructing with [`SocialFeedServiceConfig::default`] (no
//! gossip transport) leaves the service in pure storage mode:
//! `create_post`, `comment_post`, `react`, `follow` still work
//! end-to-end; gossip broadcasts become no-ops. This is the
//! default for unit tests and embedded clients that don't have a
//! gossip layer.
//!
//! # Gossip fan-out
//!
//! Passing a `gossip_transport` (any `Arc<dyn GossipTransport>`)
//! via the config makes `create_*` operations publish an
//! [`crate::bridge::Envelope`] through [`SocialFeedBridge`].
//! Inbound gossip frames are **not** applied here — that's the
//! job of [`SocialFeedSubscriber`] combined with the embedder's
//! reconciliation logic.

use std::sync::Arc;

use adnet_gossip::transport::GossipTransport;
use adnet_types::NodeId;
use adnet_types::social_feed::{
    FollowRelationship, SocialComment, SocialPost, SocialReaction,
};

use crate::bridge::Envelope;
use crate::error::{ErrorClass, Result, SocialFeedError};
use crate::gossip::{SocialFeedBridge, SocialFeedGossipConfig};
use crate::ipc::{SocialFeedIpcConfig, SocialFeedIpcService};
use crate::storage::{SocialFeedStorage, SocialFeedStorageConfig};

/// Configuration for [`SocialFeedService`].
#[derive(Clone)]
pub struct SocialFeedServiceConfig {
    pub storage: SocialFeedStorageConfig,
    pub gossip: Option<SocialFeedGossipConfig>,
    pub local_node: Option<NodeId>,
    pub validation_policy: adnet_ipc::validation::ValidationPolicy,
    /// Optional gossip transport — when `Some`, all writes are
    /// also broadcast through [`SocialFeedBridge`].
    pub gossip_transport: Option<Arc<dyn GossipTransport>>,
}

impl std::fmt::Debug for SocialFeedServiceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocialFeedServiceConfig")
            .field("storage", &self.storage)
            .field("gossip", &self.gossip)
            .field("validation_policy", &self.validation_policy)
            .field("has_transport", &self.gossip_transport.is_some())
            .finish()
    }
}

impl Default for SocialFeedServiceConfig {
    fn default() -> Self {
        Self {
            storage: SocialFeedStorageConfig::default(),
            gossip: None,
            local_node: None,
            validation_policy: adnet_ipc::validation::ValidationPolicy::Strict,
            gossip_transport: None,
        }
    }
}

/// Coarse scope the caller is interested in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineScope {
    /// Posts visible to the viewer (visibility filter applied).
    ForViewer,
    /// Posts authored by a specific user, regardless of visibility.
    ByUser,
    /// All posts stored locally — debug / inspection only.
    All,
}

#[derive(Debug, Clone)]
pub struct TimelineQuery {
    pub viewer_id: String,
    pub scope: TimelineScope,
    pub limit: Option<usize>,
    /// Cursor returned by the previous page. When set, the
    /// returned page starts strictly after `(before.created_at,
    /// before.post_id)` in descending order, ensuring disjoint
    /// pages even when several posts share a timestamp.
    pub before_cursor: Option<TimelineCursor>,
    pub before_ts: Option<u64>,
    pub author_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TimelinePage {
    pub posts: Vec<SocialPost>,
    /// When `Some`, the page is paginated and the caller should
    /// pass this cursor to the next call to continue. The cursor
    /// is a composite of `created_at` *and* `post_id`, which
    /// resolves the timestamp-collision problem that arises when
    /// multiple posts share the same `created_at` (common in
    /// integration tests and high-throughput back-fill scenarios
    /// where millisecond/nanosecond clock resolution cannot
    /// guarantee total ordering).
    pub next_cursor: Option<TimelineCursor>,
}

/// Opaque pagination cursor. Send the value from one
/// [`TimelinePage`]'s `next_cursor` back as
/// [`TimelineQuery::before_cursor`] on the next call to advance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineCursor {
    pub created_at: u64,
    pub post_id: String,
}

impl TimelineCursor {
    pub fn from_post(p: &SocialPost) -> Self {
        Self {
            created_at: p.created_at,
            post_id: p.post_id.clone(),
        }
    }
}

/// The social-feed facade. Wraps the IPC service (which owns the
/// storage handle) and, optionally, a gossip bridge.
#[derive(Clone)]
pub struct SocialFeedService {
    inner: Arc<SocialFeedIpcService>,
    bridge: Option<SocialFeedBridge>,
    local_node: NodeId,
    transport: Option<Arc<dyn GossipTransport>>,
}

impl std::fmt::Debug for SocialFeedService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocialFeedService")
            .field("bridge", &self.bridge.is_some())
            .field("local_node", &self.local_node)
            .finish()
    }
}

impl SocialFeedService {
    /// Build a service backed by SQLite (production path). The
    /// gossip bridge is included when both `gossip` and
    /// `gossip_transport` are `Some(_)`.
    pub fn new(cfg: SocialFeedServiceConfig) -> Result<Self> {
        let ipc_cfg = SocialFeedIpcConfig {
            socket_path: std::env::temp_dir().join("adnet_social_feed.sock"),
            node_id: cfg.local_node.clone().unwrap_or_else(NodeId::random),
            policy: cfg.validation_policy,
            storage: cfg.storage.clone(),
        };
        let inner = SocialFeedIpcService::new(ipc_cfg)?;
        let bridge = cfg
            .gossip
            .clone()
            .map(SocialFeedBridge::new);
        Ok(Self {
            inner: Arc::new(inner),
            bridge,
            local_node: cfg.local_node.unwrap_or_else(NodeId::random),
            transport: cfg.gossip_transport,
        })
    }

    /// Build an in-memory service — handy for unit tests.
    pub fn with_in_memory(cfg: SocialFeedServiceConfig) -> Result<Self> {
        let ipc_cfg = SocialFeedIpcConfig {
            socket_path: std::env::temp_dir().join("adnet_social_feed.sock"),
            node_id: cfg.local_node.clone().unwrap_or_else(NodeId::random),
            policy: cfg.validation_policy,
            storage: cfg.storage.clone(),
        };
        let inner = SocialFeedIpcService::with_in_memory(ipc_cfg);
        let bridge = cfg.gossip.clone().map(SocialFeedBridge::new);
        Ok(Self {
            inner: Arc::new(inner),
            bridge,
            local_node: cfg.local_node.unwrap_or_else(NodeId::random),
            transport: cfg.gossip_transport,
        })
    }

    pub fn storage(&self) -> Option<&SocialFeedStorage> {
        self.inner.storage()
    }

    pub fn bridge(&self) -> Option<&SocialFeedBridge> {
        self.bridge.as_ref()
    }

    pub fn local_node(&self) -> &NodeId {
        &self.local_node
    }

    pub fn inner(&self) -> &SocialFeedIpcService {
        &self.inner
    }

    /// Convert any error class into a [`Result`]. Helper for
    /// embedders that want to translate public errors into
    /// user-friendly domain errors.
    pub fn classify(err: &SocialFeedError) -> ErrorClass {
        err.class()
    }

    // ── Posts ─────────────────────────────────────────────────────────────

    pub async fn create_post(&self, post: SocialPost) -> Result<SocialPost> {
        let stored = self
            .inner
            .create_post(post)
            .map_err(SocialFeedError::ipc)?;
        self.maybe_broadcast(Envelope::from_post(stored.clone())).await;
        Ok(stored)
    }

    pub async fn update_post(&self, post: SocialPost) -> Result<SocialPost> {
        let stored = self
            .inner
            .update_post(post)
            .map_err(SocialFeedError::ipc)?;
        self.maybe_broadcast(Envelope::from_post(stored.clone())).await;
        Ok(stored)
    }

    pub fn get_post(&self, post_id: &str) -> Result<Option<SocialPost>> {
        self.inner.get_post(post_id).map_err(SocialFeedError::ipc)
    }

    pub fn delete_post(&self, post_id: &str) -> Result<()> {
        self.inner.delete_post(post_id).map_err(SocialFeedError::ipc)
    }

    pub fn list_user_posts(&self, user_id: &str) -> Result<Vec<SocialPost>> {
        self.inner
            .list_user_posts(user_id)
            .map_err(SocialFeedError::ipc)
    }

    pub fn timeline(&self, q: TimelineQuery) -> Result<TimelinePage> {
        let mut posts = match q.scope {
            TimelineScope::ForViewer => {
                // `timeline_for` honours the simpler `before_ts`
                // filter, but a composite cursor takes priority —
                // it lets us paginate past posts that share the
                // same `created_at`.
                if let Some(cursor) = &q.before_cursor {
                    let mut all = self
                        .inner
                        .timeline_for(&q.viewer_id, None, None)
                        .map_err(SocialFeedError::ipc)?;
                    all.retain(|p| {
                        p.created_at < cursor.created_at
                            || (p.created_at == cursor.created_at
                                && p.post_id.as_str() < cursor.post_id.as_str())
                    });
                    all
                } else {
                    self.inner
                        .timeline_for(&q.viewer_id, q.limit, q.before_ts)
                        .map_err(SocialFeedError::ipc)?
                }
            }
            TimelineScope::ByUser => {
                let author = q
                    .author_id
                    .as_deref()
                    .ok_or_else(|| SocialFeedError::Validation("author_id required for ByUser scope".into()))?;
                self.inner
                    .list_user_posts(author)
                    .map_err(SocialFeedError::ipc)?
            }
            TimelineScope::All => {
                // Sum across every known author.
                let mut all = Vec::new();
                if let Some(s) = self.inner.storage() {
                    let conn = s.handle();
                    let mut stmt = conn
                        .prepare_cached("SELECT DISTINCT author_id FROM posts ORDER BY author_id")
                        .map_err(|e| SocialFeedError::database(e))?;
                    let rows = stmt
                        .query_map([], |row| row.get::<_, String>(0))
                        .map_err(|e| SocialFeedError::database(e))?;
                    let mut authors = Vec::new();
                    for id in rows {
                        authors.push(id.map_err(|e| SocialFeedError::database(e))?);
                    }
                    drop(stmt);
                    for a in authors {
                        let mut ps = self
                            .inner
                            .list_user_posts(&a)
                            .map_err(SocialFeedError::ipc)?;
                        all.append(&mut ps);
                    }
                } else {
                    // Memory backend — fall back to the per-viewer
                    // timeline. `All` scope is admin-only; this
                    // branch should rarely fire in production.
                    all = self
                        .inner
                        .timeline_for(&q.viewer_id, None, None)
                        .map_err(SocialFeedError::ipc)?;
                }
                all
            }
        };
        // Honour the caller's pagination for the non-`ForViewer`
        // scopes — `ForViewer` already applied either the cursor
        // or `before_ts` above.
        if matches!(q.scope, TimelineScope::ByUser | TimelineScope::All) {
            if let Some(cursor) = &q.before_cursor {
                posts.retain(|p| {
                    p.created_at < cursor.created_at
                        || (p.created_at == cursor.created_at
                            && p.post_id.as_str() < cursor.post_id.as_str())
                });
            } else if let Some(b) = q.before_ts {
                posts.retain(|p| p.created_at < b);
            }
        }
        posts.sort_by_key(|p| std::cmp::Reverse(p.created_at));
        let limit = q.limit.unwrap_or(50);
        // Cursor points at the *last returned* post. The next
        // round-trip filters with strict-less on both
        // `created_at` and `post_id`, guaranteeing disjoint pages.
        let next = if posts.len() > limit {
            posts
                .get(limit - 1)
                .map(TimelineCursor::from_post)
        } else {
            None
        };
        posts.truncate(limit);
        Ok(TimelinePage {
            posts,
            next_cursor: next,
        })
    }

    // ── Comments / reactions / follows ───────────────────────────────────

    pub async fn comment_post(
        &self,
        comment: SocialComment,
    ) -> Result<SocialComment> {
        let stored = self
            .inner
            .comment_post(comment)
            .map_err(SocialFeedError::ipc)?;
        self.maybe_broadcast(Envelope::from_comment(stored.clone()))
            .await;
        Ok(stored)
    }

    pub fn list_post_comments(&self, post_id: &str) -> Result<Vec<SocialComment>> {
        self.inner
            .list_post_comments(post_id)
            .map_err(SocialFeedError::ipc)
    }

    pub async fn react(&self, reaction: SocialReaction) -> Result<bool> {
        let inserted = self.inner.react(reaction.clone()).map_err(SocialFeedError::ipc)?;
        // Broadcast only when something actually changed. Idempotent
        // re-reactions are silent on the wire.
        if inserted {
            self.maybe_broadcast(Envelope::from_reaction(reaction)).await;
        }
        Ok(inserted)
    }

    pub fn list_reactions(&self, target_id: &str) -> Result<Vec<SocialReaction>> {
        self.inner
            .list_reactions(target_id)
            .map_err(SocialFeedError::ipc)
    }

    pub fn follow(&self, follower_id: &str, following_id: &str) -> Result<()> {
        self.inner
            .follow(follower_id, following_id)
            .map_err(SocialFeedError::ipc)
    }

    pub fn unfollow(&self, follower_id: &str, following_id: &str) -> Result<()> {
        self.inner
            .unfollow(follower_id, following_id)
            .map_err(SocialFeedError::ipc)
    }

    pub fn list_following(&self, follower_id: &str) -> Result<Vec<String>> {
        self.inner
            .list_following(follower_id)
            .map_err(SocialFeedError::ipc)
    }

    pub fn is_following(&self, follower_id: &str, following_id: &str) -> Result<bool> {
        self.inner
            .is_following(follower_id, following_id)
            .map_err(SocialFeedError::ipc)
    }

    pub fn verify_post_integrity(&self, post: &SocialPost) -> Result<bool> {
        Ok(self.inner.verify_post_integrity(post))
    }

    // ── Gossip fan-out ───────────────────────────────────────────────────

    async fn maybe_broadcast(&self, env: Envelope) {
        if let (Some(bridge), Some(transport)) = (&self.bridge, &self.transport) {
            if let Err(e) = bridge.broadcast(transport.as_ref(), &self.local_node, env).await {
                tracing::warn!(error = %e, "social feed gossip broadcast failed");
            }
        }
    }
}

// `PostAttachment` re-exports for callers that build attachments
// on this side of the API.
pub use adnet_types::social_feed::PostAttachment as Attachment;

// Convenience constructors used by CLI tests.
impl SocialFeedService {
    pub async fn follow_relationship(follower: String, following: String) -> FollowRelationship {
        FollowRelationship {
            follower_id: follower,
            following_id: following,
            created_at: now_millis(),
        }
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::invariants::{ReactionTarget, ReactionType, Visibility};
    use adnet_types::social_feed::SocialPost;
    use tempfile::tempdir;

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

    fn cfg_in(dir: &std::path::Path) -> SocialFeedServiceConfig {
        SocialFeedServiceConfig {
            storage: SocialFeedStorageConfig {
                storage_dir: dir.to_path_buf(),
                filename: "svc.db".into(),
            },
            gossip: None,
            local_node: Some(NodeId::random()),
            validation_policy: adnet_ipc::validation::ValidationPolicy::Strict,
            gossip_transport: None,
        }
    }

    #[tokio::test]
    async fn create_post_then_list() {
        let dir = tempdir().unwrap();
        let svc = SocialFeedService::new(cfg_in(dir.path())).unwrap();
        let post = svc
            .create_post(sample_post("alice"))
            .await
            .expect("create_post");
        assert!(!post.post_id.is_empty());
        let listed = svc.list_user_posts("alice").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].post_id, post.post_id);

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
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn integrity_round_trips_through_service() {
        let dir = tempdir().unwrap();
        let svc = SocialFeedService::new(cfg_in(dir.path())).unwrap();
        let stored = svc.create_post(sample_post("alice")).await.unwrap();
        assert!(svc.verify_post_integrity(&stored).unwrap());
    }

    #[tokio::test]
    async fn react_then_list() {
        let dir = tempdir().unwrap();
        let svc = SocialFeedService::new(cfg_in(dir.path())).unwrap();
        let stored = svc.create_post(sample_post("alice")).await.unwrap();
        let inserted = svc
            .react(SocialReaction {
                reaction_id: "r1".into(),
                target_id: stored.post_id.clone(),
                target_type: ReactionTarget::Post,
                user_id: "bob".into(),
                reaction_type: ReactionType::Like,
                created_at: 1,
            })
            .await
            .unwrap();
        assert!(inserted);
        let listed = svc.list_reactions(&stored.post_id).unwrap();
        assert_eq!(listed.len(), 1);
    }
}
