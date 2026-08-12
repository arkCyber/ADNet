//! Provider broadcast module - announces content to the network.
//!
//! This module provides a simple provider announcement system that can be
//! integrated with the GossipBus or other network transport.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adnet_types::ContentHash;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Room ID for provider announcements.
pub const PROVIDER_ROOM: &str = "adnet-providers";

/// A provider announcement message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAnnouncement {
    /// Content hash being provided.
    pub content_hash: ContentHash,
    /// Provider's node ID (as hex string).
    pub provider_id: String,
    /// Provider's address.
    pub provider_addr: String,
    /// Timestamp of the announcement.
    pub timestamp: u64,
    /// TTL in seconds.
    pub ttl_secs: u64,
}

impl ProviderAnnouncement {
    /// Create a new provider announcement.
    pub fn new(content_hash: ContentHash, provider_id: String, provider_addr: String) -> Self {
        Self {
            content_hash,
            provider_id,
            provider_addr,
            timestamp: current_timestamp(),
            ttl_secs: 3600,
        }
    }

    /// Create with custom TTL.
    pub fn with_ttl(mut self, ttl_secs: u64) -> Self {
        self.ttl_secs = ttl_secs;
        self
    }

    /// Check if the announcement is expired.
    pub fn is_expired(&self) -> bool {
        current_timestamp() > self.timestamp + self.ttl_secs
    }
}

/// Provider cache for tracking network announcements.
pub struct ProviderCache {
    /// Map of content hash to known providers.
    providers: RwLock<HashMap<ContentHash, Vec<ProviderAnnouncement>>>,
    /// Cache TTL.
    ttl: Duration,
}

impl ProviderCache {
    /// Create a new provider cache.
    pub fn new(ttl: Duration) -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    /// Add a provider announcement.
    pub fn add(&self, announcement: ProviderAnnouncement) {
        let mut providers = self.providers.write();
        let entry = providers
            .entry(announcement.content_hash.clone())
            .or_default();

        // Remove expired announcements
        entry.retain(|a| !a.is_expired());

        // Add new announcement if not duplicate
        let is_duplicate = entry
            .iter()
            .any(|a| a.provider_id == announcement.provider_id);
        if !is_duplicate {
            entry.push(announcement);
        }
    }

    /// Get providers for content.
    pub fn get(&self, content_hash: &ContentHash) -> Vec<ProviderAnnouncement> {
        let providers = self.providers.read();
        providers
            .get(content_hash)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Check if we have any providers for content.
    pub fn has_provider(&self, content_hash: &ContentHash) -> bool {
        let providers = self.providers.read();
        providers
            .get(content_hash)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Clean up expired entries.
    pub fn cleanup(&self) {
        let mut providers = self.providers.write();
        for (_, entries) in providers.iter_mut() {
            entries.retain(|a| !a.is_expired());
        }
        providers.retain(|_, v| !v.is_empty());
    }

    /// Get total number of cached providers.
    pub fn len(&self) -> usize {
        let providers = self.providers.read();
        providers.values().map(|v| v.len()).sum()
    }
}

/// Broadcasts provider announcements.
pub struct ProviderBroadcaster {
    /// Provider cache.
    cache: Arc<ProviderCache>,
    /// Announcement sender.
    announcement_tx: broadcast::Sender<ProviderAnnouncement>,
}

impl ProviderBroadcaster {
    /// Create a new provider broadcaster.
    pub fn new(cache: Arc<ProviderCache>) -> (Self, broadcast::Receiver<ProviderAnnouncement>) {
        let (announcement_tx, announcement_rx) = broadcast::channel(100);
        let broadcaster = Self {
            cache,
            announcement_tx,
        };
        (broadcaster, announcement_rx)
    }

    /// Announce content to the network.
    pub async fn announce(
        &self,
        content_hash: &ContentHash,
        provider_id: String,
        provider_addr: String,
    ) {
        let announcement =
            ProviderAnnouncement::new(content_hash.clone(), provider_id, provider_addr);

        // Add to local cache
        self.cache.add(announcement.clone());

        // Broadcast
        let _ = self.announcement_tx.send(announcement);
    }

    /// Handle incoming provider announcement.
    pub fn handle_announcement(&self, announcement: ProviderAnnouncement) {
        self.cache.add(announcement);
    }

    /// Subscribe to provider announcements.
    pub fn subscribe(&self) -> broadcast::Receiver<ProviderAnnouncement> {
        self.announcement_tx.subscribe()
    }
}

/// Get current Unix timestamp.
fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_announcement() {
        let content = ContentHash::from_bytes(b"test-content");

        let announcement = ProviderAnnouncement::new(
            content.clone(),
            "QmTest123".to_string(),
            "/ip4/127.0.0.1/tcp/4001".to_string(),
        );

        assert_eq!(announcement.content_hash, content);
        assert!(!announcement.is_expired());
    }

    #[test]
    fn test_provider_cache() {
        let cache = ProviderCache::new(Duration::from_secs(3600));
        let content = ContentHash::from_bytes(b"test-content");

        let announcement = ProviderAnnouncement::new(
            content.clone(),
            "QmTest123".to_string(),
            "/ip4/127.0.0.1/tcp/4001".to_string(),
        );

        // Add provider
        cache.add(announcement);
        assert!(cache.has_provider(&content));

        // Get providers
        let providers = cache.get(&content);
        assert_eq!(providers.len(), 1);
    }

    #[test]
    fn test_provider_cache_deduplication() {
        let cache = ProviderCache::new(Duration::from_secs(3600));
        let content = ContentHash::from_bytes(b"test-content");

        // Add same provider multiple times
        for _ in 0..3 {
            let announcement = ProviderAnnouncement::new(
                content.clone(),
                "QmTest123".to_string(),
                "/ip4/127.0.0.1/tcp/4001".to_string(),
            );
            cache.add(announcement);
        }

        // Should only have one entry
        let providers = cache.get(&content);
        assert_eq!(providers.len(), 1);
    }
}
