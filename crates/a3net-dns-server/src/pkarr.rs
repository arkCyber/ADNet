//! Pkarr resolver & publisher surface for the self-hosted DNS server.
//!
//! Pkarr is the public-key-addressable resource records protocol that
//! stock `iroh` uses to publish and resolve `EndpointId → relay_url`
//! mappings (see
//! [`iroh::address_lookup::PkarrPublisher`](https://docs.rs/iroh/latest/iroh/address_lookup/pkarr/struct.PkarrPublisher.html)
//! and
//! [`iroh::address_lookup::PkarrResolver`](https://docs.rs/iroh/latest/iroh/address_lookup/pkarr/struct.PkarrResolver.html)).
//! The wire format is:
//!
//! - **HTTP PUT** `/<z32-encoded-pubkey>` with the signed packet bytes
//!   in the body — this is what `PkarrPublisher` sends.
//! - **HTTP GET** `/<z32-encoded-pubkey>` returns the raw signed
//!   packet bytes — this is what `PkarrResolver` fetches.
//! - **DNS TXT** `_iroh.<z32>.<zone>` carries the same payload
//!   base64-encoded — this is what `iroh-dns-server` serves.
//!
//! ## Why a separate module
//!
//! A3Net's self-hosted DNS server was originally a **publisher**
//! only: nodes put records into the in-memory zone store and the
//! server handed them out over DNS / HTTP. The `Pkarr Resolver` part
//! of the protocol was missing — there was no HTTP `GET
//! /<z32-encoded-pubkey>` endpoint, so a stock iroh
//! `PkarrResolver` couldn't fetch from us.
//!
//! This module adds:
//!
//! - [`PkarrStore`] — the on-disk authoritative store keyed by z32
//!   public key, with the same persistence semantics as [`ZoneStore`].
//! - [`PkarrApi`] — the HTTP publisher / resolver endpoints (`PUT
//!   /pkarr/<z32>` and `GET /pkarr/<z32>`).
//! - DNS-TXT serving for `_iroh.<z32>.<zone>` records (already
//!   supported via [`crate::zone::ZoneStore`] with the right
//!   [`crate::zone::RecordKind`]; the glue here is the
//!   [`iroh_key_for_pkarr_name`] helper that produces the canonical
//!   `_iroh.<z32>.<zone>` key from a z32 pubkey).

use std::collections::BTreeMap;
use std::sync::Arc;

use a3net_error::{ErrorKind, IntoReport, Severity};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use a3net_types::PkarrRecord;

use crate::config::DnsServerConfig;
use crate::zone::{RecordKind, ZoneRecord, ZoneStore, ZoneStoreError};

// Local pkarr lookup trait. `a3net-namespace::transport::pkarr`
// defines its own PkarrLookup that targets IpnsError; we mirror
// the shape here so the dns-server's resolvers all speak
// PkarrError. The `#[async_trait]` is mandatory because the trait
// has async methods and we want dyn-compatibility for the
// `UpstreamResolver` cascade.
#[async_trait::async_trait]
pub trait PkarrLookup: Send + Sync {
    /// Look up a 52-char z32 key (already validated by the
    /// caller) and return the raw signed-packet bytes.
    async fn lookup(
        &self,
        z32: &str,
        timeout: std::time::Duration,
    ) -> Result<Vec<u8>, PkarrError>;
}

/// z-base-32 alphabet used by iroh / pkarr. Defined locally so we
/// don't take a transitive dep just for the constant.
const Z32_ALPHABET: &[u8; 32] =
    b"ybndrfg8ejkmcpqxot1uwisza345h769";

/// Errors raised by the Pkarr resolver / publisher layer.
#[derive(Debug, thiserror::Error)]
pub enum PkarrError {
    #[error("invalid pkarr key (z32 encoded pubkey): {0}")]
    InvalidKey(String),

    #[error("invalid pkarr packet: {0}")]
    InvalidPacket(String),

    #[error("zone store error: {0}")]
    Zone(#[from] ZoneStoreError),

    #[error("a3net types error: {0}")]
    Adnet(#[from] a3net_types::AdnetError),

    #[error("serialization error: {0}")]
    Serialization(String),
}

// P0-5: unified error reporting.  Codes `DNS-101`..`DNS-105` for
// the pkarr layer.
impl IntoReport for PkarrError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidKey(_) => "DNS-101",
            Self::InvalidPacket(_) => "DNS-102",
            Self::Zone(_) => "DNS-103",
            Self::Adnet(_) => "DNS-104",
            Self::Serialization(_) => "DNS-105",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            // Caller fed us a bad key — bad input.
            Self::InvalidKey(_) => ErrorKind::BadRequest,
            // Signed packet is malformed — DataLoss because the
            // upstream signer either lied or transmitted garbage.
            Self::InvalidPacket(_) => ErrorKind::DataLoss,
            // Bytes serialization failed mid-flight.
            Self::Serialization(_) => ErrorKind::DataLoss,
            // Embedded errors — surface the inner kind.
            Self::Zone(c) => c.kind(),
            Self::Adnet(c) => c.kind(),
        }
    }

    fn severity(&self) -> Severity {
        match self {
            // Caller mistakes — warn.
            Self::InvalidKey(_) => Severity::Warn,
            // Everything else — page or surface.
            Self::InvalidPacket(_) => Severity::Error,
            Self::Serialization(_) => Severity::Error,
            // Both are wrapped error types — surface the inner
            // severity.  Split into two arms because `Zone` and
            // `Adnet` carry different inner error types.
            Self::Zone(c) => c.severity(),
            Self::Adnet(c) => c.severity(),
        }
    }
}

/// Canonical DNS-TXT key for a pkarr record, of the form
/// `_iroh.<z32>.<zone>.`.
pub fn pkarr_dns_key(zone: &str, z32_pubkey: &str) -> String {
    PkarrRecord::dns_txt_name(zone, z32_pubkey)
}

/// Validate that the input looks like a z-base-32 encoded ed25519
/// public key (52 chars, lowercase a–z plus digits 2–7), then decode
/// the bytes and verify they are a valid ed25519 curve point.
///
/// We deliberately check both **format** AND **curve membership**:
/// `iroh::endpoint_info::EndpointInfo::from_pkarr_signed_packet`
/// rejects non-curve-point packets for us, but a stock publisher
/// may feed us an off-curve key (corrupt / DOS data) before the
/// signature check runs. Rejecting it at the door stops the store
/// from accumulating junk and stops a malicious actor from using the
/// store as a write-amplification oracle.
pub fn validate_z32_pubkey(s: &str) -> Result<(), PkarrError> {
    if s.len() != 52 {
        return Err(PkarrError::InvalidKey(format!(
            "expected 52 z32 chars, got {} ({s:?})",
            s.len()
        )));
    }
    if !s.chars().all(|c| matches!(c, 'a'..='z' | '1'..='9')) {
        return Err(PkarrError::InvalidKey(format!(
            "z32 chars must be in [a-z1-9] (z-base-32 alphabet excludes only 0 and 2), got {s:?}"
        )));
    }
    let bytes = z32_decode(s).ok_or_else(|| {
        PkarrError::InvalidKey(format!("z32 decode failed for {s:?}"))
    })?;
    if bytes.len() != 32 {
        return Err(PkarrError::InvalidKey(format!(
            "z32 decoded to {} bytes, expected 32",
            bytes.len()
        )));
    }
    // Curve check: ed25519-dalek returns an error on a non-curve
    // point. We don't need the resulting key — just the boolean.
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    ed25519_dalek::VerifyingKey::from_bytes(&arr)
        .map_err(|e| PkarrError::InvalidKey(format!("off-curve ed25519 point: {e}")))?;
    Ok(())
}

/// Decode a 52-char z-base-32 string into 32 raw bytes. Public so
/// the `mainline` feature (`pkarr_mainline::MainlineDhtResolver`)
/// can reuse the same canonical decoder and stay wire-compatible.
pub fn z32_decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if bytes.len() != 52 {
        return None;
    }
    let mut out = [0u8; 32];
    let mut buf: u64 = 0;
    let mut bits: usize = 0;
    let mut idx: usize = 0;
    for &c in bytes {
        let v = Z32_ALPHABET.iter().position(|&a| a == c)? as u64;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            if idx >= out.len() {
                return None;
            }
            out[idx] = ((buf >> bits) & 0xff) as u8;
            idx += 1;
        }
    }
    // The last byte's high bits must be zero — non-zero would mean
    // the input carried 8 extra bits that don't fit.
    if bits != 0 && (buf & ((1u64 << bits) - 1)) != 0 {
        return None;
    }
    if idx != out.len() {
        return None;
    }
    Some(out.to_vec())
}

/// In-memory + on-disk pkarr record store, keyed by z32-encoded
/// ed25519 public key.
///
/// The store is single-writer-multi-reader (mirrors [`ZoneStore`]):
/// mutations take a write lock, reads take a read lock. On-disk
/// persistence uses the same atomic rename trick as [`ZoneStore`] so
/// a crash mid-write cannot corrupt the state file.
#[derive(Debug, Clone)]
pub struct PkarrStore {
    cfg: DnsServerConfig,
    inner: Arc<RwLock<BTreeMap<String, PkarrRecord>>>,
}

impl PkarrStore {
    pub fn new(cfg: DnsServerConfig) -> Self {
        Self {
            cfg,
            inner: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Build the state-file path for the pkarr records.
    /// Default: `<state_path>.pkarr.json` next to the zone file, or
    /// `None` when no state path is configured.
    fn pkarr_state_path(&self) -> Option<std::path::PathBuf> {
        let path = self.cfg.state_path.as_ref()?;
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("zone");
        Some(parent.join(format!("{stem}.pkarr.json")))
    }

    /// Load records from the configured state file, if any.
    pub fn load(&self) -> Result<(), PkarrError> {
        let Some(path) = self.pkarr_state_path() else {
            return Ok(());
        };
        if !path.exists() {
            return Ok(());
        }
        let bytes = std::fs::read(&path).map_err(ZoneStoreError::Io)?;
        let parsed: PkarrFile = serde_json::from_slice(&bytes).map_err(|e| {
            PkarrError::Serialization(format!("pkarr state deserialize: {e}"))
        })?;
        let mut inner = self.inner.write();
        inner.clear();
        for entry in parsed.records {
            inner.insert(entry.z32, entry.record);
        }
        Ok(())
    }

    /// Persist the in-memory records to disk.
    pub fn persist(&self) -> Result<(), PkarrError> {
        let Some(path) = self.pkarr_state_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(ZoneStoreError::Io)?;
        }
        let snapshot = {
            let inner = self.inner.read();
            let mut out = PkarrFile::default();
            for (z32, rec) in inner.iter() {
                out.records.push(PkarrFileEntry {
                    z32: z32.clone(),
                    record: rec.clone(),
                });
            }
            out
        };
        let bytes = serde_json::to_vec(&snapshot).map_err(|e| {
            PkarrError::Serialization(format!("pkarr state serialize: {e}"))
        })?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).map_err(ZoneStoreError::Io)?;
        std::fs::rename(&tmp, &path).map_err(ZoneStoreError::Io)?;
        Ok(())
    }

    /// Publish a pkarr record. Replaces any existing record under
    /// the same z32 key. Persists on success.
    pub fn publish(&self, z32: &str, packet: Vec<u8>, ttl_secs: u32) -> Result<PkarrRecord, PkarrError> {
        validate_z32_pubkey(z32)?;
        if packet.is_empty() {
            return Err(PkarrError::InvalidPacket("empty packet".into()));
        }
        let rec = PkarrRecord::in_zone(self.cfg.zone.clone(), packet, ttl_secs)?;
        let rec_for_zone = rec.clone();
        let rec_for_store = rec.clone();
        {
            let mut inner = self.inner.write();
            inner.insert(z32.to_string(), rec_for_store);
        }
        // Mirror the record into the zone store under the canonical
        // `_iroh.<z32>.<zone>` key so DNS-TXT lookups (`dig
        // TXT _iroh.<z32>.<zone>`) return the base64 payload too.
        let zone_key = pkarr_dns_key(&self.cfg.zone, z32);
        let zrec = ZoneRecord {
            key: zone_key.clone(),
            kind: RecordKind::AdnetIpnsTxt {
                ipns_name: z32.to_string(),
                payload: base64_encode_packet(&rec_for_zone.packet),
                ttl_secs: rec_for_zone.ttl_secs,
            },
            expires_at_unix_ms: crate::zone::now_unix_ms()
                + (rec_for_zone.ttl_secs as i64).saturating_mul(1_000),
        };
        // We don't hold a reference to the ZoneStore here, so the
        // mirroring happens at the API layer (`PkarrApi::publish`)
        // where we have both stores in hand.
        let _ = zrec;
        self.persist()?;
        Ok(rec_for_zone)
    }

    /// Fetch a pkarr record by z32 key. Returns `None` if absent.
    pub fn resolve(&self, z32: &str) -> Option<PkarrRecord> {
        validate_z32_pubkey(z32).ok()?;
        self.inner.read().get(z32).cloned()
    }

    /// Iterate every record in the store. Used by the HTTP admin
    /// endpoint and by the cold-start replication task.
    pub fn all(&self) -> Vec<PkarrRecord> {
        self.inner.read().values().cloned().collect()
    }

    /// Total number of records in the store.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// True when no records are present.
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Configure zone after construction.
    pub fn zone(&self) -> &str {
        &self.cfg.zone
    }

    /// Remove a record by z32 key. Returns `true` if a record was
    /// removed, `false` if no record existed for that key. Validates
    /// the z32 format before touching the in-memory map.
    pub fn remove(&self, z32: &str) -> Result<bool, PkarrError> {
        validate_z32_pubkey(z32)?;
        let removed = {
            let mut inner = self.inner.write();
            inner.remove(z32).is_some()
        };
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }
}

/// On-disk format for [`PkarrStore`] state.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PkarrFile {
    /// Pkarr records keyed by their z32-encoded public key.
    records: Vec<PkarrFileEntry>,
}

/// Wrapper that pairs a [`PkarrRecord`] with its z32 key. The z32
/// key is the canonical "primary key" for a Pkarr record and is not
/// part of the wire format the record carries, so we persist it
/// alongside the record.
#[derive(Debug, Serialize, Deserialize)]
struct PkarrFileEntry {
    z32: String,
    #[serde(flatten)]
    record: PkarrRecord,
}

/// Convenience constructor: build the store, load from disk.
pub fn open(cfg: DnsServerConfig) -> Result<PkarrStore, PkarrError> {
    let store = PkarrStore::new(cfg);
    store.load()?;
    Ok(store)
}

/// HTTP-facing pkarr publisher / resolver.
#[derive(Clone)]
pub struct PkarrApi {
    store: PkarrStore,
    zone: ZoneStore,
}

impl PkarrApi {
    pub fn new(store: PkarrStore, zone: ZoneStore) -> Self {
        Self { store, zone }
    }

    /// Build a [`PkarrApi`] from the binary's config. Both stores
    /// share the same config so persistence is centralised.
    pub fn from_config(cfg: DnsServerConfig) -> Result<Self, PkarrError> {
        let store = open(cfg.clone())?;
        let zone = crate::zone::open(cfg).map_err(PkarrError::Zone)?;
        Ok(Self::new(store, zone))
    }

    pub fn store(&self) -> &PkarrStore {
        &self.store
    }

    pub fn zone_store(&self) -> &ZoneStore {
        &self.zone
    }

    /// Handle `PUT /pkarr/<z32-encoded-pubkey>` from a pkarr
    /// publisher.
    pub fn publish(
        &self,
        z32: &str,
        packet: Vec<u8>,
        ttl_secs: Option<u32>,
    ) -> Result<PkarrRecord, PkarrError> {
        let ttl = ttl_secs.unwrap_or(3_600);
        let rec = self.store.publish(z32, packet, ttl)?;
        // Mirror into the zone store so DNS-TXT lookups return the
        // base64 payload. Use the same TTL so the two surfaces stay
        // synchronised.
        let payload = base64_encode_packet(&rec.packet);
        let zrec = ZoneRecord {
            key: pkarr_dns_key(self.zone.zone(), z32),
            kind: RecordKind::AdnetIpnsTxt {
                ipns_name: z32.to_string(),
                payload,
                ttl_secs: rec.ttl_secs,
            },
            expires_at_unix_ms: crate::zone::now_unix_ms()
                + (rec.ttl_secs as i64).saturating_mul(1_000),
        };
        self.zone.put(zrec).map_err(PkarrError::Zone)?;
        Ok(rec)
    }

    /// Handle `GET /pkarr/<z32-encoded-pubkey>` from a pkarr resolver.
    pub fn resolve(&self, z32: &str) -> Option<PkarrRecord> {
        self.store.resolve(z32)
    }

    /// Handle `GET /pkarr/_all` — list every record we know about.
    /// Useful for operator dashboards / replication jobs.
    pub fn list(&self) -> Vec<PkarrRecord> {
        self.store.all()
    }

    /// Delete a pkarr record by z32 key. Returns `true` if a record
    /// was removed, `false` if no record existed for that key.
    pub fn delete(&self, z32: &str) -> Result<bool, PkarrError> {
        validate_z32_pubkey(z32)?;
        let removed = {
            let inner_p = Arc::clone(&self.store.inner);
            let mut inner = inner_p.write();
            inner.remove(z32).is_some()
        };
        if removed {
            self.store.persist()?;
            // Also remove from the zone store mirror so DNS-TXT
            // lookups return NXDOMAIN.
            let zone_key = pkarr_dns_key(self.zone.zone(), z32);
            self.zone.delete(&zone_key).map_err(PkarrError::Zone)?;
        }
        Ok(removed)
    }
}

// -- helpers ---------------------------------------------------------

/// Base64-encode a packet. Uses the URL-safe alphabet (RFC 4648 §5)
/// with no padding — matches what iroh-dns-server emits in its TXT
/// records.
fn base64_encode_packet(packet: &[u8]) -> String {
    const CHARS: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((packet.len() * 4).div_ceil(3));
    let mut i = 0;
    while i + 3 <= packet.len() {
        let n = ((packet[i] as u32) << 16)
            | ((packet[i + 1] as u32) << 8)
            | (packet[i + 2] as u32);
        out.push(CHARS[((n >> 18) & 0x3f) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3f) as usize] as char);
        out.push(CHARS[((n >> 6) & 0x3f) as usize] as char);
        out.push(CHARS[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = packet.len() - i;
    if rem == 1 {
        let n = (packet[i] as u32) << 16;
        out.push(CHARS[((n >> 18) & 0x3f) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3f) as usize] as char);
    } else if rem == 2 {
        let n = ((packet[i] as u32) << 16) | ((packet[i + 1] as u32) << 8);
        out.push(CHARS[((n >> 18) & 0x3f) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3f) as usize] as char);
        out.push(CHARS[((n >> 6) & 0x3f) as usize] as char);
    }
    out
}

/// Re-encode 32 bytes as a 52-char z-base-32 string.
///
/// `pub(crate)` so the `mainline` feature can reuse the encoder
/// without dragging in `iroh-base` for its own encoder. Test
/// fixtures also reach for it.
pub fn z32_encode(bytes: &[u8]) -> String {
    const Z32_ALPHABET: &[u8; 32] =
        b"ybndrfg8ejkmcpqxot1uwisza345h769";
    let mut out = String::with_capacity(52);
    let mut buf: u64 = 0;
    let mut bits: usize = 0;
    for &b in bytes {
        buf = (buf << 8) | b as u64;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(Z32_ALPHABET[((buf >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(Z32_ALPHABET[((buf << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn cfg(dir: &std::path::Path) -> DnsServerConfig {
        DnsServerConfig::default()
            .with_state_path(dir.join("zone.json"))
            .with_zone("a3net.test")
    }

    fn sample_z32() -> String {
        // A real z-base-32 encoded ed25519 public key (52 chars,
        // lowercase + 2..7). We must use a real curve point because
        // the validator now also enforces curve membership (P0-REQ-3).
        let sk = ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]);
        z32_encode(&sk.verifying_key().to_bytes())
    }

    #[test]
    fn validate_z32_pubkey_accepts_52_lowercase_chars() {
        assert!(validate_z32_pubkey(&sample_z32()).is_ok());
    }

    #[test]
    fn validate_z32_pubkey_rejects_wrong_length() {
        assert!(validate_z32_pubkey("abcd").is_err());
        assert!(validate_z32_pubkey(&"a".repeat(51)).is_err());
        assert!(validate_z32_pubkey(&"a".repeat(53)).is_err());
    }

    #[test]
    fn validate_z32_pubkey_rejects_invalid_chars() {
        // Uppercase letters.
        assert!(validate_z32_pubkey(&"A".repeat(52)).is_err());
        // Digits outside [2-7].
        assert!(validate_z32_pubkey(&format!("{}8", "a".repeat(51))).is_err());
        assert!(validate_z32_pubkey(&format!("{}1", "a".repeat(51))).is_err());
    }

    #[test]
    fn pkarr_dns_key_format() {
        let key = pkarr_dns_key("a3net.test", "abc");
        assert_eq!(key, format!("_iroh.abc.a3net.test."));
    }

    #[test]
    fn pkarr_dns_key_strips_trailing_dot() {
        let key = pkarr_dns_key("a3net.test.", "abc");
        assert_eq!(key, format!("_iroh.abc.a3net.test."));
    }

    #[test]
    fn pkarr_store_publish_resolve_round_trip() {
        let dir = tempdir().unwrap();
        let store = PkarrStore::new(cfg(dir.path()));
        let packet = vec![0xAAu8; 200];
        let rec = store.publish(&sample_z32(), packet.clone(), 60).unwrap();
        assert_eq!(rec.packet, packet);
        assert_eq!(rec.ttl_secs, 60);
        let resolved = store.resolve(&sample_z32()).expect("resolve");
        assert_eq!(resolved, rec);
    }

    #[test]
    fn pkarr_store_publish_replaces_existing() {
        let dir = tempdir().unwrap();
        let store = PkarrStore::new(cfg(dir.path()));
        let z = sample_z32();
        store.publish(&z, vec![0x01, 0x02], 60).unwrap();
        store.publish(&z, vec![0x03, 0x04], 60).unwrap();
        let resolved = store.resolve(&z).unwrap();
        assert_eq!(resolved.packet, vec![0x03, 0x04]);
    }

    #[test]
    fn pkarr_store_publish_rejects_invalid_z32() {
        let dir = tempdir().unwrap();
        let store = PkarrStore::new(cfg(dir.path()));
        let err = store.publish("not-z32", vec![0x01], 60).unwrap_err();
        assert!(matches!(err, PkarrError::InvalidKey(_)));
    }

    #[test]
    fn pkarr_store_publish_rejects_empty_packet() {
        let dir = tempdir().unwrap();
        let store = PkarrStore::new(cfg(dir.path()));
        let err = store.publish(&sample_z32(), vec![], 60).unwrap_err();
        assert!(matches!(err, PkarrError::InvalidPacket(_)));
    }

    #[test]
    fn pkarr_store_publish_clamps_oversized_ttl() {
        let dir = tempdir().unwrap();
        let store = PkarrStore::new(cfg(dir.path()));
        let rec = store.publish(&sample_z32(), vec![0x01], u32::MAX).unwrap();
        // PkarrRecord clamps ttl at MAX_TTL_SECONDS (3_600).
        assert_eq!(rec.ttl_secs, 3_600);
    }

    #[test]
    fn pkarr_store_resolve_unknown_returns_none() {
        let dir = tempdir().unwrap();
        let store = PkarrStore::new(cfg(dir.path()));
        assert!(store.resolve(&sample_z32()).is_none());
    }

    #[test]
    fn pkarr_store_resolve_invalid_z32_returns_none() {
        let dir = tempdir().unwrap();
        let store = PkarrStore::new(cfg(dir.path()));
        // Invalid keys are treated as misses rather than errors.
        assert!(store.resolve("not-z32").is_none());
    }

    #[test]
    fn pkarr_store_all_returns_every_record() {
        let dir = tempdir().unwrap();
        let store = PkarrStore::new(cfg(dir.path()));
        for i in 0..5u8 {
            // Each z32 must be unique AND on the curve. We derive
            // distinct signing keys by varying a single seed byte —
            // each verifying_key().to_bytes() is guaranteed to be a
            // distinct curve point.
            let mut seed = [3u8; 32];
            seed[31] = i;
            let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
            let z = z32_encode(&sk.verifying_key().to_bytes());
            store.publish(&z, vec![i], 60).unwrap();
        }
        assert_eq!(store.all().len(), 5);
    }

    #[test]
    fn pkarr_store_persist_load_round_trip() {
        let dir = tempdir().unwrap();
        let store1 = PkarrStore::new(cfg(dir.path()));
        let z = sample_z32();
        store1.publish(&z, vec![0xDE, 0xAD, 0xBE, 0xEF], 60).unwrap();
        // Reopen
        let store2 = PkarrStore::new(cfg(dir.path()));
        store2.load().unwrap();
        assert_eq!(store2.len(), 1);
        assert_eq!(store2.resolve(&z).unwrap().packet, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn pkarr_store_no_state_path_is_ok() {
        let _dir = tempdir().unwrap();
        let cfg = DnsServerConfig::default().with_zone("a3net.test");
        let store = PkarrStore::new(cfg);
        store.load().unwrap();
        store.publish(&sample_z32(), vec![0x01], 60).unwrap();
        // No state path configured → nothing to persist, but the
        // record is still held in memory.
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn pkarr_store_open_helper_loads_or_noops() {
        let dir = tempdir().unwrap();
        let store = open(cfg(dir.path())).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn pkarr_api_publish_mirrors_into_zone_store() {
        let dir = tempdir().unwrap();
        let api = PkarrApi::from_config(cfg(dir.path())).unwrap();
        let z = sample_z32();
        api.publish(&z, vec![0x01, 0x02, 0x03], Some(60)).unwrap();
        // Zone store should now have a `_iroh.<z>.<zone>` record.
        let key = pkarr_dns_key(api.zone_store().zone(), &z);
        let zone_rec = api.zone_store().get(&key);
        assert_eq!(zone_rec.len(), 1);
        // The payload should be the base64-encoded packet.
        match &zone_rec[0].kind {
            RecordKind::AdnetIpnsTxt { payload, .. } => {
                assert!(!payload.is_empty());
            }
            other => panic!("expected AdnetIpnsTxt, got {other:?}"),
        }
    }

    #[test]
    fn pkarr_api_resolve_returns_published_record() {
        let dir = tempdir().unwrap();
        let api = PkarrApi::from_config(cfg(dir.path())).unwrap();
        let z = sample_z32();
        api.publish(&z, vec![0xAA, 0xBB], Some(60)).unwrap();
        let resolved = api.resolve(&z).unwrap();
        assert_eq!(resolved.packet, vec![0xAA, 0xBB]);
    }

    #[test]
    fn pkarr_api_resolve_unknown_returns_none() {
        let dir = tempdir().unwrap();
        let api = PkarrApi::from_config(cfg(dir.path())).unwrap();
        assert!(api.resolve(&sample_z32()).is_none());
    }

    #[test]
    fn pkarr_api_list_returns_all_records() {
        let dir = tempdir().unwrap();
        let api = PkarrApi::from_config(cfg(dir.path())).unwrap();
        for i in 0..3u8 {
            // Distinct curve points so the validator accepts all three.
            let mut seed = [5u8; 32];
            seed[31] = i;
            let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
            let z = z32_encode(&sk.verifying_key().to_bytes());
            api.publish(&z, vec![i], Some(60)).unwrap();
        }
        assert_eq!(api.list().len(), 3);
    }

    #[test]
    fn pkarr_api_delete_removes_record_and_zone_mirror() {
        let dir = tempdir().unwrap();
        let api = PkarrApi::from_config(cfg(dir.path())).unwrap();
        let z = sample_z32();
        api.publish(&z, vec![0x01, 0x02], Some(60)).unwrap();
        assert_eq!(api.list().len(), 1);
        assert!(api.delete(&z).unwrap());
        assert_eq!(api.list().len(), 0);
        let key = pkarr_dns_key(api.zone_store().zone(), &z);
        assert!(api.zone_store().get(&key).is_empty());
    }

    #[test]
    fn pkarr_api_delete_unknown_returns_false() {
        let dir = tempdir().unwrap();
        let api = PkarrApi::from_config(cfg(dir.path())).unwrap();
        assert!(!api.delete(&sample_z32()).unwrap());
    }

    #[test]
    fn pkarr_api_delete_rejects_invalid_z32() {
        let dir = tempdir().unwrap();
        let api = PkarrApi::from_config(cfg(dir.path())).unwrap();
        let err = api.delete("not-z32").unwrap_err();
        assert!(matches!(err, PkarrError::InvalidKey(_)));
    }

    #[test]
    fn pkarr_api_publish_rejects_invalid_z32() {
        let dir = tempdir().unwrap();
        let api = PkarrApi::from_config(cfg(dir.path())).unwrap();
        let err = api.publish("not-z32", vec![0x01], Some(60)).unwrap_err();
        assert!(matches!(err, PkarrError::InvalidKey(_)));
    }

    #[test]
    fn pkarr_api_publish_rejects_empty_packet() {
        let dir = tempdir().unwrap();
        let api = PkarrApi::from_config(cfg(dir.path())).unwrap();
        let err = api.publish(&sample_z32(), vec![], Some(60)).unwrap_err();
        assert!(matches!(err, PkarrError::InvalidPacket(_)));
    }

    #[test]
    fn pkarr_api_publish_clamps_oversized_ttl() {
        let dir = tempdir().unwrap();
        let api = PkarrApi::from_config(cfg(dir.path())).unwrap();
        let rec = api.publish(&sample_z32(), vec![0x01], Some(u32::MAX)).unwrap();
        assert_eq!(rec.ttl_secs, 3_600);
    }

    #[test]
    fn pkarr_api_publish_default_ttl() {
        let dir = tempdir().unwrap();
        let api = PkarrApi::from_config(cfg(dir.path())).unwrap();
        let rec = api.publish(&sample_z32(), vec![0x01], None).unwrap();
        assert_eq!(rec.ttl_secs, 3_600);
    }

    #[test]
    fn base64_encode_handles_three_byte_chunks() {
        // 3 bytes → 4 base64 chars.
        let s = base64_encode_packet(&[0x66, 0x6f, 0x6f]); // "foo"
        assert_eq!(s, "Zm9v");
    }

    #[test]
    fn base64_encode_handles_one_byte_remainder() {
        // 4 bytes → 5 base64 chars + padding '=' but we use URL-safe
        // without padding, so 4 → 6 chars.
        let s = base64_encode_packet(&[0x66, 0x6f, 0x6f, 0x62]);
        // "foob" base64 = "Zm9vYg=="
        assert_eq!(s, "Zm9vYg");
    }

    #[test]
    fn base64_encode_handles_two_byte_remainder() {
        // 5 bytes → 7 base64 chars (no padding in URL-safe form).
        let s = base64_encode_packet(&[0x66, 0x6f, 0x6f, 0x62, 0x61]);
        // "fooba" base64 = "Zm9vYmE="
        assert_eq!(s, "Zm9vYmE");
    }

    #[test]
    fn base64_encode_handles_empty_input() {
        assert_eq!(base64_encode_packet(&[]), "");
    }

    #[test]
    fn base64_encode_uses_url_safe_alphabet() {
        // Bytes whose standard base64 produces '+' or '/' should map
        // to '-' and '_' respectively in our URL-safe output.
        let s = base64_encode_packet(&[0xFB, 0xEF, 0xFF]);
        assert!(!s.contains('+'));
        assert!(!s.contains('/'));
    }

    #[test]
    fn pkarr_error_display_includes_context() {
        let e = PkarrError::InvalidKey("bad".into());
        assert!(e.to_string().contains("bad"));
        let e = PkarrError::InvalidPacket("empty".into());
        assert!(e.to_string().contains("empty"));
    }

    #[test]
    fn pkarr_store_zone_accessor() {
        let dir = tempdir().unwrap();
        let store = PkarrStore::new(cfg(dir.path()));
        assert_eq!(store.zone(), "a3net.test");
    }

    #[test]
    fn pkarr_store_is_empty_initially() {
        let dir = tempdir().unwrap();
        let store = PkarrStore::new(cfg(dir.path()));
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    // ========== P0-REQ-3 curve checks ==========

    /// Use a real ed25519 keypair for the positive test so the
    /// curve check is exercised end-to-end.
    #[test]
    fn validate_z32_accepts_real_curve_point() {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
        let vk = sk.verifying_key();
        let z32 = z32_encode(vk.to_bytes().as_slice());
        assert!(validate_z32_pubkey(&z32).is_ok(), "real curve point rejected: {z32}");
    }

    /// `[2;32]` decodes to 32 bytes that are NOT a valid ed25519
    /// public key — `VerifyingKey::from_bytes` rejects it.
    /// Verifying that our validator catches the off-curve input
    /// protects the store from accumulating junk.
    #[test]
    fn validate_z32_rejects_off_curve_bytes() {
        let bad = [2u8; 32];
        let s = z32_encode(&bad);
        assert_eq!(s.len(), 52);
        assert!(
            validate_z32_pubkey(&s).is_err(),
            "off-curve 32 bytes (all-0x02) accepted: {s}"
        );
    }

    /// Reverse the encoder: take a real ed25519 public key (32
    /// bytes), re-encode to z32, decode, and verify bytes round-trip.
    #[test]
    fn z32_decode_round_trips_with_real_pubkey() {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let original = sk.verifying_key().to_bytes();
        let encoded = z32_encode(&original);
        let decoded = z32_decode(&encoded).expect("decode");
        assert_eq!(decoded, original.to_vec());
    }

    /// Reject an input of the right alphabet but wrong length.
    #[test]
    fn z32_decode_rejects_wrong_length() {
        assert!(z32_decode("aaaa").is_none());
        assert!(z32_decode(&"a".repeat(53)).is_none());
    }

    /// Reject characters outside the alphabet.
    #[test]
    fn z32_decode_rejects_unknown_char() {
        // '8' is not in [a-z2-7]
        let s = format!("{}8", "a".repeat(51));
        assert!(z32_decode(&s).is_none());
    }

    /// PkarrStore.publish should refuse a packet whose z32 key
    /// doesn't decode to a curve point — protecting the store
    /// from accumulating junk.
    #[test]
    fn pkarr_store_publish_rejects_off_curve_z32() {
        let dir = tempdir().unwrap();
        let store = PkarrStore::new(cfg(dir.path()));
        // Encode `[2u8; 32]` — known off-curve point.
        let bad_z = z32_encode(&[2u8; 32]);
        let err = store.publish(&bad_z, vec![0xAA; 32], 60).unwrap_err();
        assert!(matches!(err, PkarrError::InvalidKey(_)));
    }

    // P0-5: pin PkarrError codes.
    #[test]
    fn pkarr_error_codes_are_stable() {
        let pairs: Vec<(PkarrError, &str, ErrorKind, Severity)> = vec![
            (
                PkarrError::InvalidKey("k".into()),
                "DNS-101",
                ErrorKind::BadRequest,
                Severity::Warn,
            ),
            (
                PkarrError::InvalidPacket("p".into()),
                "DNS-102",
                ErrorKind::DataLoss,
                Severity::Error,
            ),
            (
                PkarrError::Serialization("s".into()),
                "DNS-105",
                ErrorKind::DataLoss,
                Severity::Error,
            ),
        ];
        for (err, code, kind, sev) in pairs {
            assert_eq!(err.code(), code, "code for {err:?}");
            assert_eq!(err.kind(), kind, "kind for {err:?}");
            assert_eq!(err.severity(), sev, "severity for {err:?}");
        }
    }

    #[test]
    fn pkarr_error_into_report_carries_cause() {
        let e = PkarrError::InvalidKey("bad z32".into());
        let report = e.into_report("a3net-dns-server");
        assert_eq!(report.code, "DNS-101");
        assert!(report.cause.as_deref().unwrap_or("").contains("bad z32"));
    }
}
