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
pub mod channel_storage;
pub mod chat_reaction_service;
pub mod chat_service;
pub mod contact_service;
pub mod device_service;
pub mod draft_service;
pub mod e2e_bundle;
pub mod e2e_encryption_service;
pub mod error;
pub mod forward_service;
pub mod group_service;
pub mod group_service_types;
pub mod group_invitation_service;
pub mod group_mention;
pub mod keyring;
pub mod link_bookmark_service;
pub mod media_service;
pub mod moderation_service;
pub mod moments_service;
pub mod notification_bus;
pub mod notification_settings_service;
pub mod pairing_service;
pub mod peer_feedback_service;
pub mod pinned_service;
pub mod presence_service;
pub mod profile_service;
pub mod storage;
pub mod stream_service;
pub mod sync_service;

pub use app::{A3chatApp, A3chatAppBridge, bridge_for_iroh_docs};
pub use bot_framework::{BotConfig, BotRole, BotSession, ChatBot, ReplyGenerator};
pub use channel_storage::{ChannelStorage, ChannelStorageConfig};
pub use chat_reaction_service::{ChatReactionService, ReactionSummary};
pub use chat_service::ChatService;
pub use contact_service::ContactService;
pub use device_service::{Device, DeviceKind, DeviceService, RegisterDeviceRequest, RevokeDeviceRequest};
pub use draft_service::DraftService;
pub use e2e_encryption_service::E2eEncryptionService;
pub use e2e_bundle::{Bundle, E2eBundleService, ImportSummary, BUNDLE_VERSION};
pub use error::{AppError, AppResult, app_to_domain};
pub use forward_service::{ForwardRequest, ForwardResult, ForwardService, ForwardTargetResult};
pub use group_service::GroupService;
pub use group_invitation_service::{
    InvitationRecord, GroupInvitationService, DEFAULT_INVITATION_TTL_SECS, STATUS_PENDING,
    STATUS_ACCEPTED, STATUS_DECLINED, STATUS_REVOKED, STATUS_EXPIRED,
};
pub use keyring::E2eKeyring;
pub use link_bookmark_service::LinkBookmarkService;
pub use media_service::{
    BlobMeta, EcPolicy, EncryptionPolicy, MediaConfig, MediaError, MediaHealth, MediaService,
    WritePolicy, MAX_ATTACHMENT_BYTES, MAX_CHUNK_BYTES, SR_TAG_MEDIA_1, SR_TAG_MEDIA_10,
    SR_TAG_MEDIA_11, SR_TAG_MEDIA_2, SR_TAG_MEDIA_3, SR_TAG_MEDIA_4, SR_TAG_MEDIA_5,
    SR_TAG_MEDIA_6, SR_TAG_MEDIA_7, SR_TAG_MEDIA_8, SR_TAG_MEDIA_9, SR_TAGS,
};
pub use moderation_service::{ModerationConfig, ModerationService, ModerationOutcome};
pub use moments_service::{MomentsConfig, MomentsService};
pub use notification_bus::{NotificationBus, NotificationReceiver};
pub use notification_settings_service::{
    ConversationNotificationSettings, NotificationLevel, NotificationSettingsService,
};
pub use pairing_service::{
    AcceptInvitationRequest, AcceptInvitationResponse, CreateInvitationRequest,
    CreateInvitationResponse, DecodedInvitation, ParsedCode, PairingService,
    PairingServiceConfig, DEFAULT_INVITATION_TTL_SECONDS, MAX_INVITATION_TTL_SECONDS,
};
pub use peer_feedback_service::{FusedScore, PeerFeedbackService, DEFAULT_REFUSAL_THRESHOLD};
pub use pinned_service::{PinnedService, PinAction};
pub use presence_service::PresenceService;
pub use profile_service::{ProfileConfig, ProfileService};
pub use storage::{ChatStorage, StorageConfig};
pub use stream_service::{StreamService, StreamSubscription, StreamTopic};
pub use sync_service::SyncService;
