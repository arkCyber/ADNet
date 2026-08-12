//! DHT storage layer for provider records and IPNS records.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use crate::record::{DhtKey, DhtValue, IpnRecord, ProviderRecord};

/// Storage backend for DHT records.
pub trait DhtStorage: Send + Sync {
    /// Store a provider record.
    fn put_provider(&self, key: &DhtKey, record: ProviderRecord) -> bool;

    /// Get providers for a key.
    fn get_providers(&self, key: &DhtKey) -> Vec<ProviderRecord>;

    /// Remove expired providers.
    fn remove_expired_providers(&self) -> usize;

    /// Store an IPNS record.
    fn put_ipns(&self, name: &DhtKey, record: IpnRecord) -> bool;

    /// Get an IPNS record.
    fn get_ipns(&self, name: &DhtKey) -> Option<IpnRecord>;

    /// Store a generic DHT value.
    fn put_value(&self, key: &DhtKey, value: DhtValue) -> bool;

    /// Get a generic DHT value.
    fn get_value(&self, key: &DhtKey) -> Option<DhtValue>;

    /// Remove expired values.
    fn remove_expired_values(&self) -> usize;

    /// Clear all records (for testing).
    fn clear(&self);
}

/// In-memory DHT storage implementation.
#[derive(Debug, Default)]
pub struct InMemoryDhtStore {
    providers: RwLock<HashMap<DhtKey, Vec<ProviderRecord>>>,
    ipns: RwLock<HashMap<DhtKey, IpnRecord>>,
    values: RwLock<HashMap<DhtKey, DhtValue>>,
}

impl InMemoryDhtStore {
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
            ipns: RwLock::new(HashMap::new()),
            values: RwLock::new(HashMap::new()),
        }
    }

    /// Get the number of provider entries.
    pub fn num_providers(&self) -> usize {
        self.providers.read().unwrap().len()
    }

    /// Get the number of IPNS entries.
    pub fn num_ipns(&self) -> usize {
        self.ipns.read().unwrap().len()
    }

    /// Get all provider records (for debugging).
    pub fn all_providers(&self) -> Vec<(DhtKey, Vec<ProviderRecord>)> {
        let guard = self.providers.read().unwrap();
        guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}

impl DhtStorage for InMemoryDhtStore {
    fn put_provider(&self, key: &DhtKey, record: ProviderRecord) -> bool {
        let mut providers = self.providers.write().unwrap();
        let entry = providers.entry(key.clone()).or_insert_with(Vec::new);

        // Remove existing record for same provider
        entry.retain(|r| r.provider_id != record.provider_id);

        // Add new record
        entry.push(record);
        true
    }

    fn get_providers(&self, key: &DhtKey) -> Vec<ProviderRecord> {
        let providers = self.providers.read().unwrap();
        providers
            .get(key)
            .map(|v| {
                v.iter()
                    .filter(|r| !r.is_expired())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn remove_expired_providers(&self) -> usize {
        let mut providers = self.providers.write().unwrap();
        let mut removed = 0;

        for entry in providers.values_mut() {
            let before = entry.len();
            entry.retain(|r| !r.is_expired());
            removed += before - entry.len();
        }

        // Remove empty entries
        providers.retain(|_, v| !v.is_empty());

        removed
    }

    fn put_ipns(&self, name: &DhtKey, record: IpnRecord) -> bool {
        let mut ipns = self.ipns.write().unwrap();

        // Check if we should update (newer sequence number)
        if let Some(existing) = ipns.get(name) {
            if existing.sequence >= record.sequence {
                return false; // Don't overwrite with older record
            }
        }

        ipns.insert(name.clone(), record);
        true
    }

    fn get_ipns(&self, name: &DhtKey) -> Option<IpnRecord> {
        let ipns = self.ipns.read().unwrap();
        ipns.get(name).cloned()
    }

    fn put_value(&self, key: &DhtKey, value: DhtValue) -> bool {
        let mut values = self.values.write().unwrap();
        values.insert(key.clone(), value);
        true
    }

    fn get_value(&self, key: &DhtKey) -> Option<DhtValue> {
        let values = self.values.read().unwrap();
        values.get(key).cloned()
    }

    fn remove_expired_values(&self) -> usize {
        let mut values = self.values.write().unwrap();
        let mut removed = 0;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        values.retain(|_, v| {
            let expired = v.timestamp + v.ttl_secs < now;
            if expired {
                removed += 1;
            }
            !expired
        });

        removed
    }

    fn clear(&self) {
        self.providers.write().unwrap().clear();
        self.ipns.write().unwrap().clear();
        self.values.write().unwrap().clear();
    }
}

/// Thread-safe wrapper for DHT storage.
pub type SharedDhtStore = Arc<dyn DhtStorage>;

/// Create a new in-memory DHT store.
pub fn new_in_memory_store() -> SharedDhtStore {
    Arc::new(InMemoryDhtStore::new())
}

/// Periodic cleanup task for expired records.
pub async fn cleanup_task(store: SharedDhtStore, interval: Duration) {
    let mut interval = tokio::time::interval(interval);
    loop {
        interval.tick().await;

        let providers_removed = store.remove_expired_providers();
        let values_removed = store.remove_expired_values();

        if providers_removed > 0 || values_removed > 0 {
            tracing::debug!(
                "DHT cleanup: removed {} providers, {} values",
                providers_removed,
                values_removed
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::NodeId;

    #[test]
    fn test_provider_storage() {
        let store = InMemoryDhtStore::new();

        let key = DhtKey::from_bytes(vec![0u8; 32]);
        let node_id = NodeId::random();
        let record = ProviderRecord::new(
            key.clone(),
            node_id.clone(),
            "127.0.0.1:8080".to_string(),
        );

        let _ = store.put_provider(&key, record);
        let providers = store.get_providers(&key);
        assert_eq!(providers.len(), 1);

        // Update provider
        let record2 = ProviderRecord::new(
            key.clone(),
            node_id,
            "127.0.0.1:9090".to_string(),
        );
        let _ = store.put_provider(&key, record2);
        let providers = store.get_providers(&key);
        assert_eq!(providers.len(), 1); // Should replace, not add
        assert_eq!(providers[0].provider_addr, "127.0.0.1:9090");
    }

    #[test]
    fn test_ipns_storage() {
        let store = InMemoryDhtStore::new();

        let name = DhtKey::from_bytes(vec![1u8; 32]);
        let mut record = IpnRecord::new(name.clone(), "/ipfs/Qm...".to_string());

        let _ = store.put_ipns(&name, record.clone());

        let retrieved = store.get_ipns(&name).unwrap();
        assert_eq!(retrieved.value, "/ipfs/Qm...");

        // Update with higher sequence
        record.update("/ipfs/QmNew...".to_string());
        let _ = store.put_ipns(&name, record);

        let retrieved = store.get_ipns(&name).unwrap();
        assert_eq!(retrieved.value, "/ipfs/QmNew...");
        assert_eq!(retrieved.sequence, 2);
    }

    #[test]
    fn test_ipns_sequence_ordering() {
        let store = InMemoryDhtStore::new();

        let name = DhtKey::from_bytes(vec![2u8; 32]);

        // Insert older record first
        let mut old_record = IpnRecord::new(name.clone(), "/ipfs/old".to_string());
        old_record.sequence = 1;
        let _ = store.put_ipns(&name, old_record);

        // Try to insert even older record
        let mut newer_record = IpnRecord::new(name.clone(), "/ipfs/newer".to_string());
        newer_record.sequence = 0; // Older than current (1)
        let _ = store.put_ipns(&name, newer_record);

        // Should still have the first record
        let retrieved = store.get_ipns(&name).unwrap();
        assert_eq!(retrieved.sequence, 1);
    }
}
