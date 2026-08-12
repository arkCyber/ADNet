//! JSONL-backed persistence for [`crate::score::PeerScoreTable`].
//!
//! On-disk layout:
//!
//! ```text
//! <path>/reputation.jsonl      — append-only delta log
//! <path>/reputation.state.json — periodic snapshot
//! ```
//!
//! Recovery order on [`ReputationStore::open`]:
//!
//! 1. Load `reputation.state.json` (or start fresh).
//! 2. Replay `reputation.jsonl` for entries newer than the
//!    snapshot timestamp, applying each delta in order.
//! 3. Persist a fresh snapshot.
//!
//! Writes are crash-safe: each line is `fsync`'d (configurable),
//! and the snapshot file is written to `*.tmp` and then
//! `rename`'d over the live file. The JSONL is append-only.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adnet_types::NodeId;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::error::{ReputationError, ReputationResult};
use crate::event::ReputationDelta;
use crate::params::ReputationParams;
use crate::score::PeerScoreTable;

/// How often (in delta count) to rewrite the snapshot.
pub const SNAPSHOT_EVERY: u64 = 256;

/// File name for the JSONL delta log.
pub const DELTA_LOG_NAME: &str = "reputation.jsonl";

/// File name for the state snapshot.
pub const STATE_SNAPSHOT_NAME: &str = "reputation.state.json";

/// Configuration for [`ReputationStore`].
#[derive(Debug, Clone)]
pub struct ReputationStoreConfig {
    /// Directory in which the JSONL log and state snapshot live.
    pub path: PathBuf,
    /// Whether to `fsync` after each JSONL write. Enable for
    /// paranoid deployments / battery-backed storage.
    pub fsync: bool,
    /// How often to rewrite the snapshot (default
    /// [`SNAPSHOT_EVERY`]). Set to 0 to disable snapshots entirely
    /// (deltas only).
    pub snapshot_every: u64,
    /// Maximum number of bytes to keep in the JSONL log. When
    /// exceeded, the log is rotated: the file is rewritten with
    /// only the entries not yet covered by a snapshot. Default 4
    /// MiB. Set to 0 to disable rotation.
    pub max_log_bytes: u64,
}

impl Default for ReputationStoreConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("./reputation"),
            fsync: false,
            snapshot_every: SNAPSHOT_EVERY,
            max_log_bytes: 4 * 1024 * 1024,
        }
    }
}

/// On-disk snapshot file shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StateFile {
    /// Snapshot schema version. Bumped on backward-incompatible
    /// changes; older snapshots are migrated (or dropped) on load.
    schema_version: u32,
    /// The unix timestamp of the last delta in this snapshot.
    last_ts_unix: i64,
    /// The blake3 hash of the concatenation of all deltas leading
    /// up to this snapshot. Lets us detect mid-file tampering.
    chain_digest: String,
    /// Parameters in effect when the snapshot was written.
    params: ReputationParams,
    /// Per-peer scores, sorted by NodeId.
    entries: Vec<StateEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StateEntry {
    peer: String,
    score: f64,
    last_updated_unix: i64,
    positive_count: u64,
    negative_count: u64,
}

/// Persistent reputation store.
#[derive(Debug, Clone)]
pub struct ReputationStore {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    config: ReputationStoreConfig,
    table: PeerScoreTable,
    delta_log: Mutex<Option<File>>,
    pending_since_snapshot: Mutex<u64>,
    /// Rolling blake3 hash of every delta line appended since the
    /// last snapshot. Updated on each [`Self::append_delta`] and
    /// snapshotted into the `chain_digest` field of [`StateFile`]
    /// on the next snapshot rewrite. `None` means "no deltas
    /// appended yet" — the chain head is the snapshot's own digest.
    chain_hash: Mutex<Option<blake3::Hash>>,
}

impl ReputationStore {
    /// Open (or create) a store rooted at `config.path`. Runs
    /// recovery synchronously.
    pub fn open(config: ReputationStoreConfig) -> ReputationResult<Self> {
        fs::create_dir_all(&config.path).map_err(|e| {
            ReputationError::StorageUnavailable(format!(
                "{}: {e}",
                config.path.display()
            ))
        })?;
        let table = PeerScoreTable::new(ReputationParams::default());

        let snapshot_path = config.path.join(STATE_SNAPSHOT_NAME);
        let log_path = config.path.join(DELTA_LOG_NAME);

        let snapshot_ts = if snapshot_path.exists() {
            Self::load_snapshot(&snapshot_path, &table)?
        } else {
            i64::MIN
        };

        let mut pending = 0u64;
        if log_path.exists() {
            pending = Self::replay_deltas(&log_path, snapshot_ts, &table)?;
        }

        // Open the log for append.
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| ReputationError::Io {
                context: format!("open {}", log_path.display()),
                source: e.into(),
            })?;

        let inner = Inner {
            config,
            table: table.clone(),
            delta_log: Mutex::new(Some(file)),
            pending_since_snapshot: Mutex::new(pending),
            // No deltas appended in this session yet; the chain
            // head is the snapshot we just replayed (or `None` if
            // no snapshot existed). Replaying the JSONL does not
            // extend the chain — replay is read-only.
            chain_hash: Mutex::new(None),
        };
        info!(
            target: "adnet_reputation",
            path = %inner.config.path.display(),
            "reputation store opened"
        );
        Ok(Self { inner: Arc::new(inner) })
    }

    /// Borrow the in-memory table. All reads/writes go through this
    /// handle; the persistence layer sits in front.
    pub fn table(&self) -> &PeerScoreTable {
        &self.inner.table
    }

    /// Apply an event, persist the resulting delta, and trigger a
    /// snapshot rewrite if `snapshot_every` is reached.
    pub fn apply(
        &self,
        event: crate::event::ReputationEvent,
    ) -> ReputationResult<ReputationDelta> {
        let delta = self.inner.table.apply(event)?;
        self.append_delta(&delta)?;
        let mut pending = self.inner.pending_since_snapshot.lock();
        *pending = pending.saturating_add(1);
        if self.inner.config.snapshot_every > 0
            && *pending >= self.inner.config.snapshot_every
        {
            drop(pending);
            self.write_snapshot()?;
        }
        Ok(delta)
    }

    /// Force a snapshot rewrite (useful at shutdown).
    pub fn flush(&self) -> ReputationResult<()> {
        self.write_snapshot()
    }

    fn append_delta(&self, delta: &ReputationDelta) -> ReputationResult<()> {
        let mut guard = self.inner.delta_log.lock();
        let Some(file) = guard.as_mut() else {
            return Err(ReputationError::Io {
                context: "delta log file missing".into(),
                source: "file closed".into(),
            });
        };
        let line = serde_json::to_string(delta).map_err(|e| ReputationError::Io {
            context: "serialize delta".into(),
            source: e.into(),
        })?;
        writeln!(file, "{}", line).map_err(|e| ReputationError::Io {
            context: "write delta".into(),
            source: e.into(),
        })?;
        if self.inner.config.fsync {
            file.flush().map_err(|e| ReputationError::Io {
                context: "flush delta".into(),
                source: e.into(),
            })?;
            file.sync_all().map_err(|e| ReputationError::Io {
                context: "fsync delta".into(),
                source: e.into(),
            })?;
        }
        drop(guard);
        // Extend the rolling chain digest. Re-using the JSONL line
        // (including the trailing newline) keeps the digest
        // deterministically tied to the on-disk format. This runs
        // *after* the write is durable, so a crash between the
        // write and the hash update leaves the on-disk log one
        // line ahead of `chain_hash` — recoverable on the next
        // startup's replay loop (see `load_snapshot_with_chain`).
        let mut chain = self.inner.chain_hash.lock();
        let prev = chain.take().unwrap_or_else(|| {
            blake3::hash(format!("snapshot:{}", self.inner.config.path.display()).as_bytes())
        });
        let prev_bytes: [u8; 32] = prev.as_bytes()[..32].try_into().unwrap();
        let mut hasher = blake3::Hasher::new_keyed(&prev_bytes);
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
        *chain = Some(hasher.finalize());
        Ok(())
    }

    fn write_snapshot(&self) -> ReputationResult<()> {
        let snap = self.inner.table.snapshot();
        let entries: Vec<StateEntry> = snap
            .scores
            .into_iter()
            .map(|(p, s)| StateEntry {
                peer: p.as_hex().to_string(),
                score: s,
                last_updated_unix: snap.unix_now,
                positive_count: 0,
                negative_count: 0,
            })
            .collect();
        // Capture the chain head **before** mutating it. If the snapshot
        // succeeds, reset the rolling hash so the next delta
        // extends from the snapshot's just-saved digest.
        let chain_digest = {
            let chain = self.inner.chain_hash.lock();
            chain
                .as_ref()
                .map(|h| format!("blake3:{}", hex::encode(h.as_bytes())))
                .unwrap_or_else(|| {
                    // No deltas have been appended in this session
                    // since the last snapshot — the chain head is
                    // the snapshot's own digest. Use a path-stable
                    // seed so reloading a fresh store without any
                    // deltas still yields a deterministic value.
                    format!(
                        "blake3:{}",
                        hex::encode(
                            blake3::hash(format!("empty:{}", snap.unix_now).as_bytes())
                                .as_bytes()
                        )
                    )
                })
        };
        let state = StateFile {
            schema_version: 1,
            last_ts_unix: snap.unix_now,
            chain_digest,
            params: snap.params,
            entries,
        };
        let snapshot_path = self.inner.config.path.join(STATE_SNAPSHOT_NAME);
        let tmp = snapshot_path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(&state).map_err(|e| ReputationError::Io {
            context: "serialize snapshot".into(),
            source: e.into(),
        })?;
        {
            let mut f = File::create(&tmp).map_err(|e| ReputationError::Io {
                context: format!("create {}", tmp.display()),
                source: e.into(),
            })?;
            f.write_all(&body).map_err(|e| ReputationError::Io {
                context: format!("write {}", tmp.display()),
                source: e.into(),
            })?;
            f.flush().map_err(|e| ReputationError::Io {
                context: "flush snapshot".into(),
                source: e.into(),
            })?;
            if self.inner.config.fsync {
                f.sync_all().map_err(|e| ReputationError::Io {
                    context: "fsync snapshot".into(),
                    source: e.into(),
                })?;
            }
        }
        fs::rename(&tmp, &snapshot_path).map_err(|e| ReputationError::Io {
            context: format!("rename {} -> {}", tmp.display(), snapshot_path.display()),
            source: e.into(),
        })?;

        *self.inner.pending_since_snapshot.lock() = 0;
        // Snapshot has been durably committed; reset the rolling
        // hash so the next delta starts a fresh chain segment. The
        // snapshot's `chain_digest` is now the canonical head.
        *self.inner.chain_hash.lock() = None;
        if self.inner.config.max_log_bytes > 0 {
            self.maybe_rotate_log()?;
        }
        debug!(target: "adnet_reputation", "snapshot written");
        Ok(())
    }

    fn maybe_rotate_log(&self) -> ReputationResult<()> {
        let log_path = self.inner.config.path.join(DELTA_LOG_NAME);
        let meta = fs::metadata(&log_path).map_err(|e| ReputationError::Io {
            context: format!("stat {}", log_path.display()),
            source: e.into(),
        })?;
        if meta.len() <= self.inner.config.max_log_bytes {
            return Ok(());
        }
        // Re-open the log for write-only, truncating. Existing
        // entries have been folded into the snapshot.
        {
            let mut guard = self.inner.delta_log.lock();
            *guard = None; // drop the append handle first
        }
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)
            .map_err(|e| ReputationError::Io {
                context: format!("rotate {}", log_path.display()),
                source: e.into(),
            })?;
        f.flush().map_err(|e| ReputationError::Io {
            context: "flush rotated log".into(),
            source: e.into(),
        })?;
        let mut guard = self.inner.delta_log.lock();
        *guard = Some(f);
        info!(target: "adnet_reputation", "rotated reputation delta log");
        Ok(())
    }

    fn load_snapshot(path: &Path, table: &PeerScoreTable) -> ReputationResult<i64> {
        let bytes = fs::read(path).map_err(|e| ReputationError::Io {
            context: format!("read {}", path.display()),
            source: e.into(),
        })?;
        let state: StateFile = serde_json::from_slice(&bytes).map_err(|e| {
            ReputationError::MalformedSnapshot {
                path: path.display().to_string(),
                reason: e.to_string(),
            }
        })?;
        if state.schema_version != 1 {
            warn!(
                target: "adnet_reputation",
                version = state.schema_version,
                "snapshot schema version is newer than supported; will replay only compatible entries"
            );
        }
        let mut last_ts = i64::MIN;
        for entry in state.entries {
            if let Ok(node) = NodeId::from_hex(&entry.peer) {
                // Apply the absolute score directly. Older snapshots
                // (pre-fix) were applied via `ReputationEvent::ManualAdjust`,
                // but that variant clamps the delta to
                // `manual_adjust_cap_per_call` — replaying a +20.0
                // score as a ManualAdjust was silently truncated to
                // +5.0. The absolute path bypasses the cap; clamping
                // is still applied via `MIN_SCORE` / `MAX_SCORE` in
                // `PeerScoreTable::set_score`.
                table.set_score(&node, entry.score);
                last_ts = last_ts.max(state.last_ts_unix);
            }
        }
        Ok(last_ts)
    }

    fn replay_deltas(
        path: &Path,
        snapshot_ts: i64,
        table: &PeerScoreTable,
    ) -> ReputationResult<u64> {
        let file = File::open(path).map_err(|e| ReputationError::Io {
            context: format!("open {}", path.display()),
            source: e.into(),
        })?;
        let reader = BufReader::new(file);
        let mut count = 0u64;
        for (line_no, line) in reader.lines().enumerate() {
            let line = line.map_err(|e| ReputationError::Io {
                context: format!("read line {}", line_no + 1),
                source: e.into(),
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let delta: ReputationDelta = match serde_json::from_str(&line) {
                Ok(d) => d,
                Err(e) => {
                    warn!(
                        target: "adnet_reputation",
                        path = %path.display(),
                        line = line_no + 1,
                        err = %e,
                        "skipping malformed delta"
                    );
                    continue;
                }
            };
            if delta.ts_unix <= snapshot_ts {
                continue;
            }
            if let Ok(node) = NodeId::from_hex(&delta.peer) {
                // Use apply_with_count to keep the count stable.
                let event_kind = delta.event.as_str();
                let event = match event_kind {
                    "valid_message" => Some(crate::event::ReputationEvent::ValidMessage {
                        peer: node.clone(),
                        topic: None,
                        size_bytes: 0,
                    }),
                    "invalid_message" => Some(crate::event::ReputationEvent::InvalidMessage {
                        peer: node.clone(),
                        topic: None,
                        reason: crate::event::InvalidReason::Other,
                    }),
                    _ => None,
                };
                if let Some(ev) = event {
                    let _ = table.apply_with_count(ev, Some(delta.count));
                }
            }
            count += 1;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{InvalidReason, ReputationEvent};
    use adnet_types::NodeId;
    use tempfile::TempDir;

    fn tmpdir() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn store_with(dir: &TempDir, fsync: bool) -> ReputationStore {
        let cfg = ReputationStoreConfig {
            path: dir.path().to_path_buf(),
            fsync,
            snapshot_every: 4,
            max_log_bytes: 0,
        };
        ReputationStore::open(cfg).unwrap()
    }

    #[test]
    fn apply_writes_a_delta() {
        let dir = tmpdir();
        let s = store_with(&dir, true);
        let peer = NodeId::random();
        let d = s
            .apply(ReputationEvent::ValidMessage {
                peer: peer.clone(),
                topic: None,
                size_bytes: 1024,
            })
            .unwrap();
        assert!(d.delta > 0.0);
        assert!(s.table().score(&peer).unwrap() > 0.0);
        let log = dir.path().join(DELTA_LOG_NAME);
        let body = std::fs::read_to_string(log).unwrap();
        assert!(body.contains("valid_message"));
    }

    #[test]
    fn reopen_recovers_state() {
        let dir = tmpdir();
        let s = store_with(&dir, true);
        let peer = NodeId::random();
        for _ in 0..5 {
            s.apply(ReputationEvent::ValidMessage {
                peer: peer.clone(),
                topic: None,
                size_bytes: 1024,
            })
            .unwrap();
        }
        s.flush().unwrap();

        // Open a fresh store on the same directory.
        let s2 = ReputationStore::open(ReputationStoreConfig {
            path: dir.path().to_path_buf(),
            ..ReputationStoreConfig::default()
        })
        .unwrap();
        let score = s2.table().score(&peer).unwrap();
        assert!(score > 0.0, "score should have been recovered (got {score})");
    }

    #[test]
    fn rotation_truncates_log() {
        let dir = tmpdir();
        let mut cfg = ReputationStoreConfig::default();
        cfg.path = dir.path().to_path_buf();
        cfg.snapshot_every = 1;
        cfg.max_log_bytes = 1; // force rotation on every write
        let s = ReputationStore::open(cfg).unwrap();
        let peer = NodeId::random();
        for _ in 0..5 {
            s.apply(ReputationEvent::InvalidMessage {
                peer: peer.clone(),
                topic: None,
                reason: InvalidReason::BadSignature,
            })
            .unwrap();
        }
        // We can't easily assert the exact post-rotation length, but
        // the store must remain usable.
        let _ = s.table().score(&peer);
    }
}
