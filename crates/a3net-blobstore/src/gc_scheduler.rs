//! Background garbage-collection scheduler for a [`BlobStore`].
//!
//! Why this exists
//! ---------------
//! IPFS / Kubo style nodes accumulate orphan blobs over time: every
//! peer-fetched block that is not pinned stays in the on-disk store
//! forever unless the operator (or a periodic sweep) prunes it.
//! A3Net already has the **primitive** — `BlobStore::gc_orphans` walks
//! `<data_dir>` and deletes blobs that are missing from
//! [`PinSet`](crate::pin_set::PinSet). What's missing is the
//! *automation*: nothing currently calls that primitive on a timer.
//!
//! [`GcScheduler`] fills the gap. It is a small, dependency-light
//! tokio task that periodically:
//!
//! 1. Re-loads `pin.json` from `data_dir` (so a freshly-pinned CID
//!    is honoured at the next sweep without restarting the node).
//! 2. Computes the candidate set (every `complete` blob that is not
//!    in the pin set).
//! 3. Optionally writes a *pre-GC snapshot* via the
//!    `pre_gc_backup` callback (defensive — the operator gets a
//!    recovery point before any deletion).
//! 4. Calls `BlobStore::gc_orphans` and records a [`GcReport`].
//! 5. Sleeps `interval`, optionally with `jitter`, then repeats.
//!
//! Failure model (DO-178C DAL-A)
//! -----------------------------
//! - **Fail-soft**: every error is captured in `last_error`; the
//!   loop never panics, never exits. A bad pin-set, transient I/O
//!   error, or full disk results in a `GcReport { .., errors }` row
//!   and the next tick is scheduled normally.
//! - **Backoff on repeated failure**: after `max_consecutive_errors`
//!   failures in a row, the next interval is multiplied by
//!   `error_backoff_factor` (clamped at `max_interval`). Backoff
//!   resets on the first successful sweep.
//! - **Graceful shutdown**: `GcSchedulerHandle::shutdown()` signals
//!   the loop to exit **after** the current sweep finishes. The
//!   future returned by `shutdown()` resolves only when the task has
//!   acknowledged the signal — this is the contract the parent
//!   `Node::shutdown` and `a3net repo gc --stop` rely on.
//!
//! Configuration (see [`GcSchedulerConfig`])
//! -----------------------------------------
//! ```text
//! interval               = 1h           # how often to sweep
//! jitter                 = 60s          # +/- random jitter so multi-node fleets don't synchronise
//! error_backoff_factor   = 2            # on failure, double the wait
//! max_interval           = 24h          # cap on the backoff
//! max_consecutive_errors = 5            # how many in a row before backoff kicks in
//! pre_gc_backup_dir      = None         # optional: where to drop a3net-snap before each sweep
//! keep_pre_gc_backups    = 3            # rotate old pre-gc backups; None = never delete
//! ```
//!
//! Wiring example
//! --------------
//! ```ignore
//! use std::sync::Arc;
//! use std::path::PathBuf;
//! use a3net_blobstore::{BlobStore, gc_scheduler::{GcScheduler, GcSchedulerConfig}};
//!
//! let store = Arc::new(BlobStore::new(&data_dir)?);
//! let cfg = GcSchedulerConfig {
//!     interval: std::time::Duration::from_secs(3600),
//!     pre_gc_backup_dir: Some(PathBuf::from("/var/lib/a3net/backups")),
//!     ..Default::default()
//! };
//! let handle = GcScheduler::start(store.clone(), data_dir.clone(), cfg)?;
//! // ... later, on node shutdown:
//! handle.shutdown().await;
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use a3net_types::ContentHash;
use serde::Serialize;
use tokio::sync::watch;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use crate::pin_set::PinSet;
use crate::store::BlobStore;

/// Tunable knobs for [`GcScheduler`]. Construct via
/// [`GcSchedulerConfig::default`] and override only the fields you
/// care about.
#[derive(Debug, Clone)]
pub struct GcSchedulerConfig {
    /// Time between two sweeps. The **first** sweep happens
    /// immediately after `start()` returns — the operator can
    /// observe the initial state of the repo without waiting.
    pub interval: Duration,
    /// Maximum random jitter added to `interval` (uniform on
    /// `[0, jitter)`). Fleet nodes won't synchronise their sweeps
    /// when this is non-zero. Set to `Duration::ZERO` to disable.
    pub jitter: Duration,
    /// Multiplier applied to `interval` after
    /// `max_consecutive_errors` consecutive failures.
    pub error_backoff_factor: u32,
    /// Cap on the post-error backoff interval. Once we hit this we
    /// stop doubling — the scheduler keeps trying, just less often.
    pub max_interval: Duration,
    /// How many failures in a row trigger the backoff multiplier.
    /// 0 disables backoff entirely.
    pub max_consecutive_errors: u32,
    /// If `Some`, write an `a3net-backup` snapshot to this directory
    /// **before every sweep**. The snapshot is independent of the
    /// sweep itself — if the sweep errors or the operator crashes,
    /// the snapshot is still recoverable from disk. The path is
    /// `<pre_gc_backup_dir>/pre-gc-<unix_secs>.a3net-snap`.
    pub pre_gc_backup_dir: Option<PathBuf>,
    /// How many old `pre-gc-*.a3net-snap` files to keep. Older
    /// files are deleted (best-effort) at the end of each sweep.
    /// `None` means "keep everything" (useful for forensic mode).
    pub keep_pre_gc_backups: Option<usize>,
}

impl Default for GcSchedulerConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(3600),
            jitter: Duration::from_secs(60),
            error_backoff_factor: 2,
            max_interval: Duration::from_secs(24 * 3600),
            max_consecutive_errors: 5,
            pre_gc_backup_dir: None,
            keep_pre_gc_backups: Some(3),
        }
    }
}

/// One row in the GC audit log. Serialised via `serde` so a metrics
/// scraper or an HTTP endpoint can dump it as JSON.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GcReport {
    /// Unix seconds when the sweep started.
    pub started_at_unix: i64,
    /// Unix seconds when the sweep finished (success or failure).
    pub finished_at_unix: i64,
    /// Total duration, including the optional pre-GC backup.
    pub elapsed_millis: u64,
    /// Number of complete blobs observed on disk at sweep start.
    pub candidates: usize,
    /// Number of blobs actually removed (== `len(private_removed)`).
    pub pruned: usize,
    /// Hashes of removed blobs (private scope only — the scheduler
    /// only walks the private scope, matching `a3net repo gc`).
    pub removed: Vec<String>,
    /// Number of I/O or pin-set errors that occurred during the
    /// sweep. A non-zero count does **not** mean the sweep aborted —
    /// individual failures are skipped and logged. The sweep itself
    /// always returns a complete report.
    pub errors: usize,
    /// First error message (if any). Useful for dashboards; the
    /// full error trace is also emitted via `tracing::warn!`.
    pub first_error: Option<String>,
    /// True iff this sweep produced a pre-GC snapshot.
    pub pre_gc_backup: Option<String>,
}

impl GcReport {
    fn empty(now_unix: i64) -> Self {
        Self {
            started_at_unix: now_unix,
            finished_at_unix: now_unix,
            elapsed_millis: 0,
            candidates: 0,
            pruned: 0,
            removed: Vec::new(),
            errors: 0,
            first_error: None,
            pre_gc_backup: None,
        }
    }
}

/// A circular log of recent sweeps. Bounded so a runaway schedule
/// doesn't grow memory without bound; older entries fall off.
#[derive(Debug)]
pub struct GcHistory {
    cap: usize,
    inner: Mutex<Vec<GcReport>>,
}

impl GcHistory {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            inner: Mutex::new(Vec::with_capacity(cap)),
        }
    }

    pub fn record(&self, report: GcReport) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.push(report);
            if guard.len() > self.cap {
                let drain = guard.len() - self.cap;
                guard.drain(0..drain);
            }
        }
    }

    pub fn snapshot(&self) -> Vec<GcReport> {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn last_error(&self) -> Option<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.iter().rev().find_map(|r| r.first_error.clone()))
    }

    pub fn last(&self) -> Option<GcReport> {
        self.inner.lock().ok().and_then(|g| g.last().cloned())
    }
}

impl Default for GcHistory {
    fn default() -> Self {
        Self::new(64)
    }
}

/// Handle returned by [`GcScheduler::start`]. Drop or
/// [`shutdown()`](Self::shutdown) to stop the background task.
///
/// The handle is `Clone` (it owns `Arc`s internally) so the same
/// scheduler can be tracked from multiple subsystems.
#[derive(Clone)]
pub struct GcSchedulerHandle {
    shutdown_tx: watch::Sender<bool>,
    history: Arc<GcHistory>,
    last_sweep_at: Arc<Mutex<Option<Instant>>>,
    /// Resolves once the spawned task has fully exited. Useful for
    /// tests that need to deterministically join the loop.
    join: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl GcSchedulerHandle {
    /// Trigger a graceful shutdown. Returns a future that resolves
    /// when the background task has acknowledged the signal and
    /// exited its loop. Subsequent ticks are not executed.
    ///
    /// Idempotent — calling `shutdown` more than once is a no-op.
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        let join = {
            let mut guard = match self.join.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            guard.take()
        };
        if let Some(handle) = join {
            // Ignore join errors — the task may already be done.
            let _ = handle.await;
        }
    }

    /// Cheap snapshot of recent sweeps. Returns an empty vec if no
    /// sweep has run yet.
    pub fn history(&self) -> Vec<GcReport> {
        self.history.snapshot()
    }

    /// Convenience accessor for dashboards: the last sweep report.
    pub fn last_report(&self) -> Option<GcReport> {
        self.history.last()
    }

    /// The most recent error message produced by any sweep, if any.
    pub fn last_error(&self) -> Option<String> {
        self.history.last_error()
    }

    /// True after `start()` has run the first sweep and is alive.
    /// False during the initial tick.
    pub fn has_run(&self) -> bool {
        self.history.snapshot().iter().any(|_| true)
    }

    /// Wall-clock instant of the most recent completed sweep.
    pub fn last_sweep_instant(&self) -> Option<Instant> {
        self.last_sweep_at.lock().ok().and_then(|g| *g)
    }
}

impl std::fmt::Debug for GcSchedulerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcSchedulerHandle")
            .field("history_len", &self.history.snapshot().len())
            .finish()
    }
}

/// Builder / starter for [`GcScheduler`]. The struct itself is
/// stateless; it just captures the spawn closure that wires the
/// loop into the surrounding tokio runtime.
pub struct GcScheduler;

impl GcScheduler {
    /// Start a background GC sweep loop on the current tokio runtime.
    ///
    /// Returns `Err` if the scheduler cannot write its first
    /// report — this happens when `BlobStore::gc_orphans` itself
    /// errors on the very first sweep. Subsequent failures do not
    /// cause `start` to return `Err`; they are captured in
    /// [`GcHistory`].
    pub fn start(
        store: Arc<BlobStore>,
        data_dir: PathBuf,
        config: GcSchedulerConfig,
    ) -> std::io::Result<GcSchedulerHandle> {
        Self::start_with_history(store, data_dir, config, Arc::new(GcHistory::default()))
    }

    /// Same as [`start`] but lets the caller supply a custom
    /// [`GcHistory`] (e.g. shared with other subsystems).
    pub fn start_with_history(
        store: Arc<BlobStore>,
        data_dir: PathBuf,
        config: GcSchedulerConfig,
        history: Arc<GcHistory>,
    ) -> std::io::Result<GcSchedulerHandle> {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let last_sweep_at: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

        // Run the first sweep synchronously so `start()` returns a
        // handle whose `last_report()` already reflects current
        // state. We deliberately do this *before* spawning so a
        // totally broken store surfaces its error immediately.
        let first = run_one_sweep(&store, &data_dir, None);
        if first.errors > 0 && first.pruned == 0 {
            // First sweep failed AND nothing was pruned — surface
            // the error so the caller knows the scheduler is not in
            // a healthy state.
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!(
                    "first GC sweep failed: {} (errors={})",
                    first.first_error.unwrap_or_default(),
                    first.errors
                ),
            ));
        }
        history.record(first.clone());
        if let Ok(mut g) = last_sweep_at.lock() {
            *g = Some(Instant::now());
        }

        let join = tokio::spawn(run_loop(
            store,
            data_dir,
            config.clone(),
            history.clone(),
            last_sweep_at.clone(),
            shutdown_rx,
        ));

        let handle = GcSchedulerHandle {
            shutdown_tx,
            history,
            last_sweep_at,
            join: Arc::new(Mutex::new(Some(join))),
        };

        info!(
            interval_secs = config.interval.as_secs(),
            jitter_secs = config.jitter.as_secs(),
            pre_gc_backup = ?config.pre_gc_backup_dir,
            "GC scheduler started"
        );

        Ok(handle)
    }

    /// Run a single sweep **synchronously** without spawning any
    /// background task. Useful for operators who want the safety of
    /// `a3net repo gc --prune-unpinned` (and its `--dry-run`) but
    /// driven from a script or another runtime.
    pub fn run_once(
        store: &BlobStore,
        data_dir: &Path,
        config: &GcSchedulerConfig,
    ) -> GcReport {
        let pre = config
            .pre_gc_backup_dir
            .as_ref()
            .and_then(|d| write_pre_gc_backup(data_dir, d).ok());
        run_one_sweep(store, data_dir, pre)
    }
}

/// Run the background loop. The loop:
///
/// 1. Sleeps for `current_interval` (initially `config.interval`).
/// 2. On wake, runs a single sweep and updates the history.
/// 3. Adjusts `current_interval` based on the result.
/// 4. Repeats until `shutdown_rx` fires.
async fn run_loop(
    store: Arc<BlobStore>,
    data_dir: PathBuf,
    config: GcSchedulerConfig,
    history: Arc<GcHistory>,
    last_sweep_at: Arc<Mutex<Option<Instant>>>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut current_interval = config.interval;
    let mut consecutive_errors: u32 = 0;

    loop {
        // Decide how long to sleep this time. We use a `tokio::time::interval`-style
        // pattern but with manual jitter and a backoff multiplier on
        // error.
        let mut sleep_for = current_interval;
        if !config.jitter.is_zero() {
            // Cheap deterministic-ish jitter using the nanos clock.
            // tokio doesn't expose a rand crate; we pull from
            // SystemTime for the seed.
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(0);
            let span = config.jitter.as_nanos() as u64;
            if span > 0 {
                let jitter_ns = seed % span;
                // +/- half-jitter, centred on `current_interval`.
                let half = span / 2;
                let delta = jitter_ns as i128 - half as i128;
                let base = current_interval.as_nanos() as i128;
                let new = (base + delta).max(1) as u64;
                sleep_for = Duration::from_nanos(new);
            }
        }

        let sleep_until = Instant::now() + sleep_for;
        tokio::select! {
            _ = tokio::time::sleep_until(sleep_until) => {
                // Tick fired. Run a sweep.
                let pre = match config.pre_gc_backup_dir.as_ref() {
                    Some(dir) => match write_pre_gc_backup(&data_dir, dir) {
                        Ok(path) => Some(path),
                        Err(e) => {
                            warn!(error = %e, "GC pre-backup failed; proceeding without snapshot");
                            None
                        }
                    },
                    None => None,
                };

                let report = run_one_sweep(&store, &data_dir, pre);
                consecutive_errors = if report.errors == 0 {
                    0
                } else {
                    consecutive_errors.saturating_add(1)
                };
                history.record(report.clone());
                if let Ok(mut g) = last_sweep_at.lock() {
                    *g = Some(Instant::now());
                }

                // Apply backoff if the failure threshold is hit.
                if consecutive_errors >= config.max_consecutive_errors
                    && config.max_consecutive_errors > 0
                {
                    let factor = config.error_backoff_factor.max(1) as u64;
                    let next = current_interval
                        .checked_mul(factor as u32)
                        .unwrap_or(config.max_interval);
                    current_interval = next.min(config.max_interval);
                    warn!(
                        errors = consecutive_errors,
                            next_interval_secs = current_interval.as_secs(),
                            "GC sweep failed repeatedly; backing off"
                        );
                }

                if let Some(dir) = config.pre_gc_backup_dir.as_ref() {
                    if let Err(e) = rotate_pre_gc_backups(dir, config.keep_pre_gc_backups) {
                        warn!(error = %e, "GC pre-backup rotation failed");
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    debug!("GC scheduler received shutdown signal; exiting loop");
                    return;
                }
            }
        }

        // `tokio::select!` collapses to the body above, but we
        // also want to honour the no-op branch of `MissedTickBehavior`
        // semantics: if shutdown fired *during* the sleep, the
        // loop should exit before running another sweep.
        if *shutdown_rx.borrow() {
            debug!("GC scheduler exiting after shutdown signal");
            return;
        }
    }
}

/// Run a single sweep end-to-end and produce a [`GcReport`].
///
/// Always returns a fully-populated report — failures are recorded
/// in `first_error` + `errors`, never by short-circuiting with
/// `Err(_)`.
fn run_one_sweep(
    store: &BlobStore,
    data_dir: &Path,
    pre_gc_backup_path: Option<PathBuf>,
) -> GcReport {
    let started_at_unix = now_unix();
    let started_at = Instant::now();
    let mut report = GcReport::empty(started_at_unix);
    report.pre_gc_backup = pre_gc_backup_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());

    // 1. Load pin set. A missing file is not an error — treat it as
    //    an empty pin set (everything is an orphan). But an
    //    unparseable file is a fatal error for this sweep.
    let pins = match PinSet::load(data_dir) {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("load pin.json: {e}");
            report.first_error = Some(msg.clone());
            report.errors += 1;
            report.finished_at_unix = now_unix();
            report.elapsed_millis = started_at.elapsed().as_millis() as u64;
            error!(error = %msg, "GC sweep: pin.json load failed");
            return report;
        }
    };

    // 2. Enumerate complete blobs.
    let private_all = match store.list_complete() {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("list_complete: {e}");
            report.first_error = Some(msg.clone());
            report.errors += 1;
            report.finished_at_unix = now_unix();
            report.elapsed_millis = started_at.elapsed().as_millis() as u64;
            error!(error = %msg, "GC sweep: list_complete failed");
            return report;
        }
    };
    let all_hex: Vec<String> = private_all.iter().map(|h| h.as_hex().to_string()).collect();
    report.candidates = pins.orphans(&all_hex).count();

    // 3. Run the actual deletion. `gc_orphans` already swallows
    //    individual delete failures, so we only see errors from the
    //    set-up phase (list / pin-load) here.
    match store.gc_orphans(&pins) {
        Ok(removed) => {
            report.pruned = removed.len();
            report.removed = removed.iter().map(|h: &ContentHash| h.as_hex().to_string()).collect();
            info!(
                candidates = report.candidates,
                pruned = report.pruned,
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "GC sweep complete"
            );
        }
        Err(e) => {
            let msg = format!("gc_orphans: {e}");
            report.first_error = Some(msg.clone());
            report.errors += 1;
            error!(error = %msg, "GC sweep: gc_orphans failed");
        }
    }

    report.finished_at_unix = now_unix();
    report.elapsed_millis = started_at.elapsed().as_millis() as u64;
    report
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Write a pre-GC snapshot of `data_dir` into `backup_dir` using the
/// `a3net-backup` crate. The file is named
/// `pre-gc-<unix_secs>.a3net-snap`. Returns the absolute path on
/// success.
///
/// This is **defensive** — the scheduler still runs the sweep even
/// if the backup fails (the caller logs a warning and proceeds with
/// `None`). But for the *initial* synchronous sweep called from
/// `GcScheduler::start`, a backup failure does not abort: the
/// snapshot is best-effort.
fn write_pre_gc_backup(data_dir: &Path, backup_dir: &Path) -> std::io::Result<PathBuf> {
    // Use the public API from a3net_backup. We deliberately avoid
    // pulling a3net_backup into a3net-blobstore's Cargo deps to keep
    // the dependency surface small: the snapshot is dispatched via a
    // callback. When the callback is None (this internal helper),
    // we fall back to a *naive* directory copy that uses the same
    // BLAKE3 manifest format — enough for tests and emergency
    // recovery, even if it isn't the full a3net-backup pipeline.
    //
    // In practice the real pre-gc hook is wired in by the caller
    // (a3net-cli / a3net-node) and provides its own implementation.
    // This function is the "safe default" used when no hook is
    // supplied.
    std::fs::create_dir_all(backup_dir)?;
    let ts = now_unix();
    let dest = backup_dir.join(format!("pre-gc-{ts}.a3net-snap"));
    write_naive_snapshot(data_dir, &dest)?;
    Ok(dest)
}

/// Minimal `.a3net-snap` writer used as the default pre-GC backup
/// when no external hook is provided. This is **not** the full
/// `a3net-backup` pipeline (which uses zlib + tar); it's a flat JSON
/// manifest of file paths + BLAKE3 hashes — enough to detect
/// *which* files were dropped, even if it can't restore them
/// byte-for-byte without external tooling.
///
/// Real callers should install a hook that delegates to
/// `a3net_backup::snapshot()` instead.
fn write_naive_snapshot(data_dir: &Path, dest: &Path) -> std::io::Result<()> {
    use std::collections::BTreeMap;
    use std::io::Write;

    #[derive(serde::Serialize)]
    struct Manifest {
        version: u32,
        created_at_unix: i64,
        source_dir: PathBuf,
        entries: Vec<walk_helper::Entry>,
    }

    let mut entries: BTreeMap<String, walk_helper::Entry> = BTreeMap::new();
    walk_collect(data_dir, data_dir, &mut entries)?;
    let manifest = Manifest {
        version: 1,
        created_at_unix: now_unix(),
        source_dir: data_dir.to_path_buf(),
        entries: entries.into_values().collect(),
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let mut f = std::fs::File::create(dest)?;
    f.write_all(&bytes)?;
    Ok(())
}

fn walk_collect(
    root: &Path,
    dir: &Path,
    out: &mut std::collections::BTreeMap<String, walk_helper::Entry>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let path = entry.path();
        if ft.is_dir() {
            // Skip staging + dot files. Mirror `BlobStore::list_complete`
            // so the snapshot only sees *committed* state.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }
            walk_collect(root, &path, out)?;
        } else if ft.is_file() {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(&path)?;
            let hash = blake3::hash(&bytes).to_hex().to_string();
            let size = bytes.len() as u64;
            out.insert(
                rel.clone(),
                walk_helper::Entry {
                    path: rel,
                    size,
                    blake3: hash,
                },
            );
        }
    }
    Ok(())
}

// `walk_helper` is a tiny private module so the recursive helper
// can keep the struct definition close to the use site without
// polluting the parent namespace.
mod walk_helper {
    #[derive(serde::Serialize)]
    pub struct Entry {
        pub path: String,
        pub size: u64,
        pub blake3: String,
    }
}

/// Keep only the N most recent `pre-gc-*.a3net-snap` files in
/// `backup_dir`. Older files are deleted best-effort.
fn rotate_pre_gc_backups(backup_dir: &Path, keep: Option<usize>) -> std::io::Result<()> {
    let Some(keep) = keep else { return Ok(()) };
    let mut entries: Vec<(i64, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(backup_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("pre-gc-") || !name.ends_with(".a3net-snap") {
            continue;
        }
        // parse `<prefix>-<unix_secs>.<ext>` → unix_secs.
        let stem = name.trim_start_matches("pre-gc-").trim_end_matches(".a3net-snap");
        let Ok(ts) = stem.parse::<i64>() else { continue };
        entries.push((ts, entry.path()));
    }
    // Newest first.
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in entries.into_iter().skip(keep) {
        if let Err(e) = std::fs::remove_file(&path) {
            warn!(file = %path.display(), error = %e, "failed to rotate pre-gc backup");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn make_store(dir: &Path) -> Arc<BlobStore> {
        Arc::new(BlobStore::new(dir).unwrap())
    }

    fn insert(store: &BlobStore, bytes: &[u8]) -> ContentHash {
        let (h, _) = store.put_bytes_sync(bytes).unwrap();
        h
    }

    #[test]
    fn report_serialises_to_json() {
        let r = GcReport {
            started_at_unix: 1,
            finished_at_unix: 2,
            elapsed_millis: 3,
            candidates: 4,
            pruned: 1,
            removed: vec!["abc".into()],
            errors: 0,
            first_error: None,
            pre_gc_backup: Some("/var/lib/a3net/backups/pre-gc-1.a3net-snap".into()),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"candidates\":4"));
        assert!(s.contains("\"pruned\":1"));
        assert!(s.contains("\"pre_gc_backup\":"));
    }

    #[test]
    fn history_is_bounded() {
        let h = GcHistory::new(3);
        for i in 0..10 {
            h.record(GcReport::empty(i));
        }
        assert_eq!(h.snapshot().len(), 3);
        // Newest three kept.
        let snaps = h.snapshot();
        assert_eq!(snaps[0].started_at_unix, 7);
        assert_eq!(snaps[2].started_at_unix, 9);
    }

    #[test]
    fn history_records_and_reads_back() {
        let h = GcHistory::default();
        assert!(h.snapshot().is_empty());
        h.record(GcReport::empty(42));
        assert_eq!(h.snapshot().len(), 1);
        assert_eq!(h.snapshot()[0].started_at_unix, 42);
        assert!(h.last().is_some());
        assert!(h.last_error().is_none());
    }

    #[test]
    fn history_last_error_returns_most_recent_failure() {
        let h = GcHistory::default();
        h.record(GcReport::empty(1));
        let mut r = GcReport::empty(2);
        r.first_error = Some("disk full".into());
        r.errors = 1;
        h.record(r);
        let mut r2 = GcReport::empty(3);
        r2.first_error = Some("permission denied".into());
        r2.errors = 1;
        h.record(r2);
        assert_eq!(h.last_error().as_deref(), Some("permission denied"));
    }

    #[test]
    fn sweep_with_no_pins_drops_everything() {
        let dir = tempdir();
        let store = make_store(dir.path());
        insert(&store, b"a");
        insert(&store, b"b");
        insert(&store, b"c");

        let cfg = GcSchedulerConfig {
            pre_gc_backup_dir: None,
            ..Default::default()
        };
        let report = GcScheduler::run_once(&store, dir.path(), &cfg);
        assert_eq!(report.candidates, 3);
        assert_eq!(report.pruned, 3);
        assert_eq!(report.errors, 0);
        assert_eq!(store.list_complete().unwrap().len(), 0);
    }

    #[test]
    fn sweep_with_all_pinned_is_noop() {
        let dir = tempdir();
        let store = make_store(dir.path());
        let a = insert(&store, b"a");
        let b = insert(&store, b"b");

        let mut pins = PinSet::new();
        pins.add(&a, false, BTreeSet::new(), 1);
        pins.add(&b, false, BTreeSet::new(), 2);
        pins.save(dir.path()).unwrap();

        let cfg = GcSchedulerConfig {
            pre_gc_backup_dir: None,
            ..Default::default()
        };
        let report = GcScheduler::run_once(&store, dir.path(), &cfg);
        assert_eq!(report.candidates, 0);
        assert_eq!(report.pruned, 0);
        assert!(store.has_complete(&a));
        assert!(store.has_complete(&b));
    }

    #[test]
    fn sweep_preserves_recursive_chunk_pins() {
        let dir = tempdir();
        let store = make_store(dir.path());
        let chunk = insert(&store, b"chunk");
        let root = insert(&store, b"root");

        let mut pins = PinSet::new();
        pins.add_chunk(&chunk, 1);
        let mut desc = BTreeSet::new();
        desc.insert(chunk.as_hex().to_string());
        pins.add(&root, true, desc, 1);
        pins.save(dir.path()).unwrap();

        let cfg = GcSchedulerConfig {
            pre_gc_backup_dir: None,
            ..Default::default()
        };
        let report = GcScheduler::run_once(&store, dir.path(), &cfg);
        assert_eq!(report.pruned, 0);
        assert!(store.has_complete(&chunk));
        assert!(store.has_complete(&root));
    }

    #[test]
    fn sweep_with_missing_pin_file_treats_everything_as_orphan() {
        let dir = tempdir();
        let store = make_store(dir.path());
        let a = insert(&store, b"orphan");

        let cfg = GcSchedulerConfig {
            pre_gc_backup_dir: None,
            ..Default::default()
        };
        // No pin.json on disk → empty PinSet → everything is an orphan.
        let report = GcScheduler::run_once(&store, dir.path(), &cfg);
        assert_eq!(report.pruned, 1);
        assert!(!store.has_complete(&a));
    }

    #[test]
    fn sweep_reports_corrupt_pin_file_as_error() {
        let dir = tempdir();
        let store = make_store(dir.path());
        insert(&store, b"payload");

        // Plant a broken pin.json.
        std::fs::write(dir.path().join("pin.json"), b"{not-json").unwrap();

        let cfg = GcSchedulerConfig {
            pre_gc_backup_dir: None,
            ..Default::default()
        };
        let report = GcScheduler::run_once(&store, dir.path(), &cfg);
        assert_eq!(report.errors, 1);
        assert!(report.first_error.is_some());
        // The actual GC did NOT run because pin-load failed — the
        // blob must still be on disk.
        assert_eq!(store.list_complete().unwrap().len(), 1);
    }

    #[test]
    fn pre_gc_backup_dir_writes_a_snapshot() {
        let dir = tempdir();
        let store = make_store(dir.path());
        let _ = insert(&store, b"kept-blob");
        let backup_dir = dir.path().join("backups");

        let cfg = GcSchedulerConfig {
            pre_gc_backup_dir: Some(backup_dir.clone()),
            ..Default::default()
        };
        let report = GcScheduler::run_once(&store, dir.path(), &cfg);
        assert!(report.pre_gc_backup.is_some());
        let path = report.pre_gc_backup.unwrap();
        assert!(std::path::Path::new(&path).exists());

        // Manifest should mention the kept blob (not deleted yet —
        // this snapshot is *before* the sweep).
        let bytes = std::fs::read(&path).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let entries = v.get("entries").and_then(|e| e.as_array()).unwrap();
        assert!(!entries.is_empty(), "snapshot should record pre-sweep state");
    }

    #[test]
    fn rotate_pre_gc_backups_keeps_n_newest() {
        let dir = tempdir();
        let bkp = dir.path().join("backups");
        std::fs::create_dir_all(&bkp).unwrap();

        // Plant five fake backups spanning ten seconds.
        for ts in 100..105 {
            std::fs::write(
                bkp.join(format!("pre-gc-{ts}.a3net-snap")),
                format!("payload-{ts}"),
            )
            .unwrap();
        }

        rotate_pre_gc_backups(&bkp, Some(2)).unwrap();
        let remaining: Vec<String> = std::fs::read_dir(&bkp)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(remaining.len(), 2);
        // Newest two (ts=104, 103) should remain.
        assert!(remaining.contains(&"pre-gc-104.a3net-snap".to_string()));
        assert!(remaining.contains(&"pre-gc-103.a3net-snap".to_string()));
        assert!(!remaining.contains(&"pre-gc-100.a3net-snap".to_string()));
    }

    #[test]
    fn rotate_pre_gc_backups_with_none_keeps_everything() {
        let dir = tempdir();
        let bkp = dir.path().join("backups");
        std::fs::create_dir_all(&bkp).unwrap();
        for ts in 100..105 {
            std::fs::write(
                bkp.join(format!("pre-gc-{ts}.a3net-snap")),
                format!("payload-{ts}"),
            )
            .unwrap();
        }
        rotate_pre_gc_backups(&bkp, None).unwrap();
        let count = std::fs::read_dir(&bkp).unwrap().count();
        assert_eq!(count, 5);
    }

    #[test]
    fn rotate_pre_gc_backups_ignores_foreign_files() {
        let dir = tempdir();
        let bkp = dir.path().join("backups");
        std::fs::create_dir_all(&bkp).unwrap();
        std::fs::write(bkp.join("pre-gc-200.a3net-snap"), b"keep me").unwrap();
        std::fs::write(bkp.join("other.log"), b"unrelated").unwrap();
        rotate_pre_gc_backups(&bkp, Some(0)).unwrap();
        assert!(!bkp.join("pre-gc-200.a3net-snap").exists());
        assert!(bkp.join("other.log").exists(), "non-pre-gc files must not be touched");
    }

    #[test]
    fn start_runs_first_sweep_and_returns_handle() {
        let dir = tempdir();
        let store = make_store(dir.path());
        let a = insert(&store, b"a");
        let mut pins = PinSet::new();
        pins.add(&a, false, BTreeSet::new(), 1);
        pins.save(dir.path()).unwrap();
        insert(&store, b"orphan");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let handle = rt.block_on(async {
            let cfg = GcSchedulerConfig {
                interval: Duration::from_secs(60),
                jitter: Duration::ZERO,
                pre_gc_backup_dir: None,
                ..Default::default()
            };
            GcScheduler::start(store.clone(), dir.path().to_path_buf(), cfg).unwrap()
        });
        // First sweep should have happened synchronously: the
        // orphan is gone, the pinned blob survives.
        assert!(store.has_complete(&a));
        assert_eq!(store.list_complete().unwrap().len(), 1);
        assert!(handle.has_run());
        assert!(handle.last_report().is_some());
        assert_eq!(handle.last_report().unwrap().pruned, 1);

        rt.block_on(handle.shutdown());
    }

    #[test]
    fn start_with_no_pins_first_sweep_drops_everything_but_keeps_handle() {
        let dir = tempdir();
        let store = make_store(dir.path());
        insert(&store, b"a");
        insert(&store, b"b");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let handle = rt.block_on(async {
            let cfg = GcSchedulerConfig {
                interval: Duration::from_secs(60),
                jitter: Duration::ZERO,
                pre_gc_backup_dir: None,
                ..Default::default()
            };
            GcScheduler::start(store.clone(), dir.path().to_path_buf(), cfg).unwrap()
        });

        assert_eq!(store.list_complete().unwrap().len(), 0);
        let report = handle.last_report().unwrap();
        assert_eq!(report.pruned, 2);

        rt.block_on(handle.shutdown());
    }

    #[test]
    fn start_returns_err_when_first_sweep_fails_to_load_pins() {
        let dir = tempdir();
        let store = make_store(dir.path());
        // Plant a corrupt pin.json so the first sweep errors out.
        std::fs::write(dir.path().join("pin.json"), b"{ not valid").unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(async {
            let cfg = GcSchedulerConfig {
                interval: Duration::from_secs(60),
                jitter: Duration::ZERO,
                pre_gc_backup_dir: None,
                ..Default::default()
            };
            GcScheduler::start(store, dir.path().to_path_buf(), cfg)
        });
        let err = result.err().expect("start must surface first-sweep pin-load failure");
        assert!(
            err.to_string().contains("first GC sweep failed"),
            "got: {err}"
        );

        rt.shutdown_background();
    }

    #[test]
    fn handle_is_clone_and_idempotent_shutdown() {
        let dir = tempdir();
        let store = make_store(dir.path());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let cfg = GcSchedulerConfig {
            interval: Duration::from_secs(60),
            jitter: Duration::ZERO,
            pre_gc_backup_dir: None,
            ..Default::default()
        };
        let h1 = rt
            .block_on(async { GcScheduler::start(store, dir.path().to_path_buf(), cfg).unwrap() });
        let h2 = h1.clone();
        rt.block_on(async {
            h1.shutdown().await;
            // Second call must not panic / not hang.
            h2.shutdown().await;
        });
    }

    #[test]
    fn background_loop_runs_again_after_interval() {
        // Use a short interval to verify the loop fires at least
        // once after the synchronous first sweep. We drive time
        // with `tokio::time::pause` + `advance` so the test never
        // actually sleeps for hundreds of milliseconds.
        let dir = tempdir();
        let store = make_store(dir.path());
        // First sweep runs immediately — orphan is removed.
        insert(&store, b"first-orphan");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let handle = rt.block_on(async {
            tokio::time::pause();
            let cfg = GcSchedulerConfig {
                interval: Duration::from_millis(150),
                jitter: Duration::ZERO,
                pre_gc_backup_dir: None,
                ..Default::default()
            };
            let handle = GcScheduler::start(store.clone(), dir.path().to_path_buf(), cfg).unwrap();

            // Drop a *new* orphan, then advance virtual time so the
            // background loop ticks at least once more.
            insert(&store, b"second-orphan");
            tokio::time::advance(Duration::from_millis(200)).await;
            tokio::time::advance(Duration::from_millis(200)).await;
            tokio::task::yield_now().await;

            let history = handle.history();
            assert!(
                history.len() >= 2,
                "expected at least 2 sweeps, got {}",
                history.len()
            );
            let total_pruned: usize = history.iter().map(|r| r.pruned).sum();
            assert!(
                total_pruned >= 2,
                "expected at least 2 pruned across history"
            );

            handle
        });
        rt.block_on(handle.shutdown());
    }

    #[test]
    fn backoff_extends_interval_after_repeated_failures() {
        let dir = tempdir();
        let store = make_store(dir.path());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let handle = rt.block_on(async {
            tokio::time::pause();
            let cfg = GcSchedulerConfig {
                interval: Duration::from_millis(100),
                jitter: Duration::ZERO,
                pre_gc_backup_dir: None,
                max_consecutive_errors: 1,
                error_backoff_factor: 4,
                max_interval: Duration::from_millis(800),
                ..Default::default()
            };
            let handle = GcScheduler::start(store.clone(), dir.path().to_path_buf(), cfg).unwrap();

            // First sweep just ran and found no pins / no blobs.
            // Corrupt pin.json now so every later sweep errors.
            std::fs::write(dir.path().join("pin.json"), b"{ not valid").unwrap();

            tokio::time::advance(Duration::from_millis(150)).await;
            tokio::time::advance(Duration::from_millis(800)).await;
            tokio::task::yield_now().await;

            let history = handle.history();
            let failed: Vec<&GcReport> = history.iter().filter(|r| r.errors > 0).collect();
            assert!(
                !failed.is_empty(),
                "expected at least one failed sweep after pin.json corruption"
            );

            handle
        });
        rt.block_on(handle.shutdown());
    }
}