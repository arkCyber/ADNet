//! `a3net-eliza-bridge` — Eliza OS adapters for A3Net P2P network.
//!
//! Two adapters are exposed:
//!
//! - [`AdnetChatClient`] — lets an Eliza agent log into the A3Net
//!   "P2P WeChat" network as a regular user, send and receive
//!   direct / group messages, manage friends, and react to
//!   presence events.
//!
//! - [`AdnetFeedAdapter`] — lets an Eliza agent subscribe to news
//!   topics, publish AI-generated reports, and earn tips from
//!   readers.
//!
//! ## Quick start
//!
//! ```no_run
//! use a3net_eliza_bridge::{
//!     AdnetFeedAdapter, AdnetIdentity, FeedConfig,
//! };
//! use a3net_types::bulletin::BulletinCategory;
//!
//! # async fn run() -> anyhow::Result<()> {
//! let identity = AdnetIdentity::new("/tmp/agent".into(), "my-agent").await?;
//! let feed = AdnetFeedAdapter::new(identity, FeedConfig::default()).await?;
//! feed.connect().await?;
//! let id = feed.publish_report(
//!     "Weekly DeFi Pulse",
//!     "TVL rose 12% this week...",
//!     BulletinCategory::Tech,
//!     vec!["defi".to_string()],
//! ).await?;
//! # Ok(()) }
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod chat_client;
pub mod feed_adapter;
pub mod identity;
pub mod error;

pub use chat_client::{
    AdnetChatClient, ChatClientBuilder, ChatClientConfig, ChatEvent, ChatEventHandler,
    ChatWireMessage, ContactInfo, ConversationKind, ConversationSummary, ElizaChatMessage,
    ElizaTool, FriendRequest, UserSearchResult, open_storage,
};
pub use feed_adapter::{
    AdnetFeedAdapter, FeedAdapterBuilder, FeedConfig, FeedEvent, FeedItem, FeedTool, TipRecord,
};
pub use identity::{
    create_agent_profile, AdnetIdentity, AgentPreferences, AgentProfile, AgentType,
};
pub use error::{BridgeError, BridgeResult};
