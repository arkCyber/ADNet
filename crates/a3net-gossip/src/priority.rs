//! Multi-source retrieval strategies for gossip + local cache.
//!
//! Mirrors `RetrievalStrategy` from
//! `Exodus@src-backup/.../microservice/p2p_group_coordinator.rs`. The same
//! enum drives [`a3net_gossip::bus::GossipBus`] when the local node needs to
//! fall back from one source to another (e.g. P2P → cloud assistant, or
//! local cache → peers).
//!
//! The strategy is deliberately decoupled from the concrete transport: it's
//! a *policy*, not a mechanism.

use serde::{Deserialize, Serialize};

/// Where a message came from, ranked by freshness / trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    /// Already in our local SQLite / blob store.
    LocalCache = 0,
    /// From a directly-connected P2P peer.
    Peer = 1,
    /// From a relay / cloud-assistant fallback.
    Relay = 2,
}

impl MessageSource {
    /// Convenience predicate used by callers to decide whether to keep
    /// searching after a successful answer.
    pub fn is_local(self) -> bool {
        matches!(self, Self::LocalCache)
    }
}

/// How to chase a missing message (or asset) across the available sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalStrategy {
    /// Local cache first; fall back to peers, then relay.
    LocalFirst,
    /// Peers first; fall back to local cache, then relay.
    #[default]
    PeerFirst,
    /// Peers only — fail loudly if no peer has the data.
    PeerOnly,
    /// Relay only — useful in offline / restricted-network modes.
    RelayOnly,
}

impl RetrievalStrategy {
    /// Return the next source to try, or `None` if the strategy is
    /// exhausted.
    ///
    /// `available` is the set of sources we know we can reach right now.
    pub fn next_source(&self, available: &[MessageSource]) -> Option<MessageSource> {
        let order: &[MessageSource] = match self {
            Self::LocalFirst => &[
                MessageSource::LocalCache,
                MessageSource::Peer,
                MessageSource::Relay,
            ],
            Self::PeerFirst => &[
                MessageSource::Peer,
                MessageSource::LocalCache,
                MessageSource::Relay,
            ],
            Self::PeerOnly => &[MessageSource::Peer],
            Self::RelayOnly => &[MessageSource::Relay],
        };
        order.iter().copied().find(|s| available.contains(s))
    }

    /// After a successful retrieval from `source`, decide whether we should
    /// still consult higher-priority sources (e.g. to update local cache).
    pub fn should_continue_after_success(&self, source: MessageSource) -> bool {
        match self {
            Self::LocalFirst => !source.is_local(),
            Self::PeerFirst => source == MessageSource::Relay,
            Self::PeerOnly | Self::RelayOnly => false,
        }
    }
}

/// Compute the next [`RetrievalStrategy`] given the live peer / cloud
/// availability snapshot. Mirrors `P2pGroupCoordinator::determine_strategy`.
pub fn determine_strategy(
    prefer_p2p: bool,
    peers_online: usize,
    cloud_available: bool,
) -> RetrievalStrategy {
    match (prefer_p2p, peers_online > 0, cloud_available) {
        (true, true, _) => RetrievalStrategy::PeerFirst,
        (true, false, true) => RetrievalStrategy::RelayOnly,
        (true, false, false) => RetrievalStrategy::PeerOnly, // will fail but signal intent
        (false, _, true) => RetrievalStrategy::RelayOnly,
        (false, _, false) => RetrievalStrategy::PeerFirst, // fall back to P2P
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_first_picks_peer_when_available() {
        let s = RetrievalStrategy::PeerFirst;
        let available = vec![MessageSource::LocalCache, MessageSource::Peer];
        assert_eq!(s.next_source(&available), Some(MessageSource::Peer));
    }

    #[test]
    fn peer_first_falls_back_to_relay() {
        let s = RetrievalStrategy::PeerFirst;
        let available = vec![MessageSource::Relay];
        assert_eq!(s.next_source(&available), Some(MessageSource::Relay));
    }

    #[test]
    fn peer_only_returns_none_without_peer() {
        let s = RetrievalStrategy::PeerOnly;
        let available = vec![MessageSource::Relay, MessageSource::LocalCache];
        assert_eq!(s.next_source(&available), None);
    }

    #[test]
    fn continue_after_success_logic() {
        assert!(!RetrievalStrategy::PeerFirst.should_continue_after_success(MessageSource::Peer));
        assert!(RetrievalStrategy::PeerFirst.should_continue_after_success(MessageSource::Relay));
        assert!(RetrievalStrategy::LocalFirst.should_continue_after_success(MessageSource::Peer));
        assert!(
            !RetrievalStrategy::LocalFirst.should_continue_after_success(MessageSource::LocalCache)
        );
    }

    #[test]
    fn determine_strategy_matches_exodus() {
        // Exodus: prefer_p2p=true + peers > 0 + cloud ok → P2PFirst
        assert_eq!(
            determine_strategy(true, 2, true),
            RetrievalStrategy::PeerFirst
        );
        // Exodus: prefer_p2p=true + no peers + cloud ok → CloudFirst
        assert_eq!(
            determine_strategy(true, 0, true),
            RetrievalStrategy::RelayOnly
        );
        // Exodus: prefer_p2p=false + no peers + cloud ok → CloudFirst
        assert_eq!(
            determine_strategy(false, 0, true),
            RetrievalStrategy::RelayOnly
        );
        // Exodus: prefer_p2p=false + no peers + no cloud → P2PFirst (fallback)
        assert_eq!(
            determine_strategy(false, 0, false),
            RetrievalStrategy::PeerFirst
        );
    }
}
