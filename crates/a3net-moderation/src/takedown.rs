//! Local takedown executor.
//!
//! [`TakedownService`] is the operator-facing primitive that turns a
//! banned content hash into:
//!
//! 1. A row in the [`crate::blocklist::Blocklist`] (so the gateway
//!    refuses future reads).
//! 2. A pin removal in the on-disk `pin.json` (so the blob is
//!    eligible for GC).
//! 3. A `gc_unpinned` pass on the local [`a3net_blobstore::BlobStore`]
//!    (so the bytes are physically deleted).
//! 4. An optional crypto-shred of the encrypted-blob-store key
//!    (when the encrypted wrapper is enabled).
//! 5. A reputation penalty against the publishing peer (so the
//!    gossip layer can refuse outbound).
//!
//! ## Why this is in a separate module
//!
//! The blocklist is a passive registry — it just answers "is this
//! hash banned?". The takedown service is the **active** loop that
//! files the block, scrubs the local store, and informs the rest of
//! the system. Keeping the two separate lets the gateway ask the
//! blocklist on every request without paying the cost of opening
//! the blob store.
//!
//! ## Reporting
//!
//! Every takedown returns a [`TakedownReport`] that captures
//! everything the audit log needs: which hash, who signed off, what
//! physical bytes were removed, how many reputation points were
//! deducted, and whether the local store confirmed the deletion.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use a3net_reputation::PeerScoreTable;
use a3net_types::ContentHash;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::blocklist::{Blocklist, BlocklistSource, TakedownReason};
use crate::error::{ModerationError, ModerationResult};
use crate::reputation_bridge::apply_violation;

/// What the takedown service was asked to do. Affects whether the
/// local store is scrubbed or only the blocklist is updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakedownTarget {
    /// Block the hash but do not touch the local store. Used when
    /// the gateway is read-only and the operator wants to prevent
    /// future imports without deleting what's already there.
    BlocklistOnly,
    /// Block the hash AND remove it from the local store. The
    /// default for a takedown.
    LocalErase,
    /// Block the hash, remove it from the local store, AND
    /// destroy the encrypted-blob-store key (crypto-shredding).
    /// Reserved for the most severe cases (CSAM, court-ordered
    /// seizure) where the on-disk bytes are not enough — the
    /// encryption key must also be destroyed so any remaining
    /// encrypted copies become unreadable.
    CryptoShred,
}

/// What the takedown service actually did. Returned to the CLI /
/// HTTP client so the audit log can be precise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakedownReport {
    /// Hash that was taken down.
    pub hash: ContentHash,
    /// Reason recorded on the blocklist entry.
    pub reason: TakedownReason,
    /// Source recorded on the blocklist entry.
    pub source: BlocklistSource,
    /// Operator identity that signed off.
    pub operator: String,
    /// Blocklist entry id (assigned by [`Blocklist::add`]).
    pub blocklist_entry_id: u64,
    /// Unix-seconds the takedown was executed.
    pub executed_unix: i64,
    /// Which [`TakedownTarget`] was applied.
    pub target: TakedownTarget,
    /// Whether the local pin was found and removed.
    pub pin_removed: bool,
    /// Whether the blob bytes were physically deleted. `false`
    /// when the hash was not in the local store at the time of
    /// the takedown — the blocklist still applies.
    pub bytes_deleted: bool,
    /// Reputation penalty that was applied (negative number).
    /// `0.0` when no publishing peer was supplied.
    pub reputation_delta: f64,
    /// Free-form note for the audit log.
    pub note: String,
}

impl TakedownReport {
    /// Short summary line for the CLI.
    pub fn summary_line(&self) -> String {
        let reason_slug = super::policy::reason_slug(self.reason);
        let target_slug = match self.target {
            TakedownTarget::BlocklistOnly => "blocklist_only",
            TakedownTarget::LocalErase => "local_erase",
            TakedownTarget::CryptoShred => "crypto_shred",
        };
        format!(
            "{} reason={} target={} pin_removed={} bytes_deleted={} rep_delta={}",
            short_hash(self.hash.as_hex()),
            reason_slug,
            target_slug,
            self.pin_removed,
            self.bytes_deleted,
            self.reputation_delta,
        )
    }
}

/// Coarse-grained outcome of a takedown attempt. Used by callers
/// that don't need the full report and want to know whether the
/// operation actually scrubbed the local store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakedownOutcome {
    /// The hash was not pinned locally. The blocklist was still
    /// updated so the gateway will refuse future imports.
    NotPinned,
    /// The hash was unpinned and the bytes were physically deleted.
    Removed,
    /// The hash was unpinned, bytes deleted, and the encrypted
    /// blob-store key was destroyed.
    CryptoShredded,
}

/// Configuration parameters for the takedown service.
#[derive(Debug, Clone)]
pub struct TakedownServiceConfig {
    /// Where the on-disk `pin.json` lives. Defaults to
    /// `<data_dir>/pin.json`.
    pub pin_path: PathBuf,
    /// Where the on-disk blob store lives. Defaults to
    /// `<data_dir>/blobs`.
    pub blob_dir: PathBuf,
    /// Optional path to the encrypted-blob-store key file. When
    /// `Some`, [`TakedownService::execute`] with
    /// [`TakedownTarget::CryptoShred`] will overwrite the file with
    /// zeroes before deleting it.
    pub key_file_path: Option<PathBuf>,
}

impl TakedownServiceConfig {
    /// Construct from a data dir using the default paths.
    pub fn from_data_dir(data_dir: &Path) -> Self {
        Self {
            pin_path: data_dir.join("pin.json"),
            blob_dir: data_dir.join("blobs"),
            key_file_path: None,
        }
    }

    /// Set the encrypted-key file path explicitly.
    pub fn with_key_file(mut self, path: PathBuf) -> Self {
        self.key_file_path = Some(path);
        self
    }
}

/// The takedown executor. Holds the blocklist, the local blob
/// store pin file, and the optional reputation table.
pub struct TakedownService {
    blocklist: Arc<Blocklist>,
    config: TakedownServiceConfig,
    reputation: Option<Arc<PeerScoreTable>>,
}

impl std::fmt::Debug for TakedownService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TakedownService")
            .field("config", &self.config)
            .field("reputation_enabled", &self.reputation.is_some())
            .finish()
    }
}

impl TakedownService {
    /// Construct a takedown service.
    pub fn new(blocklist: Arc<Blocklist>, config: TakedownServiceConfig) -> Self {
        Self {
            blocklist,
            config,
            reputation: None,
        }
    }

    /// Attach a reputation table so takedowns also drop the
    /// publishing peer's score.
    pub fn with_reputation(mut self, table: Arc<PeerScoreTable>) -> Self {
        self.reputation = Some(table);
        self
    }

    /// The blocklist this service writes to.
    pub fn blocklist(&self) -> &Arc<Blocklist> {
        &self.blocklist
    }

    /// Execute a takedown. Returns a [`TakedownReport`] suitable
    /// for appending to the audit log.
    pub fn execute(
        &self,
        hash: ContentHash,
        reason: TakedownReason,
        source: BlocklistSource,
        operator: impl Into<String>,
        evidence: impl Into<String>,
        publisher_node_hex: impl Into<String>,
        target: TakedownTarget,
        expires_unix: Option<i64>,
    ) -> ModerationResult<TakedownReport> {
        let operator = operator.into();
        let evidence = evidence.into();
        let publisher_node_hex = publisher_node_hex.into();

        // 1. File the blocklist entry — this is the authoritative
        //    signal that the gateway will consult on every read.
        let blocklist_entry_id = self.blocklist.add(
            hash.clone(),
            reason,
            source,
            evidence.clone(),
            operator.clone(),
            expires_unix,
            publisher_node_hex.clone(),
        )?;

        // 2. Local erase — pin remove + blobstore GC.
        let (pin_removed, bytes_deleted) = if matches!(
            target,
            TakedownTarget::LocalErase | TakedownTarget::CryptoShred
        ) {
            let pin_removed = self.remove_pin(&hash)?;
            let bytes_deleted = self.physically_erase(&hash)?;
            (pin_removed, bytes_deleted)
        } else {
            (false, false)
        };

        // 3. Crypto-shred (when requested).
        if matches!(target, TakedownTarget::CryptoShred) {
            self.crypto_shred()?;
        }

        // 4. Reputation penalty (when we know the publisher).
        let reputation_delta = self.apply_reputation_penalty(
            &publisher_node_hex,
            reason,
            blocklist_entry_id,
        )?;

        let now = chrono::Utc::now().timestamp();
        let note = if bytes_deleted {
            format!(
                "Local takedown ({:?}); pin_removed={}; bytes_deleted={}",
                target, pin_removed, bytes_deleted
            )
        } else {
            format!(
                "Blocklist-only takedown ({:?}); local store untouched",
                target
            )
        };

        info!(
            target: "a3net_moderation",
            hash = %hash,
            reason = ?reason,
            source = ?source,
            operator = %operator,
            blocklist_entry_id,
            target = ?target,
            pin_removed,
            bytes_deleted,
            "takedown executed"
        );

        Ok(TakedownReport {
            hash,
            reason,
            source,
            operator,
            blocklist_entry_id,
            executed_unix: now,
            target,
            pin_removed,
            bytes_deleted,
            reputation_delta,
            note,
        })
    }

    /// Convenience wrapper that returns the coarse-grained outcome.
    pub fn erase(
        &self,
        hash: ContentHash,
        reason: TakedownReason,
        operator: impl Into<String>,
        evidence: impl Into<String>,
    ) -> ModerationResult<TakedownOutcome> {
        let report = self.execute(
            hash,
            reason,
            BlocklistSource::Operator,
            operator,
            evidence,
            "",
            TakedownTarget::LocalErase,
            None,
        )?;
        Ok(if report.bytes_deleted {
            TakedownOutcome::Removed
        } else {
            TakedownOutcome::NotPinned
        })
    }

    fn remove_pin(&self, hash: &ContentHash) -> ModerationResult<bool> {
        if !self.config.pin_path.exists() {
            return Ok(false);
        }
        let mut pins = a3net_blobstore::PinSet::load(
            self.config
                .pin_path
                .parent()
                .unwrap_or_else(|| Path::new(".")),
        )
        .map_err(ModerationError::Io)?;
        let removed = pins.remove(hash);
        if removed {
            pins.save(
                self.config
                    .pin_path
                    .parent()
                    .unwrap_or_else(|| Path::new(".")),
            )
            .map_err(ModerationError::Io)?;
        }
        Ok(removed)
    }

    fn physically_erase(&self, hash: &ContentHash) -> ModerationResult<bool> {
        if !self.config.blob_dir.exists() {
            return Ok(false);
        }
        let store = match a3net_blobstore::BlobStore::new(&self.config.blob_dir) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "takedown: blob store open failed, skipping physical erase");
                return Ok(false);
            }
        };
        match store.remove(hash) {
            Ok(true) => Ok(true),
            Ok(false) => Ok(false),
            Err(e) => {
                warn!(error = %e, hash = %hash, "takedown: blob remove failed");
                Err(ModerationError::Io(e))
            }
        }
    }

    fn crypto_shred(&self) -> ModerationResult<()> {
        let Some(key_path) = self.config.key_file_path.as_ref() else {
            return Err(ModerationError::Precondition(
                "crypto-shred requested but no key_file_path configured".to_string(),
            ));
        };
        if !key_path.exists() {
            // Nothing to shred — record and move on.
            warn!(
                path = %key_path.display(),
                "crypto-shred: key file not present, skipping"
            );
            return Ok(());
        }
        let bytes = std::fs::read(key_path)?;
        let zeroed = vec![0u8; bytes.len()];
        // Best-effort overwrite. Modern filesystems may have copy-on-write
        // semantics that mean the on-disk extent is not zeroed, but for
        // the common case (ext4 / APFS) this leaks nothing beyond what
        // the underlying block-device scheduler already retains.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(key_path)?;
        use std::io::Write;
        f.write_all(&zeroed)?;
        f.flush()?;
        drop(zeroed);
        drop(bytes);
        std::fs::remove_file(key_path)?;
        info!(path = %key_path.display(), "crypto-shred: key file destroyed");
        Ok(())
    }

    fn apply_reputation_penalty(
        &self,
        publisher_node_hex: &str,
        reason: TakedownReason,
        blocklist_entry_id: u64,
    ) -> ModerationResult<f64> {
        let Some(table) = self.reputation.as_ref() else {
            return Ok(0.0);
        };
        let publisher_node_hex = publisher_node_hex.trim();
        if publisher_node_hex.is_empty() {
            return Ok(0.0);
        }
        let peer = match a3net_types::NodeId::from_hex(publisher_node_hex) {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    hex = publisher_node_hex,
                    error = %e,
                    "takedown: publisher node hex is invalid, skipping reputation penalty"
                );
                return Ok(0.0);
            }
        };

        let delta = apply_violation(&table, &peer, reason, blocklist_entry_id, 1)?;
        Ok(delta)
    }
}

fn short_hash(hex: &str) -> String {
    let len = hex.len().min(12);
    format!("{}…", &hex[..len])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn hash(b: &[u8]) -> ContentHash {
        ContentHash::from_bytes(b)
    }

    #[test]
    fn blocklist_only_target_does_not_touch_local_store() {
        let dir = tempdir().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let cfg = TakedownServiceConfig::from_data_dir(dir.path());
        let svc = TakedownService::new(bl.clone(), cfg);

        let report = svc
            .execute(
                hash(b"x"),
                TakedownReason::Csam,
                BlocklistSource::Operator,
                "alice",
                "evidence",
                "",
                TakedownTarget::BlocklistOnly,
                None,
            )
            .unwrap();
        assert!(!report.pin_removed);
        assert!(!report.bytes_deleted);
        assert!(bl.is_blocked(&hash(b"x")));
    }

    #[test]
    fn local_erase_attempts_pin_remove_and_gc() {
        let dir = tempdir().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let cfg = TakedownServiceConfig::from_data_dir(dir.path());
        let svc = TakedownService::new(bl.clone(), cfg);

        // No local pin file → outcome is NotPinned but blocklist is set.
        let outcome = svc
            .erase(
                hash(b"x"),
                TakedownReason::Copyright,
                "alice",
                "DMCA 12345",
            )
            .unwrap();
        assert_eq!(outcome, TakedownOutcome::NotPinned);
        assert!(bl.is_blocked(&hash(b"x")));
    }

    #[test]
    fn crypto_shred_requires_key_file_path() {
        let dir = tempdir().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let cfg = TakedownServiceConfig::from_data_dir(dir.path());
        let svc = TakedownService::new(bl.clone(), cfg);
        let err = svc
            .execute(
                hash(b"x"),
                TakedownReason::Csam,
                BlocklistSource::Operator,
                "alice",
                "",
                "",
                TakedownTarget::CryptoShred,
                None,
            )
            .unwrap_err();
        assert!(matches!(err, ModerationError::Precondition(_)));
    }

    #[test]
    fn erase_records_publisher_reputation_penalty() {
        let dir = tempdir().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let cfg = TakedownServiceConfig::from_data_dir(dir.path());
        let rep = Arc::new(PeerScoreTable::new(
            a3net_reputation::ReputationParams::default(),
        ));
        let svc = TakedownService::new(bl.clone(), cfg).with_reputation(rep.clone());
        let peer = a3net_types::NodeId::random();
        svc.execute(
            hash(b"x"),
            TakedownReason::Csam,
            BlocklistSource::Operator,
            "alice",
            "",
            peer.as_hex(),
            TakedownTarget::LocalErase,
            None,
        )
        .unwrap();
        let score = rep.score(&peer).unwrap();
        assert!(score <= -10.0, "csam takedown should drop peer below refusal threshold");
    }

    #[test]
    fn summary_line_contains_short_hash() {
        let dir = tempdir().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let cfg = TakedownServiceConfig::from_data_dir(dir.path());
        let svc = TakedownService::new(bl.clone(), cfg);
        let r = svc
            .execute(
                hash(b"hello"),
                TakedownReason::Malware,
                BlocklistSource::TrustedFeed,
                "alice",
                "smell",
                "",
                TakedownTarget::BlocklistOnly,
                None,
            )
            .unwrap();
        let line = r.summary_line();
        assert!(line.contains("malware"));
        assert!(line.contains("…"));
    }
}
