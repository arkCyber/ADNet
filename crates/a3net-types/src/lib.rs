//! `a3net-types` — foundational types shared across the A3Net workspace.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod announce;
pub mod bulletin;
pub mod contacts;
pub mod cid;
pub mod content;
pub mod dag_codec;
pub mod error;
pub mod graphsync;
pub mod group_chat;
pub mod integrity;
pub mod invariants;
pub mod mesh;
pub mod multihash_local;
pub use multihash_local as multihash;
pub mod node;
pub mod node_identity;
pub mod node_identity_card;
pub mod node_profile;
pub mod peer_source;
pub mod pb;
pub mod range;
pub mod room;
pub mod social_feed;
pub mod ticket;
pub mod topic;
pub mod unixfs;
pub mod virtual_ip;
pub mod wallet_address;

pub use announce::{Announcement, AnnouncementPayload, MAX_ANNOUNCED_SIZE};
pub use bulletin::{
    BulletinAttachment, BulletinCategory, BulletinId, BulletinItem, BulletinKind, BulletinSeverity,
};
pub use content::{CdnContentKind, ContentHash};
pub use contacts::{
    ContactEntry, ContactSource, ContactsList, ContactsListError, ReputationTier,
    DEFAULT_REPUTATION, MAX_CONTACTS, MAX_CONTACT_NICKNAME_LEN, MAX_REPUTATION,
    MIN_REPUTATION,
};
pub use dag_codec::{DagCodec, DagCodecRegistry, DagError, DagLinkRef, extract_links, dag_size, is_directory, link_count};
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
pub use mesh::{
    InviteCode, InviteCodeRef, MeshMember, MeshMembership, MeshNetworkId, MeshPolicy,
    MeshRosterSigner, MeshRosterVerifier, MeshTopology, verify_roster_signature,
};
pub use node::{Endpoint, NodeAddr, NodeId, RelayUrl};
pub use node_identity::{
    Avatar, DnsNodeId, MAX_AVATAR_DATA_LEN, MAX_AVATAR_URL_LEN, MAX_NICKNAME_LEN,
    MAX_NODE_DESCRIPTION_LEN, MAX_NODE_FUNCTIONS, MAX_NODE_FUNCTION_CUSTOM_LEN, NodeFunction,
    NodeIdentity, NodeIdentityError, NodeKind, DNS_NODE_ID_DIGITS, DNS_NODE_ID_MAX,
    default_functions_for_kind, validate_email,
};
pub use node_identity_card::{NODE_IDENTITY_CARD_VERSION, NodeIdentityCard};
pub use node_profile::{
    MAX_PROFILE_DESC_LEN, MAX_PROFILE_TAGS, NodeCapability, NodeProfile, NodeResources, NodeRole,
    NODE_CAPABILITY_NONE,
};
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
pub use ticket::{BlobTicket, NodeAddrTicket, PeerTicket, SignedPeerTicket, validate_blob_ticket};
pub use topic::{Topic, topic_name};
pub use virtual_ip::{VirtualIp, VirtualIpv4};
pub use wallet_address::{WALLET_ADDRESS_LEN, WalletAddress};
