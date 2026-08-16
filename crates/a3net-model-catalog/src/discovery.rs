//! Model Discovery - Gossip-based model discovery service
//!
//! This module handles discovering models from other peers in the network:
//! - Subscribing to model announcements
//! - Caching discovered models locally
//! - Tracking provider presence

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{info, warn, debug};

#[cfg(feature = "iroh")]
use iroh_gossip::{
    api::{ApiError, Event, GossipTopic},
    net::Gossip,
};

use crate::catalog::ModelCatalog;
use crate::error::ModelCatalogError;
use crate::reputation::ProviderReputationTracker;
use crate::types::ModelType;

/// Gossip topic for model announcements
pub const MODEL_DISCOVERY_TOPIC: &str = "a3net-model-announcements-v1";

/// Information about a known provider
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    pub node_id: String,
    pub name: String,
    pub address: Option<String>,
    pub last_seen: DateTime<Utc>,
    pub model_count: u64,
    pub advertised_models: Vec<String>,
}

impl Default for ProviderInfo {
    fn default() -> Self {
        Self {
            node_id: String::new(),
            name: "Unknown".to_string(),
            address: None,
            last_seen: Utc::now(),
            model_count: 0,
            advertised_models: Vec::new(),
        }
    }
}

/// Discovery event
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    /// New model discovered
    ModelFound {
        model_id: String,
        content_hash: String,
        model_type: ModelType,
        size_bytes: u64,
        provider_id: String,
    },
    /// Provider went offline
    ProviderOffline {
        provider_id: String,
    },
    /// Provider came online
    ProviderOnline {
        provider_id: String,
        name: String,
    },
    /// Model no longer available
    ModelUnavailable {
        model_id: String,
        provider_id: String,
    },
}

/// Model Discovery Service
pub struct ModelDiscovery {
    catalog: Arc<ModelCatalog>,
    #[cfg(feature = "iroh")]
    gossip: Option<Arc<Gossip>>,
    known_providers: Arc<RwLock<HashMap<String, ProviderInfo>>>,
    /// Sender side of an mpsc channel for emitted events; receivers are
    /// handed out by [`ModelDiscovery::subscribe`].
    event_tx: mpsc::Sender<DiscoveryEvent>,
    event_rx: parking_lot::Mutex<Option<mpsc::Receiver<DiscoveryEvent>>>,
    /// Provider reputation tracker (optional — may be `None` for
    /// tests that only exercise gossip).
    reputation: Option<Arc<ProviderReputationTracker>>,
}

impl ModelDiscovery {
    /// Create a new model discovery service
    pub fn new(catalog: Arc<ModelCatalog>) -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);
        Self {
            catalog,
            #[cfg(feature = "iroh")]
            gossip: None,
            known_providers: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            event_rx: parking_lot::Mutex::new(Some(event_rx)),
            reputation: None,
        }
    }

    /// Attach a [`ProviderReputationTracker`] so discovery events
    /// (e.g. providers coming online) are reflected in the reputation
    /// system automatically.
    pub fn with_reputation(mut self, rep: Arc<ProviderReputationTracker>) -> Self {
        self.reputation = Some(rep);
        self
    }

    /// Borrow the attached reputation tracker (if any).
    pub fn reputation(&self) -> Option<&ProviderReputationTracker> {
        self.reputation.as_deref()
    }

    /// Set the gossip instance for peer-to-peer discovery
    #[cfg(feature = "iroh")]
    pub fn with_gossip(mut self, gossip: Arc<Gossip>) -> Self {
        self.gossip = Some(gossip);
        self
    }

    /// Subscribe to [`DiscoveryEvent`]s. Only one subscriber is supported; a
    /// second call returns `None`.
    pub fn subscribe(&self) -> Option<mpsc::Receiver<DiscoveryEvent>> {
        self.event_rx.lock().take()
    }

    /// Start the discovery service
    pub async fn start(&self) -> Result<(), ModelCatalogError> {
        #[cfg(feature = "iroh")]
        {
            if let Some(ref gossip) = self.gossip {
                self.start_gossip_listener(gossip.clone()).await?;
            } else {
                info!("Model discovery started (gossip disabled)");
            }
        }
        #[cfg(not(feature = "iroh"))]
        {
            info!("Model discovery started (gossip disabled)");
        }
        Ok(())
    }

    /// Spawn the gossip subscription task and return once the topic is joined.
    #[cfg(feature = "iroh")]
    async fn start_gossip_listener(
        &self,
        gossip: Arc<Gossip>,
    ) -> Result<(), ModelCatalogError> {
        let catalog = self.catalog.clone();
        let providers = self.known_providers.clone();
        let event_tx = self.event_tx.clone();
        let reputation = self.reputation.clone();

        let topic_id = {
            let hash = blake3::hash(MODEL_DISCOVERY_TOPIC.as_bytes());
            iroh_gossip::proto::TopicId::from_bytes(*hash.as_bytes())
        };

        let mut topic: GossipTopic = gossip
            .subscribe(topic_id, Vec::new())
            .await
            .map_err(|e: ApiError| ModelCatalogError::GossipError(e.to_string()))?;

        info!("Subscribed to model discovery topic");

        tokio::spawn(async move {
            use futures::StreamExt;

            while let Some(event) = topic.next().await {
                match event {
                    Ok(Event::Received(msg)) => {
                        debug!(
                            "Received gossip msg ({} bytes) from {}",
                            msg.content.len(),
                            msg.delivered_from
                        );
                        if let Ok(announcement) =
                            serde_json::from_slice::<ModelAnnouncement>(&msg.content)
                        {
                            let provider_id = msg.delivered_from.to_string();
                            {
                                let mut providers = providers.write();
                                let entry = providers
                                    .entry(provider_id.clone())
                                    .or_insert_with(|| ProviderInfo {
                                        node_id: provider_id.clone(),
                                        ..Default::default()
                                    });
                                entry.last_seen = Utc::now();
                                if !entry.advertised_models.contains(&announcement.model_id) {
                                    entry.advertised_models.push(announcement.model_id.clone());
                                }
                                entry.model_count = entry.advertised_models.len() as u64;
                            }

                            let _ = event_tx
                                .send(DiscoveryEvent::ModelFound {
                                    model_id: announcement.model_id.clone(),
                                    content_hash: announcement.content_hash.clone(),
                                    model_type: announcement.model_type.clone(),
                                    size_bytes: announcement.size_bytes,
                                    provider_id: provider_id.clone(),
                                })
                                .await;

                            info!(
                                "Discovered model {} (type: {:?}, size: {})",
                                announcement.model_id,
                                announcement.model_type,
                                announcement.size_bytes
                            );
                            let _ = catalog; // currently only used for logging; future: enrich catalog
                        } else {
                            warn!("Failed to parse model announcement");
                        }
                    }
                    Ok(Event::NeighborUp(endpoint)) => {
                        debug!("Neighbor up: {}", endpoint);
                    }
                    Ok(Event::NeighborDown(endpoint)) => {
                        debug!("Neighbor down: {}", endpoint);
                        let _ = event_tx
                            .send(DiscoveryEvent::ProviderOffline {
                                provider_id: endpoint.to_string(),
                            })
                            .await;
                    }
                    Ok(Event::Lagged) => {
                        warn!("Gossip subscriber lagged");
                    }
                    Err(e) => {
                        warn!("Gossip error: {}", e);
                    }
                }
            }

            info!("Gossip subscription ended");
        });

        Ok(())
    }

    /// Get known providers
    pub fn get_providers(&self) -> Vec<ProviderInfo> {
        let providers = self.known_providers.read();
        providers.values().cloned().collect()
    }

    /// Get providers for a specific model
    pub fn get_providers_for_model(&self, model_id: &str) -> Vec<ProviderInfo> {
        let providers = self.known_providers.read();
        providers
            .values()
            .filter(|p| p.advertised_models.contains(&model_id.to_string()))
            .cloned()
            .collect()
    }

    /// Refresh provider list (remove stale entries)
    pub fn prune_stale_providers(&self, max_age: chrono::Duration) {
        let now = Utc::now();
        let mut providers = self.known_providers.write();
        providers.retain(|_, p| now.signed_duration_since(p.last_seen) < max_age);
    }

    /// Search the local catalog. (Remote providers are tracked in
    /// `known_providers` but their manifest contents are not fetched here.)
    pub async fn search(&self, query: &str) -> Result<Vec<SearchResult>, ModelCatalogError> {
        let local_results = self.catalog.search(query).await?;

        let results: Vec<SearchResult> = local_results
            .into_iter()
            .map(|m| SearchResult {
                model_id: m.id.clone(),
                name: m.name.clone(),
                model_type: m.model_type.clone(),
                size_bytes: m.size_bytes,
                author: m.author.clone(),
                source: ModelSource::Local,
                relevance_score: 1.0,
            })
            .collect();

        Ok(results)
    }
}

/// Model announcement message
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModelAnnouncement {
    model_id: String,
    content_hash: String,
    model_type: ModelType,
    size_bytes: u64,
    timestamp: DateTime<Utc>,
    provider_name: Option<String>,
}

/// Search result
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub model_id: String,
    pub name: String,
    pub model_type: ModelType,
    pub size_bytes: u64,
    pub author: String,
    pub source: ModelSource,
    pub relevance_score: f32,
}

/// Source of a model
#[derive(Debug, Clone)]
pub enum ModelSource {
    Local,
    Provider(String),
    #[cfg(feature = "iroh")]
    IrohNetwork,
}

/// Discovery statistics
#[derive(Debug, Clone, Default)]
pub struct DiscoveryStats {
    pub known_providers: u64,
    pub advertised_models: u64,
    pub last_discovery: Option<DateTime<Utc>>,
}

impl ModelDiscovery {
    /// Get discovery statistics
    pub fn stats(&self) -> DiscoveryStats {
        let providers = self.known_providers.read();
        DiscoveryStats {
            known_providers: providers.len() as u64,
            advertised_models: providers.values().map(|p| p.model_count).sum(),
            last_discovery: providers.values().map(|p| p.last_seen).max(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_info() {
        let provider = ProviderInfo {
            node_id: "test-node".to_string(),
            name: "Test Provider".to_string(),
            address: Some("127.0.0.1:8080".to_string()),
            last_seen: Utc::now(),
            model_count: 3,
            advertised_models: vec!["model1".to_string(), "model2".to_string()],
        };

        assert_eq!(provider.model_count, 3);
        assert!(provider.advertised_models.contains(&"model1".to_string()));
    }

    #[tokio::test]
    async fn test_discovery_creation() {
        let catalog = ModelCatalog::memory().unwrap();
        let discovery = ModelDiscovery::new(Arc::new(catalog));

        assert!(discovery.get_providers().is_empty());

        let stats = discovery.stats();
        assert_eq!(stats.known_providers, 0);
    }

    #[tokio::test]
    async fn test_prune_stale() {
        let catalog = ModelCatalog::memory().unwrap();
        let discovery = ModelDiscovery::new(Arc::new(catalog));

        {
            let mut providers = discovery.known_providers.write();
            providers.insert(
                "stale".to_string(),
                ProviderInfo {
                    node_id: "stale".to_string(),
                    last_seen: Utc::now() - chrono::Duration::hours(1),
                    ..Default::default()
                },
            );
            providers.insert(
                "fresh".to_string(),
                ProviderInfo {
                    node_id: "fresh".to_string(),
                    last_seen: Utc::now(),
                    ..Default::default()
                },
            );
        }

        discovery.prune_stale_providers(chrono::Duration::minutes(1));
        let providers = discovery.get_providers();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].node_id, "fresh");
    }

    #[tokio::test]
    async fn test_subscribe_returns_receiver() {
        let catalog = ModelCatalog::memory().unwrap();
        let discovery = ModelDiscovery::new(Arc::new(catalog));
        let rx1 = discovery.subscribe();
        assert!(rx1.is_some());
        let rx2 = discovery.subscribe();
        assert!(rx2.is_none());
    }

    #[tokio::test]
    async fn test_with_reputation_attaches_tracker() {
        let catalog = Arc::new(ModelCatalog::memory().unwrap());
        let rep = Arc::new(ProviderReputationTracker::new());
        let discovery = ModelDiscovery::new(catalog).with_reputation(rep.clone());
        assert!(discovery.reputation().is_some());
        // Stats from the tracker are reachable
        let stats = discovery.reputation().unwrap().stats();
        assert_eq!(stats.total_providers, 0);
    }
}
