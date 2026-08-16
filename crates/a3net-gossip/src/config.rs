//! Configurable gossip parameters exposed from `iroh_gossip`.
//!
//! [`GossipParams`] wraps the low-level
//! [`iroh_gossip::proto::HyparviewConfig`] and
//! [`iroh_gossip::proto::PlumtreeConfig`] with a smaller, typed surface
//! that callers can reasonably be expected to tune. Defaults match
//! `iroh_gossip`'s own defaults.
//!
//! ## Note on `#[cfg(feature = "iroh")]`
//!
//! The `From` conversions to iroh's native config types **cannot** live in this
//! module because `iroh_gossip::proto::hyparview::Ttl` and
//! `iroh_gossip::proto::plumtree::Round` are in private submodules. Instead,
//! the conversion happens inside `IrohGossipTransportBuilder::spawn` in
//! `iroh_transport.rs`, which has direct access to those submodules. Callers
//! who just want to tune parameters can use [`GossipParams`] as a plain data
//! struct — it is serializable via serde and can be stored in a config file
//! regardless of the `iroh` feature.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// HyParView configuration — peer sampling / membership layer.
///
/// Controls how peers discover each other and maintain their view
/// of the overlay topology.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HyParViewParams {
    /// Number of peers to maintain active connections to.
    /// Paper default: 5.
    pub active_view_capacity: usize,
    /// Number of peers to remember in the passive view.
    /// Paper default: 30.
    pub passive_view_capacity: usize,
    /// Number of hops a `ForwardJoin` propagates before the new
    /// peer is added to active views. Paper default: 6.
    pub active_random_walk_length: u8,
    /// Number of hops a `ForwardJoin` propagates before the new
    /// peer is added to passive views. Paper default: 3.
    pub passive_random_walk_length: u8,
    /// Number of hops a `Shuffle` message propagates before a peer replies.
    /// Paper default: 6.
    pub shuffle_random_walk_length: u8,
    /// Number of active peers to include in each `Shuffle` request.
    /// Paper default: 3.
    pub shuffle_active_view_count: usize,
    /// Number of passive peers to include in each `Shuffle` request.
    /// Paper default: 4.
    pub shuffle_passive_view_count: usize,
    /// Interval between periodic `Shuffle` requests.
    /// Default: 60 s.
    pub shuffle_interval: Duration,
    /// Timeout for a `Neighbor` request before it is considered failed.
    /// Default: 500 ms.
    pub neighbor_request_timeout: Duration,
}

impl Default for HyParViewParams {
    fn default() -> Self {
        Self {
            active_view_capacity: 5,
            passive_view_capacity: 30,
            active_random_walk_length: 6,
            passive_random_walk_length: 3,
            shuffle_random_walk_length: 6,
            shuffle_active_view_count: 3,
            shuffle_passive_view_count: 4,
            shuffle_interval: Duration::from_secs(60),
            neighbor_request_timeout: Duration::from_millis(500),
        }
    }
}

impl HyParViewParams {
    /// Set active view capacity.
    pub fn with_active_view_capacity(mut self, v: usize) -> Self {
        self.active_view_capacity = v;
        self
    }

    /// Set passive view capacity.
    pub fn with_passive_view_capacity(mut self, v: usize) -> Self {
        self.passive_view_capacity = v;
        self
    }

    /// Set shuffle interval.
    pub fn with_shuffle_interval(mut self, v: Duration) -> Self {
        self.shuffle_interval = v;
        self
    }
}

/// PlumTree configuration — epidemic broadcast layer.
///
/// Controls how messages are spread through the overlay once a peer
/// chooses to broadcast.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlumTreeParams {
    /// Timeout after which a `Graft` message is re-sent if no reply.
    /// Paper recommends "a few round trip times". Default: 80 ms.
    pub graft_timeout_1: Duration,
    /// Second graft timeout (must be smaller than `graft_timeout_1`).
    /// Default: 40 ms.
    pub graft_timeout_2: Duration,
    /// Timeout after which `IHave` messages are flushed to peers.
    /// Default: 5 ms.
    pub dispatch_timeout: Duration,
    /// Number of hops a lazy peer must be closer to the origin than
    /// our eager peers to be promoted to eager. Default: 7.
    pub optimization_threshold: u8,
    /// How long to keep gossip messages in the internal message cache.
    /// Default: 30 s.
    pub message_cache_retention: Duration,
    /// How long to keep `MessageId`s for received messages.
    /// Must be >= `message_cache_retention`. Default: 90 s.
    pub message_id_retention: Duration,
    /// How often to run cache eviction. Default: 1 s.
    pub cache_evict_interval: Duration,
}

impl Default for PlumTreeParams {
    fn default() -> Self {
        Self {
            graft_timeout_1: Duration::from_millis(80),
            graft_timeout_2: Duration::from_millis(40),
            dispatch_timeout: Duration::from_millis(5),
            optimization_threshold: 7,
            message_cache_retention: Duration::from_secs(30),
            message_id_retention: Duration::from_secs(90),
            cache_evict_interval: Duration::from_secs(1),
        }
    }
}

impl PlumTreeParams {
    /// Set first graft timeout.
    pub fn with_graft_timeout_1(mut self, v: Duration) -> Self {
        self.graft_timeout_1 = v;
        self
    }

    /// Set optimization threshold.
    pub fn with_optimization_threshold(mut self, v: u8) -> Self {
        self.optimization_threshold = v;
        self
    }
}

/// All configurable gossip parameters.
///
/// Construct with [`Default`], then override the fields you care about:
/// ```ignore
/// let params = GossipParams::default()
///     .with_hyparview(HyParViewParams {
///         active_view_capacity: 10,
///         ..Default::default()
///     });
/// ```
///
/// Then pass to [`crate::IrohGossipTransportBuilder::with_params`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GossipParams {
    pub hyparview: HyParViewParams,
    pub plumtree: PlumTreeParams,
}

impl Default for GossipParams {
    fn default() -> Self {
        Self {
            hyparview: HyParViewParams::default(),
            plumtree: PlumTreeParams::default(),
        }
    }
}

impl GossipParams {
    /// Set the HyParView (membership) parameters.
    pub fn with_hyparview(mut self, hyparview: HyParViewParams) -> Self {
        self.hyparview = hyparview;
        self
    }

    /// Set the PlumTree (broadcast) parameters.
    pub fn with_plumtree(mut self, plumtree: PlumTreeParams) -> Self {
        self.plumtree = plumtree;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gossip_params_default_roundtrip() {
        let params = GossipParams::default();
        assert_eq!(params.hyparview.active_view_capacity, 5);
        assert_eq!(params.plumtree.graft_timeout_1, Duration::from_millis(80));
        assert_eq!(params.plumtree.message_cache_retention, Duration::from_secs(30));
    }

    #[test]
    fn gossip_params_serde_roundtrip() {
        let params = GossipParams::default()
            .with_hyparview(HyParViewParams {
                active_view_capacity: 10,
                ..Default::default()
            });
        let json = serde_json::to_string(&params).unwrap();
        let back: GossipParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.hyparview.active_view_capacity, 10);
    }

    #[test]
    fn hyparview_params_builder_pattern() {
        let params = HyParViewParams::default()
            .with_active_view_capacity(20)
            .with_passive_view_capacity(100)
            .with_shuffle_interval(Duration::from_secs(30));
        assert_eq!(params.active_view_capacity, 20);
        assert_eq!(params.passive_view_capacity, 100);
        assert_eq!(params.shuffle_interval, Duration::from_secs(30));
    }

    #[test]
    fn plumtree_params_builder_pattern() {
        let params = PlumTreeParams::default()
            .with_graft_timeout_1(Duration::from_millis(200))
            .with_optimization_threshold(10);
        assert_eq!(params.graft_timeout_1, Duration::from_millis(200));
        assert_eq!(params.optimization_threshold, 10);
    }

    #[test]
    fn gossip_params_chained_builder() {
        let params = GossipParams::default()
            .with_plumtree(PlumTreeParams::default())
            .with_hyparview(HyParViewParams {
                active_view_capacity: 8,
                passive_view_capacity: 40,
                ..Default::default()
            });
        assert_eq!(params.hyparview.active_view_capacity, 8);
        assert_eq!(params.hyparview.passive_view_capacity, 40);
    }

    #[test]
    fn gossip_params_json_roundtrip() {
        let params = GossipParams::default()
            .with_hyparview(HyParViewParams {
                active_view_capacity: 7,
                ..Default::default()
            })
            .with_plumtree(PlumTreeParams {
                graft_timeout_1: Duration::from_millis(100),
                ..Default::default()
            });
        let json = serde_json::to_string_pretty(&params).unwrap();
        let back: GossipParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.hyparview.active_view_capacity, 7);
        assert_eq!(back.plumtree.graft_timeout_1, Duration::from_millis(100));
    }
}
