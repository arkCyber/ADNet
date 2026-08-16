//! `A3chatApp` — wires every service together. Use this from
//! `a3chat-rpc` (or directly from tests) to dispatch any
//! `a3chat.*` JSON-RPC method.

use std::sync::Arc;

use a3chat_core::error::A3chatError;
use a3chat_core::id::UserId;

use crate::chat_service::{self, ChatService};
use crate::contact_service::{self, ContactService};
use crate::error::AppResult;
use crate::group_service::{self, GroupService};
use crate::keyring::E2eKeyring;
use crate::media_service::{self, MediaConfig, MediaService};
use crate::moderation_service::{self, ModerationConfig, ModerationService};
use crate::notification_bus::NotificationBus;
use crate::peer_feedback_service::{self, PeerFeedbackService};
use crate::presence_service::{self, PresenceService};
use crate::profile_service::{self, ProfileConfig, ProfileService};
use crate::storage::{ChatStorage, StorageConfig};
use crate::sync_service::{self, SyncService};

fn empty_test_store_arc() -> std::sync::Arc<dyn a3net_userstore::store::UserStore> {
    struct Empty;
    #[async_trait::async_trait]
    impl a3net_userstore::store::UserStore for Empty {
        async fn put_profile(
            &self,
            _: a3net_userstore::model::UserProfile,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        async fn get_profile(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<Option<a3net_userstore::model::UserProfile>>
        {
            Ok(None)
        }
        async fn put_preferences(
            &self,
            _: &str,
            _: a3net_userstore::model::UserPreferences,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        async fn list_profiles(
            &self,
        ) -> a3net_userstore::error::UserStoreResult<Vec<a3net_userstore::model::UserProfile>>
        {
            Ok(vec![])
        }
        async fn delete_profile(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<usize> {
            Ok(0)
        }
        async fn put_public_key(
            &self,
            _: a3net_userstore::model::UserPublicKey,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        async fn revoke_public_key(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        async fn list_public_keys(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<Vec<a3net_userstore::model::UserPublicKey>>
        {
            Ok(vec![])
        }
        async fn put_device(
            &self,
            _: a3net_userstore::model::UserDevice,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        async fn revoke_device(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            Ok(())
        }
        async fn list_devices(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<Vec<a3net_userstore::model::UserDevice>>
        {
            Ok(vec![])
        }
        async fn ensure_user_digit(
            &self,
            _: &str,
        ) -> a3net_userstore::error::UserStoreResult<String> {
            Ok("000000000000".into())
        }
        async fn resolve_user_digit(
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
    pub group: GroupService,
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
    pub bus: NotificationBus,
    pub keyring: E2eKeyring,
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
        Ok(Self {
            chat: ChatService::new(storage.clone(), bus.clone()).with_moderation(moderation.clone()),
            contact: ContactService::new(bus.clone()),
            group: GroupService::new(bus.clone()),
            sync: SyncService::new(storage.clone()),
            presence: PresenceService::new(storage.clone(), bus.clone()),
            profile,
            media,
            moderation,
            peerfeedback,
            bus,
            keyring,
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

    /// Initialise the local user schema (idempotent).
    pub async fn init_user(&self, owner: &UserId) -> AppResult<()> {
        self.chat.storage().init_user(owner).await
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
        let keyring = E2eKeyring::new(owner_key);
        // Open a media service in a tempdir so unit tests don't
        // try to write to the caller's cwd. This is safe because
        // the media store is self-contained.
        let media_dir = std::env::temp_dir().join(format!(
            "a3chat-media-test-{}",
            uuid::Uuid::new_v4()
        ));
        let media_cfg = MediaConfig {
            data_dir: media_dir,
            max_attachment_bytes: crate::media_service::MAX_ATTACHMENT_BYTES,
            max_chunk_bytes: crate::media_service::MAX_CHUNK_BYTES,
        };
        let media = MediaService::open(&media_cfg).expect("media opens in tempdir");
        let moderation = ModerationService::open_in_memory(false);
        // No reputation reporter in `with_storage` (used by tests
        // that build via a pre-constructed storage). Tests that
        // need reputation-driven paths call `with_reputation` on
        // the returned `A3chatApp`.
        let peerfeedback = PeerFeedbackService::new(storage.clone());
        Self {
            chat: ChatService::new(storage.clone(), bus.clone()).with_moderation(moderation.clone()),
            contact: ContactService::new(bus.clone()),
            group: GroupService::new(bus.clone()),
            sync: SyncService::new(storage.clone()),
            presence: PresenceService::new(storage.clone(), bus.clone()),
            // Tests that build via `with_storage` skip the
            // ProfileService — the dispatcher still works
            // because `a3chat.profile.*` methods return a
            // stub error rather than panicking.
            profile: ProfileService::from_store(empty_test_store_arc()),
            media,
            moderation,
            peerfeedback,
            bus,
            keyring,
        }
    }

    /// Dispatch any `a3chat.*` method to the right service.
    pub async fn dispatch(
        &self,
        method: &str,
        owner: &UserId,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, A3chatError> {
        // Sync methods must be matched BEFORE the broader
        // `a3chat.chat.*` prefix because `chat.sync.*` is a
        // sub-namespace of `chat.*`.
        if method.starts_with("a3chat.chat.sync") || method.starts_with("a3chat.sync.") {
            return sync_service::dispatch(Arc::new(self.sync.clone()), method, owner, params)
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
            return group_service::dispatch(Arc::new(self.group.clone()), method, owner, params)
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
        Err(A3chatError::Internal(format!(
            "A3chatApp does not handle method {method}"
        )))
    }

    /// Subscribe to all events for the given user (used by the SSE
    /// bridge in `a3chat-rpc`).
    pub fn subscribe_for(&self, owner: UserId) -> crate::notification_bus::NotificationReceiver {
        self.bus.subscribe_for(owner)
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
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        let r = app
            .dispatch(
                a3chat_core::rpc::A3chatRpcMethod::GROUP_CREATE,
                &owner(),
                serde_json::json!({ "name": "team", "description": "eng", "is_private": true }),
            )
            .await
            .unwrap();
        assert_eq!(r["name"], "team");
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
