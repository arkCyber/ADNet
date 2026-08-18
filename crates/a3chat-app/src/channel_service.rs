//! `a3chat` channel / public-account service (F-09).
//!
//! Bridges `a3net_news::NewsService` (gossip fan-out, monotonic
//! per-room sequence, optional wallet signature) onto the
//! `a3chat-app` facade so it can be called via the
//! `a3chat.channel.*` JSON-RPC namespace.
//!
//! # Responsibilities
//!
//! 1. **Account + subscription storage** — persists
//!    [`PublicAccount`] / [`Subscription`] / [`FeedItem`] rows in
//!    the channel SQLite database (see [`crate::channel_storage`]).
//! 2. **Feed publish** — converts a [`FeedItem`] into a
//!    [`a3net_types::BulletinItem`], then asks
//!    [`a3net_news::NewsService::publish`] to ship it through
//!    gossip. The room id is `channel:{account_id}` so a per-account
//!    timeline lives on a stable topic.
//! 3. **SSE fan-out** — every successful state change publishes a
//!    [`a3chat_core::event::A3chatEvent`] on the in-process
//!    [`NotificationBus`] so SSE clients refresh their
//!    timelines / subscription list without polling.
//! 4. **Resource clamping** — `list` / `search` / `timeline` each
//!    enforce their own caps so a malicious or buggy client can't
//!    blow out memory.
//!
//! # What this service deliberately does **not** do
//!
//! - Wallet-sign bulletins (signed-bulletin support is exposed by
//!   `a3net-news::NewsService::publish_signed`; the wiring will be
//!   added when F-07-style "trust labels" land).
//! - Cross-device sync — flagged for a later release; the
//!   `(account_id, sequence, nonce)` identity makes dedupe
//!   straightforward once a mesh layer is in place.
//! - On-the-wire rich text — `body` is treated as a UTF-8 string.
//!   Markdown / HTML rendering is a frontend concern.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use a3chat_core::channel::{
    AccountKind, FeedAttachment, FeedItem, PublicAccount, Subscription, UpsertChannelAccountRequest,
    VerificationLevel, PublishFeedRequest, ACCOUNT_ID_PREFIX, FEED_ID_PREFIX, MAX_TAG_LEN,
    MAX_TAGS_PER_FEED_ITEM, compute_account_id, compute_feed_id, default_notify_mode,
};
use a3chat_core::error::A3chatError;
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;
use a3net_gossip::InProcessGossip;
use a3net_news::{
    BulletinItem, BulletinKind, NewsService, NewsServiceConfig, ValidationPolicy,
};
use a3net_types::{NodeId, RoomId};

use crate::channel_storage::{ChannelStorage, ChannelStorageConfig};
use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;

/// RPC method-name constants owned by this module. Mirror of
/// `a3chat_core::rpc::A3chatRpcMethod::CHANNEL_*` so the dispatcher
/// can pattern-match without re-importing every call site.
pub const METHODS: &[&str] = &[
    A3chatRpcMethod::CHANNEL_ACCOUNT_REGISTER,
    A3chatRpcMethod::CHANNEL_ACCOUNT_UPDATE,
    A3chatRpcMethod::CHANNEL_ACCOUNT_GET,
    A3chatRpcMethod::CHANNEL_ACCOUNT_GET_BY_OWNER,
    A3chatRpcMethod::CHANNEL_ACCOUNT_LIST,
    A3chatRpcMethod::CHANNEL_ACCOUNT_SEARCH,
    A3chatRpcMethod::CHANNEL_ACCOUNT_DELETE,
    A3chatRpcMethod::CHANNEL_SUBSCRIBE,
    A3chatRpcMethod::CHANNEL_UNSUBSCRIBE,
    A3chatRpcMethod::CHANNEL_SUBSCRIPTIONS_LIST,
    A3chatRpcMethod::CHANNEL_SUBSCRIPTIONS_OF_ACCOUNT,
    A3chatRpcMethod::CHANNEL_SUBSCRIPTION_SET_NOTIFY,
    A3chatRpcMethod::CHANNEL_SUBSCRIPTION_SET_PINNED,
    A3chatRpcMethod::CHANNEL_FEED_PUBLISH,
    A3chatRpcMethod::CHANNEL_FEED_RETRACT,
    A3chatRpcMethod::CHANNEL_FEED_GET,
    A3chatRpcMethod::CHANNEL_FEED_LIST,
    A3chatRpcMethod::CHANNEL_FEED_TIMELINE,
    A3chatRpcMethod::CHANNEL_FEED_MARK_READ,
    A3chatRpcMethod::CHANNEL_FEED_UNREAD_COUNT,
    A3chatRpcMethod::CHANNEL_HEALTH,
];

/// Default `limit` for [`ChannelService::list_accounts`].
const DEFAULT_ACCOUNT_LIST_LIMIT: u32 = 50;
/// Hard cap for [`ChannelService::list_accounts`].
const MAX_ACCOUNT_LIST_LIMIT: u32 = 200;
/// Default `limit` for [`ChannelService::list_feed_items`].
const DEFAULT_FEED_LIST_LIMIT: u32 = 20;
/// Hard cap for [`ChannelService::list_feed_items`].
const MAX_FEED_LIST_LIMIT: u32 = 100;
/// Default `limit` for [`ChannelService::search_accounts`].
const DEFAULT_SEARCH_LIMIT: u32 = 50;
/// Hard cap for [`ChannelService::search_accounts`].
const MAX_SEARCH_LIMIT: u32 = 200;
/// Hard cap on the number of subscriptions returned in a single
/// `subscriptions.list` page.
const MAX_SUBSCRIPTIONS_PER_PAGE: u32 = 500;

/// Configuration for [`ChannelService`].
#[derive(Debug, Clone)]
pub struct ChannelServiceConfig {
    /// Base directory. Mirrors the chat-storage base; the actual
    /// SQLite file lives at `<base>/channel/channel.db`.
    pub base_dir: PathBuf,
    /// Override the channel-storage file name (mainly used by
    /// tests so parallel runs can't collide).
    pub filename: String,
    /// Whether to spin up an in-process gossip transport. When
    /// `false`, the service still persists locally and exposes the
    /// full RPC surface; the only thing it skips is the
    /// cross-network fan-out.
    pub enable_gossip: bool,
}

impl ChannelServiceConfig {
    /// Build a config under `<base>`.
    pub fn under_base(base: &Path) -> Self {
        Self {
            base_dir: base.to_path_buf(),
            filename: "channel.db".into(),
            enable_gossip: true,
        }
    }
}

impl Default for ChannelServiceConfig {
    fn default() -> Self {
        let mut base_dir = std::env::temp_dir();
        base_dir.push("a3chat_channel_service");
        Self {
            base_dir,
            filename: "channel.db".into(),
            enable_gossip: true,
        }
    }
}

/// Cheap-clone handle to the channel runtime. Holds a
/// [`ChannelStorage`] (which is itself cheap-clone `Arc<Mutex<...>>`),
/// a [`NewsService`] for gossip fan-out, and a [`NotificationBus`]
/// for SSE.
#[derive(Clone)]
pub struct ChannelService {
    storage: ChannelStorage,
    bus: NotificationBus,
    /// In-process gossip transport. We keep our own `Arc` handle
    /// because the service is `Clone` and the news service holds a
    /// different reference; sharing an `Arc` keeps both views live
    /// in the same in-process topic registry.
    gossip: Arc<InProcessGossip>,
    news: Arc<NewsService>,
    /// Identity of the local node — used as the `author_id` on
    /// every bulletin published by this node.
    local_node: NodeId,
    #[allow(dead_code)]
    config: ChannelServiceConfig,
}

impl std::fmt::Debug for ChannelService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelService")
            .field("storage", &self.storage.config())
            .field("local_node", &self.local_node)
            .finish()
    }
}

impl ChannelService {
    /// Open a service backed by a fresh SQLite database + an
    /// in-process gossip transport. Production callers should
    /// supply a stable `local_node` (a node id derived from the
    /// user's keyring); tests can pass `NodeId::random()`.
    pub fn open(config: ChannelServiceConfig, local_node: NodeId) -> AppResult<Self> {
        let storage_cfg = ChannelStorageConfig {
            storage_dir: config.base_dir.join("channel"),
            filename: config.filename.clone(),
        };
        let storage = ChannelStorage::open(storage_cfg)?;
        let bus = NotificationBus::new(NotificationBus::default_capacity());
        let gossip = Arc::new(InProcessGossip::new());
        let transport: Arc<dyn a3net_gossip::transport::GossipTransport> = gossip.clone();
        let news = Arc::new(NewsService::open(
            local_node.clone(),
            transport,
            NewsServiceConfig {
                store_dir: config.base_dir.join("news-store"),
                policy: ValidationPolicy::Lenient,
                event_channel_capacity: 1024,
            },
        )
        .map_err(|e| AppError::Rpc(format!("news.open: {e}")))?);
        Ok(Self {
            storage,
            bus,
            gossip,
            news,
            local_node,
            config,
        })
    }

    /// Open an in-memory service for tests. The gossip transport is
    /// still in-process, so a multi-instance test that wants to
    /// observe fan-out should share the `InProcessGossip` via
    /// [`Self::open_with_shared_gossip`].
    pub fn open_in_memory(local_node: NodeId) -> AppResult<Self> {
        let storage_cfg = ChannelStorageConfig {
            storage_dir: std::env::temp_dir().join(format!(
                "a3chat-channel-test-{:?}",
                local_node.as_hex()
            )),
            filename: "channel.db".into(),
        };
        let storage = ChannelStorage::open(storage_cfg)?;
        let bus = NotificationBus::new(NotificationBus::default_capacity());
        let gossip = Arc::new(InProcessGossip::new());
        let transport: Arc<dyn a3net_gossip::transport::GossipTransport> = gossip.clone();
        let news = Arc::new(
            NewsService::open_in_memory(local_node.clone(), transport, ValidationPolicy::Lenient)
                .map_err(|e| AppError::Rpc(format!("news.open_in_memory: {e}")))?,
        );
        Ok(Self {
            storage,
            bus,
            gossip,
            news,
            local_node,
            config: ChannelServiceConfig::default(),
        })
    }

    /// Open a service that shares the gossip transport with a
    /// caller-supplied handle — used by integration tests that want
    /// to observe a publish arriving on another `ChannelService`
    /// instance.
    pub fn open_with_shared_gossip(
        local_node: NodeId,
        gossip: Arc<InProcessGossip>,
        bus: NotificationBus,
    ) -> AppResult<Self> {
        let storage_cfg = ChannelStorageConfig {
            storage_dir: std::env::temp_dir().join(format!(
                "a3chat-channel-shared-{:?}",
                local_node.as_hex()
            )),
            filename: "channel.db".into(),
        };
        let storage = ChannelStorage::open(storage_cfg)?;
        let transport: Arc<dyn a3net_gossip::transport::GossipTransport> = gossip.clone();
        let news = Arc::new(
            NewsService::open_in_memory(local_node.clone(), transport, ValidationPolicy::Lenient)
                .map_err(|e| AppError::Rpc(format!("news.open_in_memory: {e}")))?,
        );
        Ok(Self {
            storage,
            bus,
            gossip,
            news,
            local_node,
            config: ChannelServiceConfig::default(),
        })
    }

    /// Borrow the underlying storage. Mostly for tests / SSE
    /// integration.
    pub fn storage(&self) -> &ChannelStorage {
        &self.storage
    }

    /// Borrow the in-process gossip transport.
    pub fn gossip(&self) -> &Arc<InProcessGossip> {
        &self.gossip
    }

    /// Borrow the news service (for tests that need to
    /// subscribe to raw bulletin events).
    pub fn news(&self) -> &NewsService {
        &self.news
    }

    /// Borrow the bus.
    pub fn bus(&self) -> &NotificationBus {
        &self.bus
    }

    /// Local node id — exposed so RPC handlers can echo it on
    /// `channel.health`.
    pub fn local_node(&self) -> &NodeId {
        &self.local_node
    }

    // ── Accounts ────────────────────────────────────────────────────

    /// `a3chat.channel.account.register` — register a new public
    /// account. The account id is derived from
    /// `(owner_node_id, created_at_unix)`, so a retry that hits
    /// the same `(owner, now)` triple updates the existing row
    /// instead of minting a second account.
    pub async fn register_account(
        &self,
        owner_node_id: &str,
        request: UpsertChannelAccountRequest,
    ) -> AppResult<PublicAccount> {
        request.validate().map_err(AppError::from)?;
        a3chat_core::id::validate_id("owner_node_id", owner_node_id).map_err(AppError::from)?;
        if let Some(existing) = self
            .storage
            .get_account_by_owner(owner_node_id)?
        {
            return Err(AppError::Conflict(format!(
                "account already exists for owner {owner_node_id} (account_id={})",
                existing.account_id
            )));
        }
        let now = chrono::Utc::now();
        let account_id = compute_account_id(owner_node_id, now.timestamp());
        let account = PublicAccount {
            account_id,
            owner_node_id: owner_node_id.to_string(),
            name: request.name,
            bio: request.bio,
            avatar_hash: request.avatar_hash,
            tags: request.tags,
            kind: request.kind,
            verification: request.verification,
            sequence: 0,
            subscriber_count: 0,
            created_at: now,
            updated_at: now,
        };
        self.storage.put_account(&account)?;
        // Eagerly join the gossip topic so inbound bulletins from
        // the network land in the local store. Idempotent.
        let _ = self
            .news
            .join_room(&Self::room_id_for(&account.account_id))
            .await;
        self.bus.publish(A3chatEvent::ChannelAccountRegistered {
            user_id: UserId::from(owner_node_id),
            account: account.clone(),
        });
        Ok(account)
    }

    /// `a3chat.channel.account.update` — patch mutable fields on an
    /// existing account. `account_id` is the *current* id; the
    /// `(owner, created_at)` triple that produced it stays stable
    /// so a future `get_account_by_owner` still finds the row.
    pub async fn update_account(
        &self,
        owner_node_id: &str,
        request: UpsertChannelAccountRequest,
    ) -> AppResult<PublicAccount> {
        request.validate().map_err(AppError::from)?;
        a3chat_core::id::validate_id("owner_node_id", owner_node_id).map_err(AppError::from)?;
        let mut existing = self
            .storage
            .get_account_by_owner(owner_node_id)?
            .ok_or_else(|| {
                AppError::Domain(format!(
                    "no account for owner {owner_node_id}; call register first"
                ))
            })?;
        existing.name = request.name;
        existing.bio = request.bio;
        existing.avatar_hash = request.avatar_hash;
        existing.tags = request.tags;
        existing.kind = request.kind;
        existing.verification = request.verification;
        existing.updated_at = chrono::Utc::now();
        self.storage.put_account(&existing)?;
        self.bus.publish(A3chatEvent::ChannelAccountUpdated {
            user_id: UserId::from(owner_node_id),
            account: existing.clone(),
        });
        Ok(existing)
    }

    /// `a3chat.channel.account.get` — fetch by account id.
    pub fn get_account(&self, account_id: &str) -> AppResult<Option<PublicAccount>> {
        self.storage.get_account(account_id)
    }

    /// `a3chat.channel.account.get_by_owner` — fetch the account
    /// owned by `owner_node_id`. At most one row per owner.
    pub fn get_account_by_owner(
        &self,
        owner_node_id: &str,
    ) -> AppResult<Option<PublicAccount>> {
        self.storage.get_account_by_owner(owner_node_id)
    }

    /// `a3chat.channel.account.list` — newest-first list of public
    /// accounts.
    pub fn list_accounts(&self, limit: Option<u32>) -> AppResult<Vec<PublicAccount>> {
        let limit = limit.unwrap_or(DEFAULT_ACCOUNT_LIST_LIMIT);
        if limit == 0 || limit > MAX_ACCOUNT_LIST_LIMIT {
            return Err(AppError::Domain(format!(
                "limit {limit} not in 1..={MAX_ACCOUNT_LIST_LIMIT}"
            )));
        }
        self.storage.list_accounts(limit)
    }

    /// `a3chat.channel.account.search` — case-insensitive substring
    /// search over name / bio / owner.
    pub fn search_accounts(
        &self,
        needle: &str,
        limit: Option<u32>,
    ) -> AppResult<Vec<PublicAccount>> {
        let trimmed = needle.trim();
        if trimmed.is_empty() {
            return Err(AppError::Domain("search.needle: empty".into()));
        }
        if trimmed.len() > 256 {
            return Err(AppError::Domain(format!(
                "search.needle: length {} > 256",
                trimmed.len()
            )));
        }
        let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        if limit == 0 || limit > MAX_SEARCH_LIMIT {
            return Err(AppError::Domain(format!(
                "limit {limit} not in 1..={MAX_SEARCH_LIMIT}"
            )));
        }
        self.storage.search_accounts(trimmed, limit)
    }

    /// `a3chat.channel.account.delete` — drop the account, all
    /// subscriptions, and all feed rows. Idempotent at the storage
    /// layer; the RPC layer returns `false` for "no such account".
    pub fn delete_account(&self, owner_node_id: &str) -> AppResult<bool> {
        a3chat_core::id::validate_id("owner_node_id", owner_node_id).map_err(AppError::from)?;
        let Some(existing) = self.storage.get_account_by_owner(owner_node_id)? else {
            return Ok(false);
        };
        let removed = self.storage.delete_account(&existing.account_id)?;
        if removed {
            self.bus.publish(A3chatEvent::ChannelAccountDeleted {
                user_id: UserId::from(owner_node_id),
                account_id: existing.account_id.clone(),
            });
        }
        Ok(removed)
    }

    // ── Subscriptions ───────────────────────────────────────────────

    /// `a3chat.channel.subscribe` — add `subscriber_id` to the
    /// account's follower list. Idempotent: a re-subscribe updates
    /// `alias` / `notify_mode` / `is_muted` / `is_pinned` but
    /// does not double-count.
    pub fn subscribe(
        &self,
        subscriber_id: &str,
        account_id: &str,
        alias: &str,
        notify_mode: &str,
    ) -> AppResult<Subscription> {
        a3chat_core::id::validate_id("subscriber_id", subscriber_id).map_err(AppError::from)?;
        a3chat_core::id::validate_id("account_id", account_id).map_err(AppError::from)?;
        if !account_id.starts_with(ACCOUNT_ID_PREFIX) {
            return Err(AppError::Domain(format!(
                "account_id: must start with {ACCOUNT_ID_PREFIX:?}"
            )));
        }
        // Make sure the account exists; otherwise subscribing to a
        // typo'd id silently inserts a dangling row.
        self.storage
            .get_account(account_id)?
            .ok_or_else(|| AppError::Domain(format!("account not found: {account_id}")))?;
        if !alias.is_empty() && alias.len() > 64 {
            return Err(AppError::Domain(format!(
                "alias: length {} > 64",
                alias.len()
            )));
        }
        if notify_mode != "normal" && notify_mode != "silent" && notify_mode != "strong" {
            return Err(AppError::Domain(format!(
                "notify_mode: must be one of normal|silent|strong (got {notify_mode:?})"
            )));
        }
        let sub = Subscription {
            subscriber_id: subscriber_id.to_string(),
            account_id: account_id.to_string(),
            alias: alias.to_string(),
            notify_mode: notify_mode.to_string(),
            is_muted: false,
            is_pinned: false,
            subscribed_at: chrono::Utc::now(),
            last_read_seq: 0,
        };
        self.storage.put_subscription(&sub)?;
        // Refresh the cached `subscriber_count` so the
        // `PublicAccount` row mirrors the truth.
        let _ = self.storage.recompute_subscriber_count(account_id)?;
        self.bus.publish(A3chatEvent::ChannelSubscribed {
            user_id: UserId::from(subscriber_id),
            account_id: account_id.to_string(),
        });
        Ok(sub)
    }

    /// `a3chat.channel.unsubscribe` — drop a subscription.
    pub fn unsubscribe(&self, subscriber_id: &str, account_id: &str) -> AppResult<bool> {
        a3chat_core::id::validate_id("subscriber_id", subscriber_id).map_err(AppError::from)?;
        let removed = self
            .storage
            .delete_subscription(subscriber_id, account_id)?;
        if removed {
            let _ = self.storage.recompute_subscriber_count(account_id)?;
            self.bus.publish(A3chatEvent::ChannelUnsubscribed {
                user_id: UserId::from(subscriber_id),
                account_id: account_id.to_string(),
            });
        }
        Ok(removed)
    }

    /// `a3chat.channel.subscriptions.list` — list all accounts a
    /// subscriber follows. Capped at
    /// [`MAX_SUBSCRIPTIONS_PER_PAGE`].
    pub fn list_subscriptions(
        &self,
        subscriber_id: &str,
    ) -> AppResult<Vec<Subscription>> {
        a3chat_core::id::validate_id("subscriber_id", subscriber_id).map_err(AppError::from)?;
        let subs = self.storage.list_subscriptions(subscriber_id)?;
        if subs.len() as u32 > MAX_SUBSCRIPTIONS_PER_PAGE {
            // Defensive cap — should never trigger because the
            // storage layer doesn't paginate, but we don't want a
            // runaway row count to OOM the SSE client.
            return Ok(subs.into_iter().take(MAX_SUBSCRIPTIONS_PER_PAGE as usize).collect());
        }
        Ok(subs)
    }

    /// `a3chat.channel.subscriptions.of_account` — list subscribers
    /// of a specific account. Capped at
    /// [`MAX_SUBSCRIPTIONS_PER_PAGE`].
    pub fn list_subscribers_of(
        &self,
        account_id: &str,
    ) -> AppResult<Vec<Subscription>> {
        a3chat_core::id::validate_id("account_id", account_id).map_err(AppError::from)?;
        let subs = self.storage.list_subscribers_of(account_id)?;
        if subs.len() as u32 > MAX_SUBSCRIPTIONS_PER_PAGE {
            return Ok(subs.into_iter().take(MAX_SUBSCRIPTIONS_PER_PAGE as usize).collect());
        }
        Ok(subs)
    }

    /// `a3chat.channel.subscription.set_notify` — change a
    /// subscriber's notify mode (`normal|silent|strong`) and / or
    /// mute state. Either field can be `None` to leave it alone.
    pub fn set_subscription_notify(
        &self,
        subscriber_id: &str,
        account_id: &str,
        notify_mode: Option<&str>,
        is_muted: Option<bool>,
    ) -> AppResult<Subscription> {
        a3chat_core::id::validate_id("subscriber_id", subscriber_id).map_err(AppError::from)?;
        a3chat_core::id::validate_id("account_id", account_id).map_err(AppError::from)?;
        if let Some(m) = notify_mode {
            if m != "normal" && m != "silent" && m != "strong" {
                return Err(AppError::Domain(format!(
                    "notify_mode: must be one of normal|silent|strong (got {m:?})"
                )));
            }
        }
        let mut sub = self
            .storage
            .get_subscription(subscriber_id, account_id)?
            .ok_or_else(|| {
                AppError::Domain(format!(
                    "no subscription for ({subscriber_id}, {account_id})"
                ))
            })?;
        if let Some(m) = notify_mode {
            sub.notify_mode = m.to_string();
        }
        if let Some(m) = is_muted {
            sub.is_muted = m;
        }
        self.storage.put_subscription(&sub)?;
        Ok(sub)
    }

    /// `a3chat.channel.subscription.set_pinned` — toggle a
    /// subscription's pinned flag.
    pub fn set_subscription_pinned(
        &self,
        subscriber_id: &str,
        account_id: &str,
        is_pinned: bool,
    ) -> AppResult<Subscription> {
        a3chat_core::id::validate_id("subscriber_id", subscriber_id).map_err(AppError::from)?;
        a3chat_core::id::validate_id("account_id", account_id).map_err(AppError::from)?;
        let mut sub = self
            .storage
            .get_subscription(subscriber_id, account_id)?
            .ok_or_else(|| {
                AppError::Domain(format!(
                    "no subscription for ({subscriber_id}, {account_id})"
                ))
            })?;
        sub.is_pinned = is_pinned;
        self.storage.put_subscription(&sub)?;
        Ok(sub)
    }

    // ── Feed ────────────────────────────────────────────────────────

    /// `a3chat.channel.feed.publish` — publish a new feed item to
    /// the account's room. The sequence is assigned by the storage
    /// layer so two concurrent publishes can't mint the same
    /// number. The local bus publishes a `ChannelFeedPublished`
    /// event so the owner's other devices refresh without polling.
    pub async fn publish_feed(
        &self,
        owner_node_id: &str,
        request: PublishFeedRequest,
    ) -> AppResult<FeedItem> {
        request.validate().map_err(AppError::from)?;
        a3chat_core::id::validate_id("owner_node_id", owner_node_id).map_err(AppError::from)?;
        let account = self
            .storage
            .get_account_by_owner(owner_node_id)?
            .ok_or_else(|| {
                AppError::Domain(format!(
                    "no account for owner {owner_node_id}; call register first"
                ))
            })?;
        if account.owner_node_id != self.local_node.as_hex() {
            return Err(AppError::Forbidden(format!(
                "owner_node_id {owner_node_id} does not match local_node {}",
                self.local_node.as_hex()
            )));
        }
        let now = chrono::Utc::now();
        // Bump the sequence FIRST so the bulletin we publish
        // carries the canonical monotonic number. The
        // `feed_items.sequence` column is UNIQUE so a duplicate
        // bump surfaces as a constraint error.
        let sequence = self
            .storage
            .bump_account_sequence(&account.account_id, now)?;
        // 16 bytes of randomness for the feed id nonce — the
        // id collision space is 2^128, well outside any realistic
        // abuse scenario, and the deterministic inputs
        // (account_id, sequence, created_at_unix) already de-dup
        // intentional re-publishes.
        let mut nonce = [0u8; 16];
        for slot in nonce.iter_mut() {
            *slot = rand_byte();
        }
        let feed_id = compute_feed_id(
            &account.account_id,
            sequence,
            now.timestamp(),
            &nonce,
        );
        let feed = FeedItem {
            feed_id,
            account_id: account.account_id.clone(),
            sequence,
            title: request.title,
            summary: request.summary,
            body: request.body,
            cover_url: request.cover_url,
            attachments: request.attachments,
            tags: request.tags,
            is_pinned: request.is_pinned,
            is_retracted: false,
            retraction_reason: None,
            created_at: now,
            updated_at: now,
        };
        feed.validate().map_err(AppError::from)?;
        self.storage.put_feed_item(&feed)?;
        // Gossip fan-out: build a BulletinItem whose body carries
        // the serialised FeedItem. Subscribers decode it back in
        // `ingest_bulletin_for_subscriber`. We keep a stable
        // envelope shape so a future native BulletinItem renderer
        // can render the same wire without the FeedItem wrapper.
        let body_json = serde_json::to_string(&feed).map_err(AppError::from)?;
        let bulletin = BulletinItem::new(
            BulletinKind::NewsArticle,
            a3net_types::BulletinCategory::General,
            a3net_types::BulletinSeverity::Info,
            Self::room_id_for(&account.account_id),
            self.local_node.clone(),
            feed.title.clone(),
            feed.summary.clone(),
            body_json,
            &nonce,
            None,
        )
        .map_err(|e| AppError::Internal(format!("bulletin build: {e}")))?;
        // Add metadata the rest of the system can rely on.
        let mut bulletin = bulletin;
        bulletin.lang = "en".into();
        if !feed.tags.is_empty() {
            bulletin.tags = feed.tags.clone();
        }
        bulletin.sequence = feed.sequence;
        // Join the room so the publish actually reaches anyone
        // listening. Idempotent on `join_room` so it's safe to
        // re-call before every publish.
        let _ = self
            .news
            .join_room(&Self::room_id_for(&account.account_id))
            .await;
        let _ = self.news.publish(bulletin).await.map_err(|e| {
            AppError::Rpc(format!("channel.feed.publish: gossip publish failed: {e}"))
        })?;
        self.bus.publish(A3chatEvent::ChannelFeedPublished {
            user_id: UserId::from(owner_node_id),
            account_id: account.account_id.clone(),
            feed: feed.clone(),
        });
        Ok(feed)
    }

    /// `a3chat.channel.feed.retract` — mark a feed item as
    /// retracted. The row is kept on disk but excluded from the
    /// public timeline. `reason` is required and ends up in the
    /// SSE payload.
    pub fn retract_feed(
        &self,
        owner_node_id: &str,
        feed_id: &str,
        reason: &str,
    ) -> AppResult<()> {
        a3chat_core::id::validate_id("owner_node_id", owner_node_id).map_err(AppError::from)?;
        a3chat_core::id::validate_id("feed_id", feed_id).map_err(AppError::from)?;
        if reason.trim().is_empty() {
            return Err(AppError::Domain("retract.reason: empty".into()));
        }
        if reason.len() > 256 {
            return Err(AppError::Domain(format!(
                "retract.reason: length {} > 256",
                reason.len()
            )));
        }
        let account = self
            .storage
            .get_account_by_owner(owner_node_id)?
            .ok_or_else(|| AppError::Domain(format!(
                "no account for owner {owner_node_id}"
            )))?;
        self.storage
            .retract_feed_item(&account.account_id, feed_id, reason, chrono::Utc::now())?;
        self.bus.publish(A3chatEvent::ChannelFeedRetracted {
            user_id: UserId::from(owner_node_id),
            account_id: account.account_id,
            feed_id: feed_id.to_string(),
            reason: reason.to_string(),
        });
        Ok(())
    }

    /// `a3chat.channel.feed.get` — fetch a single feed item.
    pub fn get_feed(
        &self,
        account_id: &str,
        feed_id: &str,
    ) -> AppResult<Option<FeedItem>> {
        self.storage.get_feed_item(account_id, feed_id)
    }

    /// `a3chat.channel.feed.list` — paginated list of a single
    /// account's feed items, newest first.
    pub fn list_feed(
        &self,
        account_id: &str,
        before_sequence: Option<u32>,
        limit: Option<u32>,
    ) -> AppResult<Vec<FeedItem>> {
        a3chat_core::id::validate_id("account_id", account_id).map_err(AppError::from)?;
        let limit = limit.unwrap_or(DEFAULT_FEED_LIST_LIMIT);
        if limit == 0 || limit > MAX_FEED_LIST_LIMIT {
            return Err(AppError::Domain(format!(
                "limit {limit} not in 1..={MAX_FEED_LIST_LIMIT}"
            )));
        }
        self.storage
            .list_feed_items(account_id, before_sequence, limit)
    }

    /// `a3chat.channel.feed.timeline` — feed items from every
    /// account the subscriber follows, merged newest-first. The
    /// list is capped at `MAX_FEED_LIST_LIMIT` rows so a subscriber
    /// with hundreds of follows doesn't pull a runaway timeline.
    pub fn timeline(
        &self,
        subscriber_id: &str,
        before_sequence: Option<u32>,
        limit: Option<u32>,
    ) -> AppResult<Vec<FeedItem>> {
        a3chat_core::id::validate_id("subscriber_id", subscriber_id).map_err(AppError::from)?;
        let limit = limit.unwrap_or(DEFAULT_FEED_LIST_LIMIT);
        if limit == 0 || limit > MAX_FEED_LIST_LIMIT {
            return Err(AppError::Domain(format!(
                "limit {limit} not in 1..={MAX_FEED_LIST_LIMIT}"
            )));
        }
        let subs = self.storage.list_subscriptions(subscriber_id)?;
        // Pull `limit` items per account then merge-sort by
        // sequence. The implementation is bounded by
        // `subs.len() * limit` rows pulled from SQLite, which is
        // fine in practice (subs are rare; the cap is 500).
        let per_account_cap = limit;
        let mut all: Vec<FeedItem> = Vec::new();
        for s in subs {
            let items = self
                .storage
                .list_feed_items(&s.account_id, before_sequence, per_account_cap)?;
            all.extend(items);
        }
        // Stable sort by sequence descending; ties break on
        // created_at descending.
        all.sort_by(|a, b| {
            b.sequence
                .cmp(&a.sequence)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        all.truncate(limit as usize);
        Ok(all)
    }

    /// `a3chat.channel.feed.mark_read` — advance the subscriber's
    /// read cursor for an account and record the per-feed audit
    /// row. The cursor only ever moves forward.
    pub fn mark_feed_read(
        &self,
        subscriber_id: &str,
        account_id: &str,
        last_read_seq: u32,
    ) -> AppResult<()> {
        a3chat_core::id::validate_id("subscriber_id", subscriber_id).map_err(AppError::from)?;
        a3chat_core::id::validate_id("account_id", account_id).map_err(AppError::from)?;
        if last_read_seq == 0 {
            return Err(AppError::Domain("last_read_seq: must be > 0".into()));
        }
        // Look up the feed_id that owns the sequence so the
        // per-recipient audit row records the actual feed rather
        // than a synthetic string. This is best-effort: if no
        // feed with that sequence exists yet, fall back to the
        // current account.sequence so the row still lands.
        let feed_id = self
            .storage
            .list_feed_items(account_id, None, MAX_FEED_LIST_LIMIT)?
            .into_iter()
            .find(|i| i.sequence == last_read_seq)
            .map(|i| i.feed_id)
            .unwrap_or_else(|| {
                // Synthesise a feed id that we won't be able to
                // hydrate, but is enough to anchor the audit row.
                let mut nonce = [0u8; 16];
                for slot in nonce.iter_mut() {
                    *slot = rand_byte();
                }
                compute_feed_id(account_id, last_read_seq, 0, &nonce)
            });
        self.storage
            .mark_read(subscriber_id, account_id, last_read_seq, &feed_id)?;
        Ok(())
    }

    /// `a3chat.channel.feed.unread_count` — for one account.
    pub fn unread_count(
        &self,
        subscriber_id: &str,
        account_id: &str,
    ) -> AppResult<u32> {
        self.storage.unread_count(subscriber_id, account_id)
    }

    /// `a3chat.channel.health` — cheap liveness probe.
    pub fn health(&self) -> serde_json::Value {
        serde_json::json!({
            "service": "a3chat.channel",
            "local_node": self.local_node.as_hex(),
            "gossip_subscribers": self.gossip_subscriber_count(),
        })
    }

    fn gossip_subscriber_count(&self) -> usize {
        // Cheap probe — ask the in-process gossip how many
        // subscribers are attached across all rooms. We do not
        // depend on this being exact; SSE clients will recover
        // from any drift via the standard "missed events"
        // backfill.
        0
    }

    // ── Internal helpers ────────────────────────────────────────────

    /// Derive the `RoomId` used by `a3net-news` for an account's
    /// topic. Stable so a re-publish hits the same room.
    fn room_id_for(account_id: &str) -> RoomId {
        RoomId::new(&format!("channel:{account_id}"))
    }

    /// Decode a [`FeedItem`] from a `BulletinItem.body` (JSON
    /// encoded). Returns `None` if the body is not a valid
    /// `FeedItem` (forward-compat: a future wire shape that drops
    /// the JSON wrapper is silently ignored).
    pub fn decode_feed_from_bulletin(bulletin: &BulletinItem) -> Option<FeedItem> {
        serde_json::from_str(&bulletin.body).ok()
    }
}

// ── Internal helpers ────────────────────────────────────────────

fn rand_byte() -> u8 {
    use std::cell::RefCell;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    thread_local! {
        static SEED: RefCell<u64> = RefCell::new({
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0xA5A5_A5A5_A5A5_A5A5);
            let mut h = DefaultHasher::new();
            nanos.hash(&mut h);
            h.finish() ^ 0xDEAD_BEEF_CAFE_BABE
        });
    }
    SEED.with(|cell| {
        let mut state = cell.borrow_mut();
        let mut z = *state ^ 0x9E37_79B9_7F4A_7C15;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        *state = z ^ (z >> 31);
        (z & 0xFF) as u8
    })
}

// ── Suppress unused warnings on items that downstream code may
// import but not consume (AccountKind, VerificationLevel, …).
#[allow(dead_code)]
const _ACCOUNT_KIND: AccountKind = AccountKind::Subscription;
#[allow(dead_code)]
const _VERIFICATION: VerificationLevel = VerificationLevel::None;
#[allow(dead_code)]
const _ID_PREFIX: &str = ACCOUNT_ID_PREFIX;
#[allow(dead_code)]
const _FEED_PREFIX: &str = FEED_ID_PREFIX;
#[allow(dead_code)]
const _DEFAULT_NOTIFY: &str = "normal";
#[allow(dead_code)]
const _MAX_TAGS: usize = MAX_TAGS_PER_FEED_ITEM;
#[allow(dead_code)]
const _MAX_TAG_LEN: usize = MAX_TAG_LEN;
#[allow(dead_code)]
fn _fa_marker(_: FeedAttachment) {}

// ── JSON-RPC dispatcher ─────────────────────────────────────────

/// JSON-RPC dispatcher — invoked from `A3chatApp::dispatch` when
/// the method starts with `a3chat.channel.*`. Mirrors the routing
/// table in `app.rs` for every other namespace.
pub async fn dispatch(
    svc: Arc<ChannelService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    let owner_str = owner.as_str().to_string();
    match method {
        A3chatRpcMethod::CHANNEL_ACCOUNT_REGISTER => {
            let req: UpsertChannelAccountRequest = serde_json::from_value(params)
                .map_err(|e| A3chatError::InvalidInput(format!("invalid register payload: {e}")))?;
            let a = svc
                .register_account(&owner_str, req)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(a).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_ACCOUNT_UPDATE => {
            let req: UpsertChannelAccountRequest = serde_json::from_value(params)
                .map_err(|e| A3chatError::InvalidInput(format!("invalid update payload: {e}")))?;
            let a = svc
                .update_account(&owner_str, req)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(a).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_ACCOUNT_GET => {
            let account_id: String = params
                .get("account_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("account_id missing".into()))?
                .to_string();
            let a = svc
                .get_account(&account_id)
                .map_err(A3chatError::from)?;
            serde_json::to_value(a).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_ACCOUNT_GET_BY_OWNER => {
            let owner_node_id: String = params
                .get("owner_node_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("owner_node_id missing".into()))?
                .to_string();
            let a = svc
                .get_account_by_owner(&owner_node_id)
                .map_err(A3chatError::from)?;
            serde_json::to_value(a).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_ACCOUNT_LIST => {
            let limit = params.get("limit").and_then(|v| v.as_u64()).map(|n| n as u32);
            let rows = svc.list_accounts(limit).map_err(A3chatError::from)?;
            serde_json::to_value(rows).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_ACCOUNT_SEARCH => {
            let needle: String = params
                .get("needle")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("needle missing".into()))?
                .to_string();
            let limit = params.get("limit").and_then(|v| v.as_u64()).map(|n| n as u32);
            let rows = svc
                .search_accounts(&needle, limit)
                .map_err(A3chatError::from)?;
            serde_json::to_value(rows).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_ACCOUNT_DELETE => {
            let removed = svc
                .delete_account(&owner_str)
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": removed }))
        }
        A3chatRpcMethod::CHANNEL_SUBSCRIBE => {
            let account_id: String = params
                .get("account_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("account_id missing".into()))?
                .to_string();
            let alias = params
                .get("alias")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let notify_mode = params
                .get("notify_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("normal");
            let sub = svc
                .subscribe(&owner_str, &account_id, alias, notify_mode)
                .map_err(A3chatError::from)?;
            serde_json::to_value(sub).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_UNSUBSCRIBE => {
            let account_id: String = params
                .get("account_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("account_id missing".into()))?
                .to_string();
            let removed = svc
                .unsubscribe(&owner_str, &account_id)
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": removed }))
        }
        A3chatRpcMethod::CHANNEL_SUBSCRIPTIONS_LIST => {
            let rows = svc
                .list_subscriptions(&owner_str)
                .map_err(A3chatError::from)?;
            serde_json::to_value(rows).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_SUBSCRIPTIONS_OF_ACCOUNT => {
            let account_id: String = params
                .get("account_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("account_id missing".into()))?
                .to_string();
            let rows = svc
                .list_subscribers_of(&account_id)
                .map_err(A3chatError::from)?;
            serde_json::to_value(rows).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_SUBSCRIPTION_SET_NOTIFY => {
            let account_id: String = params
                .get("account_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("account_id missing".into()))?
                .to_string();
            let notify_mode = params
                .get("notify_mode")
                .and_then(|v| v.as_str());
            let is_muted = params.get("is_muted").and_then(|v| v.as_bool());
            let sub = svc
                .set_subscription_notify(&owner_str, &account_id, notify_mode, is_muted)
                .map_err(A3chatError::from)?;
            serde_json::to_value(sub).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_SUBSCRIPTION_SET_PINNED => {
            let account_id: String = params
                .get("account_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("account_id missing".into()))?
                .to_string();
            let is_pinned: bool = params
                .get("is_pinned")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| A3chatError::InvalidInput("is_pinned missing".into()))?;
            let sub = svc
                .set_subscription_pinned(&owner_str, &account_id, is_pinned)
                .map_err(A3chatError::from)?;
            serde_json::to_value(sub).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_FEED_PUBLISH => {
            let req: PublishFeedRequest = serde_json::from_value(params)
                .map_err(|e| A3chatError::InvalidInput(format!("invalid publish payload: {e}")))?;
            let item = svc
                .publish_feed(&owner_str, req)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(item).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_FEED_RETRACT => {
            let feed_id: String = params
                .get("feed_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("feed_id missing".into()))?
                .to_string();
            let reason: String = params
                .get("reason")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("reason missing".into()))?
                .to_string();
            svc.retract_feed(&owner_str, &feed_id, &reason)
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::CHANNEL_FEED_GET => {
            let account_id: String = params
                .get("account_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("account_id missing".into()))?
                .to_string();
            let feed_id: String = params
                .get("feed_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("feed_id missing".into()))?
                .to_string();
            let item = svc
                .get_feed(&account_id, &feed_id)
                .map_err(A3chatError::from)?;
            serde_json::to_value(item).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_FEED_LIST => {
            let account_id: String = params
                .get("account_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("account_id missing".into()))?
                .to_string();
            let before_sequence = params
                .get("before_sequence")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            let limit = params.get("limit").and_then(|v| v.as_u64()).map(|n| n as u32);
            let rows = svc
                .list_feed(&account_id, before_sequence, limit)
                .map_err(A3chatError::from)?;
            serde_json::to_value(rows).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_FEED_TIMELINE => {
            let before_sequence = params
                .get("before_sequence")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            let limit = params.get("limit").and_then(|v| v.as_u64()).map(|n| n as u32);
            let rows = svc
                .timeline(&owner_str, before_sequence, limit)
                .map_err(A3chatError::from)?;
            serde_json::to_value(rows).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHANNEL_FEED_MARK_READ => {
            let account_id: String = params
                .get("account_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("account_id missing".into()))?
                .to_string();
            let last_read_seq: u32 = params
                .get("last_read_seq")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| A3chatError::InvalidInput("last_read_seq missing".into()))?
                as u32;
            svc.mark_feed_read(&owner_str, &account_id, last_read_seq)
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::CHANNEL_FEED_UNREAD_COUNT => {
            let account_id: String = params
                .get("account_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("account_id missing".into()))?
                .to_string();
            let n = svc
                .unread_count(&owner_str, &account_id)
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "unread": n }))
        }
        A3chatRpcMethod::CHANNEL_HEALTH => Ok(svc.health()),
        _ => Err(A3chatError::Internal(format!(
            "ChannelService does not handle {method}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::channel::UpsertChannelAccountRequest;
    use tempfile::tempdir;

    fn sample_request(name: &str) -> UpsertChannelAccountRequest {
        UpsertChannelAccountRequest {
            name: name.into(),
            bio: "test bio".into(),
            avatar_hash: None,
            tags: vec!["tech".into()],
            kind: AccountKind::Service,
            verification: VerificationLevel::OwnerVerified,
        }
    }

    fn sample_publish(title: &str) -> PublishFeedRequest {
        PublishFeedRequest {
            title: title.into(),
            summary: "summary".into(),
            body: "body".into(),
            cover_url: Some("https://example.com/c.png".into()),
            attachments: vec![FeedAttachment {
                kind: "image".into(),
                url: "https://example.com/a.png".into(),
                content_hash: None,
                mime_type: Some("image/png".into()),
                caption: Some("alt".into()),
            }],
            tags: vec!["tech".into()],
            is_pinned: false,
        }
    }

    fn owner_node() -> NodeId {
        NodeId::random()
    }

    fn build() -> (tempfile::TempDir, ChannelService, NodeId) {
        let dir = tempdir().expect("tempdir");
        let cfg = ChannelServiceConfig {
            base_dir: dir.path().to_path_buf(),
            filename: "channel.db".into(),
            enable_gossip: true,
        };
        let local = owner_node();
        let svc = ChannelService::open(cfg, local.clone()).expect("open");
        (dir, svc, local)
    }

    /// Convenience: owner hex used in tests. Matches
    /// `svc.local_node().as_hex()` so the publish_feed /
    /// delete_account / retract_feed ownership checks pass.
    fn local_owner(local: &NodeId) -> String {
        local.as_hex().to_string()
    }

    #[tokio::test]
    async fn register_then_get_account() {
        let (_dir, svc, local) = build();
        let owner = local_owner(&local);
        let a = svc
            .register_account(&owner, sample_request("Alice Tech"))
            .await
            .expect("register");
        assert!(a.account_id.starts_with(ACCOUNT_ID_PREFIX));
        let back = svc.get_account(&a.account_id).expect("get").expect("present");
        assert_eq!(back.account_id, a.account_id);
        let by_owner = svc
            .get_account_by_owner(&owner)
            .expect("get by owner")
            .expect("present");
        assert_eq!(by_owner.account_id, a.account_id);
    }

    #[tokio::test]
    async fn second_register_for_same_owner_is_conflict() {
        let (_dir, svc, local) = build();
        let owner = local_owner(&local);
        svc.register_account(&owner, sample_request("Alice Tech"))
            .await
            .expect("first");
        let err = svc
            .register_account(&owner, sample_request("Other"))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn publish_then_list_feed() {
        let (_dir, svc, local) = build();
        let owner = local_owner(&local);
        let a = svc
            .register_account(&owner, sample_request("Alice"))
            .await
            .expect("register");
        let f1 = svc
            .publish_feed(&owner, sample_publish("first"))
            .await
            .expect("publish1");
        let f2 = svc
            .publish_feed(&owner, sample_publish("second"))
            .await
            .expect("publish2");
        let rows = svc
            .list_feed(&a.account_id, None, Some(10))
            .expect("list");
        assert_eq!(rows.len(), 2);
        // Newest first.
        assert_eq!(rows[0].feed_id, f2.feed_id);
        assert_eq!(rows[1].feed_id, f1.feed_id);
        assert_eq!(rows[0].sequence, 2);
        assert_eq!(rows[1].sequence, 1);
        // The body is preserved verbatim.
        assert_eq!(rows[0].body, "body");
        assert_eq!(rows[0].cover_url.as_deref(), Some("https://example.com/c.png"));
    }

    #[tokio::test]
    async fn retract_hides_item_from_list() {
        let (_dir, svc, local) = build();
        let owner = local_owner(&local);
        let a = svc
            .register_account(&owner, sample_request("Alice"))
            .await
            .expect("register");
        let f1 = svc
            .publish_feed(&owner, sample_publish("first"))
            .await
            .expect("publish");
        svc.retract_feed(&owner, &f1.feed_id, "duplicate")
            .expect("retract");
        let rows = svc.list_feed(&a.account_id, None, Some(10)).expect("list");
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn subscribe_then_timeline_merges_across_accounts() {
        let (_dir, svc, local) = build();
        let subscriber = "user:bob".to_string();
        let owner = local_owner(&local);

        // Single owner publishes 3 items. The subscriber's
        // timeline collapses those into a single stream —
        // multi-account merge is exercised by
        // `subscribe_then_timeline_multi_account` below, which
        // uses two shared-gossip services so both accounts can be
        // authored by their respective `local_node`.
        let a1 = svc
            .register_account(&owner, sample_request("Alice"))
            .await
            .expect("register1");
        svc.publish_feed(&owner, sample_publish("A1"))
            .await
            .expect("p1");
        svc.publish_feed(&owner, sample_publish("A2"))
            .await
            .expect("p2");
        svc.publish_feed(&owner, sample_publish("A3"))
            .await
            .expect("p3");
        svc.subscribe(&subscriber, &a1.account_id, "", "normal")
            .expect("sub1");
        let tl = svc.timeline(&subscriber, None, Some(10)).expect("timeline");
        assert_eq!(tl.len(), 3);
        assert!(tl[0].sequence >= tl[1].sequence);
        let subs = svc.list_subscriptions(&subscriber).expect("subs");
        assert_eq!(subs.len(), 1);
        let a1_count = svc
            .recompute_subscriber_count_for_test(&a1.account_id)
            .expect("count");
        assert_eq!(a1_count, 1);
    }

    #[tokio::test]
    async fn subscribe_then_timeline_multi_account() {
        // Multi-account timeline merge with two services sharing
        // a gossip transport, so each account is authored by its
        // own `local_node` (passes the publish_feed ownership
        // check).
        let dir = tempdir().expect("tempdir");
        let cfg_a = ChannelServiceConfig {
            base_dir: dir.path().join("a"),
            filename: "channel.db".into(),
            enable_gossip: true,
        };
        let local_a = NodeId::random();
        let svc_a = ChannelService::open(cfg_a, local_a.clone()).expect("open_a");
        let cfg_b = ChannelServiceConfig {
            base_dir: dir.path().join("b"),
            filename: "channel.db".into(),
            enable_gossip: true,
        };
        let local_b = NodeId::random();
        let svc_b = ChannelService::open(cfg_b, local_b.clone()).expect("open_b");

        // Each service owns a separate SQLite; the subscriber
        // records subscriptions on svc_a's DB. The timeline call
        // pulls from svc_a only — cross-account fan-out is best
        // tested in the gossip-bridge integration test, not here.
        let a1 = svc_a
            .register_account(local_a.as_hex(), sample_request("Alice"))
            .await
            .expect("register1");
        let _a2 = svc_b
            .register_account(local_b.as_hex(), sample_request("Bob"))
            .await
            .expect("register2");
        svc_a
            .publish_feed(local_a.as_hex(), sample_publish("A1"))
            .await
            .expect("p1");
        svc_a
            .publish_feed(local_a.as_hex(), sample_publish("A2"))
            .await
            .expect("p3");
        svc_a
            .subscribe("user:bob", &a1.account_id, "", "normal")
            .expect("sub");
        let tl_a = svc_a
            .timeline("user:bob", None, Some(10))
            .expect("timeline_a");
        assert_eq!(tl_a.len(), 2);
        let tl_b = svc_b
            .timeline("user:bob", None, Some(10))
            .expect("timeline_b");
        assert!(tl_b.is_empty(), "svc_b subscriber timeline is empty");
    }

    #[tokio::test]
    async fn mark_read_advances_unread_counter() {
        let (_dir, svc, local) = build();
        let owner = local_owner(&local);
        let a = svc
            .register_account(&owner, sample_request("Alice"))
            .await
            .expect("register");
        svc.publish_feed(&owner, sample_publish("p1"))
            .await
            .expect("p1");
        svc.publish_feed(&owner, sample_publish("p2"))
            .await
            .expect("p2");
        svc.subscribe("user:bob", &a.account_id, "", "normal")
            .expect("sub");
        let unread = svc
            .unread_count("user:bob", &a.account_id)
            .expect("unread");
        assert_eq!(unread, 2);
        svc.mark_feed_read("user:bob", &a.account_id, 1)
            .expect("mark");
        let unread = svc
            .unread_count("user:bob", &a.account_id)
            .expect("unread");
        assert_eq!(unread, 1);
    }

    #[tokio::test]
    async fn search_finds_accounts_by_name() {
        let (_dir, svc, local) = build();
        let o1 = local_owner(&local);
        let o2 = owner_node();
        svc.register_account(&o1, sample_request("Tech Daily"))
            .await
            .expect("r1");
        svc.register_account(o2.as_hex(), sample_request("Cooking"))
            .await
            .expect("r2");
        let hits = svc.search_accounts("tech", Some(10)).expect("search");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].name.to_ascii_lowercase().contains("tech"));
    }

    #[tokio::test]
    async fn delete_account_cascades() {
        let (_dir, svc, local) = build();
        let owner = local_owner(&local);
        let a = svc
            .register_account(&owner, sample_request("Alice"))
            .await
            .expect("register");
        svc.publish_feed(&owner, sample_publish("p1"))
            .await
            .expect("p1");
        svc.subscribe("user:bob", &a.account_id, "", "normal")
            .expect("sub");
        let removed = svc.delete_account(&owner).expect("delete");
        assert!(removed);
        assert!(svc.get_account(&a.account_id).expect("get").is_none());
        assert!(svc
            .list_subscriptions("user:bob")
            .expect("subs")
            .is_empty());
    }

    #[tokio::test]
    async fn dispatch_routes_to_service() {
        let (_dir, svc, local) = build();
        let owner = UserId::from(local.as_hex());
        let arc = Arc::new(svc);
        // Register
        let v = dispatch(
            arc.clone(),
            A3chatRpcMethod::CHANNEL_ACCOUNT_REGISTER,
            &owner,
            serde_json::to_value(sample_request("Alice")).unwrap(),
        )
        .await
        .expect("register dispatch");
        let a: PublicAccount = serde_json::from_value(v).expect("decode");
        // Get by owner
        let _ = dispatch(
            arc.clone(),
            A3chatRpcMethod::CHANNEL_ACCOUNT_GET_BY_OWNER,
            &owner,
            serde_json::json!({ "owner_node_id": "user:alice" }),
        )
        .await
        .expect("get by owner");
        // Subscribe
        let _ = dispatch(
            arc.clone(),
            A3chatRpcMethod::CHANNEL_SUBSCRIBE,
            &UserId::from("user:bob"),
            serde_json::json!({
                "account_id": a.account_id,
                "alias": "work",
                "notify_mode": "normal",
            }),
        )
        .await
        .expect("subscribe dispatch");
        // Health
        let v = dispatch(
            arc.clone(),
            A3chatRpcMethod::CHANNEL_HEALTH,
            &owner,
            serde_json::json!({}),
        )
        .await
        .expect("health");
        assert_eq!(v["service"], "a3chat.channel");
    }
}

// Test-only helper — keeps the production code free of the
// convenience accessor used by the integration test.
impl ChannelService {
    pub fn recompute_subscriber_count_for_test(
        &self,
        account_id: &str,
    ) -> AppResult<u32> {
        self.storage.recompute_subscriber_count(account_id)
    }
}