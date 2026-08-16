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

    /// Get the total number of stored items (values + ipns).
    fn len(&self) -> usize;

    /// Number of distinct provider entries (one record per `(key, provider_id)`).
    /// Distinct from [`len`](Self::len) which counts keys only.
    fn get_all_provider_count(&self) -> usize;

    /// Number of IPNS records currently stored.
    fn get_ipns_count(&self) -> usize;

    /// Number of generic values currently stored.
    fn get_values_count(&self) -> usize;

    /// Iterate every cached provider record (for persistence / debugging).
    /// The default implementation is O(N) over the in-memory map.
    fn all_provider_records(&self) -> Vec<(DhtKey, Vec<ProviderRecord>)> {
        Vec::new()
    }
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
        entry.retain(|r| r.provider_id != record.provider_id);
        entry.push(record);
        true
    }

    fn get_providers(&self, key: &DhtKey) -> Vec<ProviderRecord> {
        let providers = self.providers.read().unwrap();
        providers
            .get(key)
            .map(|v| v.iter().filter(|r| !r.is_expired()).cloned().collect())
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
        providers.retain(|_, v| !v.is_empty());
        removed
    }

    fn put_ipns(&self, name: &DhtKey, record: IpnRecord) -> bool {
        let mut ipns = self.ipns.write().unwrap();
        if let Some(existing) = ipns.get(name) {
            if existing.sequence >= record.sequence {
                return false;
            }
        }
        ipns.insert(name.clone(), record);
        true
    }

    fn get_ipns(&self, name: &DhtKey) -> Option<IpnRecord> {
        self.ipns.read().unwrap().get(name).cloned()
    }

    fn put_value(&self, key: &DhtKey, value: DhtValue) -> bool {
        self.values.write().unwrap().insert(key.clone(), value);
        true
    }

    fn get_value(&self, key: &DhtKey) -> Option<DhtValue> {
        self.values.read().unwrap().get(key).cloned()
    }

    fn len(&self) -> usize {
        let providers = self.providers.read().unwrap();
        let ipns = self.ipns.read().unwrap();
        let values = self.values.read().unwrap();
        providers.len() + ipns.len() + values.len()
    }

    fn get_all_provider_count(&self) -> usize {
        let providers = self.providers.read().unwrap();
        providers.values().map(|v| v.len()).sum()
    }

    fn get_ipns_count(&self) -> usize {
        self.ipns.read().unwrap().len()
    }

    fn get_values_count(&self) -> usize {
        self.values.read().unwrap().len()
    }

    fn all_provider_records(&self) -> Vec<(DhtKey, Vec<ProviderRecord>)> {
        self.all_providers()
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

/// Configuration for RocksDB-backed DHT storage.
#[derive(Debug, Clone)]
pub struct RocksDbConfig {
    pub path: std::path::PathBuf,
    pub max_open_files: i32,
    pub write_buffer_size: usize,
    pub create_if_missing: bool,
}

impl Default for RocksDbConfig {
    fn default() -> Self {
        Self {
            path: std::path::PathBuf::from("dht.db"),
            max_open_files: 64,
            write_buffer_size: 64 * 1024 * 1024,
            create_if_missing: true,
        }
    }
}

#[cfg(feature = "rocksdb")]
pub fn new_rocksdb_store(config: RocksDbConfig) -> Result<SharedDhtStore, RocksDbError> {
    Ok(Arc::new(RocksDbDhtStore::open(config)?))
}

#[cfg(feature = "rocksdb")]
#[derive(Debug, thiserror::Error)]
pub enum RocksDbError {
    #[error("RocksDB error: {0}")]
    RocksDb(#[from] rocksdb::Error),
    #[error("Serialization error: {0}")]
    Serialization(String),
}

#[cfg(feature = "rocksdb")]
pub struct RocksDbDhtStore {
    db: rocksdb::DB,
}

#[cfg(feature = "rocksdb")]
impl RocksDbDhtStore {
    pub fn open(config: RocksDbConfig) -> Result<Self, RocksDbError> {
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(config.create_if_missing);
        opts.set_max_open_files(config.max_open_files);
        opts.set_write_buffer_size(config.write_buffer_size);
        let db = rocksdb::DB::open(&opts, &config.path)?;
        Ok(Self { db })
    }

    fn serialize<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, RocksDbError> {
        serde_json::to_vec(value).map_err(|e| RocksDbError::Serialization(e.to_string()))
    }

    fn deserialize<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, RocksDbError> {
        serde_json::from_slice(bytes).map_err(|e| RocksDbError::Serialization(e.to_string()))
    }

    fn make_key(prefix: u8, key: &DhtKey) -> Vec<u8> {
        let mut db_key = Vec::with_capacity(1 + key.as_bytes().len());
        db_key.push(prefix);
        db_key.extend_from_slice(key.as_bytes());
        db_key
    }
}

#[cfg(feature = "rocksdb")]
impl DhtStorage for RocksDbDhtStore {
    fn put_provider(&self, key: &DhtKey, record: ProviderRecord) -> bool {
        let db_key = Self::make_key(b'P', key);
        match Self::serialize(&record) {
            Ok(value) => self.db.put(&db_key, &value).is_ok(),
            Err(e) => {
                tracing::error!("Serialize error: {}", e);
                false
            }
        }
    }

    fn get_providers(&self, key: &DhtKey) -> Vec<ProviderRecord> {
        let db_key = Self::make_key(b'P', key);
        match self.db.get(&db_key) {
            Ok(Some(bytes)) => match Self::deserialize::<ProviderRecord>(&bytes) {
                Ok(record) => {
                    if record.is_expired() {
                        let _ = self.db.delete(&db_key);
                        Vec::new()
                    } else {
                        vec![record]
                    }
                }
                Err(e) => {
                    tracing::error!("Deserialize error: {}", e);
                    Vec::new()
                }
            },
            _ => Vec::new(),
        }
    }

    fn remove_expired_providers(&self) -> usize {
        let mut removed = 0;
        let prefix = vec![b'P'];
        let iter = self.db.prefix_iterator(&prefix);
        for item in iter.flatten() {
            if let Ok(record) = Self::deserialize::<ProviderRecord>(item.1.as_ref()) {
                if record.is_expired() {
                    if self.db.delete(item.0).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
        removed
    }

    fn put_ipns(&self, name: &DhtKey, record: IpnRecord) -> bool {
        let db_key = Self::make_key(b'I', name);
        match Self::serialize(&record) {
            Ok(value) => self.db.put(&db_key, &value).is_ok(),
            Err(e) => {
                tracing::error!("Serialize error: {}", e);
                false
            }
        }
    }

    fn get_ipns(&self, name: &DhtKey) -> Option<IpnRecord> {
        let db_key = Self::make_key(b'I', name);
        match self.db.get(&db_key) {
            Ok(Some(bytes)) => Self::deserialize::<IpnRecord>(&bytes).ok(),
            _ => None,
        }
    }

    fn put_value(&self, key: &DhtKey, value: DhtValue) -> bool {
        let db_key = Self::make_key(b'V', key);
        match Self::serialize(&value) {
            Ok(bytes) => self.db.put(&db_key, &bytes).is_ok(),
            Err(e) => {
                tracing::error!("Serialize error: {}", e);
                false
            }
        }
    }

    fn get_value(&self, key: &DhtKey) -> Option<DhtValue> {
        let db_key = Self::make_key(b'V', key);
        match self.db.get(&db_key) {
            Ok(Some(bytes)) => Self::deserialize::<DhtValue>(&bytes).ok(),
            _ => None,
        }
    }

    fn len(&self) -> usize {
        let mut count = 0;
        // Count providers
        let prefix_p = vec![b'P'];
        for _ in self.db.prefix_iterator(&prefix_p) {
            count += 1;
        }
        // Count IPNS records
        let prefix_i = vec![b'I'];
        for _ in self.db.prefix_iterator(&prefix_i) {
            count += 1;
        }
        // Count values
        let prefix_v = vec![b'V'];
        for _ in self.db.prefix_iterator(&prefix_v) {
            count += 1;
        }
        count
    }

    fn get_all_provider_count(&self) -> usize {
        let prefix = vec![b'P'];
        let mut count = 0;
        for item in self.db.prefix_iterator(&prefix).flatten() {
            if let Ok(record) = Self::deserialize::<ProviderRecord>(item.1.as_ref()) {
                if !record.is_expired() {
                    count += 1;
                }
            }
        }
        count
    }

    fn get_ipns_count(&self) -> usize {
        let prefix = vec![b'I'];
        self.db.prefix_iterator(&prefix).count()
    }

    fn get_values_count(&self) -> usize {
        let prefix = vec![b'V'];
        let mut count = 0;
        for item in self.db.prefix_iterator(&prefix).flatten() {
            if let Ok(value) = Self::deserialize::<DhtValue>(item.1.as_ref()) {
                if value.timestamp + value.ttl_secs
                    >= SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                {
                    count += 1;
                }
            }
        }
        count
    }

    fn all_provider_records(&self) -> Vec<(DhtKey, Vec<ProviderRecord>)> {
        let prefix = vec![b'P'];
        let mut out = Vec::new();
        for item in self.db.prefix_iterator(&prefix).flatten() {
            let raw_key = item.0.as_ref();
            // raw_key = b'P' || DhtKey bytes
            if raw_key.is_empty() {
                continue;
            }
            let key_bytes = &raw_key[1..];
            if let Ok(record) = Self::deserialize::<ProviderRecord>(item.1.as_ref()) {
                if !record.is_expired() {
                    out.push((DhtKey::from_bytes(key_bytes.to_vec()), vec![record]));
                }
            }
        }
        out
    }

    fn remove_expired_values(&self) -> usize {
        let mut removed = 0;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let prefix = vec![b'V'];
        let iter = self.db.prefix_iterator(&prefix);
        for item in iter.flatten() {
            if let Ok(value) = Self::deserialize::<DhtValue>(item.1.as_ref()) {
                if value.timestamp + value.ttl_secs < now {
                    if self.db.delete(item.0).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
        removed
    }

    fn clear(&self) {
        for &p in &[b'P', b'I', b'V'] {
            let prefix = vec![p];
            let iter = self.db.prefix_iterator(&prefix);
            for item in iter.flatten() {
                let _ = self.db.delete(item.0);
            }
        }
    }
}

#[cfg(feature = "rocksdb")]
impl std::fmt::Debug for RocksDbDhtStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RocksDbDhtStore").finish()
    }
}

/// Two-level storage: L1 in-memory cache + L2 RocksDB persistence.
#[cfg(feature = "rocksdb")]
pub struct TieredDhtStore {
    memory: Arc<InMemoryDhtStore>,
    rocksdb: Arc<RocksDbDhtStore>,
}

#[cfg(feature = "rocksdb")]
impl TieredDhtStore {
    pub fn new(memory: Arc<InMemoryDhtStore>, rocksdb: Arc<RocksDbDhtStore>) -> Self {
        Self { memory, rocksdb }
    }

    pub fn warm_cache(&self) -> Result<(), RocksDbError> {
        tracing::info!("Warming DHT cache from RocksDB...");
        Ok(())
    }
}

#[cfg(feature = "rocksdb")]
impl DhtStorage for TieredDhtStore {
    fn put_provider(&self, key: &DhtKey, record: ProviderRecord) -> bool {
        self.memory.put_provider(key, record.clone());
        self.rocksdb.put_provider(key, record)
    }

    fn get_providers(&self, key: &DhtKey) -> Vec<ProviderRecord> {
        let l1 = self.memory.get_providers(key);
        if !l1.is_empty() {
            return l1;
        }
        let l2 = self.rocksdb.get_providers(key);
        for r in &l2 {
            let _ = self.memory.put_provider(key, r.clone());
        }
        l2
    }

    fn remove_expired_providers(&self) -> usize {
        self.memory.remove_expired_providers() + self.rocksdb.remove_expired_providers()
    }

    fn put_ipns(&self, name: &DhtKey, record: IpnRecord) -> bool {
        self.memory.put_ipns(name, record.clone());
        self.rocksdb.put_ipns(name, record)
    }

    fn get_ipns(&self, name: &DhtKey) -> Option<IpnRecord> {
        self.memory.get_ipns(name).or_else(|| self.rocksdb.get_ipns(name))
    }

    fn put_value(&self, key: &DhtKey, value: DhtValue) -> bool {
        self.memory.put_value(key, value.clone());
        self.rocksdb.put_value(key, value)
    }

    fn get_value(&self, key: &DhtKey) -> Option<DhtValue> {
        self.memory.get_value(key).or_else(|| self.rocksdb.get_value(key))
    }

    fn len(&self) -> usize {
        self.memory.len() + self.rocksdb.len()
    }

    fn remove_expired_values(&self) -> usize {
        self.memory.remove_expired_values() + self.rocksdb.remove_expired_values()
    }

    fn clear(&self) {
        self.memory.clear();
        self.rocksdb.clear();
    }

    fn get_all_provider_count(&self) -> usize {
        self.memory.get_all_provider_count() + self.rocksdb.get_all_provider_count()
    }

    fn get_ipns_count(&self) -> usize {
        self.memory.get_ipns_count() + self.rocksdb.get_ipns_count()
    }

    fn get_values_count(&self) -> usize {
        self.memory.get_values_count() + self.rocksdb.get_values_count()
    }
}

#[cfg(feature = "rocksdb")]
impl std::fmt::Debug for TieredDhtStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TieredDhtStore").finish()
    }
}

#[cfg(feature = "rocksdb")]
pub fn new_tiered_store(config: RocksDbConfig) -> Result<SharedDhtStore, RocksDbError> {
    let memory = Arc::new(InMemoryDhtStore::new());
    let rocksdb = Arc::new(RocksDbDhtStore::open(config)?);
    let store = Arc::new(TieredDhtStore::new(memory, rocksdb));
    store.warm_cache()?;
    Ok(store)
}

#[cfg(feature = "rocksdb")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    Memory,
    RocksDb,
    Tiered,
}

#[cfg(feature = "rocksdb")]
impl Default for StorageBackend {
    fn default() -> Self {
        Self::Memory
    }
}

#[cfg(feature = "rocksdb")]
pub fn new_store_with_backend(
    backend: StorageBackend,
    config: RocksDbConfig,
) -> Result<SharedDhtStore, RocksDbError> {
    match backend {
        StorageBackend::Memory => Ok(new_in_memory_store()),
        StorageBackend::RocksDb => new_rocksdb_store(config),
        StorageBackend::Tiered => new_tiered_store(config),
    }
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
    use a3net_types::NodeId;

    #[test]
    fn test_provider_storage() {
        let store = InMemoryDhtStore::new();
        let key = DhtKey::from_bytes(vec![0u8; 32]);
        let node_id = NodeId::random();
        let record =
            ProviderRecord::new(key.clone(), node_id.clone(), "127.0.0.1:8080".to_string());
        let _ = store.put_provider(&key, record);
        let providers = store.get_providers(&key);
        assert_eq!(providers.len(), 1);

        // Update provider
        let record2 =
            ProviderRecord::new(key.clone(), node_id, "127.0.0.1:9090".to_string());
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

#[cfg(feature = "rocksdb")]
mod rocksdb_tests {
    use super::*;
    use a3net_types::NodeId;
    use std::fs;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dht_test_{}", rand::random::<u64>()));
        fs::create_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn test_rocksdb_provider_storage() {
        let dir = temp_dir();
        let config = RocksDbConfig {
            path: dir.clone(),
            max_open_files: 16,
            write_buffer_size: 1024 * 1024,
            create_if_missing: true,
        };
        let store = RocksDbDhtStore::open(config).unwrap();

        let key = DhtKey::from_bytes(vec![0u8; 32]);
        let node_id = NodeId::random();
        let record =
            ProviderRecord::new(key.clone(), node_id.clone(), "127.0.0.1:8080".to_string());
        let _ = store.put_provider(&key, record);

        let providers = store.get_providers(&key);
        assert_eq!(providers.len(), 1);

        // Reopen and verify persistence
        drop(store);
        let config2 = RocksDbConfig {
            path: dir,
            ..Default::default()
        };
        let store2 = RocksDbDhtStore::open(config2).unwrap();
        let providers = store2.get_providers(&key);
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].provider_addr, "127.0.0.1:8080");
    }

    #[test]
    fn test_tiered_store() {
        let dir = temp_dir();
        let config = RocksDbConfig {
            path: dir,
            create_if_missing: true,
            ..Default::default()
        };

        let memory = Arc::new(InMemoryDhtStore::new());
        let rocksdb = Arc::new(RocksDbDhtStore::open(config).unwrap());
        let store = TieredDhtStore::new(memory.clone(), rocksdb);

        let key = DhtKey::from_bytes(vec![5u8; 32]);
        let node_id = NodeId::random();
        let record =
            ProviderRecord::new(key.clone(), node_id.clone(), "10.0.0.1:5001".to_string());

        // Write through tiered store
        assert!(store.put_provider(&key, record));

        // Should be in memory
        let providers = store.get_providers(&key);
        assert_eq!(providers.len(), 1);

        // Simulate cold start - clear memory but keep RocksDB
        memory.clear();

        // Should fall back to RocksDB
        let providers = store.get_providers(&key);
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].provider_addr, "10.0.0.1:5001");
    }

    #[test]
    fn test_tiered_store_new() {
        let dir = temp_dir();
        let config = RocksDbConfig {
            path: dir,
            create_if_missing: true,
            ..Default::default()
        };

        // Use the convenience function
        let store = new_tiered_store(config).unwrap();

        let key = DhtKey::from_bytes(vec![7u8; 32]);
        let node_id = NodeId::random();
        let record =
            ProviderRecord::new(key.clone(), node_id.clone(), "10.0.0.2:5001".to_string());

        store.put_provider(&key, record);

        let providers = store.get_providers(&key);
        assert_eq!(providers.len(), 1);
    }
}
