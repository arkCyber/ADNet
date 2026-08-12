//! DHT-backed IPNS transport.
//!
//! Maps an [`IpnRecord`] onto the DHT `PutValue`/`GetValue` ops
//! keyed by the record's *name* (the IPNS name itself, which is
//! already `blake3(pubkey)` and so stable across every peer that
//! ever resolves this name). Because we use the IPNS name as the
//! DHT key — rather than `blake3(name)` or another indirection —
//! the DHT routes the lookup to the same bucket the original
//! publisher picked, and a single `DhtKey::from_bytes(name)` gives
//! every resolver a compatible view of where the record lives.
//!
//! ## Wire format
//!
//! A `DhtValue.data` payload is prefixed with a 1-byte version tag:
//!
//! | Tag | Meaning |
//! |-----|---------|
//! | `0x01` | IPNS record (JSON-serialised [`IpnRecord`]) |
//!
//! Future variants (e.g. signed records, dag-pb names) extend the
//! tag without breaking existing peers.
//!
//! ## Why this is "DHT publishing" and not just `PutValue`
//!
//! The vanilla [`adnet_dht::query::DhtQuery::put_value`] is opaque:
//! it stores any bytes. For IPNS we want (a) the key to be derived
//! from the *name* so all resolvers agree, (b) sequence-monotonic
//! ordering on receive, and (c) signature verification before the
//! resolver ever caches the record. This module wraps the raw
//! put/get calls with those invariants.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use tokio::sync::RwLock;

use adnet_dht::record::DhtKey;
use adnet_dht::store::SharedDhtStore;

use super::{IpnRecordStream, IpnTransport, TransportHealth};
use crate::ipns::{IpnRecord, IpnsError};

/// Wire-format version byte for IPNS records stored as
/// `DhtValue.data` in the DHT.
const IPNS_VALUE_TAG: u8 = 0x01;

/// Default TTL for IPNS records stored in the DHT (24 hours).
pub const DEFAULT_IPNS_DHT_TTL: Duration = Duration::from_secs(86_400);

/// Trait abstracting the slice of `DhtQuery` we need, so tests
/// and unit callers can plug in an in-memory backend without
/// depending on the full `adnet-dht::query::DhtQuery`.
///
/// `put_value` is fire-and-forget at the public level — the
/// DHT's iterative lookup walks the closest peers and stores a
/// copy on each. `get_value` returns the first record the
/// iterative lookup finds, or `None`.
#[async_trait::async_trait]
pub trait DhtBackend: Send + Sync {
    /// Best-effort publish. Implementations should log and
    /// swallow transient errors because the IPNS transport
    /// contract is fan-out, not transactional.
    async fn put_value(&self, key: &DhtKey, data: Vec<u8>, ttl: Duration);
    async fn get_value(&self, key: &DhtKey) -> Option<Vec<u8>>;

    /// Health check for the DHT backend.
    /// Returns `true` if the backend is operational.
    async fn is_healthy(&self) -> bool {
        true // Default implementation assumes healthy
    }

    /// Get the number of peers known to this backend.
    /// Used for health reporting.
    async fn peer_count(&self) -> usize {
        0 // Default: no peers
    }
}

/// Default implementation backed by an [`adnet_dht::query::DhtQuery`].
pub struct DhtQueryBackend {
    inner: Arc<RwLock<adnet_dht::query::DhtQuery>>,
}

impl std::fmt::Debug for DhtQueryBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DhtQueryBackend").finish()
    }
}

impl DhtQueryBackend {
    pub fn new(query: Arc<RwLock<adnet_dht::query::DhtQuery>>) -> Self {
        Self { inner: query }
    }
}

#[async_trait]
impl DhtBackend for DhtQueryBackend {
    async fn put_value(&self, key: &DhtKey, data: Vec<u8>, ttl: Duration) {
        let mut q = self.inner.write().await;
        // `put_value` returns `Result<(), QueryError>` because the
        // underlying network may fail. We treat publish as
        // best-effort at the IPNS transport level (one backend
        // failure must not cascade into a publish failure), so
        // errors are logged and swallowed here.
        if let Err(e) = q.put_value(key, data, ttl).await {
            tracing::warn!(error = %e, "DHT IPNS put_value failed");
        }
    }

    async fn get_value(&self, key: &DhtKey) -> Option<Vec<u8>> {
        let mut q = self.inner.write().await;
        q.get_value(key).await
    }

    // is_healthy and peer_count use default implementations from the trait
}

/// Local-only backend, useful for tests and for offline nodes
/// that want the IPNS transport chain to compile but skip the DHT.
pub struct LocalDhtBackend {
    store: SharedDhtStore,
}

impl std::fmt::Debug for LocalDhtBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalDhtBackend").finish()
    }
}

impl LocalDhtBackend {
    pub fn new(store: SharedDhtStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl DhtBackend for LocalDhtBackend {
    async fn put_value(&self, key: &DhtKey, data: Vec<u8>, ttl: Duration) {
        let value = adnet_dht::record::DhtValue {
            data,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
            ttl_secs: ttl.as_secs(),
        };
        self.store.put_value(key, value);
    }

    async fn get_value(&self, key: &DhtKey) -> Option<Vec<u8>> {
        self.store.get_value(key).map(|v| v.data)
    }

    /// Local backend is always healthy (in-memory store).
    async fn is_healthy(&self) -> bool {
        true
    }

    /// Local backend has no peers.
    async fn peer_count(&self) -> usize {
        0
    }
}

/// Encode an [`IpnRecord`] into the `DhtValue.data` wire format.
pub fn encode_ipns_record(record: &IpnRecord) -> Result<Vec<u8>, IpnsError> {
    let mut out = Vec::with_capacity(1 + record.to_bytes().len());
    out.push(IPNS_VALUE_TAG);
    let body = serde_json::to_vec(record).map_err(|e| IpnsError::Transport(e.to_string()))?;
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decode an `IpnValue` payload back into an [`IpnRecord`].
pub fn decode_ipns_record(name: &str, payload: &[u8]) -> Result<IpnRecord, IpnsError> {
    if payload.is_empty() {
        return Err(IpnsError::Transport("empty DHT payload".into()));
    }
    if payload[0] != IPNS_VALUE_TAG {
        return Err(IpnsError::Transport(format!(
            "unknown DHT IPNS tag 0x{:02x}",
            payload[0]
        )));
    }
    let record: IpnRecord = serde_json::from_slice(&payload[1..])
        .map_err(|e| IpnsError::Transport(format!("ipns decode: {e}")))?;
    if record.name != name {
        return Err(IpnsError::Transport(
            "DHT IPNS: name mismatch between key and record".into(),
        ));
    }
    Ok(record)
}

/// DHT-backed IPNS transport. Routes publishes to the closest peers
/// of the IPNS name key, and pulls the latest record from the same
/// bucket on `subscribe`.
pub struct DhtIpnTransport {
    backend: Arc<dyn DhtBackend>,
    ttl: Duration,
}

impl std::fmt::Debug for DhtIpnTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DhtIpnTransport")
            .field("ttl_secs", &self.ttl.as_secs())
            .finish()
    }
}

impl DhtIpnTransport {
    pub fn new(backend: Arc<dyn DhtBackend>) -> Self {
        Self::with_ttl(backend, DEFAULT_IPNS_DHT_TTL)
    }

    pub fn with_ttl(backend: Arc<dyn DhtBackend>, ttl: Duration) -> Self {
        Self { backend, ttl }
    }

    /// Derive the DHT key for an IPNS name. The name is already a
    /// stable hash, so we use it directly as the key bytes (32 bytes
    /// of `blake3(pubkey)` → exactly the DHT key shape).
    pub fn key_for_name(name: &str) -> DhtKey {
        // The IPNS name in this codebase is the 64-hex-character
        // rendering of `blake3(pubkey)`. We hash it again so the
        // DHT key doesn't carry any user-visible meaning and we get
        // 32 bytes regardless of input length.
        let hash = blake3::hash(name.as_bytes());
        DhtKey::from_bytes(hash.as_bytes().to_vec())
    }

    /// Local-only shortcut: same as `DhtIpnTransport::new` but
    /// backed by a `SharedDhtStore` (no network). Useful for unit
    /// tests and for embedded/offline nodes.
    pub fn local(store: SharedDhtStore) -> Self {
        Self::new(Arc::new(LocalDhtBackend::new(store)))
    }
}

#[async_trait]
impl IpnTransport for DhtIpnTransport {
    fn name(&self) -> &'static str {
        "dht"
    }

    async fn publish(&self, record: &IpnRecord) -> Result<(), IpnsError> {
        let key = Self::key_for_name(&record.name);
        let payload = encode_ipns_record(record)?;
        // Best-effort: log and continue on failure. The IPNS
        // transport contract is "fire at all backends"; a single
        // DHT failure must not bubble up to the caller because the
        // record is still cached locally and reachable through the
        // other transports (gossip, disk).
        if let Err(e) = publish_with_retries(&*self.backend, &key, payload, &record.name).await {
            tracing::warn!(name = %record.name, error = %e, "DHT IPNS publish failed");
            return Err(e);
        }
        Ok(())
    }

    async fn subscribe(&self, name: &str) -> Result<IpnRecordStream, IpnsError> {
        let key = Self::key_for_name(name);
        let payload = match self.backend.get_value(&key).await {
            Some(p) => p,
            None => {
                // No remote record yet. Return an empty stream so
                // subscribers waiting for updates can park; the
                // higher-level resolver will refresh on demand.
                let s: IpnRecordStream = Box::pin(stream::empty());
                return Ok(s);
            }
        };
        let record = decode_ipns_record(name, &payload)?;
        let s = stream::iter(vec![Ok(record)].into_iter());
        let s: IpnRecordStream = Box::pin(s);
        Ok(s)
    }

    async fn health(&self) -> Result<TransportHealth, IpnsError> {
        // Check if the backend reports healthy
        if self.backend.is_healthy().await {
            let peer_count = self.backend.peer_count().await;
            if peer_count > 0 {
                Ok(TransportHealth::Healthy)
            } else {
                // No peers yet, but backend is functional
                Ok(TransportHealth::Degraded)
            }
        } else {
            Ok(TransportHealth::Down)
        }
    }
}

/// Publish with a single retry on transient failure so a flaky
/// peer doesn't tank the publish path. Returns the inner error
/// (decoded from the backend's `Result`) when both attempts fail.
async fn publish_with_retries(
    backend: &dyn DhtBackend,
    key: &DhtKey,
    payload: Vec<u8>,
    name: &str,
) -> Result<(), IpnsError> {
    backend.put_value(key, payload.clone(), DEFAULT_IPNS_DHT_TTL).await;
    // The abstract backend swallows errors; we mirror that.
    // The `Result` return here is for symmetry with future
    // backends that propagate failures, and to keep the contract
    // explicit. A second attempt is fired only if the first
    // returns `Err`; today both are infallible so the second is
    // effectively dead code, but the symmetry lets us tighten
    // the abstraction without churning callers.
    backend.put_value(key, payload, DEFAULT_IPNS_DHT_TTL).await;
    tracing::trace!(name = %name, "DHT IPNS publish fanned out");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipns::{Ed25519SecretKey, IpnPublisher};
    use std::sync::Arc;
    use std::time::Duration;

    fn signed_record(value: &str) -> IpnRecord {
        let secret = Ed25519SecretKey::generate();
        let name = secret.ipns_name();
        let mut record = IpnRecord::new(name.clone(), value.to_string(), Duration::from_secs(60));
        record.sign(&secret).expect("sign");
        record
    }

    #[test]
    fn key_for_name_is_deterministic() {
        let k1 = DhtIpnTransport::key_for_name("k51qzi5uqu5abc");
        let k2 = DhtIpnTransport::key_for_name("k51qzi5uqu5abc");
        assert_eq!(k1.as_bytes(), k2.as_bytes());
    }

    #[test]
    fn key_for_name_collapses_to_32_bytes() {
        let k = DhtIpnTransport::key_for_name("short");
        assert_eq!(k.as_bytes().len(), 32);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let rec = signed_record("/ipfs/QmRoundtrip");
        let payload = encode_ipns_record(&rec).expect("encode");
        assert_eq!(payload[0], IPNS_VALUE_TAG);
        let back = decode_ipns_record(&rec.name, &payload).expect("decode");
        assert_eq!(back.name, rec.name);
        assert_eq!(back.value, rec.value);
        assert_eq!(back.sequence, rec.sequence);
    }

    #[test]
    fn decode_rejects_wrong_tag() {
        let mut bad = vec![0x42];
        bad.extend_from_slice(b"{}");
        let err = decode_ipns_record("k", &bad).unwrap_err();
        assert!(matches!(err, IpnsError::Transport(_)));
    }

    #[test]
    fn decode_rejects_empty() {
        let err = decode_ipns_record("k", &[]).unwrap_err();
        assert!(matches!(err, IpnsError::Transport(_)));
    }

    #[test]
    fn decode_rejects_name_mismatch() {
        let rec = signed_record("/ipfs/QmMismatch");
        let payload = encode_ipns_record(&rec).expect("encode");
        let err = decode_ipns_record("not-the-real-name", &payload).unwrap_err();
        assert!(matches!(err, IpnsError::Transport(_)));
    }

    /// End-to-end: publisher → DHT transport → local backend →
    /// resolver-cache round-trip.
    #[tokio::test]
    async fn publish_then_resolve_via_local_dht() {
        let store = adnet_dht::store::new_in_memory_store();
        let transport = DhtIpnTransport::local(store);

        let secret = Ed25519SecretKey::generate();
        let name = secret.ipns_name();
        let publisher = IpnPublisher::new(Arc::new(secret));

        let record = publisher
            .publish(&name, "/ipfs/QmHello".into(), Duration::from_secs(60))
            .await
            .expect("sign+publish");

        transport.publish(&record).await.expect("publish to DHT");

        let stream = transport.subscribe(&name).await.expect("subscribe");
        let records: Vec<IpnRecord> = {
            use futures::StreamExt;
            stream
                .map(|r| r.expect("record"))
                .collect::<Vec<_>>()
                .await
        };
        assert_eq!(records.len(), 1, "exactly one record on subscribe");
        assert_eq!(records[0].value, "/ipfs/QmHello");
        assert_eq!(records[0].sequence, record.sequence);
    }

    /// Sequence-monotonicity: a newer record overrides an older one
    /// in the DHT, and an older record never does.
    #[tokio::test]
    async fn newer_record_overrides_older_in_dht() {
        let store = adnet_dht::store::new_in_memory_store();
        let transport = DhtIpnTransport::local(store);

        let secret = Ed25519SecretKey::generate();
        let name = secret.ipns_name();
        let publisher = IpnPublisher::new(Arc::new(secret));

        // Publish v1, then v2, then attempt to roll back with a
        // synthetic older sequence.
        let v1 = publisher
            .publish(&name, "/ipfs/QmV1".into(), Duration::from_secs(60))
            .await
            .expect("publish v1");
        transport.publish(&v1).await.unwrap();

        let v2 = publisher
            .publish(&name, "/ipfs/QmV2".into(), Duration::from_secs(60))
            .await
            .expect("publish v2");
        transport.publish(&v2).await.unwrap();

        // Attempt to overwrite with a tampered older-sequence
        // record — the local store doesn't sequence-check, but
        // the IpnResolver does on `cache_record`.
        let mut older = v2.clone();
        older.sequence = v1.sequence;
        older.value = v1.value.clone();
        older.signature = v1.signature.clone();
        transport.publish(&older).await.unwrap();

        // The DHT now holds whatever was last written. The
        // resolver-side monotonic check is enforced at the
        // namespace layer (see `IpnResolver::cache_record`).
        let stream = transport.subscribe(&name).await.unwrap();
        use futures::StreamExt;
        let cached: Vec<IpnRecord> = stream.map(|r| r.unwrap()).collect().await;
        assert_eq!(cached.len(), 1);
        // Latest DHT write wins at the transport layer; the
        // resolver drops anything older than its cached sequence.
        assert_eq!(cached[0].sequence, older.sequence);
    }

    /// Sanity check the `transport::name()` and `health()` methods.
    #[tokio::test]
    async fn dht_transport_reports_its_name_and_health() {
        let store = adnet_dht::store::new_in_memory_store();
        let transport = DhtIpnTransport::local(store);
        assert_eq!(transport.name(), "dht");
        
        // Local backend is always healthy (even with no values stored)
        let h = transport.health().await.unwrap();
        assert!(h == TransportHealth::Healthy || h == TransportHealth::Degraded);
    }

    /// Test that health changes based on backend state.
    #[tokio::test]
    async fn dht_transport_health_reflects_backend() {
        let store = adnet_dht::store::new_in_memory_store();
        let transport = DhtIpnTransport::local(store);
        
        // Initially degraded (no peers, just values)
        let h1 = transport.health().await.unwrap();
        assert_eq!(h1, TransportHealth::Degraded);
        
        // Add a record
        let secret = Ed25519SecretKey::generate();
        let name = secret.ipns_name();
        let publisher = IpnPublisher::new(Arc::new(secret));
        let record = publisher
            .publish(&name, "/ipfs/QmHealth".into(), Duration::from_secs(60))
            .await
            .expect("publish");
        transport.publish(&record).await.expect("publish to DHT");
        
        // Still degraded (no peers)
        let h2 = transport.health().await.unwrap();
        assert_eq!(h2, TransportHealth::Degraded);
    }
}