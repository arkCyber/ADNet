//! Re-export module so the public surface exposes the gossip types
//! under their declared names without requiring `crate::gossip::*`
//! in downstream code.

pub use crate::gossip::{
    Envelope, EnvelopeKind, SocialFeedBridge, SocialFeedGossipConfig, SocialFeedSubscriber,
};
