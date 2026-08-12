//! IPNS service for gateway integration.
//!
//! This module provides IPNS (InterPlanetary Naming System) support for the gateway,
//! enabling mutable content addressing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adnet_blobstore::BlobStore;
use adnet_namespace::{
    Ed25519SecretKey, Ed25519Verifier, IpnRecord, IpnTransport, SecretKey,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;

/// IPNS service errors.
#[derive(Debug, Error)]
pub enum IpnServiceError {
    #[error("record not found: {0}")]
    NotFound(String),

    #[error("invalid record: {0}")]
    InvalidRecord(String),

    #[error("signature verification failed")]
    InvalidSignature,

    #[error("record expired")]
    Expired,

    #[error("internal error: {0}")]
    Internal(String),

    #[error("permission denied")]
    PermissionDenied,
}

/// IPNS record metadata for API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpnRecordInfo {
    pub name: String,
    pub value: String,
    pub sequence: u64,
    pub ttl_secs: u64,
    pub created: String,
    pub expires: String,
    pub validity: String,
}

/// IPNS resolve result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpnResolveResult {
    pub path: String,
    pub response_path: Option<String>,
}

/// IPNS publish result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpnPublishResult {
    pub name: String,
    pub value: String,
}

/// IPNS service for managing IPNS names.
pub struct IpnService {
    #[allow(dead_code)]
    blob_store: Arc<BlobStore>,
    local_records: Arc<RwLock<HashMap<String, IpnRecord>>>,
    secret_key: Arc<dyn SecretKey>,
    ipns_name: String,
    db_path: PathBuf,
    /// Pluggable transport chain (DHT, gossip, Pkarr, …).
    /// `publish` fans out to every transport; `resolve` queries
    /// them in order when local storage misses.
    transports: Vec<Arc<dyn IpnTransport>>,
}

impl IpnService {
    /// Create a new IPNS service.
    pub fn new(
        blob_store: Arc<BlobStore>,
        data_dir: PathBuf,
        secret_key: Option<Arc<dyn SecretKey>>,
    ) -> Self {
        Self::with_transports(blob_store, data_dir, secret_key, Vec::new())
    }

    /// Create an IPNS service that fans out publishes to the
    /// provided transport chain (DHT / gossip / Pkarr). Pass an
    /// empty vec to keep the service local-only.
    pub fn with_transports(
        blob_store: Arc<BlobStore>,
        data_dir: PathBuf,
        secret_key: Option<Arc<dyn SecretKey>>,
        transports: Vec<Arc<dyn IpnTransport>>,
    ) -> Self {
        let secret_key = secret_key.unwrap_or_else(|| {
            Arc::new(Ed25519SecretKey::generate()) as Arc<dyn SecretKey>
        });

        let ipns_name = Self::derive_ipns_name(&*secret_key);

        Self {
            blob_store,
            local_records: Arc::new(RwLock::new(HashMap::new())),
            secret_key,
            ipns_name,
            db_path: data_dir.join("ipns_records.json"),
            transports,
        }
    }

    /// Register an additional transport after construction.
    pub fn add_transport(&mut self, transport: Arc<dyn IpnTransport>) {
        self.transports.push(transport);
    }

    /// Derive IPNS name from secret key.
    fn derive_ipns_name(secret_key: &dyn SecretKey) -> String {
        use std::fmt::Write;
        let pubkey = secret_key.public_key_bytes();
        let hash = blake3::hash(&pubkey);
        // Convert hash to hex string manually
        let hex: String = hash.as_bytes().iter().take(32).fold(String::new(), |mut acc, b| {
            let _ = write!(&mut acc, "{:02x}", b);
            acc
        });
        format!("k51{}", &hex[..59])
    }

    /// Get the local IPNS name (based on public key).
    pub fn local_name(&self) -> &str {
        &self.ipns_name
    }

    /// Load IPNS records from disk.
    pub async fn load(&self) -> Result<(), IpnServiceError> {
        if !self.db_path.exists() {
            return Ok(());
        }

        let data = tokio::fs::read(&self.db_path)
            .await
            .map_err(|e| IpnServiceError::Internal(e.to_string()))?;

        let records: Vec<IpnRecord> = serde_json::from_slice(&data)
            .map_err(|e| IpnServiceError::Internal(e.to_string()))?;

        let mut local = self.local_records.write().await;
        for record in records {
            local.insert(record.name.clone(), record);
        }

        Ok(())
    }

    /// Save IPNS records to disk.
    pub async fn save(&self) -> Result<(), IpnServiceError> {
        let local = self.local_records.read().await;
        let records: Vec<&IpnRecord> = local.values().collect();

        let data = serde_json::to_vec_pretty(&records)
            .map_err(|e| IpnServiceError::Internal(e.to_string()))?;

        tokio::fs::write(&self.db_path, data)
            .await
            .map_err(|e| IpnServiceError::Internal(e.to_string()))?;

        Ok(())
    }

    /// Publish a new value for an IPNS name.
    pub async fn publish(
        &self,
        value: String,
        ttl: Option<Duration>,
    ) -> Result<IpnPublishResult, IpnServiceError> {
        let ttl = ttl.unwrap_or(Duration::from_secs(86400)); // Default 24 hours

        let mut record = {
            let local = self.local_records.read().await;
            local.get(&self.ipns_name).cloned().unwrap_or_else(|| {
                IpnRecord::new(self.ipns_name.clone(), value.clone(), ttl)
            })
        };

        // Update the record
        record.update(value.clone());
        record.set_ttl(ttl);

        // Sign the record
        record.sign(&*self.secret_key)
            .map_err(|e| IpnServiceError::Internal(e.to_string()))?;

        // Store locally
        {
            let mut local = self.local_records.write().await;
            local.insert(self.ipns_name.clone(), record.clone());
        }

        // Persist to disk
        self.save().await?;

        // Fan out to every transport (DHT, gossip, Pkarr, …).
        // Errors are logged but do not fail the publish — at
        // least one successful storage (local + disk) has
        // already happened, so the user-facing call succeeds
        // even if every transport rejects.
        self.fanout_publish(&record).await;

        Ok(IpnPublishResult {
            name: self.ipns_name.clone(),
            value,
        })
    }

    /// Publish `record` to every registered transport. Best-effort.
    async fn fanout_publish(&self, record: &IpnRecord) {
        if self.transports.is_empty() {
            return;
        }
        let transports = self.transports.clone();
        for transport in transports {
            let record = record.clone();
            // Spawn so a slow transport doesn't block the others.
            tokio::spawn(async move {
                if let Err(e) = transport.publish(&record).await {
                    tracing::warn!(
                        backend = transport.name(),
                        name = %record.name,
                        error = %e,
                        "IPN transport publish failed"
                    );
                }
            });
        }
    }

    /// Resolve an IPNS name to its current value.
    pub async fn resolve(&self, name: &str) -> Result<IpnResolveResult, IpnServiceError> {
        // 1. Local records (fresh).
        if let Some(resolved) = self.resolve_local_cached(name).await? {
            return Ok(resolved);
        }

        // 2. Transports, in order. The first transport that
        //    returns a fresh record wins.
        for transport in &self.transports {
            match self.resolve_via_transport(name, transport.clone()).await {
                Ok(Some(resolved)) => {
                    // Cache for next time, enforcing
                    // sequence-monotonicity via the namespace
                    // resolver's rules.
                    if let Some(record) = self.fetch_record_via(name, transport.clone()).await {
                        let mut local = self.local_records.write().await;
                        self.insert_monotonic(&mut local, record);
                    }
                    return Ok(resolved);
                }
                Ok(None) => continue,
                Err(e) => {
                    tracing::debug!(
                        backend = transport.name(),
                        name = %name,
                        error = %e,
                        "IPN transport resolve failed"
                    );
                    continue;
                }
            }
        }

        Err(IpnServiceError::NotFound(name.to_string()))
    }

    /// Resolve from local cache. Returns `Ok(Some(...))` if a
    /// fresh record exists, `Ok(None)` if no record or it
    /// expired.
    async fn resolve_local_cached(
        &self,
        name: &str,
    ) -> Result<Option<IpnResolveResult>, IpnServiceError> {
        let local = self.local_records.read().await;
        if let Some(record) = local.get(name) {
            if record.is_expired() {
                return Ok(None);
            }
            return Ok(Some(IpnResolveResult {
                path: record.value.clone(),
                response_path: None,
            }));
        }
        Ok(None)
    }

    /// Query a single transport for an IPNS record. Pulls the
    /// first record from the transport's subscribe stream and
    /// turns it into a `IpnResolveResult`.
    async fn resolve_via_transport(
        &self,
        name: &str,
        transport: Arc<dyn IpnTransport>,
    ) -> Result<Option<IpnResolveResult>, IpnServiceError> {
        match self.fetch_record_via(name, transport).await {
            Some(record) => {
                if record.is_expired() {
                    return Ok(None);
                }
                Ok(Some(IpnResolveResult {
                    path: record.value.clone(),
                    response_path: None,
                }))
            }
            None => Ok(None),
        }
    }

    /// Subscribe to a transport and pull the first record. We
    /// impose a short timeout so a misbehaving transport can't
    /// stall the resolver.
    async fn fetch_record_via(
        &self,
        name: &str,
        transport: Arc<dyn IpnTransport>,
    ) -> Option<IpnRecord> {
        let mut stream = match transport.subscribe(name).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(backend = transport.name(), error = %e, "IPN subscribe failed");
                return None;
            }
        };
        // 3-second budget per transport so the worst-case
        // resolve latency stays bounded.
        match tokio::time::timeout(Duration::from_secs(3), stream.next()).await {
            Ok(Some(Ok(record))) => Some(record),
            Ok(Some(Err(e))) => {
                tracing::debug!(backend = transport.name(), error = %e, "IPN stream error");
                None
            }
            Ok(None) => None,
            Err(_) => {
                tracing::debug!(backend = transport.name(), name = %name, "IPN resolve timeout");
                None
            }
        }
    }

    /// Insert a record into the local cache, applying the
    /// sequence-monotonicity rule: an incoming record only
    /// replaces an existing one if its sequence is strictly
    /// greater. Mirrors [`adnet_namespace::IpnResolver::cache_record`].
    fn insert_monotonic(
        &self,
        local: &mut HashMap<String, IpnRecord>,
        record: IpnRecord,
    ) {
        match local.get(&record.name) {
            Some(existing) if record.sequence <= existing.sequence => {
                // Older or equal — drop.
            }
            _ => {
                local.insert(record.name.clone(), record);
            }
        }
    }

    /// Resolve from local storage only (no network lookup).
    /// Retained for backwards compatibility with callers that
    /// explicitly want to bypass the transport chain.
    #[allow(dead_code)]
    async fn resolve_local_only(&self, name: &str) -> Result<IpnResolveResult, IpnServiceError> {
        let local = self.local_records.read().await;

        if let Some(record) = local.get(name) {
            if record.is_expired() {
                return Err(IpnServiceError::Expired);
            }
            return Ok(IpnResolveResult {
                path: record.value.clone(),
                response_path: None,
            });
        }

        Err(IpnServiceError::NotFound(name.to_string()))
    }

    /// List all local IPNS records.
    pub async fn list_local(&self) -> Vec<IpnRecordInfo> {
        let local = self.local_records.read().await;

        local
            .values()
            .filter(|r| !r.is_expired())
            .map(|r| self.record_to_info(r))
            .collect()
    }

    /// Get record info by name.
    pub async fn get_record(&self, name: &str) -> Option<IpnRecordInfo> {
        let local = self.local_records.read().await;
        local.get(name).map(|r| self.record_to_info(r))
    }

    /// Convert IpnRecord to IpnRecordInfo.
    fn record_to_info(&self, record: &IpnRecord) -> IpnRecordInfo {
        let validity = if record.is_expired() {
            "expired".to_string()
        } else {
            "valid".to_string()
        };

        IpnRecordInfo {
            name: record.name.clone(),
            value: record.value.clone(),
            sequence: record.sequence,
            ttl_secs: record.ttl_secs,
            created: unix_timestamp_to_rfc3339(record.created),
            expires: unix_timestamp_to_rfc3339(record.expires),
            validity,
        }
    }

    /// Verify a signature on an IPNS record using the record's
    /// embedded public key (the IPNS name is `blake3(pubkey)`, so the
    /// caller is responsible for the inverse: deriving the pubkey
    /// from the trusted name before calling this). The previous
    /// implementation was a placeholder that returned `true` for any
    /// non-empty signature — a real Ed25519 check is now required.
    pub fn verify_record(&self, record: &IpnRecord, pubkey_bytes: &[u8]) -> bool {
        let pk_arr: [u8; 32] = match pubkey_bytes.try_into() {
            Ok(a) => a,
            Err(_) => return false,
        };
        match Ed25519Verifier::from_bytes(&pk_arr) {
            Ok(verifier) => record.verify_signature(&verifier),
            Err(_) => false,
        }
    }

    /// Export the public key for sharing.
    pub fn public_key_bytes(&self) -> Vec<u8> {
        self.secret_key.public_key_bytes()
    }
}

/// Convert Unix timestamp to RFC3339 string.
fn unix_timestamp_to_rfc3339(timestamp: u64) -> String {
    use std::time::UNIX_EPOCH;
    let duration = std::time::Duration::from_secs(timestamp);
    let _datetime = UNIX_EPOCH + duration; // Used implicitly by format

    // Simple ISO 8601 format
    let secs = duration.as_secs();
    let remaining = secs % 86400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;

    // Approximate RFC3339 (without timezone info)
    format!("1970-01-01T{:02}:{:02}:{:02}Z", hours, minutes, seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ipns_service_publish_resolve() {
        let temp_dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            adnet_blobstore::BlobStore::new(temp_dir.path()).unwrap()
        );

        let service = IpnService::new(
            blob_store,
            temp_dir.path().to_path_buf(),
            None,
        );

        // Publish a value
        let result = service.publish("/ipfs/QmTest123".to_string(), None).await.unwrap();
        assert_eq!(result.name, service.local_name());

        // Resolve the value
        let resolved = service.resolve(&service.local_name()).await.unwrap();
        assert_eq!(resolved.path, "/ipfs/QmTest123");
    }

    #[tokio::test]
    async fn test_ipns_service_update() {
        let temp_dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            adnet_blobstore::BlobStore::new(temp_dir.path()).unwrap()
        );

        let service = IpnService::new(
            blob_store,
            temp_dir.path().to_path_buf(),
            None,
        );

        // Publish initial value
        service.publish("/ipfs/QmOld".to_string(), None).await.unwrap();

        // Update to new value
        service.publish("/ipfs/QmNew".to_string(), None).await.unwrap();

        // Resolve should return the new value
        let resolved = service.resolve(&service.local_name()).await.unwrap();
        assert_eq!(resolved.path, "/ipfs/QmNew");
    }

    #[tokio::test]
    async fn test_ipns_list_local() {
        let temp_dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            adnet_blobstore::BlobStore::new(temp_dir.path()).unwrap()
        );

        let service = IpnService::new(
            blob_store,
            temp_dir.path().to_path_buf(),
            None,
        );

        // Publish a value
        service.publish("/ipfs/QmTest".to_string(), None).await.unwrap();

        // List should return the record
        let records = service.list_local().await;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].value, "/ipfs/QmTest");
    }

    /// Regression test for the `SimpleVerifier::verify = true` hole.
    /// The previous placeholder accepted any non-empty signature;
    /// the new Ed25519 verifier must reject tampered records and
    /// records signed by the wrong key.
    #[test]
    fn verify_record_uses_real_ed25519() {
        use adnet_namespace::Ed25519SecretKey;
        let signer = Ed25519SecretKey::generate();
        let pk = signer.public_key_bytes();
        let mut record =
            IpnRecord::with_name_value(signer.ipns_name(), "/ipfs/QmReal".into());
        record.sign(&signer).expect("sign");
        // Need an IpnService to call verify_record; build a dummy one.
        let temp_dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            adnet_blobstore::BlobStore::new(temp_dir.path()).unwrap(),
        );
        let service = IpnService::new(
            blob_store,
            temp_dir.path().to_path_buf(),
            None,
        );
        // Correct pubkey: must verify.
        assert!(service.verify_record(&record, &pk));

        // Wrong pubkey: must NOT verify.
        let other = Ed25519SecretKey::generate();
        assert!(!service.verify_record(&record, &other.public_key_bytes()));

        // Tampered value: must NOT verify.
        let mut tampered = record.clone();
        tampered.value = "/ipfs/QmReplaced".into();
        assert!(!service.verify_record(&tampered, &pk));

        // Wrong pubkey length: must NOT verify.
        assert!(!service.verify_record(&record, &[0u8; 16]));
        assert!(!service.verify_record(&record, &[]));
    }

    /// Resolve via the DHT transport: publish through one
    /// service (no transports — local-only), then ask a second
    /// service that has *only* a DHT transport (no local record)
    /// to resolve. The second service must find the record
    /// through the DHT.
    #[tokio::test]
    async fn resolve_via_dht_transport() {
        // We use the in-process DHT transport from adnet-namespace.
        use adnet_namespace::{DhtIpnTransport, IpnPublisher};
        use std::sync::Arc;

        let store = adnet_dht::store::new_in_memory_store();
        let transport: Arc<dyn adnet_namespace::IpnTransport> =
            Arc::new(DhtIpnTransport::local(store.clone()));

        // Service 1: local-only publisher. Publishes a record,
        // then explicitly publishes it via the DHT transport.
        let temp_dir_1 = tempfile::tempdir().unwrap();
        let _blob_store_1 = Arc::new(
            adnet_blobstore::BlobStore::new(temp_dir_1.path()).unwrap(),
        );
        let secret = adnet_namespace::Ed25519SecretKey::generate();
        let name = secret.ipns_name();
        let publisher =
            IpnPublisher::new(Arc::new(secret) as Arc<dyn adnet_namespace::SecretKey>);
        let record = publisher
            .publish(
                &name,
                "/ipfs/QmDhtPath".into(),
                std::time::Duration::from_secs(60),
            )
            .expect("sign+publish");
        transport.publish(&record).await.expect("publish to DHT");

        // Service 2: empty cache, only the DHT transport. It
        // must find the record through the transport.
        let temp_dir_2 = tempfile::tempdir().unwrap();
        let blob_store_2 = Arc::new(
            adnet_blobstore::BlobStore::new(temp_dir_2.path()).unwrap(),
        );
        let service2 = IpnService::with_transports(
            blob_store_2,
            temp_dir_2.path().to_path_buf(),
            None,
            vec![transport],
        );

        let resolved = service2.resolve(&name).await.expect("resolve");
        assert_eq!(resolved.path, "/ipfs/QmDhtPath");
    }

    /// Without any transports registered, an unknown name
    /// resolves to `NotFound` — no network, no record.
    #[tokio::test]
    async fn resolve_without_transports_returns_not_found() {
        let temp_dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            adnet_blobstore::BlobStore::new(temp_dir.path()).unwrap(),
        );
        let service = IpnService::new(
            blob_store,
            temp_dir.path().to_path_buf(),
            None,
        );
        let err = service
            .resolve("k51qzi5uqu5notreallyaname")
            .await
            .unwrap_err();
        assert!(matches!(err, IpnServiceError::NotFound(_)));
    }

    /// Publish should fan out to every registered transport
    /// without failing if a transport is unhappy. We use a
    /// simple stub transport that records the call.
    #[tokio::test]
    async fn publish_fans_out_to_all_transports() {
        use adnet_namespace::IpnTransport;
        use adnet_namespace::TransportHealth;
        use std::pin::Pin;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingTransport {
            name: &'static str,
            calls: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl IpnTransport for CountingTransport {
            fn name(&self) -> &'static str {
                self.name
            }
            async fn publish(
                &self,
                _record: &adnet_namespace::IpnRecord,
            ) -> Result<(), adnet_namespace::IpnsError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            async fn subscribe(
                &self,
                _name: &str,
            ) -> Result<
                Pin<
                    Box<
                        dyn futures::Stream<
                                Item = Result<adnet_namespace::IpnRecord, adnet_namespace::IpnsError>,
                            > + Send,
                    >,
                >,
                adnet_namespace::IpnsError,
            > {
                Ok(Box::pin(futures::stream::empty()))
            }
            async fn health(&self) -> Result<TransportHealth, adnet_namespace::IpnsError> {
                Ok(TransportHealth::Healthy)
            }
        }

        let a_calls = Arc::new(AtomicUsize::new(0));
        let b_calls = Arc::new(AtomicUsize::new(0));
        let a: Arc<dyn IpnTransport> = Arc::new(CountingTransport {
            name: "stub-a",
            calls: a_calls.clone(),
        });
        let b: Arc<dyn IpnTransport> = Arc::new(CountingTransport {
            name: "stub-b",
            calls: b_calls.clone(),
        });

        let temp_dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            adnet_blobstore::BlobStore::new(temp_dir.path()).unwrap(),
        );
        let service = IpnService::with_transports(
            blob_store,
            temp_dir.path().to_path_buf(),
            None,
            vec![a, b],
        );

        service
            .publish("/ipfs/QmFanout".to_string(), None)
            .await
            .expect("publish");

        // Give the spawned fanout tasks time to run.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(a_calls.load(Ordering::SeqCst), 1);
        assert_eq!(b_calls.load(Ordering::SeqCst), 1);
    }
}
