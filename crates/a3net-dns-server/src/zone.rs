//! Zone store — the in-memory authoritative record database.
//!
//! Records are stored as `(name, qtype) -> RecordSet` pairs, and
//! persisted as NDJSON when a `state_path` is configured. The store
//! is **single-writer-multi-reader**: mutations take a write lock
//! and reads take a read lock.
//!
//! ## Compatibility with pkarr wire format
//!
//! Each A3Net IPNS publish is stored under two keys:
//!
//!   * `_a3net.<ipns-name>.<zone>` (TXT) — base64 of the pkarr packet
//!     bytes that the publisher would otherwise have sent to a public
//!     relay. This is what untrusted public resolvers fetch when a
//!     client says "I want IPNS for `<name>`".
//!   * `<ipns-name>.<zone>` (A/AAAA) — the relay addresses advertised
//!     by the IPNS record. This lets a `dig alice.a3net.example`
//!     query short-circuit straight to a socket address.
//!
//! The second is purely an optimisation; the first is the canonical
//! pkarr-compatible representation.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::config::DnsServerConfig;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordKind {
    /// A3Net IPNS TXT record (pkarr-compatible wire payload).
    AdnetIpnsTxt { ipns_name: String, payload: String, ttl_secs: u32 },
    /// A/AAAA relay address(es) extracted from a pkarr packet.
    RelayAddr { ipns_name: String, addr: String, ttl_secs: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneRecord {
    pub key: String,    // lowercase FQDN
    pub kind: RecordKind,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ZoneFile {
    records: Vec<ZoneRecord>,
}

#[derive(Debug, Clone)]
pub struct ZoneStore {
    cfg: DnsServerConfig,
    inner: Arc<RwLock<BTreeMap<String, Vec<ZoneRecord>>>>,
}

impl ZoneStore {
    pub fn new(cfg: DnsServerConfig) -> Self {
        Self {
            cfg,
            inner: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Load records from the configured state file if present.
    pub fn load(&self) -> Result<(), ZoneStoreError> {
        let Some(path) = &self.cfg.state_path else {
            return Ok(());
        };
        if !path.exists() {
            return Ok(());
        }
        let bytes = std::fs::read(path).map_err(ZoneStoreError::Io)?;
        let parsed: ZoneFile = serde_json::from_slice(&bytes)
            .map_err(|e| ZoneStoreError::Deserialize(e.to_string()))?;
        let mut inner = self.inner.write();
        inner.clear();
        for r in parsed.records {
            inner.entry(r.key.clone()).or_default().push(r);
        }
        Ok(())
    }

    /// Persist the in-memory zone to disk. Called after every
    /// successful mutation by the HTTP / publish endpoint.
    pub fn persist(&self) -> Result<(), ZoneStoreError> {
        let Some(path) = &self.cfg.state_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(ZoneStoreError::Io)?;
        }
        let snapshot = {
            let inner = self.inner.read();
            let mut out = ZoneFile::default();
            for recs in inner.values() {
                out.records.extend(recs.iter().cloned());
            }
            out
        };
        let bytes = serde_json::to_vec(&snapshot)
            .map_err(|e| ZoneStoreError::Deserialize(e.to_string()))?;
        // Atomic-ish write: write to tmp file, rename.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).map_err(ZoneStoreError::Io)?;
        std::fs::rename(&tmp, path).map_err(ZoneStoreError::Io)?;
        Ok(())
    }

    /// Insert or replace the record set for a given key.
    pub fn put(&self, rec: ZoneRecord) -> Result<(), ZoneStoreError> {
        {
            let mut inner = self.inner.write();
            inner.insert(rec.key.clone(), vec![rec]);
        }
        self.persist()
    }

    /// Look up records by exact key (case-insensitive).
    pub fn get(&self, key: &str) -> Vec<ZoneRecord> {
        let needle = key.to_ascii_lowercase();
        self.inner
            .read()
            .get(&needle)
            .cloned()
            .unwrap_or_default()
    }

    /// Iterate every record in the zone. Used by the HTTP /zone
    /// admin endpoint and by the cold-start replication task.
    pub fn all(&self) -> Vec<ZoneRecord> {
        self.inner
            .read()
            .values()
            .flat_map(|v| v.iter().cloned())
            .collect()
    }

    pub fn zone(&self) -> &str {
        &self.cfg.zone
    }

    /// Key the IPNS TXT record is served under. Stable wire format.
    pub fn ipns_txt_key(&self, ipns_name: &str) -> String {
        let zone = self.cfg.zone.trim_end_matches('.').to_ascii_lowercase();
        format!("_a3net.{}.{}.", ipns_name.to_ascii_lowercase(), zone)
    }

    /// Key the relay A/AAAA record is served under.
    pub fn relay_key(&self, ipns_name: &str) -> String {
        let zone = self.cfg.zone.trim_end_matches('.').to_ascii_lowercase();
        format!("{}.{}.", ipns_name.to_ascii_lowercase(), zone)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ZoneStoreError {
    #[error("io: {0}")]
    Io(std::io::Error),
    #[error("deserialize: {0}")]
    Deserialize(String),
}

/// Convenience constructor: builds the store, loads from disk, returns
/// a ready-to-use handle.
pub fn open(cfg: DnsServerConfig) -> Result<ZoneStore, ZoneStoreError> {
    let store = ZoneStore::new(cfg);
    store.load()?;
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(tmp: &std::path::Path) -> DnsServerConfig {
        DnsServerConfig::default()
            .with_state_path(tmp.join("zone.json"))
            .with_zone("a3net.test")
    }

    #[test]
    fn keys_are_lowercased_and_zoned() {
        let store = ZoneStore::new(DnsServerConfig::default().with_zone("AdNet.Test"));
        assert_eq!(store.ipns_txt_key("Alice"), "_a3net.alice.a3net.test.");
        assert_eq!(store.relay_key("Alice"), "alice.a3net.test.");
    }

    #[test]
    fn put_then_get_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ZoneStore::new(cfg(dir.path()));
        let rec = ZoneRecord {
            key: store.ipns_txt_key("alice"),
            kind: RecordKind::AdnetIpnsTxt {
                ipns_name: "alice".into(),
                payload: "AAECAwQFBg==".into(),
                ttl_secs: 60,
            },
        };
        store.put(rec.clone()).unwrap();
        let got = store.get(&store.ipns_txt_key("ALICE"));
        assert_eq!(got, vec![rec]);
    }

    #[test]
    fn persist_then_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ZoneStore::new(cfg(dir.path()));
        let rec = ZoneRecord {
            key: store.ipns_txt_key("bob"),
            kind: RecordKind::AdnetIpnsTxt {
                ipns_name: "bob".into(),
                payload: "BwgJCg==".into(),
                ttl_secs: 120,
            },
        };
        store.put(rec).unwrap();

        // Reopen and verify the file-backed read.
        let store2 = ZoneStore::new(cfg(dir.path()));
        store2.load().unwrap();
        let got = store2.get(&store2.ipns_txt_key("bob"));
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn missing_state_path_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = DnsServerConfig::default().with_state_path(dir.path().join("nope.json"));
        let store = ZoneStore::new(cfg);
        store.load().unwrap();
    }

    #[test]
    fn all_iterates_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = ZoneStore::new(cfg(dir.path()));
        for name in ["alice", "bob", "carol"] {
            store
                .put(ZoneRecord {
                    key: store.ipns_txt_key(name),
                    kind: RecordKind::AdnetIpnsTxt {
                        ipns_name: name.into(),
                        payload: "AAA=".into(),
                        ttl_secs: 60,
                    },
                })
                .unwrap();
        }
        let all = store.all();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn open_helper_loads_or_noops() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let store = open(cfg).unwrap();
        assert!(store.all().is_empty());
    }
}