//! Persistent trusted-device store.
//!
//! The store is a single append-only JSONL file (`devices.jsonl`) that
//! holds one [`TrustedDeviceRecord`] per line. This avoids SQLite
//! while still providing:
//!
//!  - atomic writes (rename-over to swap in a new file),
//!  - linear-scan lookups by `credential_id` (the file is small enough
//!    that this is not a performance concern),
//!  - trivial tooling (`jq`, `grep`, …) without a database.
//!
//! ## File layout
//!
//! ```text
//! devices.jsonl   — one JSON line per record
//! devices.jsonl.new — staging file during atomic rename
//! ```
//!
//! ## Concurrency
//!
//! The store uses a `parking_lot::RwLock` for in-memory state. Writes
//! serialise through the OS rename syscall, which is atomic on all
//! platforms we support. The in-memory cache is fully rebuilt on open.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use parking_lot::RwLock;

use crate::error::{PairingError, PairingResult};
use crate::transport_identity::{CredentialId, Nonce32};
use crate::trusted_device::{TrustedDeviceRecord, TrustedDeviceStatus};

/// Configuration for the trusted-device store.
#[derive(Debug, Clone)]
pub struct TrustedDeviceStoreConfig {
    /// Directory in which `devices.jsonl` lives.
    pub path: PathBuf,

    /// Maximum records to keep in memory before forcing a flush.
    /// Default 10 000; not a hard limit.
    pub flush_every: usize,

    /// Whether to fsync after each write. Enable for
    /// battery-backed storage or paranoid deployments.
    pub sync: bool,
}

impl Default for TrustedDeviceStoreConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("devices.jsonl"),
            flush_every: 10_000,
            sync: false,
        }
    }
}

/// The trusted-device store.
#[derive(Debug)]
pub struct TrustedDeviceStore {
    config: TrustedDeviceStoreConfig,
    /// In-memory map: `credential_id` -> `TrustedDeviceRecord`.
    records: RwLock<HashMap<CredentialId, TrustedDeviceRecord>>,
    /// In-memory map of accepted pairing-request nonces → unix time recorded.
    /// Used for replay detection and TTL-based eviction.
    seen_nonces: RwLock<HashMap<Nonce32, i64>>,
    /// Optional reputation hook. When set, `insert`, `revoke`,
    /// and `update_capabilities` call into the global PeerScore
    /// table so the rest of A3Net can react to a pairing / a
    /// revocation without each call site wiring it up. Opt-in to
    /// avoid forcing a dependency on `a3net-reputation` when the
    /// pairing layer is used standalone.
    #[cfg(feature = "reputation")]
    reputation: std::sync::Mutex<Option<a3net_reputation::ReputationReporter>>,
}

impl TrustedDeviceStore {
    /// Open (or create) a trusted-device store at `config.path`.
    pub fn open(config: TrustedDeviceStoreConfig) -> PairingResult<Self> {
        let records = if config.path.exists() {
            Self::load_from_disk(&config.path)?
        } else {
            HashMap::new()
        };
        Ok(Self {
            config,
            records: RwLock::new(records),
            seen_nonces: RwLock::new(HashMap::new()),
            #[cfg(feature = "reputation")]
            reputation: std::sync::Mutex::new(None),
        })
    }

    /// Attach a [`a3net_reputation::ReputationReporter`] so that
    /// every `insert`/`revoke` records a corresponding
    /// [`crate::event::ReputationEvent::PairingEstablished`] /
    /// `PairingRevoked` into the global PeerScore. Requires the
    /// `reputation` feature.
    #[cfg(feature = "reputation")]
    pub fn with_reputation(
        &self,
        reporter: a3net_reputation::ReputationReporter,
    ) -> &Self {
        *self.reputation.lock().expect("reputation mutex") = Some(reporter);
        self
    }

    /// Borrow the installed reporter (cfg-gated). `None` when no
    /// reporter has been installed.
    #[cfg(feature = "reputation")]
    pub fn reputation(&self) -> Option<a3net_reputation::ReputationReporter> {
        self.reputation.lock().expect("reputation mutex").clone()
    }

    #[cfg(feature = "reputation")]
    fn record_pairing_established(&self, rec: &TrustedDeviceRecord) {
        let guard = self.reputation.lock().expect("reputation mutex");
        if let Some(rep) = guard.as_ref() {
            let peer = parse_node_id(&rec.node_id);
            if let Ok(peer) = peer {
                a3net_reputation::reporter::PairingSignal(rep).established(
                    peer,
                    short_cred(&rec.credential_id),
                );
            }
        }
    }

    #[cfg(feature = "reputation")]
    fn record_pairing_revoked(&self, rec: &TrustedDeviceRecord) {
        let guard = self.reputation.lock().expect("reputation mutex");
        if let Some(rep) = guard.as_ref() {
            let peer = parse_node_id(&rec.node_id);
            if let Ok(peer) = peer {
                a3net_reputation::reporter::PairingSignal(rep).revoked(
                    peer,
                    short_cred(&rec.credential_id),
                );
            }
        }
    }

    fn load_from_disk(path: &PathBuf) -> PairingResult<HashMap<CredentialId, TrustedDeviceRecord>> {
        let file = File::open(path)
            .map_err(|e| PairingError::Storage(format!("cannot open {}: {}", path.display(), e)))?;
        let reader = BufReader::new(file);
        let mut map = std::collections::HashMap::new();
        for (line_no, line) in reader.lines().enumerate() {
            let line = line.map_err(|e| {
                PairingError::Storage(format!(
                    "{}:{}: read error: {}",
                    path.display(),
                    line_no + 1,
                    e
                ))
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let rec: TrustedDeviceRecord = serde_json::from_str(&line).map_err(|e| {
                PairingError::Storage(format!(
                    "{}:{}: JSON parse error: {}",
                    path.display(),
                    line_no + 1,
                    e
                ))
            })?;
            if let Err(e) = rec.validate() {
                return Err(PairingError::Storage(format!(
                    "{}:{}: invalid record: {}",
                    path.display(),
                    line_no + 1,
                    e
                )));
            }
            if map.insert(rec.credential_id, rec.clone()).is_some() {
                // Duplicate credential_id on disk — last line wins, warn
                // but don't fail.
                log::warn!(
                    "duplicate credential_id {:?} in {}: keeping last entry",
                    &rec.credential_id[..4],
                    path.display()
                );
            }
        }
        Ok(map)
    }

    /// Persist the current in-memory map to disk atomically.
    ///
    /// This is called automatically by `insert`, `revoke`, and
    /// `update_capabilities` when the in-memory map has been dirtied.
    pub fn flush(&self) -> PairingResult<()> {
        let records = self.records.read();
        let tmp_path = self.config.path.with_extension("jsonl.new");
        {
            let mut tmp = File::create(&tmp_path).map_err(|e| {
                PairingError::Storage(format!("create {}: {}", tmp_path.display(), e))
            })?;
            for rec in records.values() {
                let line = serde_json::to_string(rec).map_err(PairingError::Serialization)?;
                writeln!(tmp, "{}", line).map_err(|e| {
                    PairingError::Storage(format!("write {}: {}", tmp_path.display(), e))
                })?;
            }
            if self.config.sync {
                tmp.flush().map_err(|e| {
                    PairingError::Storage(format!("fsync {}: {}", tmp_path.display(), e))
                })?;
                tmp.sync_all().map_err(|e| {
                    PairingError::Storage(format!("sync {}: {}", tmp_path.display(), e))
                })?;
            }
        }
        fs::rename(&tmp_path, &self.config.path).map_err(|e| {
            PairingError::Storage(format!(
                "rename {} -> {}: {}",
                tmp_path.display(),
                self.config.path.display(),
                e
            ))
        })?;
        Ok(())
    }

    /// Insert a new trusted-device record.
    ///
    /// Returns an error if a record with the same `credential_id`
    /// already exists. Use `update` if you want to overwrite.
    pub fn insert(&self, record: TrustedDeviceRecord) -> PairingResult<()> {
        let mut records = self.records.write();
        if records.contains_key(&record.credential_id) {
            return Err(PairingError::Malformed {
                what: "trusted_device_store",
                reason: format!(
                    "credential_id {:?} already exists",
                    &record.credential_id[..4]
                ),
            });
        }
        records.insert(record.credential_id, record.clone());
        drop(records);
        self.flush()?;
        #[cfg(feature = "reputation")]
        self.record_pairing_established(&record);
        Ok(())
    }

    /// Look up a record by `credential_id`.
    pub fn get(&self, credential_id: &CredentialId) -> Option<TrustedDeviceRecord> {
        self.records.read().get(credential_id).cloned()
    }

    /// Return `true` if a credential is active (exists + is not expired/revoked).
    pub fn is_active(&self, credential_id: &CredentialId, now_unix: i64) -> bool {
        match self.get(credential_id) {
            None => false,
            Some(rec) => rec.status == TrustedDeviceStatus::Active && !rec.is_expired(now_unix),
        }
    }

    /// Check a capability for an active credential.
    ///
    /// Returns `Ok(true)` if the credential is active and has the
    /// requested capability. Returns `Ok(false)` if active but lacks
    /// the capability. Returns an error for revoked / expired / missing
    /// so the caller can distinguish "forbidden" from "re-pair needed".
    pub fn check_capability(
        &self,
        credential_id: &CredentialId,
        cap: crate::capability::Capability,
        now_unix: i64,
    ) -> PairingResult<bool> {
        let rec = self
            .get(credential_id)
            .ok_or(PairingError::DeviceNotFound(*credential_id))?;
        if rec.status == TrustedDeviceStatus::Revoked {
            return Err(PairingError::DeviceRevoked(*credential_id));
        }
        if rec.is_expired(now_unix) {
            // Expired records should tell the caller so they can prompt
            // the user to re-pair rather than silently failing.
            return Err(PairingError::DeviceExpired {
                id: *credential_id,
                expired_at_unix: rec.expires_at_unix,
            });
        }
        Ok(rec.has_capability(cap))
    }

    /// Revoke a record by `credential_id`.
    pub fn revoke(&self, credential_id: &CredentialId) -> PairingResult<()> {
        #[cfg_attr(not(feature = "reputation"), allow(unused_variables))]
        let snapshot = {
            let mut records = self.records.write();
            let rec = records
                .get_mut(credential_id)
                .ok_or(PairingError::DeviceNotFound(*credential_id))?;
            rec.revoke(chrono::Utc::now().timestamp());
            rec.clone()
        };
        self.flush()?;
        #[cfg(feature = "reputation")]
        self.record_pairing_revoked(&snapshot);
        Ok(())
    }

    /// Update capabilities on an existing record.
    pub fn update_capabilities(
        &self,
        credential_id: &CredentialId,
        caps: crate::capability::CapabilitySet,
    ) -> PairingResult<()> {
        let mut records = self.records.write();
        let rec = records
            .get_mut(credential_id)
            .ok_or(PairingError::DeviceNotFound(*credential_id))?;
        rec.update_capabilities(caps)?;
        drop(records);
        self.flush()?;
        Ok(())
    }

    /// Touch `last_seen_unix` to mark a device as active.
    pub fn touch(&self, credential_id: &CredentialId, now_unix: i64) -> PairingResult<()> {
        let mut records = self.records.write();
        let rec = records
            .get_mut(credential_id)
            .ok_or(PairingError::DeviceNotFound(*credential_id))?;
        rec.touch(now_unix);
        drop(records);
        self.flush()?;
        Ok(())
    }

    /// Return all records.
    pub fn all(&self) -> Vec<TrustedDeviceRecord> {
        self.records.read().values().cloned().collect()
    }

    /// Return active (non-revoked, non-expired) records.
    pub fn active(&self, now_unix: i64) -> Vec<TrustedDeviceRecord> {
        self.records
            .read()
            .values()
            .filter(|r| r.status == TrustedDeviceStatus::Active && !r.is_expired(now_unix))
            .cloned()
            .collect()
    }

    /// Remove a record entirely (used when the pairing is manually deleted).
    pub fn remove(&self, credential_id: &CredentialId) -> PairingResult<()> {
        {
            let mut records = self.records.write();
            records.remove(credential_id);
        }
        self.flush()?;
        Ok(())
    }

    /// Number of records currently in the store.
    pub fn len(&self) -> usize {
        self.records.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Check that a pairing-request nonce has not been replayed, then
    /// record it so future replays are rejected.
    ///
    /// Returns `Ok(())` if this is the first time seeing `nonce`.
    /// Returns `Err(PairingError::NonceReplay)` if `nonce` was already
    /// accepted by this store.
    ///
    /// **P0: must be called before processing every incoming pairing
    /// request.** If the issuer skips this check, an attacker can replay
    /// the same signed pairing request against the issuer's transport
    /// and cause the store to re-issue a pairing response. Combined
    /// with capability escalation (the attacker already knows the
    /// capabilities), this is a MITM replay attack.
    ///
    /// Entries with `recorded_at < now - nonce_ttl_seconds` are evicted
    /// before the check so the HashMap's memory growth is bounded. A
    /// nonce that was recorded less than `nonce_ttl_seconds` ago is still
    /// live and will be rejected on replay.
    pub fn check_and_record_nonce(
        &self,
        nonce: Nonce32,
        nonce_ttl_seconds: i64,
    ) -> PairingResult<()> {
        let now = chrono::Utc::now().timestamp();
        self.prune_expired_nonces(now.saturating_sub(nonce_ttl_seconds));
        let mut seen = self.seen_nonces.write();
        if let Some(&_recorded_at) = seen.get(&nonce) {
            let prefix: [u8; 8] = nonce[..8].try_into().unwrap_or([0u8; 8]);
            return Err(PairingError::NonceReplay {
                nonce_prefix: prefix,
            });
        }
        seen.insert(nonce, now);
        Ok(())
    }

    /// Remove a nonce from the seen set. Call this when a pairing
    /// ceremony completes (response written, record inserted) and you
    /// want to allow a fresh pairing with the same invitee later.
    /// Without this, the invitee would be permanently blocked from
    /// re-pairing with the same credential_id.
    pub fn clear_nonce(&self, nonce: Nonce32) {
        self.seen_nonces.write().remove(&nonce);
    }

    /// Remove nonce entries older than `cutoff_unix`. Called
    /// automatically by `check_and_record_nonce`. Can also be called
    /// directly from a background task.
    pub fn prune_expired_nonces(&self, cutoff_unix: i64) {
        let mut seen = self.seen_nonces.write();
        seen.retain(|_, recorded_at| *recorded_at > cutoff_unix);
    }

    /// Number of nonces currently held in the replay guard.
    pub fn nonce_count(&self) -> usize {
        self.seen_nonces.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::node::NodeId;
    use tempfile::TempDir;

    fn tmp_store() -> (TrustedDeviceStore, TempDir) {
        let tmp = TempDir::new().unwrap();
        let config = TrustedDeviceStoreConfig {
            path: tmp.path().join("devices.jsonl"),
            ..Default::default()
        };
        let store = TrustedDeviceStore::open(config.clone()).unwrap();
        (store, tmp)
    }

    fn make_record(cred_id: [u8; 16]) -> TrustedDeviceRecord {
        let node_id = NodeId::from_bytes(&[0x01u8; 32]).unwrap().to_string();
        let issuer_node_id = NodeId::from_bytes(&[0x02u8; 32]).unwrap().to_string();
        TrustedDeviceRecord {
            credential_id: cred_id,
            role: crate::trusted_device::TrustedDeviceRole::Issuer,
            device_name: "Test".into(),
            paired_at_unix: 1_700_000_000,
            expires_at_unix: i64::MAX,
            last_seen_unix: 1_700_000_000,
            node_id,
            transport_pubkey: vec![1u8; 32],
            wallet_address: None,
            capabilities: crate::capability::CapabilitySet::from_names(["chat"]),
            status: TrustedDeviceStatus::Active,
            record_version: 1,
            issuer_node_id,
            revoked_at_unix: 0,
        }
    }

    #[test]
    fn insert_and_get() {
        let (store, _tmp) = tmp_store();
        let rec = make_record([0u8; 16]);
        store.insert(rec.clone()).unwrap();
        assert_eq!(store.get(&rec.credential_id), Some(rec));
    }

    #[test]
    fn duplicate_rejected() {
        let (store, _tmp) = tmp_store();
        let rec = make_record([1u8; 16]);
        store.insert(rec.clone()).unwrap();
        let err = store.insert(rec).unwrap_err();
        assert!(matches!(
            err,
            PairingError::Malformed {
                what: "trusted_device_store",
                ..
            }
        ));
    }

    #[test]
    fn revoke() {
        let (store, _tmp) = tmp_store();
        let id: CredentialId = [2u8; 16];
        store.insert(make_record(id)).unwrap();
        store.revoke(&id).unwrap();
        let err = store
            .check_capability(&id, crate::capability::Capability::CHAT, i64::MAX)
            .unwrap_err();
        assert!(matches!(err, PairingError::DeviceRevoked(_)));
    }

    #[test]
    fn active_filter() {
        let (store, _tmp) = tmp_store();
        store.insert(make_record([3u8; 16])).unwrap();
        store
            .insert({
                let mut r = make_record([4u8; 16]);
                r.status = TrustedDeviceStatus::Revoked;
                r
            })
            .unwrap();
        let active = store.active(i64::MAX);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].credential_id, [3u8; 16]);
    }

    #[test]
    fn remove() {
        let (store, _tmp) = tmp_store();
        let id: CredentialId = [5u8; 16];
        store.insert(make_record(id)).unwrap();
        store.remove(&id).unwrap();
        assert!(store.get(&id).is_none());
    }

    #[test]
    fn flush_to_disk_and_reload() {
        let (store, tmp) = tmp_store();
        let id: CredentialId = [6u8; 16];
        store.insert(make_record(id)).unwrap();
        drop(store);
        let config = TrustedDeviceStoreConfig {
            path: tmp.path().join("devices.jsonl"),
            ..Default::default()
        };
        let reloaded = TrustedDeviceStore::open(config).unwrap();
        assert_eq!(reloaded.get(&id).map(|r| r.credential_id), Some(id));
    }

    // ── Nonce-replay guard ────────────────────────────────────────

    #[test]
    fn nonce_replay_rejected() {
        let (store, _tmp) = tmp_store();
        let nonce: Nonce32 = [1u8; 32];
        store.check_and_record_nonce(nonce, 3600).unwrap();
        let err = store.check_and_record_nonce(nonce, 3600).unwrap_err();
        assert!(matches!(err, PairingError::NonceReplay { .. }));
    }

    #[test]
    fn nonce_first_use_accepted() {
        let (store, _tmp) = tmp_store();
        let nonce: Nonce32 = [9u8; 32];
        store.check_and_record_nonce(nonce, 3600).unwrap();
    }

    #[test]
    fn nonce_different_values_accepted() {
        let (store, _tmp) = tmp_store();
        let n1: Nonce32 = [1u8; 32];
        let n2: Nonce32 = [2u8; 32];
        store.check_and_record_nonce(n1, 3600).unwrap();
        store.check_and_record_nonce(n2, 3600).unwrap();
    }

    #[test]
    fn nonce_clear_allows_reuse() {
        let (store, _tmp) = tmp_store();
        let nonce: Nonce32 = [3u8; 32];
        store.check_and_record_nonce(nonce, 3600).unwrap();
        store.clear_nonce(nonce);
        // After clearing, the same nonce should be accepted again.
        store.check_and_record_nonce(nonce, 3600).unwrap();
    }

    #[test]
    fn nonce_ttl_eviction() {
        let (store, _tmp) = tmp_store();
        let nonce: Nonce32 = [0xFFu8; 32];
        // Record with 3600s TTL.
        store.check_and_record_nonce(nonce, 3600).unwrap();
        assert_eq!(store.nonce_count(), 1);

        // A second replay attempt within TTL is still rejected.
        let err = store.check_and_record_nonce(nonce, 3600).unwrap_err();
        assert!(matches!(err, PairingError::NonceReplay { .. }));

        // Prune with a cutoff far in the future (past expiry).
        store.prune_expired_nonces(chrono::Utc::now().timestamp() + 7200);
        assert_eq!(store.nonce_count(), 0);

        // After pruning, fresh nonces still work.
        let fresh: Nonce32 = [0xEEu8; 32];
        store.check_and_record_nonce(fresh, 3600).unwrap();
        assert_eq!(store.nonce_count(), 1);
    }
}

// ─────────────────────────────────────────────────────────────────
// Reputation hooks (cfg-gated)
// ─────────────────────────────────────────────────────────────────

/// Parse a 64-hex `node_id` field into an `a3net_types::NodeId`. We
/// validate the hex carefully because the field came from a
/// persistent JSON file and a malformed value would silently lose
/// the reputation event.
#[cfg(feature = "reputation")]
fn parse_node_id(hex: &str) -> Result<a3net_types::NodeId, String> {
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("invalid node_id hex ({} chars)", hex.len()));
    }
    a3net_types::NodeId::from_hex(hex).map_err(|e| e.to_string())
}

/// Render the first 16 chars of a credential id as hex for log /
/// metric use. Never returns the full credential because that would
////// leak a fingerprinting surface.
#[cfg(feature = "reputation")]
fn short_cred(cred: &[u8]) -> String {
    hex::encode(&cred[..cred.len().min(8)])
}
