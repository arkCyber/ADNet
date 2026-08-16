//! Persistence — bind the media DAG to `a3net-blobstore`.
//!
//! ## Storage layout
//!
//! ```text
//! <blobstore_data_dir>/
//!   <content_hash>/                # segments (existing blobstore layout)
//!   media-manifests/<digest>.bin   # MediaManifest bytes (this module)
//!   media-aliases.json             # name -> manifest digest (this module)
//!   media-segments.json            # LP-digest -> blobstore hash (this module)
//! ```
//!
//! Segments are written into the existing [`BlobStore`] — they are
//! already BLAKE3-addressed and discoverable by the DHT layer.
//! The manifest itself uses a separate filesystem path because
//! its `MediaDigest::root` is **not** equal to a raw `BLAKE3`
//! over the serialized bytes; it is a domain-separated digest
//! over a canonical serialization (see `manifest::compute_root`).
//!
//! ## LP-tag integrity
//!
//! Segment digests in the DAG carry a `kind` byte (`LP_VIDEO` or
//! `LP_AUDIO`) used to domain-separate the BLAKE3 hash. The
//! blobstore itself is content-addressed by raw `BLAKE3` (no LP).
//! `load_segment_with_kind` re-verifies the bytes using the
//! caller-supplied kind so the address space stays collision-free.
//!
//! ## DO-178C SR-10
//!
//! Persisting a manifest MUST NOT mutate its byte content. The
//! `MediaDigest::root` of the persisted bytes equals the
//! pre-existing `MediaManifest.root`. The aerospace compliance
//! suite enforces this on every save / load cycle.

use crate::dag::MediaDag;
use crate::error::{MediaError, MediaResult};
use crate::integrity::{LP_AUDIO, LP_VIDEO};
use crate::manifest::MediaManifest;
use a3net_blobstore::BlobStore;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

/// Outcome of persisting a DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaStoreReport {
    pub video_segments: usize,
    pub audio_segments: usize,
    /// MediaDigest root of the persisted manifest.
    pub manifest_hash: String,
    pub bytes_written: u64,
    pub alias: Option<String>,
}

/// One entry in the segment-index sidecar.
///
/// The blobstore addresses segments by raw BLAKE3 of the payload;
/// the DAG addresses them by LP-prefixed BLAKE3 with a `kind` byte
/// (LP_VIDEO or LP_AUDIO). The sidecar stores BOTH the blobstore
/// key and the LP kind so `load_segment_with_kind` can re-verify
/// the bytes without ambiguity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentIndexEntry {
    /// Raw BLAKE3 of the payload bytes — the blobstore key.
    pub blobstore_hash: String,
    /// LP tag the segment was hashed under (LP_VIDEO or LP_AUDIO).
    pub kind: u8,
}

/// Alias map: human-readable name → manifest root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasMap {
    entries: std::collections::BTreeMap<String, String>,
}

impl AliasMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(|s| s.as_str())
    }

    pub fn insert(&mut self, name: impl Into<String>, root: impl Into<String>) -> Option<String> {
        self.entries.insert(name.into(), root.into())
    }

    pub fn remove(&mut self, name: &str) -> Option<String> {
        self.entries.remove(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug)]
pub struct MediaStore {
    blobstore: BlobStore,
    data_dir: PathBuf,
    manifests_dir: PathBuf,
    alias_path: PathBuf,
    /// LP-digest → blobstore raw-BLAKE3 hash. Written
    /// atomically after each `persist_dag_with_segments` so that
    /// `verify_complete` can resolve DAG digests into blobstore
    /// keys.
    index_path: PathBuf,
}

// `MediaStore` deliberately does NOT implement `Clone`: two
// instances on the same data dir would race on the alias / index
// sidecars. If shared access is needed, wrap the store in an
// `Arc` or hand it through channels.

impl MediaStore {
    pub fn open(blobstore: BlobStore) -> MediaResult<Self> {
        let data_dir = blobstore.data_dir().to_path_buf();
        let manifests_dir = data_dir.join("media-manifests");
        let alias_path = data_dir.join("media-aliases.json");
        let index_path = data_dir.join("media-segments.json");
        std::fs::create_dir_all(&manifests_dir)?;
        Ok(Self {
            blobstore,
            data_dir,
            manifests_dir,
            alias_path,
            index_path,
        })
    }

    pub fn blobstore(&self) -> &BlobStore {
        &self.blobstore
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    /// Persist a DAG with concrete segment payloads. Each segment
    /// is re-verified against its DAG-declared digest before
    /// being written to the blobstore (SR-1 / SR-4).
    ///
    /// SR-10 partial-failure semantics: if a segment write
    /// fails mid-batch, the partial segment-index entries that
    /// have *already* been written to disk are NOT rolled back
    /// from the blobstore (the bytes are content-addressed and
    /// safe to leave). However the segment-index sidecar is
    /// rolled back to its pre-call state, so `verify_complete`
    /// remains consistent with what the caller has actually
    /// acknowledged via `Ok(_)` reports.
    pub fn persist_dag_with_segments(
        &self,
        dag: &MediaDag,
        manifest: &MediaManifest,
        video_segments: &[Vec<u8>],
        audio_segments: &[Vec<u8>],
    ) -> MediaResult<MediaStoreReport> {
        // SR-10: refuse to persist anything where the manifest
        // root does not match the DAG root.
        if manifest.root.as_hex() != hex::encode(dag.root.bytes) {
            return Err(MediaError::ManifestHashMismatch {
                expected: hex::encode(dag.root.bytes),
                actual: manifest.root.as_hex(),
            });
        }
        // H-10: cap individual segment size (DoS guard).
        for (i, p) in video_segments.iter().chain(audio_segments.iter()).enumerate() {
            if p.len() > MAX_SEGMENT_BYTES {
                return Err(MediaError::InvalidConfig(format!(
                    "segment {i} exceeds MAX_SEGMENT_BYTES ({} > {})",
                    p.len(),
                    MAX_SEGMENT_BYTES
                )));
            }
        }

        // 1. Manifest → filesystem (NOT into the blobstore — the
        //    MediaDigest is domain-separated, not raw BLAKE3).
        //    The manifest file is written LAST (after segments)
        //    to guarantee we never leave a manifest pointing at
        //    segments that are missing from the index. If any
        //    segment write fails we skip the manifest write
        //    entirely — no rollback needed because we never
        //    wrote it.
        let manifest_bytes = bincode::serialize(manifest)?;
        let recomputed_root = compute_root_from_bytes(&manifest_bytes)?;
        if recomputed_root != manifest.root.as_hex() {
            return Err(MediaError::ManifestHashMismatch {
                expected: manifest.root.as_hex(),
                actual: recomputed_root,
            });
        }
        let manifest_path = self.manifest_path(&manifest.root.as_hex());

        // Snapshot the index BEFORE we mutate it so we can roll
        // back on partial failure. The blobstore writes are
        // idempotent — we never delete them — but the index must
        // reflect the caller's view of acknowledged segments.
        let mut index = self.load_segment_index()?;
        let index_snapshot = index.clone();

        let mut bytes_written: u64 = 0;

        // 2. Video segments.
        let mut video_idx = 0usize;
        for v in &dag.variants {
            for s in &v.segments {
                let segment_digest_hex = hex::encode(s.digest.bytes);
                let payload = video_segments.get(video_idx).ok_or(
                    MediaError::SegmentOutOfRange {
                        index: video_idx,
                        count: video_segments.len(),
                    },
                )?;
                let computed = crate::integrity::SegmentDigest::compute(LP_VIDEO, payload);
                if computed.bytes != s.digest.bytes {
                    self.save_segment_index(&index_snapshot)?;
                    return Err(MediaError::ManifestHashMismatch {
                        expected: segment_digest_hex,
                        actual: hex::encode(computed.bytes),
                    });
                }
                // put_bytes_sync can fail with std::io::Error
                // (disk full, permission denied, etc.). On any
                // IO failure we MUST roll the index back so the
                // on-disk index never references segments we did
                // not acknowledge via Ok(_).
                let put_result = self.blobstore.put_bytes_sync(payload);
                let (stored_hash, size) = match put_result {
                    Ok(v) => v,
                    Err(e) => {
                        self.save_segment_index(&index_snapshot)?;
                        return Err(MediaError::Io(format!(
                            "blobstore.put_bytes_sync failed: {e}"
                        )));
                    }
                };
                index.insert(
                    segment_digest_hex,
                    SegmentIndexEntry {
                        blobstore_hash: stored_hash.as_hex().to_string(),
                        kind: LP_VIDEO,
                    },
                );
                bytes_written = bytes_written
                    .checked_add(size)
                    .ok_or_else(|| MediaError::InvalidConfig("byte counter overflow".into()))?;
                video_idx += 1;
            }
        }

        // 3. Audio segments.
        let mut audio_idx = 0usize;
        if let Some(audio) = &dag.audio {
            for s in &audio.segments {
                let segment_digest_hex = hex::encode(s.digest.bytes);
                let payload = audio_segments.get(audio_idx).ok_or(
                    MediaError::SegmentOutOfRange {
                        index: audio_idx,
                        count: audio_segments.len(),
                    },
                )?;
                let computed = crate::integrity::SegmentDigest::compute(LP_AUDIO, payload);
                if computed.bytes != s.digest.bytes {
                    self.save_segment_index(&index_snapshot)?;
                    return Err(MediaError::ManifestHashMismatch {
                        expected: segment_digest_hex,
                        actual: hex::encode(computed.bytes),
                    });
                }
                let put_result = self.blobstore.put_bytes_sync(payload);
                let (stored_hash, size) = match put_result {
                    Ok(v) => v,
                    Err(e) => {
                        self.save_segment_index(&index_snapshot)?;
                        return Err(MediaError::Io(format!(
                            "blobstore.put_bytes_sync failed: {e}"
                        )));
                    }
                };
                index.insert(
                    segment_digest_hex,
                    SegmentIndexEntry {
                        blobstore_hash: stored_hash.as_hex().to_string(),
                        kind: LP_AUDIO,
                    },
                );
                bytes_written = bytes_written
                    .checked_add(size)
                    .ok_or_else(|| MediaError::InvalidConfig("byte counter overflow".into()))?;
                audio_idx += 1;
            }
        }

        // 4. Persist the manifest file now that all segments are
        //    in the index. write_atomic is rename-based, so a
        //    partial write cannot leave a corrupt manifest on
        //    disk.
        write_atomic(&manifest_path, &manifest_bytes)?;
        bytes_written = bytes_written
            .checked_add(manifest_bytes.len() as u64)
            .ok_or_else(|| MediaError::InvalidConfig("byte counter overflow".into()))?;

        // 5. Final flush of the index.
        self.save_segment_index(&index)?;

        info!(bytes_written, "media DAG persisted");
        Ok(MediaStoreReport {
            video_segments: video_idx,
            audio_segments: audio_idx,
            manifest_hash: manifest.root.as_hex(),
            bytes_written,
            alias: None,
        })
    }

    /// Persist the manifest under a human-readable alias.
    pub fn persist_with_alias(
        &self,
        dag: &MediaDag,
        manifest: &MediaManifest,
        video_segments: &[Vec<u8>],
        audio_segments: &[Vec<u8>],
        alias: &str,
    ) -> MediaResult<MediaStoreReport> {
        if alias.is_empty() {
            return Err(MediaError::InvalidConfig("alias must not be empty".into()));
        }
        let mut report = self.persist_dag_with_segments(
            dag,
            manifest,
            video_segments,
            audio_segments,
        )?;
        let mut map = self.load_alias_map()?;
        let prev = map.insert(alias.to_string(), manifest.root.as_hex());
        if prev.is_some() {
            tracing::warn!(alias, prev = ?prev, "alias overwrite — previous DAG orphaned");
        }
        self.save_alias_map(&map)?;
        report.alias = Some(alias.to_string());
        Ok(report)
    }

    /// Load a manifest by its MediaDigest root hex. The
    /// `root_hex` must be exactly 64 lowercase hex chars; any
    /// other input is rejected without touching the filesystem
    /// (H-10 — path-traversal guard).
    ///
    /// H-10: additionally verifies that the sum of segment
    /// byte sizes fits inside `declared_byte_size` and that the
    /// sum of segment durations is consistent with
    /// `declared_duration_ms`. `declared_byte_size` is the raw
    /// input total (audio samples + raw video frames), so it
    /// MUST be ≥ the sum of segment byte sizes.
    pub fn load_manifest(&self, root_hex: &str) -> MediaResult<MediaManifest> {
        validate_root_hex(root_hex)?;
        let path = self.manifest_path(root_hex);
        if !path.exists() {
            return Err(MediaError::Quarantined {
                cid: root_hex.to_string(),
            });
        }
        let bytes = std::fs::read(&path)?;
        let manifest: MediaManifest = bincode::deserialize(&bytes)?;
        let recomputed = compute_root_from_bytes(&bytes)?;
        if recomputed != manifest.root.as_hex() {
            return Err(MediaError::ManifestHashMismatch {
                expected: manifest.root.as_hex(),
                actual: recomputed,
            });
        }

        // H-10 declared_byte_size check. Use the variant with the
        // longest total bytes as the bound (a manifest may carry
        // multiple variants of the same clip — the byte budget
        // applies to the largest one, since `declared_byte_size`
        // is the raw input total).
        let max_variant_bytes: u64 = manifest
            .variants
            .iter()
            .map(|v| v.segments.iter().map(|s| s.byte_size).sum::<u64>())
            .max()
            .unwrap_or(0);
        let audio_bytes: u64 = manifest.audio.segments.iter().map(|s| s.byte_size).sum();
        let sum_bytes = max_variant_bytes.saturating_add(audio_bytes);
        if sum_bytes > manifest.declared_byte_size {
            return Err(MediaError::ManifestHashMismatch {
                expected: manifest.declared_byte_size.to_string(),
                actual: sum_bytes.to_string(),
            });
        }
        // Duration check: each variant + the audio track should
        // independently be within 1s of `declared_duration_ms`.
        // A variant whose segments sum to 60s while the clip is
        // "30s" is corrupt; a single variant at "30s ± 1s" is OK.
        let duration_drift_ms = |actual: u64| -> i64 {
            (actual as i64 - manifest.declared_duration_ms as i64).abs()
        };
        for v in &manifest.variants {
            let sum: u64 = v.segments.iter().map(|s| s.duration_ms).sum();
            if duration_drift_ms(sum) > 1_000 {
                return Err(MediaError::DurationMismatch {
                    declared: manifest.declared_duration_ms,
                    computed: sum,
                });
            }
        }
        let audio_duration_ms: u64 = manifest.audio.segments.iter().map(|s| s.duration_ms).sum();
        if duration_drift_ms(audio_duration_ms) > 1_000 {
            return Err(MediaError::DurationMismatch {
                declared: manifest.declared_duration_ms,
                computed: audio_duration_ms,
            });
        }
        Ok(manifest)
    }

    /// Resolve an alias to a manifest, then load it. The alias
    /// map's stored root must itself be a valid 64-char hex
    /// string before we touch the filesystem.
    pub fn load_by_alias(&self, alias: &str) -> MediaResult<MediaManifest> {
        if alias.is_empty() {
            return Err(MediaError::InvalidConfig("alias must not be empty".into()));
        }
        let map = self.load_alias_map()?;
        let root = map.get(alias).ok_or_else(|| {
            MediaError::InvalidConfig(format!("alias not found: {alias}"))
        })?;
        // The alias map could have been tampered with on disk;
        // validate the stored root before using it as a path
        // component.
        validate_root_hex(root)?;
        self.load_manifest(root)
    }

    /// Read a single segment by its DAG-level LP-prefixed digest
    /// AND the LP tag it was hashed under. The bytes returned
    /// are re-hashed and MUST match the LP-digest the caller
    /// asked for; otherwise we treat the store as tampered with.
    ///
    /// H-10: the kind byte is mandatory — without it we cannot
    /// know which domain-separated BLAKE3 hash to verify against.
    /// The DAG carries the kind on `SegmentDigest::kind`; the
    /// index sidecar records it on write so re-verify is
    /// unambiguous.
    pub fn load_segment_with_kind(
        &self,
        lp_digest_hex: &str,
        kind: u8,
    ) -> MediaResult<Vec<u8>> {
        validate_root_hex(lp_digest_hex)?;
        if kind != LP_VIDEO && kind != LP_AUDIO {
            return Err(MediaError::InvalidConfig(format!(
                "unknown segment kind 0x{kind:02x}; expected LP_VIDEO (0x01) or LP_AUDIO (0x02)"
            )));
        }
        let index = self.load_segment_index()?;
        let entry = index.get(lp_digest_hex).ok_or_else(|| {
            MediaError::IndexCorrupt {
                missing: lp_digest_hex.to_string(),
            }
        })?;
        if entry.kind != kind {
            return Err(MediaError::InvalidConfig(format!(
                "segment {lp_digest_hex} was stored under LP 0x{:02x} but caller asked for LP 0x{kind:02x}",
                entry.kind
            )));
        }
        let bytes = self.blobstore.get_sync_by_hex(&entry.blobstore_hash).ok_or_else(|| {
            MediaError::Quarantined { cid: entry.blobstore_hash.clone() }
        })?;
        let expected = decode_digest_hex(lp_digest_hex)?;
        let computed = crate::integrity::SegmentDigest::compute(kind, &bytes);
        if computed.bytes != expected {
            return Err(MediaError::ManifestHashMismatch {
                expected: lp_digest_hex.to_string(),
                actual: hex::encode(computed.bytes),
            });
        }
        Ok(bytes)
    }

    /// Verify the blobstore actually holds every segment
    /// referenced by the manifest. Re-loads the manifest through
    /// [`Self::load_manifest`] so the SR-2 root check and the
    /// H-10 byte/duration cross-checks run as part of every
    /// verify.
    pub fn verify_complete(
        &self,
        manifest: &MediaManifest,
    ) -> MediaResult<usize> {
        // Re-load the manifest by its declared root. This catches
        // cases where the in-memory manifest the caller passed us
        // has been tampered with, or where the on-disk file is
        // out of sync with the in-memory copy.
        let from_disk = self.load_manifest(&manifest.root.as_hex())?;
        if from_disk.root != manifest.root {
            return Err(MediaError::ManifestHashMismatch {
                expected: manifest.root.as_hex(),
                actual: from_disk.root.as_hex(),
            });
        }

        let index = self.load_segment_index()?;
        let mut count = 0usize;
        for v in &from_disk.variants {
            for s in &v.segments {
                let stored_hex = &s.digest.as_hex();
                let entry = index.get(stored_hex).ok_or_else(|| {
                    MediaError::IndexCorrupt {
                        missing: stored_hex.clone(),
                    }
                })?;
                if entry.kind != LP_VIDEO {
                    return Err(MediaError::InvalidConfig(format!(
                        "variant segment {stored_hex} stored under wrong LP tag 0x{:02x}",
                        entry.kind
                    )));
                }
                if !self.blobstore.has_complete_by_hex(&entry.blobstore_hash) {
                    return Err(MediaError::Quarantined {
                        cid: stored_hex.clone(),
                    });
                }
                count += 1;
            }
        }
        for s in &from_disk.audio.segments {
            let stored_hex = &s.digest.as_hex();
            let entry = index.get(stored_hex).ok_or_else(|| {
                MediaError::IndexCorrupt {
                    missing: stored_hex.clone(),
                }
            })?;
            if entry.kind != LP_AUDIO {
                return Err(MediaError::InvalidConfig(format!(
                    "audio segment {stored_hex} stored under wrong LP tag 0x{:02x}",
                    entry.kind
                )));
            }
            if !self.blobstore.has_complete_by_hex(&entry.blobstore_hash) {
                return Err(MediaError::Quarantined {
                    cid: stored_hex.clone(),
                });
            }
            count += 1;
        }
        Ok(count)
    }

    fn load_segment_index(
        &self,
    ) -> MediaResult<std::collections::BTreeMap<String, SegmentIndexEntry>> {
        if !self.index_path.exists() {
            return Ok(Default::default());
        }
        let bytes = std::fs::read(&self.index_path)?;
        if bytes.is_empty() {
            return Ok(Default::default());
        }
        let map: std::collections::BTreeMap<String, SegmentIndexEntry> =
            serde_json::from_slice(&bytes)?;
        Ok(map)
    }

    fn save_segment_index(
        &self,
        map: &std::collections::BTreeMap<String, SegmentIndexEntry>,
    ) -> MediaResult<()> {
        let bytes = serde_json::to_vec_pretty(map)?;
        let tmp = self.index_path.with_extension("json.tmp");
        if let Some(parent) = tmp.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.index_path)?;
        Ok(())
    }

    fn manifest_path(&self, root_hex: &str) -> PathBuf {
        self.manifests_dir.join(format!("{root_hex}.bin"))
    }

    fn load_alias_map(&self) -> MediaResult<AliasMap> {
        if !self.alias_path.exists() {
            return Ok(AliasMap::new());
        }
        let bytes = std::fs::read(&self.alias_path)?;
        if bytes.is_empty() {
            return Ok(AliasMap::new());
        }
        let map: AliasMap = serde_json::from_slice(&bytes)?;
        Ok(map)
    }

    fn save_alias_map(&self, map: &AliasMap) -> MediaResult<()> {
        let bytes = serde_json::to_vec_pretty(map)?;
        let tmp = self.alias_path.with_extension("json.tmp");
        if let Some(parent) = tmp.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.alias_path)?;
        Ok(())
    }
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> MediaResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("bin.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// Extensions on BlobStore so we can look up by raw hex without
// constructing an a3net_types ContentHash.
trait BlobStoreHexExt {
    fn get_sync_by_hex(&self, hex: &str) -> Option<Vec<u8>>;
    fn has_complete_by_hex(&self, hex: &str) -> bool;
}

impl BlobStoreHexExt for BlobStore {
    fn get_sync_by_hex(&self, hex: &str) -> Option<Vec<u8>> {
        let hash = a3net_types::content::ContentHash::from_hex(hex).ok()?;
        self.get_sync(&hash)
    }
    fn has_complete_by_hex(&self, hex: &str) -> bool {
        let Ok(hash) = a3net_types::content::ContentHash::from_hex(hex) else {
            return false;
        };
        self.has_complete(&hash)
    }
}

fn compute_root_from_bytes(bytes: &[u8]) -> MediaResult<String> {
    let manifest: MediaManifest = bincode::deserialize(bytes)?;
    let mut copy = manifest.clone();
    copy.compute_root()?;
    Ok(copy.root.as_hex())
}

/// Decode a 64-char lowercase hex string into its 32 raw bytes.
/// `validate_root_hex` MUST have been called first; this function
/// still returns `Result` so a future change to validation does
/// not silently degrade (H-6 — never `unwrap` a hex decode).
fn decode_digest_hex(s: &str) -> MediaResult<[u8; 32]> {
    let bytes = hex::decode(s).map_err(|e| {
        MediaError::InvalidConfig(format!("hex decode failed: {e}"))
    })?;
    if bytes.len() != 32 {
        return Err(MediaError::InvalidConfig(format!(
            "digest decoded to {} bytes, expected 32",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Maximum payload size for a single segment. Matches the IPFS
/// default block size and protects against OOM via caller-supplied
/// payloads (H-10 / DoS guard).
pub const MAX_SEGMENT_BYTES: usize = 64 * 1024 * 1024;

/// SR-10 path-traversal guard: only 64-char lowercase hex
/// (matching `MediaDigest::as_hex`) is allowed as a manifest root
/// or LP-segment digest in any public API. Anything else is
/// rejected without touching the filesystem.
fn validate_root_hex(s: &str) -> MediaResult<()> {
    if s.len() != 64 {
        return Err(MediaError::InvalidConfig(format!(
            "digest must be 64-char hex, got {} chars",
            s.len()
        )));
    }
    // A single pass: every byte must be hex AND every byte must
    // be lowercase. We accept only the canonical
    // `MediaDigest::as_hex` output to avoid case-insensitive
    // lookups creating duplicate filesystem entries.
    for &b in s.as_bytes() {
        let is_hex = b.is_ascii_digit()
            || (b'a'..=b'f').contains(&b)
            || (b'A'..=b'F').contains(&b);
        if !is_hex {
            return Err(MediaError::InvalidConfig(format!(
                "digest contains non-hex byte: 0x{b:02x}"
            )));
        }
        if b.is_ascii_uppercase() {
            return Err(MediaError::InvalidConfig(
                "digest must be lowercase hex".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_map_round_trip() {
        let mut m = AliasMap::new();
        let hex = "d".repeat(64);
        m.insert("intro", hex.clone());
        assert_eq!(m.get("intro"), Some(hex.as_str()));
        assert!(m.remove("intro").is_some());
        assert!(m.is_empty());
    }

    #[test]
    fn alias_map_serializes_to_json() {
        let mut m = AliasMap::new();
        let key = "intro".to_string();
        let hex = "b".repeat(64);
        m.insert(key.clone(), hex.clone());
        let bytes = serde_json::to_vec(&m).unwrap();
        let back: AliasMap = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.get(&key), Some(hex.as_str()));
    }

    #[test]
    fn alias_map_iter() {
        let mut m = AliasMap::new();
        m.insert("a", "a".repeat(64));
        m.insert("b", "b".repeat(64));
        let count = m.iter().count();
        assert_eq!(count, 2);
    }

    #[test]
    fn validate_root_hex_accepts_canonical() {
        assert!(validate_root_hex(&"a".repeat(64)).is_ok());
        assert!(validate_root_hex(&"0".repeat(64)).is_ok());
        assert!(validate_root_hex(&format!("{:064x}", u64::MAX)).is_ok());
    }

    #[test]
    fn validate_root_hex_rejects_wrong_length() {
        assert!(matches!(
            validate_root_hex(""),
            Err(MediaError::InvalidConfig(_))
        ));
        assert!(matches!(
            validate_root_hex("abc"),
            Err(MediaError::InvalidConfig(_))
        ));
        assert!(matches!(
            validate_root_hex(&"a".repeat(63)),
            Err(MediaError::InvalidConfig(_))
        ));
        assert!(matches!(
            validate_root_hex(&"a".repeat(65)),
            Err(MediaError::InvalidConfig(_))
        ));
    }

    #[test]
    fn validate_root_hex_rejects_non_hex() {
        // Replace the first character with 'g', which is not a
        // hex digit. Build a fresh String rather than mutating
        // bytes in place (the crate forbids unsafe).
        let mut bad = String::from("a");
        bad.push_str(&"a".repeat(63));
        bad.replace_range(0..1, "g");
        assert!(matches!(
            validate_root_hex(&bad),
            Err(MediaError::InvalidConfig(_))
        ));
    }

    #[test]
    fn validate_root_hex_rejects_uppercase() {
        let bad = "A".repeat(64);
        assert!(matches!(
            validate_root_hex(&bad),
            Err(MediaError::InvalidConfig(_))
        ));
    }

    #[test]
    fn validate_root_hex_rejects_path_traversal() {
        let attacks = [
            "../etc/passwd",
            "/etc/passwd",
            "..",
            ".",
            "abc",
            "deadbeef",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbe", // 63 chars
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeeff", // 65 chars
            &"g".repeat(64),
        ];
        for a in attacks {
            assert!(
                matches!(validate_root_hex(a), Err(MediaError::InvalidConfig(_))),
                "expected {a:?} to be rejected"
            );
        }
    }

    #[test]
    fn decode_digest_hex_round_trip() {
        let hex = "deadbeef".repeat(8);
        let bytes = decode_digest_hex(&hex).unwrap();
        let mut expected = [0u8; 32];
        expected[..8].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef]);
        expected[8..16].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef]);
        expected[16..24].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef]);
        expected[24..32].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn decode_digest_hex_rejects_bad_input() {
        assert!(decode_digest_hex("zz").is_err());
        assert!(decode_digest_hex(&"a".repeat(63)).is_err());
        assert!(decode_digest_hex(&"a".repeat(65)).is_err());
    }
}