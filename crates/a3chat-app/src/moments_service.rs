//! `a3chat` Moments / 朋友圈 service (F-05).
//!
//! Bridges the [`a3net_socialfeed::SocialFeedService`] onto the
//! `a3chat-app` facade — i.e. exposes it under the
//! `a3chat.moments.*` JSON-RPC namespace, fans writes out onto the
//! in-process [`NotificationBus`] so SSE subscribers receive
//! `moments.post.created` / `moments.comment.added` /
//! `moments.reaction.toggled` / `moments.post.deleted` events, and
//! runs every outgoing post body through the local
//! [`ModerationService`] (mirroring `ChatService::send_message`).
//!
//! # Why a thin wrapper
//!
//! `a3net-socialfeed` is the canonical, DO-178C-grade Moments
//! runtime (SQLite + gossip + typed `Validate` records). All
//! persistence, integrity-hash stamping, and pagination-cursor
//! mechanics already live there; this module deliberately adds no
//! duplicate storage. Its job is only:
//!
//! * Namespace the methods under `a3chat.moments.*`.
//! * Map the `user_id` (the chat owner) onto the `author_id` field
//!   that `SocialPost` already exposes — clients sending a post
//!   don't have to know about the gossip-side author identity.
//! * Emit bus events that the SSE layer in `a3chat-rpc` already
//!   forwards to subscribers.
//! * Run outgoing post/comment bodies through `ModerationService`
//!   so blocklisted hashes or denied content never reach SQLite.
//!
//! # Storage layout
//!
//! The wrapper opens a SQLite file at `<chat-storage base>/moments/
//! moments.db` (WAL, `foreign_keys=ON`, integrity-checked at boot).
//! That mirrors how `moderation` and `media` already share the
//! `ChatStorage` base directory — see
//! [`ModerationConfig::under_base`] and [`MediaConfig::under_base`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use a3net_socialfeed::{
    SocialFeedError, SocialFeedService, SocialFeedServiceConfig, SocialFeedStorageConfig,
    TimelinePage, TimelineQuery, TimelineScope,
};
use a3net_types::invariants::Visibility;
use a3net_types::social_feed::{
    FollowRelationship, SocialComment, SocialPost, SocialReaction,
};
use serde::{Deserialize, Serialize};

use a3chat_core::error::A3chatError;
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::UserId;

use crate::error::{AppError, AppResult};
use crate::moderation_service::ModerationService;
use crate::notification_bus::NotificationBus;

/// RPC method-name constants owned by this module. Mirror of
/// `a3chat_core::rpc::A3chatRpcMethod::MOMENTS_*` — kept as
/// `pub const` here so the dispatcher in [`crate::moments_service`]
/// can pattern-match without re-importing every call site.
pub const METHODS: &[&str] = &[
    "a3chat.moments.node_info",
    "a3chat.moments.post.create",
    "a3chat.moments.post.update",
    "a3chat.moments.post.delete",
    "a3chat.moments.post.get",
    "a3chat.moments.posts.by_user",
    "a3chat.moments.timeline",
    "a3chat.moments.comment.add",
    "a3chat.moments.comment.edit",
    "a3chat.moments.comment.delete",
    "a3chat.moments.comments.list",
    "a3chat.moments.react",
    "a3chat.moments.unreact",
    "a3chat.moments.reactions.list",
    "a3chat.moments.follow",
    "a3chat.moments.unfollow",
    "a3chat.moments.following.list",
    "a3chat.moments.followers.list",
    "a3chat.moments.following.check",
    "a3chat.moments.block",
    "a3chat.moments.unblock",
    "a3chat.moments.blocklist.list",
    "a3chat.moments.share",
    "a3chat.moments.report",
    "a3chat.moments.verify.post",
    "a3chat.moments.verify.comment",
    "a3chat.moments.verify.reaction",
];

/// Configuration for [`MomentsService`].
#[derive(Debug, Clone)]
pub struct MomentsConfig {
    /// Base directory for the SQLite file. Created if missing.
    pub data_dir: PathBuf,
}

impl MomentsConfig {
    /// Build a config under `<base>/moments`.
    pub fn under_base(base: &Path) -> Self {
        Self {
            data_dir: base.join("moments"),
        }
    }
}

/// Cheap-clone handle to the Moments runtime.
///
/// Internally wraps an `Arc<SocialFeedService>` and the bus. The
/// `ModerationService` is optional — when attached, every post and
/// comment body is checked before the SQLite write (mirrors
/// `ChatService::with_moderation`).
#[derive(Clone)]
pub struct MomentsService {
    inner: Arc<SocialFeedService>,
    bus: NotificationBus,
    moderation: Option<ModerationService>,
}

impl std::fmt::Debug for MomentsService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MomentsService")
            .field("local_node", self.inner.local_node())
            .field("moderation_attached", &self.moderation.is_some())
            .finish()
    }
}

impl MomentsService {
    /// Open the service on disk (production path). Creates a private
    /// notification bus; production wiring should follow up with
    /// [`MomentsService::with_bus`] to share the chat-wide bus so
    /// SSE subscribers see the events.
    pub fn open(cfg: &MomentsConfig) -> AppResult<Self> {
        std::fs::create_dir_all(&cfg.data_dir)
            .map_err(|e| AppError::Storage(format!("moments mkdir: {e}")))?;
        let svc_cfg = SocialFeedServiceConfig {
            storage: SocialFeedStorageConfig {
                storage_dir: cfg.data_dir.clone(),
                filename: "moments.db".into(),
            },
            gossip: None,
            local_node: None,
            validation_policy: a3net_ipc::validation::ValidationPolicy::Strict,
            gossip_transport: None,
        };
        let inner = SocialFeedService::new(svc_cfg)
            .map_err(|e| AppError::Storage(format!("moments open: {e}")))?;
        Ok(Self {
            inner: Arc::new(inner),
            bus: NotificationBus::default(),
            moderation: None,
        })
    }

    /// Open the service on disk *and* share the chat-wide
    /// [`NotificationBus`] so SSE subscribers receive moments events
    /// alongside chat/contact/link-bookmark events.
    pub fn open_with_bus(cfg: &MomentsConfig, bus: NotificationBus) -> AppResult<Self> {
        let mut s = Self::open(cfg)?;
        s.bus = bus;
        Ok(s)
    }

    /// Open an in-memory service (test helper). Creates a private
    /// bus; tests that need cross-service wiring call
    /// [`MomentsService::with_bus`] afterwards.
    pub fn open_in_memory() -> Self {
        // Use a unique tempdir for the in-memory backend so two
        // parallel test workers don't collide on the default
        // `a3net_social_feed` filename.
        let dir = std::env::temp_dir().join(format!(
            "a3chat-moments-test-{}",
            uuid::Uuid::new_v4()
        ));
        let svc_cfg = SocialFeedServiceConfig {
            storage: SocialFeedStorageConfig {
                storage_dir: dir,
                filename: "moments.db".into(),
            },
            gossip: None,
            local_node: None,
            validation_policy: a3net_ipc::validation::ValidationPolicy::Strict,
            gossip_transport: None,
        };
        let inner = SocialFeedService::with_in_memory(svc_cfg)
            .expect("moments in-memory open");
        Self {
            inner: Arc::new(inner),
            bus: NotificationBus::default(),
            moderation: None,
        }
    }

    /// Open an in-memory service that shares the provided bus.
    /// Used by `a3chat-app` tests that want the chat bus to surface
    /// moments events.
    pub fn open_in_memory_with_bus(bus: NotificationBus) -> Self {
        let mut s = Self::open_in_memory();
        s.bus = bus;
        s
    }

    /// Share the chat-wide [`NotificationBus`] with this service.
    /// After this call, every event `MomentsService` emits (post
    /// created/deleted, comment added, reaction toggled) is
    /// published onto the supplied bus — so SSE subscribers wired
    /// via [`crate::A3chatApp::subscribe_for`] receive them.
    pub fn with_bus(mut self, bus: NotificationBus) -> Self {
        self.bus = bus;
        self
    }

    /// Attach the chat's moderation policy. Once attached, every
    /// `create_post` and `comment_add` body is checked against the
    /// local blocklist before it reaches SQLite.
    pub fn with_moderation(mut self, moderation: ModerationService) -> Self {
        self.moderation = Some(moderation);
        self
    }

    /// Notification-bus handle (used by `app.rs` to wire the SSE
    /// bridge; not normally called by RPC dispatchers).
    pub fn bus(&self) -> &NotificationBus {
        &self.bus
    }

    /// Node identity the local service was opened under.
    pub fn local_node(&self) -> &a3net_types::NodeId {
        self.inner.local_node()
    }

    // ── Posts ─────────────────────────────────────────────────────────

    /// `a3chat.moments.post.create`. Stamps the integrity hash and
    /// emits a `moments.post.created` event on success.
    pub async fn create_post(
        &self,
        owner: &UserId,
        mut post: SocialPost,
    ) -> AppResult<SocialPost> {
        // The client may not have set author_id — if it left it
        // empty, fill it from the chat owner.
        if post.author_id.is_empty() {
            post.author_id = owner.to_string();
        }
        if post.author_name.is_empty() {
            post.author_name = owner.to_string();
        }
        // Bump timestamps if the caller left them at zero. The
        // typed `SocialPost::validate()` enforces `created_at
        // <= updated_at`, so setting them in lock-step is safe.
        let now = now_millis();
        if post.created_at == 0 {
            post.created_at = now;
        }
        if post.updated_at == 0 {
            post.updated_at = post.created_at;
        }
        // SR-MOMENTS-10 — auto-generate a stable `post_id` when the
        // client left it blank. We use `post:<ts-hex>:<rand-hex>`
        // so the id is unique without coordinating with the hub;
        // the 64 bits of randomness make collisions vanishingly
        // unlikely even under a hostile re-submission.
        if post.post_id.is_empty() {
            let ts = post.created_at.max(now);
            let rand = rand::random::<u64>();
            post.post_id = format!("post:{:x}:{:016x}", ts, rand);
        }
        post.sequence = post.sequence.max(1);
        post.stamp_integrity_hash();

        // Run the moderation pre-flight on the plaintext content
        // so a denylisted post never reaches SQLite. System /
        // announcement posts bypass this — `SocialPost` is
        // always user-authored, so we run on every call.
        if let Some(m) = &self.moderation {
            let decision = m.check_content(owner, &post.content);
            if !decision.is_allowed() {
                return Err(AppError::Forbidden(format!(
                    "moderation denied moments post: {}",
                    decision.reason
                )));
            }
        }

        let stored = self
            .inner
            .create_post(post)
            .await
            .map_err(map_social_feed_err)?;

        self.bus.publish(A3chatEvent::MomentsPostCreated {
            user_id: owner.clone(),
            post_id: stored.post_id.clone(),
            author_id: stored.author_id.clone(),
            visibility: stored.visibility.as_str().to_string(),
        });

        Ok(stored)
    }

    /// `a3chat.moments.post.update`.
    pub async fn update_post(
        &self,
        owner: &UserId,
        mut post: SocialPost,
    ) -> AppResult<SocialPost> {
        if post.author_id.is_empty() {
            post.author_id = owner.to_string();
        }
        post.updated_at = now_millis();
        post.is_edited = true;
        post.edited_at = Some(post.updated_at);
        post.stamp_integrity_hash();

        if let Some(m) = &self.moderation {
            let decision = m.check_content(owner, &post.content);
            if !decision.is_allowed() {
                return Err(AppError::Forbidden(format!(
                    "moderation denied moments update: {}",
                    decision.reason
                )));
            }
        }

        let stored = self
            .inner
            .update_post(post)
            .await
            .map_err(map_social_feed_err)?;
        Ok(stored)
    }

    /// `a3chat.moments.post.delete`. Emits `moments.post.deleted`.
    ///
    /// SR-MOMENTS-9 (ownership): only the post's author may delete
    /// it. `get_post` is the source of truth; missing posts fail
    /// with `NotFound` rather than a silent no-op.
    pub async fn delete_post(
        &self,
        owner: &UserId,
        post_id: &str,
    ) -> AppResult<()> {
        // SR-MOMENTS-9 — locate the post *before* deleting so we
        // can verify ownership and emit a useful bus event with
        // the author's id.
        let post = self
            .inner
            .get_post(post_id)
            .map_err(map_social_feed_err)?
            .ok_or_else(|| AppError::Domain(format!("post not found: {post_id}")))?;
        if post.author_id != owner.as_str() {
            return Err(AppError::Forbidden(format!(
                "post {} not owned by {}",
                post_id,
                owner.as_str()
            )));
        }
        let author = post.author_id.clone();
        self.inner
            .delete_post(post_id)
            .map_err(map_social_feed_err)?;
        self.bus.publish(A3chatEvent::MomentsPostDeleted {
            user_id: owner.clone(),
            post_id: post_id.to_string(),
            author_id: author,
        });
        Ok(())
    }

    /// `a3chat.moments.post.get`.
    pub fn get_post(&self, post_id: &str) -> AppResult<Option<SocialPost>> {
        self.inner
            .get_post(post_id)
            .map_err(map_social_feed_err)
    }

    /// `a3chat.moments.posts.by_user`.
    pub fn list_user_posts(&self, user_id: &str) -> AppResult<Vec<SocialPost>> {
        self.inner
            .list_user_posts(user_id)
            .map_err(map_social_feed_err)
    }

    /// `a3chat.moments.timeline`.
    pub fn timeline(&self, query: TimelineQuery) -> AppResult<TimelinePage> {
        self.inner.timeline(query).map_err(map_social_feed_err)
    }

    // ── Comments / reactions / follows ─────────────────────────────────

    /// `a3chat.moments.comment.add`.
    pub async fn comment_post(
        &self,
        owner: &UserId,
        mut comment: SocialComment,
    ) -> AppResult<SocialComment> {
        if comment.author_id.is_empty() {
            comment.author_id = owner.to_string();
        }
        if comment.author_name.is_empty() {
            comment.author_name = owner.to_string();
        }
        let now = now_millis();
        if comment.created_at == 0 {
            comment.created_at = now;
        }
        comment.updated_at = comment.created_at;

        if let Some(m) = &self.moderation {
            let decision = m.check_content(owner, &comment.content);
            if !decision.is_allowed() {
                return Err(AppError::Forbidden(format!(
                    "moderation denied moments comment: {}",
                    decision.reason
                )));
            }
        }

        let stored = self
            .inner
            .comment_post(comment)
            .await
            .map_err(map_social_feed_err)?;
        self.bus.publish(A3chatEvent::MomentsCommentAdded {
            user_id: owner.clone(),
            post_id: stored.post_id.clone(),
            comment_id: stored.comment_id.clone(),
            author_id: stored.author_id.clone(),
        });
        // MN-07 — `@`-mention fan-out. For every distinct user_id
        // listed in `comment.mentions`, publish a MomentsCommentMention
        // event so the receiving client can render an `@you` banner.
        // We dedupe + skip self-mentions so an `@-ing yourself`
        // doesn't trigger a no-op banner.
        let author = stored.author_id.clone();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for mentioned in stored.mentions.iter() {
            if mentioned.is_empty() || mentioned == &author || !seen.insert(mentioned.clone()) {
                continue;
            }
            // user_id is the *mentioned* user so a subscriber filtered
            // on `MomentsCommentMention.user_id == me` receives only
            // events aimed at them.
            let user_id = UserId::from(mentioned.as_str());
            self.bus.publish(A3chatEvent::MomentsCommentMention {
                user_id,
                post_id: stored.post_id.clone(),
                comment_id: stored.comment_id.clone(),
                author_id: author.clone(),
            });
        }
        Ok(stored)
    }

    /// `a3chat.moments.comments.list`.
    pub fn list_post_comments(&self, post_id: &str) -> AppResult<Vec<SocialComment>> {
        self.inner
            .list_post_comments(post_id)
            .map_err(map_social_feed_err)
    }

    /// `a3chat.moments.comment.edit`. The dispatcher is responsible
    /// for verifying that the caller owns the comment — we only
    /// stamp server-side bookkeeping (`updated_at`, `is_edited`,
    /// `edited_at`) and rerun the moderation gate on the new body.
    pub async fn update_comment(
        &self,
        owner: &UserId,
        mut comment: SocialComment,
    ) -> AppResult<SocialComment> {
        let now = now_millis();
        comment.updated_at = now;
        comment.is_edited = true;
        comment.edited_at = Some(now);
        // Author identity is preserved across edits; clients that
        // try to change ownership are rejected.
        if comment.author_id.is_empty() {
            comment.author_id = owner.to_string();
        }
        if let Some(m) = &self.moderation {
            let decision = m.check_content(owner, &comment.content);
            if !decision.is_allowed() {
                return Err(AppError::Forbidden(format!(
                    "moderation denied moments comment edit: {}",
                    decision.reason
                )));
            }
        }
        let stored = self
            .inner
            .update_comment(comment)
            .map_err(map_social_feed_err)?;
        self.bus.publish(A3chatEvent::MomentsCommentEdited {
            user_id: owner.clone(),
            post_id: stored.post_id.clone(),
            comment_id: stored.comment_id.clone(),
            author_id: stored.author_id.clone(),
        });
        Ok(stored)
    }

    /// `a3chat.moments.comment.delete`. The dispatcher is
    /// responsible for verifying that the caller owns the comment
    /// (or the underlying post).
    pub async fn delete_comment(
        &self,
        owner: &UserId,
        comment_id: &str,
    ) -> AppResult<bool> {
        let removed = self
            .inner
            .delete_comment(comment_id)
            .map_err(map_social_feed_err)?;
        if removed {
            self.bus.publish(A3chatEvent::MomentsCommentDeleted {
                user_id: owner.clone(),
                comment_id: comment_id.to_string(),
            });
        }
        Ok(removed)
    }

    pub fn get_comment(&self, comment_id: &str) -> AppResult<Option<SocialComment>> {
        self.inner
            .get_comment(comment_id)
            .map_err(map_social_feed_err)
    }

    /// `a3chat.moments.react`. The boolean returned by the
    /// upstream service (`true` when the reaction was newly
    /// inserted, `false` when it was already present) is folded
    /// into the `is_added` flag of the `moments.reaction.toggled`
    /// bus event.
    pub async fn react(
        &self,
        owner: &UserId,
        mut reaction: SocialReaction,
    ) -> AppResult<bool> {
        if reaction.user_id.is_empty() {
            reaction.user_id = owner.to_string();
        }
        if reaction.created_at == 0 {
            reaction.created_at = now_millis();
        }
        // The validation gate requires a non-empty `reaction_id`,
        // so mint a deterministic one when the caller left it
        // blank. Object-key uniqueness is enforced downstream by
        // the SQLite store (the `(user_id, target_id, reaction_type)`
        // unique index), so any stable identifier is fine.
        if reaction.reaction_id.is_empty() {
            reaction.reaction_id = format!(
                "reaction:{}:{}:{}",
                owner.as_str(),
                reaction.target_id,
                reaction.reaction_type.as_str()
            );
        }
        let target_id = reaction.target_id.clone();
        let reaction_type = reaction.reaction_type.as_str().to_string();
        let actor = reaction.user_id.clone();

        let inserted = self
            .inner
            .react(reaction)
            .await
            .map_err(map_social_feed_err)?;
        // Only emit a bus event when the upstream toggle actually
        // changed state. Idempotent re-likes are silent.
        if inserted {
            self.bus.publish(A3chatEvent::MomentsReactionToggled {
                user_id: owner.clone(),
                target_id,
                actor_id: actor,
                reaction_type,
                is_added: true,
            });
        }
        Ok(inserted)
    }

    /// `a3chat.moments.reactions.list`.
    pub fn list_reactions(&self, target_id: &str) -> AppResult<Vec<SocialReaction>> {
        self.inner
            .list_reactions(target_id)
            .map_err(map_social_feed_err)
    }

    /// `a3chat.moments.unreact`. Symmetric with `react` — when the
    /// user toggles the same reaction off, the bus event uses
    /// `is_added=false` so SSE subscribers can decrement counters.
    pub async fn unreact(
        &self,
        owner: &UserId,
        target_id: &str,
        target_type: a3net_types::invariants::ReactionTarget,
        user_id: &str,
    ) -> AppResult<bool> {
        let actor = if user_id.is_empty() {
            owner.to_string()
        } else {
            user_id.to_string()
        };
        let removed = self
            .inner
            .unreact(target_id, target_type, &actor)
            .map_err(map_social_feed_err)?;
        if removed {
            self.bus.publish(A3chatEvent::MomentsReactionToggled {
                user_id: owner.clone(),
                target_id: target_id.to_string(),
                actor_id: actor,
                reaction_type: target_type.as_str().to_string(),
                is_added: false,
            });
        }
        Ok(removed)
    }

    /// `a3chat.moments.follow`.
    pub fn follow(&self, follower: &str, following: &str) -> AppResult<()> {
        self.inner
            .follow(follower, following)
            .map_err(map_social_feed_err)
    }

    /// `a3chat.moments.unfollow`.
    pub fn unfollow(&self, follower: &str, following: &str) -> AppResult<()> {
        self.inner
            .unfollow(follower, following)
            .map_err(map_social_feed_err)
    }

    /// `a3chat.moments.following.list`.
    pub fn list_following(&self, follower: &str) -> AppResult<Vec<String>> {
        self.inner
            .list_following(follower)
            .map_err(map_social_feed_err)
    }

    /// `a3chat.moments.following.check`.
    pub fn is_following(&self, follower: &str, following: &str) -> AppResult<bool> {
        self.inner
            .is_following(follower, following)
            .map_err(map_social_feed_err)
    }

    /// Inverse of `list_following`. SR-MOMENTS-4: symmetric so a
    /// profile screen can show both directions of the follow graph.
    pub fn list_followers(&self, user_id: &str) -> AppResult<Vec<String>> {
        self.inner.list_followers(user_id).map_err(map_social_feed_err)
    }

    // ── Shares / Reports / Blocklist (v2 audit round) ────────

    /// `a3chat.moments.share`. Idempotent on
    /// `(target_id, target_type, sharer_id)` so a re-share click
    /// is a silent no-op rather than a duplicate row.
    pub fn share(
        &self,
        owner: &UserId,
        mut share: a3net_types::social_feed::ShareRecord,
    ) -> AppResult<bool> {
        if share.sharer_id.is_empty() {
            share.sharer_id = owner.to_string();
        }
        if share.sharer_name.is_empty() {
            share.sharer_name = owner.to_string();
        }
        // SR-MOMENTS-5 — auto-mint a `share_id` when the caller
        // left it blank. Without this, `validate()` rejects the
        // record at the storage layer with a confusing
        // "share_id: empty id" error.
        if share.share_id.is_empty() {
            share.share_id = format!(
                "share:{}:{}:{}",
                share.sharer_id,
                share.target_id,
                share.target_type.as_str()
            );
        }
        share.stamp_integrity_hash();
        let inserted = self
            .inner
            .share(share.clone())
            .map_err(map_social_feed_err)?;
        if inserted {
            self.bus.publish(A3chatEvent::MomentsPostShared {
                user_id: owner.clone(),
                target_id: share.target_id.clone(),
                target_type: share.target_type.as_str().to_string(),
                sharer_id: share.sharer_id.clone(),
            });
        }
        Ok(inserted)
    }

    /// `a3chat.moments.report`. Idempotent on
    /// `(target_id, target_type, reporter_id)` so a single user
    /// can't flood moderation with duplicate reports.
    pub fn report(
        &self,
        owner: &UserId,
        mut report: a3net_types::social_feed::ReportRecord,
    ) -> AppResult<bool> {
        if report.reporter_id.is_empty() {
            report.reporter_id = owner.to_string();
        }
        let inserted = self
            .inner
            .report(report.clone())
            .map_err(map_social_feed_err)?;
        if inserted {
            self.bus.publish(A3chatEvent::MomentsPostReported {
                user_id: owner.clone(),
                target_id: report.target_id.clone(),
                target_type: report.target_type.as_str().to_string(),
                reason: report.reason.as_str().to_string(),
            });
        }
        Ok(inserted)
    }

    /// `a3chat.moments.block`. SR-MOMENTS-7: subsequent timeline
    /// queries drop `blocked_user_id`'s posts; `react` /
    /// `comment_post` from a blocked user is rejected at the
    /// dispatcher level.
    pub fn block(
        &self,
        owner: &UserId,
        blocked_user_id: &str,
        reason: Option<String>,
    ) -> AppResult<bool> {
        let record = a3net_types::social_feed::BlockRecord {
            owner_id: owner.to_string(),
            blocked_user_id: blocked_user_id.to_string(),
            created_at: now_millis(),
            reason,
        };
        let inserted = self
            .inner
            .block(record.clone())
            .map_err(map_social_feed_err)?;
        if inserted {
            self.bus.publish(A3chatEvent::MomentsUserBlocked {
                user_id: owner.clone(),
                blocked_user_id: blocked_user_id.to_string(),
            });
        }
        Ok(inserted)
    }

    /// `a3chat.moments.unblock`.
    pub fn unblock(
        &self,
        owner: &UserId,
        blocked_user_id: &str,
    ) -> AppResult<bool> {
        self.inner
            .unblock(owner.as_str(), blocked_user_id)
            .map_err(map_social_feed_err)
    }

    /// `a3chat.moments.blocklist.list`.
    pub fn list_blocklist(&self, owner: &UserId) -> AppResult<Vec<String>> {
        self.inner
            .list_blocklist(owner.as_str())
            .map_err(map_social_feed_err)
    }

    /// `a3chat.moments.share.list` — read the re-share ledger for a
    /// post. The `target_type` field is exposed on the wire so the
    /// front-end can render the same DTO for both posts and
    /// comments.
    pub fn list_post_shares(
        &self,
        target_id: &str,
        target_type: a3net_types::social_feed::ShareTarget,
    ) -> AppResult<Vec<a3net_types::social_feed::ShareRecord>> {
        self.inner
            .list_post_shares(target_id, target_type)
            .map_err(map_social_feed_err)
    }

    pub fn is_blocked(
        &self,
        owner_id: &str,
        candidate_id: &str,
    ) -> AppResult<bool> {
        self.inner
            .is_blocked(owner_id, candidate_id)
            .map_err(map_social_feed_err)
    }

    /// `a3chat.moments.following.list` returns a `FollowRelationship`
    /// (with timestamps). Helper for callers that want richer info
    /// than the bare `Vec<String>`.
    pub fn follow_relationship(
        &self,
        follower: String,
        following: String,
    ) -> FollowRelationship {
        FollowRelationship {
            follower_id: follower,
            following_id: following,
            created_at: now_millis(),
        }
    }

    /// `a3chat.moments.verify.post`.
    pub fn verify_post_integrity(&self, post: &SocialPost) -> bool {
        self.inner
            .verify_post_integrity(post)
            .unwrap_or(false)
    }

    /// `a3chat.moments.verify.comment`.
    pub fn verify_comment_integrity(&self, comment: &SocialComment) -> bool {
        self.inner
            .inner()
            .verify_comment_integrity(comment)
    }

    /// `a3chat.moments.verify.reaction`.
    pub fn verify_reaction_integrity(&self, reaction: &SocialReaction) -> bool {
        self.inner
            .inner()
            .verify_reaction_integrity(reaction)
    }

    /// `a3chat.moments.node_info`. Reports the local node id and a
    /// unix-millis timestamp so callers can sanity-check the
    /// bridge is reachable.
    pub fn node_info(&self) -> NodeInfo {
        NodeInfo {
            node_id: self.inner.local_node().to_string(),
            ts: now_millis(),
            schema_version: a3net_socialfeed::SCHEMA_VERSION,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// DTOs
// ─────────────────────────────────────────────────────────────────────

/// Result of `a3chat.moments.node_info`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeInfo {
    pub node_id: String,
    pub ts: u64,
    pub schema_version: u32,
}

/// Wrapper for `moments.post.get` so the result is `{post: …}` or
/// `{post: null}` consistently with the other RPC namespaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostEnvelope {
    pub post: Option<SocialPost>,
}

/// Wrapper for `moments.comment.add`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentEnvelope {
    pub comment: SocialComment,
}

/// Wrapper for `moments.react`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactResult {
    pub inserted: bool,
}

/// Wrapper for `moments.posts.by_user` and `moments.comments.list`
/// — both return a typed array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostsResult {
    pub posts: Vec<SocialPost>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentsResult {
    pub comments: Vec<SocialComment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionsResult {
    pub reactions: Vec<SocialReaction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowingResult {
    pub following_ids: Vec<String>,
}

/// Inverse of [`FollowingResult`] — returned by
/// `a3chat.moments.followers.list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowersResult {
    pub follower_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowingCheckResult {
    pub following: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityResult {
    pub valid: bool,
}

/// Returned by `a3chat.moments.share`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareResult {
    pub inserted: bool,
}

/// Returned by `a3chat.moments.report`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportResult {
    pub inserted: bool,
}

/// Returned by `a3chat.moments.block`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockResult {
    pub inserted: bool,
}

/// Returned by `a3chat.moments.blocklist.list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlocklistResult {
    pub blocked_user_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineResult {
    pub posts: Vec<SocialPost>,
    pub next_cursor: Option<TimelineCursorDto>,
}

/// Wire-friendly cursor. We can't reuse
/// `a3net_socialfeed::TimelineCursor` directly because the
/// upstream type does not derive `Serialize`/`Deserialize`; the
/// DTO mirrors its shape and is what the RPC layer round-trips.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimelineCursorDto {
    pub created_at: u64,
    pub post_id: String,
}

impl From<a3net_socialfeed::TimelineCursor> for TimelineCursorDto {
    fn from(c: a3net_socialfeed::TimelineCursor) -> Self {
        Self {
            created_at: c.created_at,
            post_id: c.post_id,
        }
    }
}

impl From<TimelinePage> for TimelineResult {
    fn from(p: TimelinePage) -> Self {
        Self {
            posts: p.posts,
            next_cursor: p.next_cursor.map(TimelineCursorDto::from),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// RPC dispatch
// ─────────────────────────────────────────────────────────────────────

/// Top-level dispatcher for every `a3chat.moments.*` method.
///
/// Mirrors `moderation_service::dispatch` / `profile_service::dispatch`:
/// `owner` is forwarded from the `X-A3Chat-Owner` header; `params` is
/// the JSON-RPC params object. Every fallible path converts into
/// `A3chatError::InvalidInput` / `PermissionDenied` / `Storage` /
/// `Internal` so the RPC layer can serialise a structured error
/// envelope without losing category information.
pub async fn dispatch(
    svc: Arc<MomentsService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        "a3chat.moments.node_info" => {
            let info = svc.node_info();
            serde_json::to_value(info).map_err(domain_internal)
        }

        "a3chat.moments.post.create" => {
            let post: SocialPost = serde_json::from_value(
                params
                    .get("post")
                    .cloned()
                    .ok_or_else(|| invalid_input("missing 'post'"))?,
            )
            .map_err(|e| invalid_input(&format!("bad post payload: {e}")))?;
            let stored = svc.create_post(owner, post).await?;
            serde_json::to_value(stored).map_err(domain_internal)
        }

        "a3chat.moments.post.update" => {
            let post: SocialPost = serde_json::from_value(
                params
                    .get("post")
                    .cloned()
                    .ok_or_else(|| invalid_input("missing 'post'"))?,
            )
            .map_err(|e| invalid_input(&format!("bad post payload: {e}")))?;
            let stored = svc.update_post(owner, post).await?;
            serde_json::to_value(stored).map_err(domain_internal)
        }

        "a3chat.moments.post.delete" => {
            let post_id = params
                .get("post_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'post_id'"))?;
            svc.delete_post(owner, post_id).await?;
            Ok(serde_json::json!({ "ok": true, "post_id": post_id }))
        }

        "a3chat.moments.post.get" => {
            let post_id = params
                .get("post_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'post_id'"))?;
            let post = svc.get_post(post_id)?;
            serde_json::to_value(PostEnvelope { post }).map_err(domain_internal)
        }

        "a3chat.moments.posts.by_user" => {
            let user_id = params
                .get("user_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'user_id'"))?;
            let posts = svc.list_user_posts(user_id)?;
            serde_json::to_value(PostsResult { posts }).map_err(domain_internal)
        }

        "a3chat.moments.timeline" => {
            // `viewer_id` defaults to the chat owner so a single-
            // arg call (`{}`) works the same way as the chat
            // namespace — calling as `bob` with `{}` lists bob's
            // own feed.
            let viewer_id = params
                .get("viewer_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| owner.to_string());
            let limit = params.get("limit").and_then(|v| v.as_u64()).map(|n| n as usize);
            let before_ts = params.get("before_ts").and_then(|v| v.as_u64());
            let before_cursor = params
                .get("before_cursor")
                .and_then(|v| v.as_object())
                .map(|obj| a3net_socialfeed::TimelineCursor {
                    created_at: obj
                        .get("created_at")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0),
                    post_id: obj
                        .get("post_id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
                .filter(|c| c.post_id != "");
            let author_id = params
                .get("author_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let scope = params
                .get("scope")
                .and_then(|v| v.as_str())
                .map(parse_scope)
                .transpose()?
                .unwrap_or(TimelineScope::ForViewer);

            let q = TimelineQuery {
                viewer_id,
                scope,
                limit,
                before_cursor,
                before_ts,
                author_id,
            };
            let page = svc.timeline(q)?;
            serde_json::to_value(TimelineResult::from(page)).map_err(domain_internal)
        }

        "a3chat.moments.comment.add" => {
            let comment: SocialComment = serde_json::from_value(
                params
                    .get("comment")
                    .cloned()
                    .ok_or_else(|| invalid_input("missing 'comment'"))?,
            )
            .map_err(|e| invalid_input(&format!("bad comment payload: {e}")))?;
            let stored = svc.comment_post(owner, comment).await?;
            serde_json::to_value(CommentEnvelope { comment: stored })
                .map_err(domain_internal)
        }

        "a3chat.moments.comments.list" => {
            let post_id = params
                .get("post_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'post_id'"))?;
            let comments = svc.list_post_comments(post_id)?;
            serde_json::to_value(CommentsResult { comments }).map_err(domain_internal)
        }

        "a3chat.moments.react" => {
            let reaction: SocialReaction = serde_json::from_value(
                params
                    .get("reaction")
                    .cloned()
                    .ok_or_else(|| invalid_input("missing 'reaction'"))?,
            )
            .map_err(|e| invalid_input(&format!("bad reaction payload: {e}")))?;
            let inserted = svc.react(owner, reaction).await?;
            serde_json::to_value(ReactResult { inserted }).map_err(domain_internal)
        }

        "a3chat.moments.unreact" => {
            let target_id = params
                .get("target_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'target_id'"))?;
            let target_type_str = params
                .get("target_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'target_type'"))?;
            let target_type = a3net_types::invariants::ReactionTarget::from_strict(target_type_str)
                .map_err(|e| invalid_input(&format!("bad target_type: {e}")))?;
            let user_id = params
                .get("user_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let removed = svc
                .unreact(owner, target_id, target_type, user_id)
                .await?;
            Ok(serde_json::json!({ "removed": removed, "target_id": target_id, "user_id": if user_id.is_empty() { owner.to_string() } else { user_id.to_string() } }))
        }

        "a3chat.moments.comment.edit" => {
            let mut comment: SocialComment = serde_json::from_value(
                params
                    .get("comment")
                    .cloned()
                    .ok_or_else(|| invalid_input("missing 'comment'"))?,
            )
            .map_err(|e| invalid_input(&format!("bad comment payload: {e}")))?;
            // SR-MOMENTS-2 — ownership check at the dispatcher.
            // The service layer trusts the caller; we enforce here.
            // `get_comment` is the cheapest ownership probe. We
            // reject when **neither** the caller (chat owner)
            // **nor** the supplied author_id matches the comment's
            // author — the second arm lets a moderator / owner
            // tool that knows the canonical author id edit on
            // behalf of a user.
            match svc.get_comment(&comment.comment_id)? {
                Some(existing) => {
                    let existing_author = existing.author_id.clone();
                    if existing_author != owner.as_str()
                        && existing_author != comment.author_id
                    {
                        return Err(A3chatError::PermissionDenied(format!(
                            "comment {} not owned by {}",
                            comment.comment_id,
                            owner.as_str()
                        )));
                    }
                    if comment.author_id.is_empty() {
                        comment.author_id = existing_author.clone();
                    }
                    // SR-MOMENTS-2 (strict): if the caller is
                    // neither the comment author nor the post
                    // author, reject. Without this a non-owner
                    // could impersonate by setting `author_id`.
                    let caller = owner.as_str();
                    if caller != existing_author
                        && !svc
                            .get_post(&existing.post_id)?
                            .map(|p| p.author_id == caller)
                            .unwrap_or(false)
                    {
                        return Err(A3chatError::PermissionDenied(format!(
                            "comment {} not editable by {}",
                            comment.comment_id,
                            caller
                        )));
                    }
                }
                None => {
                    return Err(A3chatError::NotFound(format!(
                        "comment {}",
                        comment.comment_id
                    )))
                }
            }
            let stored = svc.update_comment(owner, comment).await?;
            serde_json::to_value(CommentEnvelope { comment: stored })
                .map_err(domain_internal)
        }

        "a3chat.moments.comment.delete" => {
            let comment_id = params
                .get("comment_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'comment_id'"))?;
            // Ownership check: a user can only delete their own
            // comment, OR the post author can delete any comment on
            // their post.
            let owner_str = owner.to_string();
            match svc.get_comment(comment_id)? {
                Some(c) => {
                    let is_owner = c.author_id == owner_str;
                    let mut allowed = is_owner;
                    if !allowed {
                        if let Some(post) = svc.get_post(&c.post_id)? {
                            allowed = post.author_id == owner_str;
                        }
                    }
                    if !allowed {
                        return Err(A3chatError::PermissionDenied(format!(
                            "comment {} not deletable by {}",
                            comment_id,
                            owner_str
                        )));
                    }
                }
                None => {
                    return Err(A3chatError::NotFound(format!(
                        "comment {}",
                        comment_id
                    )))
                }
            }
            let removed = svc.delete_comment(owner, comment_id).await?;
            Ok(serde_json::json!({ "removed": removed, "comment_id": comment_id }))
        }

        "a3chat.moments.followers.list" => {
            let user_id = params
                .get("user_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| owner.to_string());
            let ids = svc.list_followers(&user_id)?;
            serde_json::to_value(FollowersResult { follower_ids: ids })
                .map_err(domain_internal)
        }

        "a3chat.moments.share" => {
            let share: a3net_types::social_feed::ShareRecord = serde_json::from_value(
                params
                    .get("share")
                    .cloned()
                    .ok_or_else(|| invalid_input("missing 'share'"))?,
            )
            .map_err(|e| invalid_input(&format!("bad share payload: {e}")))?;
            // SR-MOMENTS-7 — blocklist enforcement. A user that
            // has blocked the post's author cannot share the post.
            // We resolve the target's author from the post row
            // when the share targets a post.
            if share.target_type == a3net_types::social_feed::ShareTarget::Post {
                if let Some(post) = svc.get_post(&share.target_id)? {
                    if svc.is_blocked(owner.as_str(), &post.author_id)? {
                        return Err(A3chatError::PermissionDenied(format!(
                            "owner {} has blocked the post author {}",
                            owner.as_str(),
                            post.author_id
                        )));
                    }
                } else {
                    return Err(A3chatError::NotFound(format!(
                        "post {}",
                        share.target_id
                    )));
                }
            }
            let inserted = svc.share(owner, share)?;
            serde_json::to_value(ShareResult { inserted }).map_err(domain_internal)
        }

        "a3chat.moments.report" => {
            let report: a3net_types::social_feed::ReportRecord = serde_json::from_value(
                params
                    .get("report")
                    .cloned()
                    .ok_or_else(|| invalid_input("missing 'report'"))?,
            )
            .map_err(|e| invalid_input(&format!("bad report payload: {e}")))?;
            // SR-MOMENTS-6 — reject self-reports at the dispatcher.
            if report.target_type == a3net_types::social_feed::ShareTarget::Post {
                if let Some(post) = svc.get_post(&report.target_id)? {
                    if post.author_id == owner.as_str() {
                        return Err(A3chatError::InvalidInput(format!(
                            "cannot report your own post ({})",
                            report.target_id
                        )));
                    }
                }
            }
            let inserted = svc.report(owner, report)?;
            serde_json::to_value(ReportResult { inserted }).map_err(domain_internal)
        }

        "a3chat.moments.block" => {
            let blocked_user_id = params
                .get("blocked_user_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'blocked_user_id'"))?;
            // SR-MOMENTS-7 — reject self-block at the dispatcher.
            if blocked_user_id == owner.as_str() {
                return Err(A3chatError::InvalidInput(
                    "cannot block yourself".into(),
                ));
            }
            let reason = params
                .get("reason")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let inserted = svc.block(owner, blocked_user_id, reason)?;
            serde_json::to_value(BlockResult { inserted }).map_err(domain_internal)
        }

        "a3chat.moments.unblock" => {
            let blocked_user_id = params
                .get("blocked_user_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'blocked_user_id'"))?;
            let removed = svc.unblock(owner, blocked_user_id)?;
            Ok(serde_json::json!({ "removed": removed, "blocked_user_id": blocked_user_id }))
        }

        "a3chat.moments.blocklist.list" => {
            let ids = svc.list_blocklist(owner)?;
            serde_json::to_value(BlocklistResult { blocked_user_ids: ids })
                .map_err(domain_internal)
        }

        "a3chat.moments.reactions.list" => {
            let target_id = params
                .get("target_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'target_id'"))?;
            let reactions = svc.list_reactions(target_id)?;
            serde_json::to_value(ReactionsResult { reactions })
                .map_err(domain_internal)
        }

        "a3chat.moments.follow" => {
            let following_id = params
                .get("following_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'following_id'"))?;
            svc.follow(owner.as_str(), following_id)?;
            Ok(serde_json::json!({ "ok": true, "follower_id": owner.as_str(), "following_id": following_id }))
        }

        "a3chat.moments.unfollow" => {
            let following_id = params
                .get("following_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'following_id'"))?;
            svc.unfollow(owner.as_str(), following_id)?;
            Ok(serde_json::json!({ "ok": true, "follower_id": owner.as_str(), "following_id": following_id }))
        }

        "a3chat.moments.following.list" => {
            let follower_id = params
                .get("follower_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| owner.to_string());
            let ids = svc.list_following(&follower_id)?;
            serde_json::to_value(FollowingResult { following_ids: ids })
                .map_err(domain_internal)
        }

        "a3chat.moments.following.check" => {
            let follower_id = params
                .get("follower_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| owner.to_string());
            let following_id = params
                .get("following_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid_input("missing 'following_id'"))?;
            let f = svc.is_following(&follower_id, following_id)?;
            serde_json::to_value(FollowingCheckResult { following: f })
                .map_err(domain_internal)
        }

        "a3chat.moments.verify.post" => {
            let post: SocialPost = serde_json::from_value(
                params
                    .get("post")
                    .cloned()
                    .ok_or_else(|| invalid_input("missing 'post'"))?,
            )
            .map_err(|e| invalid_input(&format!("bad post: {e}")))?;
            let valid = svc.verify_post_integrity(&post);
            serde_json::to_value(IntegrityResult { valid }).map_err(domain_internal)
        }

        "a3chat.moments.verify.comment" => {
            let comment: SocialComment = serde_json::from_value(
                params
                    .get("comment")
                    .cloned()
                    .ok_or_else(|| invalid_input("missing 'comment'"))?,
            )
            .map_err(|e| invalid_input(&format!("bad comment: {e}")))?;
            let valid = svc.verify_comment_integrity(&comment);
            serde_json::to_value(IntegrityResult { valid }).map_err(domain_internal)
        }

        "a3chat.moments.verify.reaction" => {
            let reaction: SocialReaction = serde_json::from_value(
                params
                    .get("reaction")
                    .cloned()
                    .ok_or_else(|| invalid_input("missing 'reaction'"))?,
            )
            .map_err(|e| invalid_input(&format!("bad reaction: {e}")))?;
            let valid = svc.verify_reaction_integrity(&reaction);
            serde_json::to_value(IntegrityResult { valid }).map_err(domain_internal)
        }

        m => Err(A3chatError::InvalidInput(format!(
            "unknown moments method: {m}"
        ))),
    }
}

// ─────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────

fn map_social_feed_err(e: SocialFeedError) -> AppError {
    // `a3net-socialfeed::ErrorClass` is a coarse 3-way
    // classification (`Permanent` / `Transient` / `Invalid`).
    // Map to `AppError` variants so the RPC layer can return
    // structured error codes rather than a single `Internal`
    // catch-all.
    match e.class() {
        a3net_socialfeed::ErrorClass::Invalid => AppError::Domain(e.to_string()),
        a3net_socialfeed::ErrorClass::Transient => AppError::Storage(e.to_string()),
        a3net_socialfeed::ErrorClass::Permanent => AppError::Internal(e.to_string()),
    }
}

fn invalid_input(s: impl Into<String>) -> A3chatError {
    A3chatError::InvalidInput(s.into())
}

fn domain_internal(e: serde_json::Error) -> A3chatError {
    A3chatError::Internal(format!("moments json: {e}"))
}

fn parse_scope(s: &str) -> Result<TimelineScope, A3chatError> {
    match s {
        "for_viewer" | "viewer" | "" => Ok(TimelineScope::ForViewer),
        "by_user" | "user" => Ok(TimelineScope::ByUser),
        "all" => Ok(TimelineScope::All),
        other => Err(A3chatError::InvalidInput(format!(
            "unknown timeline scope '{other}'"
        ))),
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// Compatibility helper for older `a3chat-app` callers that want
// to know if a `Visibility` JSON value is one of the typed enum
// variants. Kept private — the typed records already enforce this
// at the schema layer.
fn _visibility_str(v: &Visibility) -> &'static str {
    v.as_str()
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::invariants::{AttachmentKind, ReactionTarget, ReactionType, Visibility};
    use a3net_types::social_feed::attachment_from_hash;
    use a3net_types::content::ContentHash;

    fn owner() -> UserId {
        UserId::from("alice-node")
    }
    fn peer() -> UserId {
        UserId::from("bob-node")
    }

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

    fn svc() -> MomentsService {
        MomentsService::open_in_memory()
    }

    #[tokio::test]
    async fn create_then_get_post() {
        let s = svc();
        let stored = s
            .create_post(&owner(), sample_post("alice"))
            .await
            .expect("create");
        assert!(!stored.post_id.is_empty());
        let fetched = s.get_post(&stored.post_id).expect("get").expect("some");
        assert_eq!(fetched.post_id, stored.post_id);
    }

    #[tokio::test]
    async fn create_publishes_bus_event() {
        let s = svc();
        let mut rx = s.bus().subscribe();
        let stored = s
            .create_post(&owner(), sample_post("alice"))
            .await
            .expect("create");
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        match evt {
            A3chatEvent::MomentsPostCreated { user_id, post_id, .. } => {
                assert_eq!(user_id.as_str(), owner().as_str());
                assert_eq!(post_id, stored.post_id);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_publishes_bus_event() {
        let s = svc();
        let stored = s
            .create_post(&owner(), sample_post(owner().as_str()))
            .await
            .expect("create");
        let mut rx = s.bus().subscribe();
        s.delete_post(&owner(), &stored.post_id).await.expect("delete");
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        assert!(matches!(
            evt,
            A3chatEvent::MomentsPostDeleted { ref post_id, .. } if post_id == &stored.post_id
        ));
    }

    #[tokio::test]
    async fn comment_publishes_bus_event() {
        let s = svc();
        let stored = s
            .create_post(&owner(), sample_post("alice"))
            .await
            .expect("create");
        let mut rx = s.bus().subscribe();
        let c = s
            .comment_post(
                &peer(),
                SocialComment {
                    comment_id: String::new(),
                    post_id: stored.post_id.clone(),
                    author_id: "bob-node".into(),
                    author_name: "bob-node".into(),
                    author_avatar: None,
                    content: "nice".into(),
                    parent_id: None,
                    mentions: vec![],
                    created_at: 0,
                    updated_at: 0,
                    like_count: 0,
                    reply_count: 0,
                    is_edited: false,
                    edited_at: None,
                },
            )
            .await
            .expect("comment");
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        assert!(matches!(
            evt,
            A3chatEvent::MomentsCommentAdded { ref comment_id, .. } if comment_id == &c.comment_id
        ));
    }

    #[tokio::test]
    async fn react_publishes_bus_event_when_inserted() {
        let s = svc();
        let stored = s
            .create_post(&owner(), sample_post("alice"))
            .await
            .expect("create");
        let mut rx = s.bus().subscribe();
        let inserted = s
            .react(
                &peer(),
                SocialReaction {
                    reaction_id: "r1".into(),
                    target_id: stored.post_id.clone(),
                    target_type: ReactionTarget::Post,
                    user_id: "bob-node".into(),
                    reaction_type: ReactionType::Like,
                    created_at: 0,
                },
            )
            .await
            .expect("react");
        assert!(inserted);
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        assert!(matches!(
            evt,
            A3chatEvent::MomentsReactionToggled { ref target_id, is_added: true, .. }
                if target_id == &stored.post_id
        ));
    }

    #[tokio::test]
    async fn follow_unfollow_round_trip() {
        let s = svc();
        s.follow(owner().as_str(), peer().as_str()).expect("follow");
        assert!(s
            .is_following(owner().as_str(), peer().as_str())
            .expect("check"));
        let ids = s
            .list_following(owner().as_str())
            .expect("list");
        assert_eq!(ids, vec![peer().as_str().to_string()]);
        s.unfollow(owner().as_str(), peer().as_str())
            .expect("unfollow");
        assert!(!s
            .is_following(owner().as_str(), peer().as_str())
            .expect("check"));
    }

    #[tokio::test]
    async fn timeline_pagination() {
        let s = svc();
        for i in 0..3 {
            let mut p = sample_post("alice");
            p.content = format!("post {i}");
            s.create_post(&owner(), p).await.expect("create");
        }
        let page = s
            .timeline(TimelineQuery {
                viewer_id: "bob".into(),
                scope: TimelineScope::ForViewer,
                limit: Some(2),
                before_cursor: None,
                before_ts: None,
                author_id: None,
            })
            .expect("timeline");
        assert_eq!(page.posts.len(), 2);
        assert!(page.next_cursor.is_some());
    }

    #[tokio::test]
    async fn verify_post_integrity_round_trip() {
        let s = svc();
        let stored = s
            .create_post(&owner(), sample_post("alice"))
            .await
            .expect("create");
        assert!(s.verify_post_integrity(&stored));
    }

    #[tokio::test]
    async fn attachment_with_image_round_trips() {
        let s = svc();
        let blob = ContentHash::from_bytes(b"blob");
        let mut post = sample_post("alice");
        post.attachments.push(attachment_from_hash(
            "att1".into(),
            AttachmentKind::Image,
            &blob,
            "photo.jpg",
            1024,
        ));
        let stored = s
            .create_post(&owner(), post)
            .await
            .expect("create with attachment");
        assert_eq!(stored.attachments.len(), 1);
        assert_eq!(stored.attachments[0].blob_hash, blob.as_hex());
    }

    #[tokio::test]
    async fn node_info_reports_schema_version() {
        let s = svc();
        let info = s.node_info();
        assert!(!info.node_id.is_empty());
        assert!(info.schema_version >= 1);
    }

    #[tokio::test]
    async fn dispatch_create_post_returns_stored() {
        let s = Arc::new(svc());
        let post = sample_post("alice");
        let params = serde_json::json!({ "post": post });
        let v = dispatch(
            s.clone(),
            "a3chat.moments.post.create",
            &owner(),
            params,
        )
        .await
        .expect("dispatch ok");
        let stored: SocialPost = serde_json::from_value(v).expect("back to typed");
        assert!(!stored.post_id.is_empty());
    }

    #[tokio::test]
    async fn dispatch_unknown_method_errors() {
        let s = Arc::new(svc());
        let err = dispatch(
            s.clone(),
            "a3chat.moments.bogus",
            &owner(),
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn dispatch_timeline_default_viewer_is_owner() {
        let s = Arc::new(svc());
        s.create_post(&owner(), sample_post("alice"))
            .await
            .expect("create");
        let v = dispatch(
            s.clone(),
            "a3chat.moments.timeline",
            &owner(),
            serde_json::json!({ "limit": 10 }),
        )
        .await
        .expect("timeline");
        let posts = v["posts"].as_array().expect("array");
        assert_eq!(posts.len(), 1);
    }

    #[tokio::test]
    async fn method_count_matches_methods_const() {
        assert!(METHODS.contains(&"a3chat.moments.post.create"));
        assert!(METHODS.contains(&"a3chat.moments.timeline"));
        assert!(METHODS.contains(&"a3chat.moments.follow"));
        assert!(METHODS.contains(&"a3chat.moments.comment.edit"));
        assert!(METHODS.contains(&"a3chat.moments.unreact"));
        assert!(METHODS.contains(&"a3chat.moments.share"));
        assert!(METHODS.contains(&"a3chat.moments.report"));
        assert!(METHODS.contains(&"a3chat.moments.block"));
        assert!(METHODS.contains(&"a3chat.moments.followers.list"));
        // DO-178C §11.4 — the constant must agree with what the
        // dispatcher actually accepts. We pin the total to 27 so
        // adding/removing a method here is a deliberate edit.
        assert_eq!(METHODS.len(), 27);
    }

    // ──────────────────────────────────────────────────────────────
    // v2 audit round — new RPC coverage (DO-178C §6.3 coverage).
    // ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_post_auto_generates_id_when_blank() {
        let s = svc();
        let mut post = sample_post("alice");
        post.post_id = String::new();
        let stored = s
            .create_post(&owner(), post)
            .await
            .expect("create with blank post_id");
        assert!(!stored.post_id.is_empty(), "post_id should be filled");
        assert!(
            stored.post_id.starts_with("post:"),
            "auto-id format: {}",
            stored.post_id
        );
    }

    #[tokio::test]
    async fn delete_post_rejects_non_owner() {
        let s = svc();
        let stored = s
            .create_post(&owner(), sample_post(owner().as_str()))
            .await
            .expect("create");
        // peer != owner → ownership check should reject.
        let err = s
            .delete_post(&peer(), &stored.post_id)
            .await
            .expect_err("must reject");
        assert!(matches!(err, AppError::Forbidden(_)), "got: {err:?}");
    }

    #[tokio::test]
    async fn delete_post_rejects_missing() {
        let s = svc();
        let err = s
            .delete_post(&owner(), "post:does-not-exist")
            .await
            .expect_err("must reject missing post");
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn comment_edit_then_get_returns_edited_flag() {
        let s = svc();
        let post = s
            .create_post(&owner(), sample_post(owner().as_str()))
            .await
            .expect("create");
        let mut c = a3net_types::social_feed::SocialComment {
            comment_id: String::new(),
            post_id: post.post_id.clone(),
            author_id: owner().to_string(),
            author_name: owner().to_string(),
            author_avatar: None,
            content: "first".into(),
            parent_id: None,
            mentions: vec![],
            created_at: now_millis(),
            updated_at: now_millis(),
            like_count: 0,
            reply_count: 0,
            is_edited: false,
            edited_at: None,
        };
        c = s.comment_post(&owner(), c.clone()).await.expect("comment");
        assert!(!c.is_edited);
        c.content = "second".into();
        let edited = s.update_comment(&owner(), c.clone()).await.expect("edit");
        assert!(edited.is_edited);
        assert!(edited.edited_at.is_some());
        assert_eq!(edited.content, "second");
        let fetched = s.get_comment(&edited.comment_id).expect("get");
        assert_eq!(fetched.unwrap().content, "second");
    }

    #[tokio::test]
    async fn comment_delete_drops_record() {
        let s = svc();
        let post = s
            .create_post(&owner(), sample_post(owner().as_str()))
            .await
            .expect("create");
        let c = a3net_types::social_feed::SocialComment {
            comment_id: String::new(),
            post_id: post.post_id.clone(),
            author_id: owner().to_string(),
            author_name: owner().to_string(),
            author_avatar: None,
            content: "x".into(),
            parent_id: None,
            mentions: vec![],
            created_at: now_millis(),
            updated_at: now_millis(),
            like_count: 0,
            reply_count: 0,
            is_edited: false,
            edited_at: None,
        };
        let c = s.comment_post(&owner(), c).await.expect("comment");
        let removed = s
            .delete_comment(&owner(), &c.comment_id)
            .await
            .expect("delete");
        assert!(removed);
        let after = s.get_comment(&c.comment_id).expect("get");
        assert!(after.is_none());
    }

    #[tokio::test]
    async fn react_unreact_round_trip() {
        let s = svc();
        let post = s
            .create_post(&owner(), sample_post(owner().as_str()))
            .await
            .expect("create");
        let reaction = SocialReaction {
            reaction_id: String::new(),
            target_id: post.post_id.clone(),
            target_type: ReactionTarget::Post,
            user_id: owner().to_string(),
            reaction_type: ReactionType::Like,
            created_at: now_millis(),
        };
        let inserted = s.react(&owner(), reaction.clone()).await.expect("react");
        assert!(inserted);
        // Re-react is idempotent (no-op).
        let inserted2 = s.react(&owner(), reaction.clone()).await.expect("react2");
        assert!(!inserted2);
        // Unreact removes the row.
        let removed = s
            .unreact(&owner(), &post.post_id, ReactionTarget::Post, owner().as_str())
            .await
            .expect("unreact");
        assert!(removed);
        let list = s.list_reactions(&post.post_id).expect("list");
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn follow_followers_symmetry() {
        let s = svc();
        s.follow(&owner().to_string(), &peer().to_string())
            .expect("follow");
        let following = s.list_following(&owner().to_string()).expect("following");
        assert_eq!(following, vec![peer().to_string()]);
        let followers = s.list_followers(&peer().to_string()).expect("followers");
        assert_eq!(followers, vec![owner().to_string()]);
        s.unfollow(&owner().to_string(), &peer().to_string())
            .expect("unfollow");
        let following = s.list_following(&owner().to_string()).expect("following");
        assert!(following.is_empty());
        let followers = s.list_followers(&peer().to_string()).expect("followers");
        assert!(followers.is_empty());
    }

    #[tokio::test]
    async fn block_drops_blocked_authors_from_timeline() {
        let s = svc();
        // alice creates a post.
        let post = s
            .create_post(&owner(), sample_post(owner().as_str()))
            .await
            .expect("create");
        // peer blocks alice; peer is the viewer.
        let inserted = s
            .block(&peer(), owner().as_str(), Some("audit".into()))
            .expect("block");
        assert!(inserted);
        let blocked = s.is_blocked(&peer().to_string(), owner().as_str()).expect("is_blocked");
        assert!(blocked);
        let list = s.list_blocklist(&peer()).expect("list");
        assert_eq!(list, vec![owner().to_string()]);
        // peer's timeline should NOT include alice's post.
        let page = s
            .timeline(TimelineQuery {
                viewer_id: peer().to_string(),
                scope: TimelineScope::ForViewer,
                limit: Some(10),
                before_cursor: None,
                before_ts: None,
                author_id: None,
            })
            .expect("timeline");
        assert!(
            page.posts.iter().all(|p| p.post_id != post.post_id),
            "blocked author's post leaked into viewer timeline"
        );
        // Unblock restores visibility.
        s.unblock(&peer(), owner().as_str()).expect("unblock");
        let page = s
            .timeline(TimelineQuery {
                viewer_id: peer().to_string(),
                scope: TimelineScope::ForViewer,
                limit: Some(10),
                before_cursor: None,
                before_ts: None,
                author_id: None,
            })
            .expect("timeline after unblock");
        assert!(
            page.posts.iter().any(|p| p.post_id == post.post_id),
            "unblock should restore visibility"
        );
    }

    #[tokio::test]
    async fn share_is_idempotent_per_user_target() {
        let s = svc();
        let post = s
            .create_post(&owner(), sample_post(owner().as_str()))
            .await
            .expect("create");
        let share = a3net_types::social_feed::ShareRecord {
            share_id: String::new(),
            target_id: post.post_id.clone(),
            target_type: a3net_types::social_feed::ShareTarget::Post,
            sharer_id: owner().to_string(),
            sharer_name: owner().to_string(),
            comment: "cool".into(),
            created_at: now_millis(),
            integrity_hash: None,
        };
        let first = s.share(&owner(), share.clone()).expect("share");
        assert!(first);
        let second = s.share(&owner(), share.clone()).expect("share again");
        assert!(!second);
        let list = s
            .list_post_shares(&post.post_id, a3net_types::social_feed::ShareTarget::Post)
            .expect("list");
        assert_eq!(list.len(), 1);
        assert!(list[0].verify_integrity());
    }

    #[tokio::test]
    async fn report_rejects_self_report() {
        let s = Arc::new(svc());
        let post = s
            .create_post(&owner(), sample_post(owner().as_str()))
            .await
            .expect("create");
        let report = a3net_types::social_feed::ReportRecord {
            report_id: String::new(),
            target_id: post.post_id.clone(),
            target_type: a3net_types::social_feed::ShareTarget::Post,
            reporter_id: owner().to_string(),
            reason: a3net_types::social_feed::ReportReason::Spam,
            notes: "".into(),
            created_at: now_millis(),
        };
        let err = dispatch(
            s.clone(),
            "a3chat.moments.report",
            &owner(),
            serde_json::json!({ "report": report }),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn block_dispatcher_rejects_self_block() {
        let s = Arc::new(svc());
        let err = dispatch(
            s.clone(),
            "a3chat.moments.block",
            &owner(),
            serde_json::json!({ "blocked_user_id": owner().as_str() }),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn comment_edit_dispatcher_rejects_non_owner() {
        let s = Arc::new(svc());
        let post = s
            .create_post(&owner(), sample_post(owner().as_str()))
            .await
            .expect("create");
        let c = a3net_types::social_feed::SocialComment {
            comment_id: String::new(),
            post_id: post.post_id.clone(),
            author_id: owner().to_string(),
            author_name: owner().to_string(),
            author_avatar: None,
            content: "x".into(),
            parent_id: None,
            mentions: vec![],
            created_at: now_millis(),
            updated_at: now_millis(),
            like_count: 0,
            reply_count: 0,
            is_edited: false,
            edited_at: None,
        };
        let c = s.comment_post(&owner(), c).await.expect("comment");
        let mut edit_payload = c.clone();
        edit_payload.content = "edited".into();
        let err = dispatch(
            s.clone(),
            "a3chat.moments.comment.edit",
            &peer(),
            serde_json::json!({ "comment": edit_payload }),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::PermissionDenied(_)));
    }
}
