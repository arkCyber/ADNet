//! `A3chatApp` — wires every service together. Use this from
//! `a3chat-rpc` (or directly from tests) to dispatch any
//! `a3chat.*` JSON-RPC method.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use a3chat_core::error::A3chatError;
use a3chat_core::id::{ConversationId, UserId};

use crate::channel_service::{self as channel_service_mod, ChannelService};
use crate::chat_reaction_service::{self as reaction_service, ChatReactionService};
use crate::chat_service::{self, ChatService};
use crate::contact_service::{self, ContactService};
use crate::device_service::{self as device_service_mod, DeviceService};
use crate::draft_service::{self as draft_service_mod, DraftService};
use crate::e2e_bundle::{self as e2e_bundle_service, E2eBundleService};
use crate::e2e_encryption_service::{self as e2e_encryption_service_mod, E2eEncryptionService};
use crate::error::{AppError, AppResult};
use crate::forward_service::{self as forward_service_mod, ForwardService};
use crate::group_service::{self, GroupService};
use crate::group_invitation_service::GroupInvitationService;
#[cfg(feature = "iroh")]
use crate::group_sync_service::{self as group_sync_service_mod, GroupSyncService};
use crate::keyring::E2eKeyring;
use crate::link_bookmark_service::{self as link_bookmark_service, LinkBookmarkService};
use crate::media_service::{self, MediaConfig, MediaService};
use crate::moderation_service::{self, ModerationConfig, ModerationService};
use crate::moments_service::{self, MomentsConfig, MomentsService};
use crate::notification_bus::NotificationBus;
use crate::notification_settings_service::{self as notification_settings_service_mod, NotificationSettingsService};
use crate::pairing_service::{self as pairing_service_mod, PairingService, PairingServiceConfig};
use crate::peer_feedback_service::{self, PeerFeedbackService};
use crate::pinned_service::{self as pinned_service_mod, PinnedService};
use crate::presence_service::{self, PresenceService};
use crate::profile_service::{self, ProfileConfig, ProfileService};
use crate::storage::{ChatStorage, StorageConfig};
use crate::stream_service::{self as stream_service_mod, StreamService};
use crate::sync_service::{self, SyncService};

/// Wrapper for the iroh-docs bridge so the rest of the API surface
/// does not need to compile `a3net-chatstore` (which is an
/// `iroh` feature). Always present; the `with_iroh_docs_chat`
/// hook is no-op when the feature is off.
#[derive(Clone)]
pub struct A3chatAppBridge {
    #[cfg(feature = "iroh")]
    inner: std::sync::Arc<a3net_chatstore::IrohDocsChat>,
}

impl std::fmt::Debug for A3chatAppBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("A3chatAppBridge").finish()
    }
}

/// Public wrapper to convert a strongly-typed iroh-docs bridge into
/// the `A3chatApp`'s stable wrapper. This keeps the lean build
/// (no `iroh` feature) compiling while still letting the
/// `enable-iroh` build of `a3chat-cli` wire the bridge through.
#[cfg(feature = "iroh")]
pub fn bridge_for_iroh_docs(bridge: a3net_chatstore::IrohDocsChat) -> A3chatAppBridge {
    A3chatAppBridge {
        inner: std::sync::Arc::new(bridge),
    }
}

#[cfg(not(feature = "iroh"))]
pub fn bridge_for_iroh_docs(_bridge: ()) -> A3chatAppBridge {
    A3chatAppBridge {}
}

fn empty_test_store_arc() -> std::sync::Arc<dyn a3net_userstore::store::UserStore> {
    struct Empty;
    impl a3net_userstore::store::UserStore for Empty {
        fn put_profile(
            &self,
            _: a3net_userstore::model::UserProfile,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        fn get_profile(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<Option<a3net_userstore::model::UserProfile>>
        {
            Ok(None)
        }
        fn put_preferences(
            &self,
            _: &str,
            _: a3net_userstore::model::UserPreferences,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        fn list_profiles(
            &self,
        ) -> a3net_userstore::error::UserStoreResult<Vec<a3net_userstore::model::UserProfile>>
        {
            Ok(vec![])
        }
        fn delete_profile(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<usize> {
            Ok(0)
        }
        fn put_public_key(
            &self,
            _: a3net_userstore::model::UserPublicKey,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        fn revoke_public_key(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        fn list_public_keys(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<Vec<a3net_userstore::model::UserPublicKey>>
        {
            Ok(vec![])
        }
        fn set_public_key_label(
            &self,
            _: &str,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        fn set_kind(
            &self,
            _: &str,
            _: a3net_userstore::model::UserKind,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        fn get_kind(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<a3net_userstore::model::UserKind> {
            Ok(a3net_userstore::model::UserKind::Human)
        }
        fn put_device(
            &self,
            _: a3net_userstore::model::UserDevice,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        fn revoke_device(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        fn list_devices(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<Vec<a3net_userstore::model::UserDevice>>
        {
            Ok(vec![])
        }
        fn ensure_user_digit(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<String> {
            Ok("000000000000".into())
        }
        fn resolve_user_digit(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<Option<String>> {
            Ok(None)
        }
    }
    std::sync::Arc::new(Empty)
}

/// The full app. Each service is independently cloneable; cloning
/// the `A3chatApp` itself clones every inner Arc.
#[derive(Clone)]
pub struct A3chatApp {
    pub chat: ChatService,
    pub contact: ContactService,
    pub group: Arc<GroupService>,
    pub sync: SyncService,
    pub presence: PresenceService,
    /// User-profile layer backed by `a3net-userstore`.
    /// Exposes the `a3chat.profile.*` RPC namespace.
    pub profile: ProfileService,
    /// Media (attachment) layer backed by `a3net-blobstore`.
    /// Exposes the `a3chat.media.*` RPC namespace.
    pub media: MediaService,
    /// Content moderation layer backed by `a3net-moderation`.
    /// Exposes the `a3chat.moderation.*` RPC namespace.
    pub moderation: ModerationService,
    /// Peer-feedback (trust + reports) layer backed by
    /// `a3net-reputation`. Exposes the `a3chat.peerfeedback.*`
    /// RPC namespace.
    pub peerfeedback: PeerFeedbackService,
    /// Moments / 朋友圈 (F-05) service backed by `a3net-socialfeed`.
    /// Exposes the `a3chat.moments.*` RPC namespace and publishes
    /// events onto [`A3chatApp::bus`] (which `a3chat-rpc` then
    /// bridges onto SSE).
    pub moments: MomentsService,
    /// Link bookmark / favorites (F-06) service. Shares the same
    /// [`ChatStorage`] as the rest of the app so bookmark data lives
    /// in the same SQLite file. Exposes the `a3chat.link.bookmark.*`
    /// RPC namespace.
    pub link: LinkBookmarkService,
    /// F-09 / Channel / public-account (公众号) service.
    /// Backed by `a3net-news::NewsService` for gossip fan-out and
    /// by [`crate::channel_storage::ChannelStorage`] for the
    /// `account_id` / `feed_id` / `subscription` SQLite tables.
    /// Exposes the `a3chat.channel.*` RPC namespace and publishes
    /// events onto [`A3chatApp::bus`] for SSE subscribers.
    pub channel: ChannelService,
    /// F-07: Per-conversation draft persistence.
    /// Exposes the `a3chat.chat.draft.*` RPC namespace.
    pub draft: DraftService,
    /// F-07: Message reactions (emoji replies).
    /// Exposes the `a3chat.chat.reaction.*` RPC namespace.
    pub reaction: ChatReactionService,
    /// F-07: Per-user multi-device management.
    /// Exposes the `a3chat.device.*` RPC namespace.
    pub device: DeviceService,
    /// F-07: Notification settings (DND + per-conversation overrides).
    /// Exposes the `a3chat.chat.notification.*` RPC namespace.
    pub notification_settings: NotificationSettingsService,
    /// F-07: Pinned conversations service.
    /// Exposes the `a3chat.chat.conversation.{pin,unpin,toggle,list_pinned}` RPC namespace.
    pub pinned: PinnedService,
    /// F-07: Message forwarding service.
    /// Exposes the `a3chat.chat.message.forward` RPC namespace.
    pub forward: ForwardService,
    /// F-07: E2E encryption service (handshake status helpers).
    /// Exposes the `a3chat.e2e.handshake.*` RPC namespace.
    pub e2e_encryption: E2eEncryptionService,
    /// F-07: E2E bundle export/import service.
    /// Exposes the `a3chat.e2e.bundle.*` RPC namespace.
    pub e2e_bundle: E2eBundleService,
    /// F-07: SSE stream subscription service.
    /// Exposes the `a3chat.stream.*` RPC namespace.
    pub stream: StreamService,
    /// F-08 / B-24: group invitation store. Shares the same
    /// [`ChatStorage`] so invitation rows live in the same SQLite
    /// file as the chat data, and exposes the
    /// `a3chat.group.invite*` RPC namespace.
    pub invitations: GroupInvitationService,
    /// P2P device-pairing service backed by `a3net-pairing`.
    /// Exposes the `a3chat.pairing.*` RPC namespace. Wrapped in a
    /// `Mutex` so `with_pairing` can install / replace the service
    /// without requiring `&mut self`. Stays `None` until the
    /// operator calls [`A3chatApp::with_pairing`] during bootstrap;
    /// pairing RPCs in that mode return a clear
    /// `PairingService-not-configured` error.
    pub pairing: std::sync::Arc<std::sync::Mutex<Option<PairingService>>>,
    pub bus: NotificationBus,
    /// Local device identity. Set in [`A3chatApp::new`] and used
    /// to derive the keyring, channel node ID, and as the `owner`
    /// argument when initialising services.
    owner: UserId,
    pub keyring: E2eKeyring,
    /// Unix timestamp at which this app started serving requests.
    /// Used by [`A3chatApp::dispatch`] to answer `a3chat.healthz`.
    bus_start_unix: Arc<AtomicI64>,
    /// Phase 5c: iroh-docs distributed message store.
    /// Initialise via [`A3chatApp::with_iroh_docs_chat`] after
    /// construction. When `Some`, every outbound message (DM or
    /// group) is dual-written: SQLite first (authoritative), then
    /// iroh-docs (best-effort fan-out).
    #[cfg(feature = "iroh")]
    pub iroh_docs_chat: Option<Arc<a3net_chatstore::IrohDocsChat>>,
    /// Phase 5c: iroh-docs P2P group sync service.
    /// Manages per-group sync subscriptions, backfills messages from iroh-docs
    /// into SQLite, and notifies SSE subscribers. Initialise via
    /// [`A3chatApp::with_group_sync_service`] after construction.
    #[cfg(feature = "iroh")]
    pub group_sync: Option<GroupSyncService>,
}

impl A3chatApp {
    /// Build a new `A3chatApp` from a storage config + owner id.
    /// All services share the same [`ChatStorage`], [`E2eKeyring`],
    /// and [`NotificationBus`].
    pub fn new(config: StorageConfig, owner: UserId) -> AppResult<Self> {
        let keyring = E2eKeyring::new(owner.clone());
        let storage = ChatStorage::new(config, keyring.clone());
        let bus = NotificationBus::new(NotificationBus::default_capacity());
        // Profile store lives alongside the chat store; if the
        // directory creation fails, we surface it as a
        // Storage error so the operator notices the broken
        // a3net-userstore bridge.
        let profile_cfg = ProfileConfig::under_base(&storage.config().base_dir);
        let profile = ProfileService::open(&profile_cfg)?;
        // Media store lives under `<base>/media` and is backed by
        // `a3net-blobstore::BlobStore` (BLAKE3 content-addressed).
        let media_cfg = MediaConfig::under_base(&storage.config().base_dir);
        let media = MediaService::open(&media_cfg)?;
        // Moderation policy lives under `<base>/moderation` and is
        // backed by `a3net-moderation::Blocklist` (persistent JSON).
        let moderation_cfg = ModerationConfig::under_base(&storage.config().base_dir);
        let moderation = ModerationService::open(&moderation_cfg)?;
        // Peer-feedback service shares the same `ChatStorage` so
        // its trust writes hit the same `chat_trust` table as the
        // rest of the chat store. No reporter is attached by
        // default — callers (production bootstrap) must invoke
        // [`A3chatApp::with_reputation`] to wire one in.
        let peerfeedback = PeerFeedbackService::new(storage.clone());
        // Moments / 朋友圈 (F-05). SQLite-backed via
        // `a3net-socialfeed`; shares the chat-wide
        // [`NotificationBus`] so SSE subscribers see the
        // `moments.*` events next to chat/contact events; and
        // routes every post/comment body through the chat
        // moderation blocklist (mirrors `ChatService`).
        let moments_cfg = MomentsConfig::under_base(&storage.config().base_dir);
        let moments = MomentsService::open_with_bus(&moments_cfg, bus.clone())?
            .with_moderation(moderation.clone());
        // Link bookmark / favorites (F-06). Re-uses the same
        // [`ChatStorage`] so bookmarks land in the same SQLite file
        // as chat messages and contact data.
        let link_cfg = link_bookmark_service::LinkBookmarkConfig::under_base(&storage.config().base_dir);
        let link = LinkBookmarkService::new(storage.clone(), bus.clone(), link_cfg);
        // Channel / 公众号 (F-09). Owns its own SQLite file under
        // `<base>/channel/channel.db`. Backed by
        // `a3net-news::NewsService` for gossip fan-out. The local
        // node id is parsed from `owner` when it's a 64-char hex
        // string, otherwise we fall back to a random id so the
        // service still constructs in test environments that pass
        // user-named owners.
        let channel_local_node = a3net_types::NodeId::from_hex(owner.as_str())
            .unwrap_or_else(|_| a3net_types::NodeId::random());
        let channel_cfg = crate::channel_service::ChannelServiceConfig {
            base_dir: storage.config().base_dir.clone(),
            filename: "channel.db".into(),
            enable_gossip: true,
        };
        let channel = crate::channel_service::ChannelService::open(channel_cfg, channel_local_node)
            .expect("channel opens");
        // Contact roster lives under `<base>/contacts` backed by a3net-roster SQLite.
        let contact_cfg = contact_service::ContactServiceConfig::under_base(&storage.config().base_dir);
        // F-07: per-conversation message drafts (now persisted via ChatStorage).
        let draft = DraftService::with_storage(Arc::new(storage.clone()));
        // F-07: message reactions (emoji replies).
        let reaction = ChatReactionService::new(bus.clone());
        // F-07: per-user multi-device management.
        let device = DeviceService::new_with_bus(bus.clone());
        // F-07: notification settings (DND + per-conversation overrides).
        let notification_settings = NotificationSettingsService::new(bus.clone());
        // F-07: pinned conversations.
        let pinned = PinnedService::new(storage.clone(), bus.clone());
        // F-07: message forwarding.
        let forward = ForwardService::new(storage.clone(), bus.clone());
        // F-07: E2E encryption handshake helpers.
        let e2e_encryption = E2eEncryptionService::new(keyring.clone());
        // F-07: E2E bundle export/import.
        let e2e_bundle = E2eBundleService::new(storage.clone(), keyring.clone());
        // F-07: SSE stream subscription registry.
        let stream = StreamService::new();
        // F-08 / B-24: group invitation store, sharing the chat
        // SQLite for transactional consistency with chat writes.
        let invitations = GroupInvitationService::with_storage(Arc::new(storage.clone()));
        // Capture the start time once so `a3chat.healthz` can compute
        // uptime without re-allocating the clock.
        let bus_start_unix = Arc::new(AtomicI64::new(chrono::Utc::now().timestamp()));
        Ok(Self {
            chat: ChatService::new(storage.clone(), bus.clone()).with_moderation(moderation.clone()),
            contact: ContactService::new(contact_cfg, owner.clone()),
            group: Arc::new(GroupService::new(bus.clone())),
            sync: SyncService::new(storage.clone()),
            presence: PresenceService::new(storage.clone(), bus.clone()),
            profile,
            media,
            moderation,
            peerfeedback,
            moments,
            link,
            channel,
            draft,
            reaction,
            device,
            notification_settings,
            pinned,
            forward,
            e2e_encryption,
            e2e_bundle,
            stream,
            invitations,
            // Pairing service requires a wallet secret + a NodeId
            // which `A3chatApp::new` does not know about. Production
            // builds must call [`A3chatApp::with_pairing`] after
            // constructing the app. Until then pairing RPCs return
            // a `NotImplemented` error so callers see a clear
            // diagnostic rather than a silent 500.
            pairing: std::sync::Arc::new(std::sync::Mutex::new(None)),
            bus,
            keyring,
            owner: owner.clone(),
            bus_start_unix,
            #[cfg(feature = "iroh")]
            iroh_docs_chat: None,
            #[cfg(feature = "iroh")]
            group_sync: None,
        })
    }

/// Install a [`a3net_reputation::ReputationReporter`] so the
    /// peer-feedback service can (a) emit `ChatTrustReport` events
    /// from `file_report` and (b) drive `fused_score` from the
    /// global PeerScore. Idempotent — calling twice with the same
    /// reporter is a no-op.
    pub async fn with_reputation(
        &self,
        reporter: a3net_reputation::ReputationReporter,
    ) -> &Self {
        self.peerfeedback.with_reporter(reporter).await;
        self
    }

    /// Install the P2P pairing service backed by `a3net-pairing`.
    ///
    /// `config` carries the wallet secret (32 raw bytes from the
    /// user's identity keychain) and the local transport NodeId.
    /// Calling this twice replaces the previous pairing service;
    /// in-flight trusted-device records survive because both
    /// instances point at the same on-disk JSONL file when
    /// `data_dir` matches.
    pub fn with_pairing(
        &self,
        owner: &UserId,
        config: PairingServiceConfig,
    ) -> Result<&Self, AppError> {
        let svc = PairingService::open(config, owner.clone())?;
        *self
            .pairing
            .lock()
            .expect("pairing slot mutex poisoned") = Some(svc);
        Ok(self)
    }

    /// Clone the currently-installed pairing service, if any.
    /// Returns `None` when [`A3chatApp::with_pairing`] has not yet
    /// been called.
    pub fn pairing_service(&self) -> Option<PairingService> {
        self.pairing
            .lock()
            .expect("pairing slot mutex poisoned")
            .clone()
    }

    /// Initialise the local user schema (idempotent).
    pub async fn init_user(&self, owner: &UserId) -> AppResult<()> {
        self.chat.storage().init_user(owner).await
    }

    /// GB-22 — install the group-mute gate that lets
    /// [`ChatService::send_message`] consult [`GroupService`] before
    /// persisting an outbound group message. Call once after
    /// [`A3chatApp::new`] (and after the [`GroupService`] has been
    /// augmented with `with_hub` / `with_storage` /
    /// `with_invitation_state` if you need them).
    pub fn install_group_mute_gate(&mut self) -> &mut Self {
        let group = self.group.clone();
        let gate: crate::chat_service::MuteGate = std::sync::Arc::new(
            move |conv_id: ConversationId, sender: UserId| {
                let group = group.clone();
                Box::pin(async move {
                    group.is_member_muted(&conv_id, &sender).await.unwrap_or(false)
                })
            },
        );
        // `with_mute_gate` consumes `ChatService` and returns the
        // updated copy — re-install it on `self` so subsequent RPC
        // calls see the gate.
        self.chat = self.chat.clone().with_mute_gate(gate);
        self
    }

    /// F-25 / B-7 — install the blocklist gate so
    /// [`ChatService::send_message`] consults [`ContactService`]
    /// before persisting. Call once after [`A3chatApp::new`]. The
    /// gate is a closure that holds a clone of `Arc<ContactService>`
    /// so the chat service never imports the contact module
    /// directly (avoids a circular dependency).
    pub fn install_blocklist_gate(&mut self) -> &mut Self {
        let contact = self.contact.clone();
        let gate: crate::chat_service::BlocklistGate = std::sync::Arc::new(
            move |owner: UserId, peer: UserId| {
                let contact = contact.clone();
                Box::pin(async move { contact.is_blocked(&owner, &peer).await })
            },
        );
        self.chat = self.chat.clone().with_blocklist_gate(gate);
        self
    }

    /// Install the presence touch gate so that when a group message
    /// is sent, the sender's `last_seen` and `is_online` are updated
    /// in the group membership table.
    pub fn install_presence_touch_gate(&mut self) -> &mut Self {
        let group = self.group.clone();
        let gate: crate::chat_service::PresenceTouchGate = std::sync::Arc::new(
            move |conv_id: a3chat_core::id::ConversationId, sender: UserId, is_online: bool| {
                let group = group.clone();
                Box::pin(async move {
                    group.touch_member(&conv_id, &sender, is_online).await.ok();
                })
            },
        );
        self.chat = self.chat.clone().with_presence_touch_gate(gate);
        self
    }

    /// Phase 5c: attach the distributed message store.
    ///
    /// After calling `A3chatApp::new`, if the process is configured
    /// to use iroh-docs, call this method to hand the bridge to
    /// `ChatService`. Every subsequent outbound message is then
    /// dual-written: SQLite first (authoritative), then iroh-docs
    /// (best-effort fan-out).
    ///
    /// Idempotent — replacing an existing bridge is safe.
    #[cfg(feature = "iroh")]
    pub async fn with_iroh_docs_chat(&self, bridge: A3chatAppBridge) {
        self.chat.with_iroh_docs_chat(bridge.inner.clone()).await;
    }

    /// Phase 5c: install the iroh-docs P2P group sync service.
    ///
    /// Call this **after** `with_iroh_docs_chat` and **after**
    /// `init_user`, so that the local `owner` UserId is known.
    /// The `group_sync` service keeps per-conversation subscriptions
    /// to iroh-docs docs and periodically backfills new messages
    /// into SQLite.
    ///
    /// The `iroh_docs_chat` must be set before calling this.
    #[cfg(feature = "iroh")]
    pub fn with_group_sync_service(&mut self) -> &mut Self {
        let Some(docs_chat) = &self.iroh_docs_chat else {
            tracing::warn!("with_group_sync_service called but iroh_docs_chat is None — skipping");
            return self;
        };
        let svc = GroupSyncService::new(
            self.owner.clone(),
            self.chat.storage().clone(),
            (**docs_chat).clone(),
            self.bus.clone(),
        );
        self.group_sync = Some(svc);
        tracing::info!("GroupSyncService installed");
        self
    }

    /// Build the app from a pre-constructed [`ChatStorage`]
    /// (used by tests that want to share a storage instance with
    /// the RPC layer). The keyring is also derived from
    /// `owner_key`.
    pub fn with_storage(
        storage: ChatStorage,
        bus: NotificationBus,
        owner_key: UserId,
    ) -> Self {
        let owner_for_contact = owner_key.clone();
        let keyring = E2eKeyring::new(owner_for_contact.clone());
        // Open a media service in a tempdir so unit tests don't
        // try to write to the caller's cwd. This is safe because
        // the media store is self-contained.
        let media_dir = std::env::temp_dir().join(format!(
            "a3chat-media-test-{}",
            uuid::Uuid::new_v4()
        ));
        let media_cfg = crate::media_service::MediaConfig::local_only_under_base(&media_dir);
        let media = MediaService::open(&media_cfg).expect("media opens in tempdir");
        let moderation = ModerationService::open_in_memory(false);
        // No reputation reporter in `with_storage` (used by tests
        // that build via a pre-constructed storage). Tests that
        // need reputation-driven paths call `with_reputation` on
        // the returned `A3chatApp`.
        let peerfeedback = PeerFeedbackService::new(storage.clone());
        // Moments / 朋友圈 (F-05). Tests that build via
        // `with_storage` get a shared in-memory service that
        // publishes onto the supplied bus so `app.subscribe_for`
        // routes the events out to SSE.
        let moments = MomentsService::open_in_memory_with_bus(bus.clone())
            .with_moderation(moderation.clone());
        // Link bookmark / favorites (F-06). Re-uses the supplied
        // [`ChatStorage`] so bookmarks hit the same SQLite file as
        // the rest of the chat data.
        let link_cfg = link_bookmark_service::LinkBookmarkConfig::under_base(storage.config().base_dir.as_path());
        let link = LinkBookmarkService::new(storage.clone(), bus.clone(), link_cfg);
        // F-09 Channel / 公众号 — tests via `with_storage` get an
        // in-memory service that publishes onto the supplied bus.
        // We prefer `NodeId::from_hex(owner)` so a test rig that
        // supplies a 64-hex owner (the conventional way to make
        // `owner == local_node` hold across all services) gets the
        // canonical node id; otherwise fall back to random so
        // concurrent tests still have isolated nodes.
        let channel_local_node = a3net_types::NodeId::from_hex(owner_for_contact.as_str())
            .unwrap_or_else(|_| a3net_types::NodeId::random());
        let channel_cfg = crate::channel_service::ChannelServiceConfig {
            base_dir: storage.config().base_dir.clone(),
            filename: "channel.db".into(),
            enable_gossip: true,
        };
        let channel = match crate::channel_service::ChannelService::open(channel_cfg, channel_local_node.clone()) {
            Ok(svc) => svc,
            Err(_) => crate::channel_service::ChannelService::open_in_memory(channel_local_node)
                .expect("channel in-memory opens"),
        };
        // Contact roster: tests use in-memory store but still
        // wired with the canonical owner so the per-service
        // `require_owner` check fires (otherwise a multi-tenant
        // test rig could cross-read). Also shares the in-process
        // bus so bus-event tests can observe `Contact*` events
        // bubbling up to the app-level subscriber.
        let contact = ContactService::with_store_and_bus_for_test(owner_for_contact, bus.clone());
        // F-07 services (in-memory, shared bus). Drafts share the storage
        // so tests observe the same persistence path as production.
        let draft = DraftService::with_storage(Arc::new(storage.clone()));
        let reaction = ChatReactionService::new(bus.clone());
        let device = DeviceService::new_with_bus(bus.clone());
        let notification_settings = NotificationSettingsService::new(bus.clone());
        let pinned = PinnedService::new(storage.clone(), bus.clone());
        let forward = ForwardService::new(storage.clone(), bus.clone());
        let e2e_encryption = E2eEncryptionService::new(keyring.clone());
        let e2e_bundle = E2eBundleService::new(storage.clone(), keyring.clone());
        let stream = StreamService::new();
        let bus_start_unix = Arc::new(AtomicI64::new(chrono::Utc::now().timestamp()));
        // F-08 / B-24 invitation store with the shared storage handle.
        let invitations = GroupInvitationService::with_storage(Arc::new(storage.clone()));
        Self {
            chat: ChatService::new(storage.clone(), bus.clone()).with_moderation(moderation.clone()),
            contact,
            group: Arc::new(GroupService::new(bus.clone())),
            sync: SyncService::new(storage.clone()),
            presence: PresenceService::new(storage.clone(), bus.clone()),
            profile: ProfileService::from_store(empty_test_store_arc()),
            media,
            moderation,
            peerfeedback,
            moments,
            link,
            channel,
            draft,
            reaction,
            device,
            notification_settings,
            pinned,
            forward,
            e2e_encryption,
            e2e_bundle,
            stream,
            invitations,
            bus,
            keyring,
            bus_start_unix,
            // Pairing service is NOT installed in `with_storage`
            // because that constructor is reserved for tests that
            // do not care about the wallet. Tests that need pairing
            // call `with_pairing` on the returned `A3chatApp`.
            pairing: std::sync::Arc::new(std::sync::Mutex::new(None)),
            owner: owner_key.clone(),
            #[cfg(feature = "iroh")]
            iroh_docs_chat: None,
            #[cfg(feature = "iroh")]
            group_sync: None,
        }
    }

    /// Dispatch any `a3chat.*` method to the right service.
    pub async fn dispatch(
        &self,
        method: &str,
        owner: &UserId,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, A3chatError> {
        // Liveness probe first — must succeed even when other
        // services are unavailable. Hit this BEFORE any prefix
        // match so `a3chat.healthz` works mid-bootstrap.
        if method == "a3chat.healthz" || method == "a3chat.rpc.health" {
            return Ok(self.healthz_payload(owner));
        }
        // Sync methods must be matched BEFORE the broader
        // `a3chat.chat.*` prefix because `chat.sync.*` is a
        // sub-namespace of `chat.*`.
        if method.starts_with("a3chat.chat.sync") || method.starts_with("a3chat.sync.") {
            return sync_service::dispatch(Arc::new(self.sync.clone()), method, owner, params)
                .await;
        }
        // F-07: per-conversation message drafts.
        // `a3chat.chat.draft.*` is a sub-namespace of `chat.*` but
        // chat_service does not handle it, so we route it here.
        // IMPORTANT: must be matched BEFORE the broader
        // `a3chat.chat.*` prefix below.
        if method.starts_with("a3chat.chat.draft.") {
            return draft_service_mod::dispatch(
                Arc::new(self.draft.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        // F-07: message reactions. Sub-namespace of `chat.*` —
        // match BEFORE the broader prefix.
        if method.starts_with("a3chat.chat.reaction.") {
            return reaction_service::dispatch(
                Arc::new(self.reaction.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        // F-07: notification settings (DND + per-conversation
        // overrides). `a3chat.chat.notification.*` is a
        // sub-namespace of `chat.*` — match BEFORE the broader
        // prefix.
        if method.starts_with("a3chat.chat.notification.") {
            return notification_settings_service_mod::dispatch(
                Arc::new(self.notification_settings.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        // F-07: pinned conversation FSM. Sub-namespace of
        // `chat.*` — match BEFORE the broader prefix.
        if method.starts_with("a3chat.chat.conversation.pin")
            || method.starts_with("a3chat.chat.conversation.unpin")
            || method.starts_with("a3chat.chat.conversation.toggle_pin")
            || method.starts_with("a3chat.chat.conversation.list_pinned")
        {
            return pinned_service_mod::dispatch(
                Arc::new(self.pinned.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        // F-07: message forwarding. Sub-namespace of `chat.*` —
        // match BEFORE the broader prefix.
        if method == "a3chat.chat.message.forward" {
            return forward_service_mod::dispatch(
                Arc::new(self.forward.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        if method.starts_with("a3chat.chat.") {
            return chat_service::dispatch(Arc::new(self.chat.clone()), method, owner, params)
                .await;
        }
        if method.starts_with("a3chat.contact.") {
            return contact_service::dispatch(
                Arc::new(self.contact.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        if method.starts_with("a3chat.group.") {
            return group_service::dispatch(self.group.clone(), method, owner, params)
                .await;
        }
        // Phase 5c: iroh-docs P2P group sync. Routes through the
        // optional `GroupSyncService`; if not configured returns NotImplemented.
        #[cfg(feature = "iroh")]
        if method.starts_with("a3chat.group.sync.") {
            let Some(svc) = &self.group_sync else {
                return Err(A3chatError::Internal(
                    "GroupSyncService is not configured. \
                     Ensure with_iroh_docs_chat and with_group_sync_service \
                     were called during bootstrap.".into(),
                ));
            };
            return group_sync_service_mod::dispatch(
                Arc::new(svc.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        if method.starts_with("a3chat.presence.") {
            return presence_service::dispatch(
                Arc::new(self.presence.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        if method.starts_with("a3chat.profile.") {
            return profile_service::dispatch(
                Arc::new(self.profile.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        if method.starts_with("a3chat.media.") {
            return media_service::dispatch(
                Arc::new(self.media.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        if method.starts_with("a3chat.moderation.") {
            return moderation_service::dispatch(
                Arc::new(self.moderation.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        if method.starts_with("a3chat.peerfeedback.") {
            return peer_feedback_service::dispatch(
                Arc::new(self.peerfeedback.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        if method.starts_with("a3chat.moments.") {
            return moments_service::dispatch(
                Arc::new(self.moments.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        if method.starts_with("a3chat.link.") {
            return link_bookmark_service::dispatch(
                Arc::new(self.link.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        // F-09: Channel / 公众号 (public-account) service. Wraps
        // `a3net-news::NewsService` for gossip fan-out and exposes
        // the `a3chat.channel.*` JSON-RPC namespace.
        if method.starts_with("a3chat.channel.") {
            return channel_service_mod::dispatch(
                Arc::new(self.channel.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        // F-07: device management.
        if method.starts_with("a3chat.device.") {
            return device_service_mod::dispatch(
                Arc::new(self.device.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        // F-07: E2E handshake introspection + encrypt/decrypt stubs.
        // The `a3chat.e2e.handshake.*` namespace routes introspection
        // helpers; `a3chat.e2e.encrypt` / `a3chat.e2e.decrypt` are
        // also routed here so they surface a NotImplemented error
        // rather than silently 500-ing.
        // MUST be matched BEFORE `a3chat.e2e.bundle.*` because the
        // bundle prefix is broader.
        if method.starts_with("a3chat.e2e.handshake.")
            || method == "a3chat.e2e.encrypt"
            || method == "a3chat.e2e.decrypt"
        {
            return e2e_encryption_service_mod::dispatch(
                Arc::new(self.e2e_encryption.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        // F-07: E2E bundle export/import.
        if method.starts_with("a3chat.e2e.bundle.") {
            return e2e_bundle_service::dispatch(
                Arc::new(self.e2e_bundle.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        // F-07: SSE stream subscription registry.
        if method.starts_with("a3chat.stream.") {
            return stream_service_mod::dispatch(
                Arc::new(self.stream.clone()),
                method,
                owner,
                params,
            )
            .await;
        }
        // P2P device-pairing namespace. Routes through the
        // optional `PairingService`; if the operator has not called
        // `with_pairing` yet we surface a clear `NotImplemented`
        // error so the RPC client can render an actionable message.
        if method.starts_with("a3chat.pairing.") {
            let svc = self
                .pairing_service()
                .ok_or_else(|| A3chatError::Internal(
                    "PairingService is not configured. Call A3chatApp::with_pairing(...) \
                     during bootstrap (wallet secret + local NodeId)."
                        .into(),
                ))?;
            return pairing_service_mod::dispatch(
                Arc::new(svc),
                method,
                owner,
                params,
            )
            .await;
        }
        Err(A3chatError::Internal(format!(
            "A3chatApp does not handle method {method}"
        )))
    }

    /// Subscribe to all events for the given user (used by the SSE
    /// bridge in `a3chat-rpc`).
    pub fn subscribe_for(&self, owner: UserId) -> crate::notification_bus::NotificationReceiver {
        self.bus.subscribe_for(owner)
    }

    /// Build the payload returned by `a3chat.healthz`. Mirrors
    /// the same JSON shape expected by the load-balancer probe so
    /// consumers can use a single parser.
    pub fn healthz_payload(&self, owner: &UserId) -> serde_json::Value {
        let started_unix = self.bus_start_unix.load(Ordering::Relaxed);
        let now = chrono::Utc::now().timestamp();
        let uptime_secs = now.saturating_sub(started_unix);
        serde_json::json!({
            "ok": true,
            "service": "a3chat.app",
            "version": env!("CARGO_PKG_VERSION"),
            "owner": owner.as_str(),
            "started_unix": started_unix,
            "uptime_secs": uptime_secs,
            "bus_receivers": self.bus.receiver_count(),
            "stream_handles": self.stream.handle_count(),
        })
    }
}

impl NotificationBus {
    pub fn default_capacity() -> usize {
        crate::notification_bus::DEFAULT_CAPACITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::message::{MessageBody, MessageEnvelope, MessageType};
    use tempfile::tempdir;

    fn owner() -> UserId {
        UserId::from("alice-node-id")
    }
    fn peer() -> UserId {
        UserId::from("bob-node-id")
    }

    #[tokio::test]
    async fn build_app_and_init_user() {
        let dir = tempdir().unwrap();
        let cfg = StorageConfig::new(dir.path().to_path_buf());
        let app = A3chatApp::new(cfg, owner()).unwrap();
        app.init_user(&owner()).await.unwrap();
        assert!(app.chat.storage().config().path_for(&owner()).exists());
    }

    #[tokio::test]
    async fn dispatch_chat_send_routes_to_chat_service() {
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        app.init_user(&owner()).await.unwrap();
        let env = MessageEnvelope {
            conversation_id: a3chat_core::id::ConversationId::from("dm:a:b"),
            receiver_id: peer(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "hi".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
        };
        let r = app
            .dispatch(
                a3chat_core::rpc::A3chatRpcMethod::CHAT_MESSAGE_SEND,
                &owner(),
                serde_json::to_value(&env).unwrap(),
            )
            .await
            .unwrap();
        assert!(r.get("message").is_some());
    }

    #[tokio::test]
    async fn dispatch_unknown_method_errors() {
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        let err = app
            .dispatch("a3chat.bogus", &owner(), serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, A3chatError::Internal(_)));
    }

    #[tokio::test]
    async fn dispatch_contact_routes_to_contact_service() {
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        let r = app
            .dispatch(
                a3chat_core::rpc::A3chatRpcMethod::CONTACT_LIST,
                &owner(),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        assert!(r.get("contacts").is_some());
        assert!(r.get("blocklist").is_some());
    }

    #[tokio::test]
    async fn dispatch_group_create_routes_to_group_service() {
        // Routing test: verify GROUP_CREATE reaches GroupService even
        // without hub set. Without hub the service returns NotInitialised
        // but that's sufficient to prove the dispatch path is wired correctly.
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        let r = app
            .dispatch(
                a3chat_core::rpc::A3chatRpcMethod::GROUP_CREATE,
                &owner(),
                serde_json::json!({ "name": "team", "description": "eng", "isPrivate": true }),
            )
            .await;
        // Routing works if we get NotInitialised (hub not set is expected here).
        // The alternative — a missing-method error — would mean dispatch didn't route.
        let err = r.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("hub") || msg.contains("Hub"),
            "expected hub-not-set error (routing succeeded), got: {err}"
        );
    }

    #[tokio::test]
    async fn dispatch_presence_publish_routes_to_presence_service() {
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        app.init_user(&owner()).await.unwrap();
        let r = app
            .dispatch(
                a3chat_core::rpc::A3chatRpcMethod::PRESENCE_PUBLISH,
                &owner(),
                serde_json::json!({ "status": "online" }),
            )
            .await
            .unwrap();
        assert_eq!(r["status"], "online");
    }

    #[tokio::test]
    async fn dispatch_sync_snapshot_routes_to_sync_service() {
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        app.init_user(&owner()).await.unwrap();
        let r = app
            .dispatch(
                a3chat_core::rpc::A3chatRpcMethod::CHAT_SYNC_SNAPSHOT,
                &owner(),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        assert!(r.get("conversations").is_some());
        assert!(r.get("messages").is_some());
    }

    #[tokio::test]
    async fn subscribe_for_returns_receiver() {
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        let _rx = app.subscribe_for(owner());
        // Just check the call works without panicking.
    }

    #[test]
    fn default_capacity_is_a_positive_number() {
        assert!(NotificationBus::default_capacity() >= 64);
    }

    #[tokio::test]
    async fn app_is_send_sync() {
        // Compile-time test: if A3chatApp is Send + Sync, this compiles.
        fn assert_send<T: Send + Sync>() {}
        assert_send::<A3chatApp>();
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        let _h = std::thread::spawn(move || {
            // Spawning proves we can move it across threads.
            let _ = app;
        });
    }
}
