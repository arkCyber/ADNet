//! `a3chat-core` — domain types, JSON Schema, and zero-dep `RpcClient` trait.
//!
//! Shared across Rust server, Tauri desktop, and Flutter mobile clients.
//! No encryption logic lives here — that is `a3chat-crypto`'s job. This
//! crate only defines the *shape* of ciphertext fields.
//!
//! # Module map
//!
//! - [`error`]       — [`A3chatError`] unified error type
//! - [`id`]          — typed identifiers (`UserId`, `MessageId`, …)
//! - [`conversation`]
//! - [`message`]     — [`ChatMessage`] + [`MessageBody`] (Plain/Encrypted)
//! - [`contact`]     — friends, requests, blocklist
//! - [`group`]       — group metadata + membership
//! - [`presence`]    — online/away/offline/invisible
//! - [`event`]       — [`A3chatEvent`] server-pushed notification
//! - [`schema`]      — JSON Schema export for frontend codegen
//! - [`rpc`]         — [`RpcClient`] trait + method constants
//! - [`validation`]  — shared field validators

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod contact;
pub mod conversation;
pub mod error;
pub mod event;
pub mod group;
pub mod id;
pub mod link_bookmark;
pub mod message;
pub mod presence;
pub mod rpc;
pub mod schema;
pub mod validation;

pub use conversation::{ConversationKind, ConversationMeta, ConversationRecord};
pub use error::{A3chatError, A3chatResult};
pub use event::{
    A3chatEvent, A3chatNotification, NOTIFICATION_KIND_CHAT, NOTIFICATION_KIND_GROUP_INVITATION,
    NOTIFICATION_KIND_LINK_BOOKMARK_ADDED, NOTIFICATION_KIND_LINK_BOOKMARK_DELETED,
    NOTIFICATION_KIND_LINK_BOOKMARK_UPDATED, NOTIFICATION_KIND_MOMENTS_COMMENT_ADDED,
    NOTIFICATION_KIND_MOMENTS_POST_CREATED, NOTIFICATION_KIND_MOMENTS_POST_DELETED,
    NOTIFICATION_KIND_MOMENTS_REACTION_TOGGLED, NOTIFICATION_KIND_PRESENCE,
};
pub use group::{Group, GroupInvitation, GroupMember, InvitationStatus, MemberRole};
pub use id::{
    ConversationId, DeviceId, MessageId, UserId, generate_conversation_id, generate_device_id,
    generate_message_id, generate_user_id,
};
pub use link_bookmark::{
    BookmarkSource, DEFAULT_FOLDER, INTEGRITY_HASH_TAG, LinkBookmark, LinkBookmarkCount,
    LinkBookmarkListFilter, LinkBookmarkSearchQuery, LinkFolderNode, LinkTagCount,
    MAX_DESCRIPTION_LEN, MAX_FOLDER_DEPTH, MAX_FOLDER_LEN, MAX_SNAPSHOT_LEN, MAX_TAG_LEN,
    MAX_TAGS_PER_BOOKMARK, MAX_TITLE_LEN, UpsertLinkBookmarkRequest, compute_bookmark_id,
    normalize_tag, normalize_tags, validate_folder,
};
pub use message::{
    Attachment, AttachmentKind, ChatMessage, MAX_PREVIEW_LEN, MessageBody, MessageEnvelope,
    MessageType, truncate_preview,
};
pub use presence::{Presence, PresenceEvent, PresenceStatus};
pub use rpc::{A3chatRpcMethod, RpcClient};
