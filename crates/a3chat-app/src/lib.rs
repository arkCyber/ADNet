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
pub mod chat_service;
pub mod contact_service;
pub mod error;
pub mod group_service;
pub mod keyring;
pub mod media_service;
pub mod moderation_service;
pub mod notification_bus;
pub mod peer_feedback_service;
pub mod presence_service;
pub mod profile_service;
pub mod storage;
pub mod sync_service;

pub use app::A3chatApp;
pub use chat_service::ChatService;
pub use contact_service::ContactService;
pub use error::{AppError, AppResult, app_to_domain};
pub use group_service::GroupService;
pub use keyring::E2eKeyring;
pub use media_service::{MediaConfig, MediaError, MediaService, MediaHealth, MAX_ATTACHMENT_BYTES, MAX_CHUNK_BYTES};
pub use moderation_service::{ModerationConfig, ModerationService, ModerationOutcome};
pub use notification_bus::{NotificationBus, NotificationReceiver};
pub use peer_feedback_service::{FusedScore, PeerFeedbackService, DEFAULT_REFUSAL_THRESHOLD};
pub use presence_service::PresenceService;
pub use profile_service::{ProfileConfig, ProfileService};
pub use storage::{ChatStorage, StorageConfig};
pub use sync_service::SyncService;
