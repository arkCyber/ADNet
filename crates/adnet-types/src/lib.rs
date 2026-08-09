//! `adnet-types` — foundational types shared across the ADNet workspace.
//!
//! Every higher crate (`blobstore`, `gossip`, `mesh`, `transport`, `node`)
//! depends on this one. Anything placed here must be **stable** and free of
//! runtime / IO semantics.
//!
//! Modules:
//! - [`node`]      : `NodeId`, `NodeAddr` (host + port + relay url)
//! - [`content`]   : `ContentHash` (BLAKE3), content kinds, range specs
//! - [`ticket`]    : peer / blob tickets with optional range
//! - [`topic`]     : gossip topic naming (iroh-gossip compatible)
//! - [`announce`]  : room/lobby announcement payloads
//! - [`room`]      : room identifiers and asset records
//! - [`peer_source`] : per-hash peer availability records
//! - [`error`]     : cross-crate error helpers
//! - [`invariants`]: typed enums (`Visibility`, `MessageType`, …) + length /
//!   character / temporal validators used by the chat and social-feed
//!   records.
//! - [`integrity`] : SHA-256 tamper-detection helpers, length-prefixed
//!   digests, strict [`VerifyOutcome`] verifier.
//! - [`group_chat`] : typed group & direct chat records (built on
//!   [`invariants`] + [`integrity`]).
//! - [`social_feed`] : typed social-feed records (built on
//!   [`invariants`] + [`integrity`]).

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod announce;
pub mod content;
pub mod error;
pub mod group_chat;
pub mod integrity;
pub mod invariants;
pub mod node;
pub mod peer_source;
pub mod range;
pub mod room;
pub mod social_feed;
pub mod ticket;
pub mod topic;
pub mod wallet_address;

pub use announce::{Announcement, AnnouncementPayload, MAX_ANNOUNCED_SIZE};
pub use content::{CdnContentKind, ContentHash};
pub use error::{AdnetError, Result};
pub use group_chat::{
    DirectChat, DirectMessage, GroupChat, GroupInvitation, GroupMember, GroupMessage,
    GroupSequence, MAX_SEQUENCE, MessageAttachment, MessageReceipt, UserSequence, Validate,
    attachment_from_hash, attachment_from_hash_str, next_group_message_id,
};
pub use integrity::{
    IntegrityScope, VerifyOutcome, direct_hash, group_hash, hash_fields, post_hash, verify_direct,
    verify_direct_bool, verify_group, verify_group_bool, verify_post, verify_post_bool,
};
pub use invariants::{
    AttachmentKind, InvitationStatus, MAX_ATTACHMENTS, MAX_CONTENT_LEN, MAX_ID_LEN, MAX_MEMBERS,
    MAX_MENTIONS, MAX_NAME_LEN, MAX_TAG_LEN, MAX_TAGS, MemberRole, MessageType, PostAttachmentKind,
    ReactionTarget, ReactionType, Sequence, Visibility, validate_content, validate_id,
    validate_name, validate_ordered, validate_tag, validate_url,
};
pub use node::{Endpoint, NodeAddr, NodeId, RelayUrl};
pub use peer_source::{
    MAX_CLOCK_SKEW_HOURS, MAX_PEER_SOURCES_PER_HASH, MAX_RTT_MS, MAX_TRACKED_HASHES, PeerMap,
    PeerSource,
};
pub use range::{ByteRange, RangeSpec};
pub use room::{RoomAsset, RoomId};
pub use social_feed::{
    FollowRelationship, PostAttachment, SocialComment, SocialPost, SocialReaction, VIS_FRIENDS,
    VIS_PRIVATE, VIS_PUBLIC, attachment_from_hash as post_attachment_from_hash,
    attachment_from_hash_str as post_attachment_from_hash_str,
};
pub use ticket::{BlobTicket, PeerTicket, SignedPeerTicket, validate_blob_ticket};
pub use topic::{Topic, topic_name};
pub use wallet_address::{WALLET_ADDRESS_LEN, WalletAddress};
