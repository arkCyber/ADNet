//! `a3chat-app` — business services for a3chat.
//!
//! Wires together the domain types ([`a3chat_core`]), the E2E
//! crypto ([`a3chat_crypto`]), the persistence layer
//! ([`a3net_chatstore`]), and the RPC layer
//! ([`a3chat_rpc`]). Service structs are `Arc`-friendly and share
//! a [`ChatStorage`] + [`NotificationBus`] + [`E2eKeyring`].

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod app;
pub mod bot_framework;
pub mod chat_service;
pub mod contact_service;
pub mod error;
pub mod group_service;
pub mod group_service_types;
pub mod keyring;
pub mod link_bookmark_service;
pub mod media_service;
pub mod moderation_service;
pub mod moments_service;
pub mod notification_bus;
pub mod peer_feedback_service;
pub mod presence_service;
pub mod profile_service;
pub mod storage;
pub mod sync_service;

pub use app::A3chatApp;
pub use bot_framework::{BotConfig, BotRole, BotSession, ChatBot, ReplyGenerator};
pub use chat_service::ChatService;
pub use contact_service::ContactService;
pub use error::{AppError, AppResult, app_to_domain};
pub use group_service::GroupService;
pub use keyring::E2eKeyring;
pub use media_service::{
    BlobMeta, EcPolicy, EncryptionPolicy, MediaConfig, MediaError, MediaHealth, MediaService,
    WritePolicy, MAX_ATTACHMENT_BYTES, MAX_CHUNK_BYTES, SR_TAG_MEDIA_1, SR_TAG_MEDIA_10,
    SR_TAG_MEDIA_11, SR_TAG_MEDIA_2, SR_TAG_MEDIA_3, SR_TAG_MEDIA_4, SR_TAG_MEDIA_5,
    SR_TAG_MEDIA_6, SR_TAG_MEDIA_7, SR_TAG_MEDIA_8, SR_TAG_MEDIA_9, SR_TAGS,
};
pub use moderation_service::{ModerationConfig, ModerationService, ModerationOutcome};
pub use moments_service::{MomentsConfig, MomentsService};
pub use notification_bus::{NotificationBus, NotificationReceiver};
pub use peer_feedback_service::{FusedScore, PeerFeedbackService, DEFAULT_REFUSAL_THRESHOLD};
pub use presence_service::PresenceService;
pub use profile_service::{ProfileConfig, ProfileService};
pub use storage::{ChatStorage, StorageConfig};
pub use sync_service::SyncService;
