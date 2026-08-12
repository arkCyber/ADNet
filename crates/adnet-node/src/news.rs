//! News + authoritative announcement seam for `adnet-node`.
//!
//! Wraps [`adnet_news::NewsService`] so callers can:
//!
//! * publish signed alerts / news / authoritative announcements
//! * fetch a paginated timeline per room
//! * subscribe to the live event stream
//! * replay the persisted history after a restart
//!
//! The handle is feature-gated behind `feature = "news"` so crates
//! that don't need the bulletin layer don't pull in `rusqlite` or
//! the gossip envelope code.

use std::path::PathBuf;
use std::sync::Arc;

use adnet_gossip::GossipBus;
use adnet_news::{
    BulletinEnvelope, BulletinEvent, BulletinItem, BulletinStore, NewsService,
    NewsServiceConfig, ValidationPolicy,
};
use adnet_types::{BulletinId, NodeId, RoomId};
use thiserror::Error;
use tokio::sync::broadcast;

/// Configuration for the news layer on a node.
#[derive(Debug, Clone)]
pub struct NewsNodeConfig {
    /// SQLite directory passed through to the underlying store.
    pub storage_dir: PathBuf,
    /// Validation policy applied to inbound envelopes. Defaults
    /// to [`ValidationPolicy::Strict`].
    pub policy: ValidationPolicy,
    /// Fan-out size for the local event broadcast channel.
    pub event_channel_capacity: usize,
}

impl Default for NewsNodeConfig {
    fn default() -> Self {
        Self {
            storage_dir: std::env::temp_dir().join("adnet-news"),
            policy: ValidationPolicy::Strict,
            event_channel_capacity: 1024,
        }
    }
}

/// Errors surfaced by the news seam.
#[derive(Debug, Error)]
pub enum NewsNodeError {
    #[error("news: gossip bus not initialised on this node")]
    NoGossipBus,
    #[error("news: service not initialised — call `enable_news` first")]
    NotInitialised,
    #[error("news: store error: {0}")]
    Store(String),
    #[error("news: gossip transport error: {0}")]
    Gossip(String),
    #[error("news: validation error: {0}")]
    Validation(String),
    #[error("news: serde error: {0}")]
    Serde(String),
}

/// Cheap-to-clone handle around the node's `NewsService`.
#[derive(Debug, Clone)]
pub struct NewsHandle {
    inner: Arc<NewsService>,
}

impl NewsHandle {
    /// Wrap an already-built [`NewsService`]. Useful for tests
    /// that construct the service directly; production callers
    /// should use `Node::enable_news` instead.
    pub fn from_service(svc: NewsService) -> Self {
        Self {
            inner: Arc::new(svc),
        }
    }

    /// Local node id this service publishes under.
    pub fn local_node(&self) -> &NodeId {
        self.inner.local_node()
    }

    /// Validation policy in effect.
    pub fn policy(&self) -> ValidationPolicy {
        self.inner.policy()
    }

    /// Direct access to the underlying store (for tests, debugging).
    pub fn store(&self) -> &BulletinStore {
        self.inner.store()
    }

    /// Publish a bulletin authored by the local node.
    pub async fn publish(&self, item: BulletinItem) -> Result<BulletinItem, NewsNodeError> {
        self.inner
            .publish(item)
            .await
            .map_err(news_to_seam_error)
    }

    /// Subscribe to the local event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<BulletinEvent> {
        self.inner.subscribe()
    }

    /// Fetch a paginated timeline (newest first).
    pub fn timeline(
        &self,
        room: &RoomId,
        before_seq: Option<u32>,
        limit: usize,
    ) -> Result<Vec<adnet_news::StoredBulletin>, NewsNodeError> {
        self.inner
            .timeline(room, before_seq, limit)
            .map_err(news_to_seam_error)
    }

    /// Look up a single bulletin.
    pub fn get(
        &self,
        room: &RoomId,
        id: &BulletinId,
    ) -> Result<Option<adnet_news::StoredBulletin>, NewsNodeError> {
        self.inner.get(room, id).map_err(news_to_seam_error)
    }

    /// Mark a bulletin read by the local node.
    pub fn mark_read(&self, room: &RoomId, id: &BulletinId) -> Result<(), NewsNodeError> {
        self.inner
            .mark_read(room, id)
            .map_err(news_to_seam_error)
    }

    /// Join a room on the gossip transport so remote bulletins
    /// flow into this service's store.
    pub async fn join_room(&self, room: &RoomId) -> Result<(), NewsNodeError> {
        self.inner
            .join_room(room)
            .await
            .map_err(news_to_seam_error)
    }

    /// Ingest a single envelope received from a peer.
    pub async fn ingest_envelope(
        &self,
        envelope: BulletinEnvelope,
    ) -> Result<BulletinItem, NewsNodeError> {
        self.inner
            .ingest_envelope(envelope)
            .await
            .map_err(news_to_seam_error)
    }
}

/// Public helper — build a [`NewsService`] from a node's gossip
/// bus + config. The owning node typically calls this from
/// `enable_news`; tests can call it directly.
pub fn build_news_service(
    bus: &GossipBus,
    config: NewsNodeConfig,
) -> Result<NewsService, NewsNodeError> {
    let svc_config = NewsServiceConfig {
        store_dir: config.storage_dir,
        policy: config.policy,
        event_channel_capacity: config.event_channel_capacity,
    };
    NewsService::open(bus.local_node().clone(), bus.transport().clone(), svc_config)
        .map_err(news_to_seam_error)
}

fn news_to_seam_error(e: adnet_news::NewsError) -> NewsNodeError {
    use adnet_news::NewsError;
    match e {
        NewsError::Store(s) => NewsNodeError::Store(s.to_string()),
        NewsError::Gossip(s) => NewsNodeError::Gossip(s),
        NewsError::Validation(s) => NewsNodeError::Validation(s),
        NewsError::Serde(s) => NewsNodeError::Serde(s.to_string()),
        other => NewsNodeError::Validation(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_gossip::InProcessGossip;

    #[test]
    fn config_default_uses_strict_policy() {
        let cfg = NewsNodeConfig::default();
        assert_eq!(cfg.policy, ValidationPolicy::Strict);
    }

    #[test]
    fn build_news_service_uses_bus_transport() {
        let bus = GossipBus::new(NodeId::random(), Arc::new(InProcessGossip::new()));
        let dir = std::env::temp_dir().join(format!(
            "adnet-news-node-test-{}-{}",
            std::process::id(),
            chrono::Utc::now()
                .timestamp_nanos_opt()
                .unwrap_or(0)
        ));
        let cfg = NewsNodeConfig {
            storage_dir: dir,
            policy: ValidationPolicy::Lenient,
            event_channel_capacity: 64,
        };
        let svc = build_news_service(&bus, cfg).expect("build");
        assert_eq!(svc.policy(), ValidationPolicy::Lenient);
        assert_eq!(svc.local_node(), bus.local_node());
    }
}
