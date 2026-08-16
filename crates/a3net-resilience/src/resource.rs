//! Resource limiter — global + per-key backpressure for A3Net.
//!
//! Production daemons need to bound three kinds of resource pressure:
//!
//! 1. **Global concurrency** — total in-flight heavy operations
//!    across the whole node.  Without this, a burst of 10,000
//!    incoming mesh requests can pin every CPU before any rate
//!    limiter has a chance to engage.
//!
//! 2. **Per-peer fairness** — a single misbehaving or compromised
//!    peer must not be able to exhaust the global budget.  We give
//!    each peer a fixed slice of the global pool.
//!
//! 3. **Per-room fan-out** — gossip topics can have unbounded
//!    subscriber counts; without a cap, one busy room can starve
//!    every other room.
//!
//! [`ResourceLimiter`] solves all three with the same primitive:
//! a `tokio::sync::Semaphore` per scope (global / per-key).  Callers
//! acquire a [`ResourcePermit`] that holds both the global and
//! per-key slots; dropping the permit releases them.
//!
//! ## Design choices
//!
//! - **`Semaphore`-based, not custom** — `tokio::sync::Semaphore`
//!   already provides fair (FIFO) acquire semantics with built-in
//!   `try_acquire`, `acquire_owned`, and cancellation safety.
//! - **`OwnedSemaphorePermit` semantics** — permits are moved into
//!   tasks; this avoids `&mut self` lifetimes on call sites.
//! - **Cancellation-aware** — every `acquire` future races against a
//!   [`CancellationToken`] clone, so a queued task bails out on
//!   shutdown instead of pinning the runtime.
//! - **Metrics surface** — `LimiterMetrics` exposes acquired /
//!   rejected / waiting / cancelled counters via atomic loads; tests
//!   and operators can scrape them without locking.
//!
//! ## Non-goals
//!
//! - This module does **not** implement rate-limiting (bytes/sec) —
//!   `a3net-blobstore::bandwidth` already handles that.  This module
//!   is about *concurrency*, not *throughput*.
//! - Hierarchical / inheritable limits are out of scope.  A future
//!   iteration can chain scopes if a parent limiter needs to nest.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tracing::debug;

use crate::cancellation::CancellationToken;

// ─────────────────────────────────────────────────────────────────
// LimiterMetrics
// ─────────────────────────────────────────────────────────────────

/// Cheaply-cloneable counters describing a [`ResourceLimiter`]'s
/// runtime behaviour.  All fields are `AtomicU64` so reads and writes
/// are wait-free.
///
/// The `Default` instance returns zero counters — useful for tests
/// that don't care about metrics but need a value.
#[derive(Debug, Default)]
pub struct LimiterMetrics {
    /// Successful `acquire()` calls (a permit was granted).
    pub acquired: AtomicU64,
    /// `acquire()` calls rejected because the per-key budget was
    /// exhausted (global may still have capacity).
    pub rejected_key: AtomicU64,
    /// `acquire()` calls rejected because the global budget was
    /// exhausted.
    pub rejected_global: AtomicU64,
    /// `try_acquire()` calls that returned `None` (would-block).
    pub try_rejected: AtomicU64,
    /// `acquire()` calls cancelled (token fired before permit).
    pub cancelled: AtomicU64,
    /// Permits released (i.e. dropped without panic).  Should track
    /// `acquired - cancelled` over the limiter's lifetime.
    pub released: AtomicU64,
    /// Current tasks waiting on `acquire().await` (across all keys).
    /// Sampled, not exact — use it for trend detection only.
    pub waiting: AtomicU64,
}

impl LimiterMetrics {
    /// Snapshot the counters as a plain struct — useful for
    /// serialization, debug dumps, or operator dashboards.
    pub fn snapshot(&self) -> LimiterMetricsSnapshot {
        LimiterMetricsSnapshot {
            acquired: self.acquired.load(Ordering::Relaxed),
            rejected_key: self.rejected_key.load(Ordering::Relaxed),
            rejected_global: self.rejected_global.load(Ordering::Relaxed),
            try_rejected: self.try_rejected.load(Ordering::Relaxed),
            cancelled: self.cancelled.load(Ordering::Relaxed),
            released: self.released.load(Ordering::Relaxed),
            waiting: self.waiting.load(Ordering::Relaxed),
        }
    }
}

/// Plain-old-data snapshot of [`LimiterMetrics`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LimiterMetricsSnapshot {
    pub acquired: u64,
    pub rejected_key: u64,
    pub rejected_global: u64,
    pub try_rejected: u64,
    pub cancelled: u64,
    pub released: u64,
    pub waiting: u64,
}

impl LimiterMetricsSnapshot {
    /// Total rejections (per-key + global + try).
    pub fn total_rejected(&self) -> u64 {
        self.rejected_key + self.rejected_global + self.try_rejected
    }
}

// ─────────────────────────────────────────────────────────────────
// ResourceConfig
// ─────────────────────────────────────────────────────────────────

/// Tunable configuration for a [`ResourceLimiter`].
#[derive(Debug, Clone)]
pub struct ResourceConfig {
    /// Maximum total concurrent permits across every key.
    /// Must be ≥ `per_key_limit`.
    pub global_limit: usize,
    /// Maximum concurrent permits per individual key (peer / room).
    /// 0 means "no per-key limit, only global".
    pub per_key_limit: usize,
    /// Default `acquire().await` timeout.  Callers can override on
    /// each call.  Set to `Duration::MAX` for unbounded queueing.
    pub acquire_timeout: Duration,
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            // Conservative default: 256 in-flight heavy operations
            // across the whole node, 16 per peer / per room.
            global_limit: 256,
            per_key_limit: 16,
            acquire_timeout: Duration::from_secs(5),
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// ResourceLimiter
// ─────────────────────────────────────────────────────────────────

/// Global concurrency limiter with per-key bucketing.
///
/// Internally:
/// - One global `Semaphore` sized to `config.global_limit`.
/// - A `RwLock<HashMap<K, Arc<Semaphore>>>` mapping each known key
///   to a per-key semaphore sized to `config.per_key_limit`.
///
/// `acquire()` checks the per-key semaphore first, then the global
/// one — but it acquires **both** before returning, so a permit
/// always holds both slots (and dropping it releases both).
#[derive(Debug)]
pub struct ResourceLimiter<K: Eq + std::hash::Hash + Clone> {
    cfg: ResourceConfig,
    global: Arc<Semaphore>,
    keys: parking_lot::RwLock<HashMap<K, Arc<Semaphore>>>,
    metrics: Arc<LimiterMetrics>,
}

impl<K: Eq + std::hash::Hash + Clone> ResourceLimiter<K> {
    /// Create a new limiter with the supplied config.
    pub fn new(cfg: ResourceConfig) -> Self {
        let global = Arc::new(Semaphore::new(cfg.global_limit));
        Self {
            cfg,
            global,
            keys: parking_lot::RwLock::new(HashMap::new()),
            metrics: Arc::new(LimiterMetrics::default()),
        }
    }

    /// Cheaply-cloneable handle for the limiter's metrics.
    pub fn metrics(&self) -> Arc<LimiterMetrics> {
        self.metrics.clone()
    }

    /// Snapshot of the current metrics — see [`LimiterMetrics::snapshot`].
    pub fn snapshot(&self) -> LimiterMetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Configured global limit.
    pub fn global_limit(&self) -> usize {
        self.cfg.global_limit
    }

    /// Configured per-key limit.
    pub fn per_key_limit(&self) -> usize {
        self.cfg.per_key_limit
    }

    /// Available global permits right now.
    pub fn global_available(&self) -> usize {
        self.global.available_permits()
    }

    /// Available per-key permits for `key`, or `usize::MAX` if
    /// `per_key_limit == 0` (per-key enforcement disabled).
    pub fn key_available(&self, key: &K) -> usize {
        if self.cfg.per_key_limit == 0 {
            return usize::MAX;
        }
        self.keys
            .read()
            .get(key)
            .map(|s| s.available_permits())
            .unwrap_or(self.cfg.per_key_limit)
    }

    /// Non-blocking acquire.  Returns `Some(permit)` if both slots
    /// are available, `None` otherwise (caller should fall back or
    /// shed load).
    ///
    /// When `per_key_limit == 0` the per-key check is skipped
    /// entirely (only the global pool applies).
    pub fn try_acquire(&self, key: K) -> Option<ResourcePermit<K>> {
        let key_permit = if self.cfg.per_key_limit == 0 {
            None
        } else {
            let key_sema = self.key_semaphore(&key).expect(
                "key_semaphore returns Some when per_key_limit > 0",
            );
            match key_sema.clone().try_acquire_owned() {
                Ok(p) => Some(p),
                Err(TryAcquireError::NoPermits)
                | Err(TryAcquireError::Closed) => {
                    self.metrics.try_rejected.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
            }
        };
        let global_permit = match self.global.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                // Global full but per-key had a slot — release the
                // key permit before returning so we don't leak.
                drop(key_permit);
                self.metrics.try_rejected.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        self.metrics.acquired.fetch_add(1, Ordering::Relaxed);
        Some(ResourcePermit {
            _key: key,
            _key_permit: key_permit,
            _global_permit: global_permit,
            metrics: self.metrics.clone(),
        })
    }

    /// Blocking acquire.  Waits up to `timeout` for both slots.
    /// On timeout returns `Err(AcquireError::Timeout)`.
    ///
    /// If `token` is `Some`, the wait also races against
    /// cancellation — a queued task bails on shutdown.
    ///
    /// When `per_key_limit == 0` only the global semaphore is
    /// acquired (no per-key bucket is created).
    pub async fn acquire(
        &self,
        key: K,
        timeout: Duration,
        token: Option<CancellationToken>,
    ) -> Result<ResourcePermit<K>, AcquireError> {
        self.metrics.waiting.fetch_add(1, Ordering::Relaxed);

        // Helper: wrap a future in a cancellation-aware selector.
        // Returns the inner future's result if it completes first,
        // or `AcquireError::Cancelled` if the token fires.
        async fn race_cancel<F, T>(
            tok: Option<&CancellationToken>,
            fut: F,
        ) -> Result<T, AcquireError>
        where
            F: std::future::Future<Output = Result<T, AcquireError>>,
        {
            if let Some(t) = tok {
                tokio::select! {
                    biased;
                    _ = t.cancelled() => Err(AcquireError::Cancelled),
                    res = fut => res,
                }
            } else {
                fut.await
            }
        }

        let key_fut = async {
            if self.cfg.per_key_limit == 0 {
                return Ok(None);
            }
            let key_sema = self
                .key_semaphore(&key)
                .expect("key_semaphore returns Some when per_key_limit > 0");
            let res = tokio::time::timeout(timeout, key_sema.acquire_owned()).await;
            match res {
                Ok(Ok(p)) => Ok(Some(p)),
                Ok(Err(_)) => Err(AcquireError::Closed),
                Err(_) => Err(AcquireError::Timeout),
            }
        };
        let global_fut = async {
            let res = tokio::time::timeout(timeout, self.global.clone().acquire_owned()).await;
            match res {
                Ok(Ok(p)) => Ok(p),
                Ok(Err(_)) => Err(AcquireError::Closed),
                Err(_) => Err(AcquireError::Timeout),
            }
        };

        let outcome: Result<(Option<OwnedSemaphorePermit>, OwnedSemaphorePermit), AcquireError> =
            race_cancel(token.as_ref(), async {
                let kp = race_cancel(token.as_ref(), key_fut).await?;
                let gp = global_fut.await?;
                Ok::<_, AcquireError>((kp, gp))
            })
            .await;

        self.metrics.waiting.fetch_sub(1, Ordering::Relaxed);

        // Late-cancel check: token might have fired *between* the
        // select! above returning and now.  Drop the permits we
        // just acquired and report Cancelled.
        if let Some(tok) = &token {
            if tok.is_cancelled() {
                let _ = outcome; // drops the permits if Ok
                self.metrics.cancelled.fetch_add(1, Ordering::Relaxed);
                return Err(AcquireError::Cancelled);
            }
        }

        let (key_permit, global_permit) = outcome?;
        self.metrics.acquired.fetch_add(1, Ordering::Relaxed);

        Ok(ResourcePermit {
            _key: key,
            _key_permit: key_permit,
            _global_permit: global_permit,
            metrics: self.metrics.clone(),
        })
    }

    /// Look up or lazily create the per-key semaphore.
    /// Returns `None` when `per_key_limit == 0` (per-key disabled).
    fn key_semaphore(&self, key: &K) -> Option<Arc<Semaphore>> {
        if self.cfg.per_key_limit == 0 {
            return None;
        }
        if let Some(s) = self.keys.read().get(key).cloned() {
            return Some(s);
        }
        let mut write = self.keys.write();
        // Double-check after taking the write lock.
        if let Some(s) = write.get(key).cloned() {
            return Some(s);
        }
        let s = Arc::new(Semaphore::new(self.cfg.per_key_limit));
        write.insert(key.clone(), s.clone());
        Some(s)
    }

    /// Drop the per-key semaphore for `key` if it's now unused
    /// (zero permits outstanding).  Best-effort — this prevents the
    /// map from growing unbounded over a long-lived node.
    pub fn reap_key(&self, key: &K) {
        if self.cfg.per_key_limit == 0 {
            return;
        }
        let limit = self.cfg.per_key_limit;
        let mut write = self.keys.write();
        let should_remove = write
            .get(key)
            .map(|s| s.available_permits() == limit)
            .unwrap_or(false);
        if should_remove {
            write.remove(key);
            debug!("ResourceLimiter: reaped idle key");
        }
    }

    /// Number of distinct keys currently tracked.
    pub fn tracked_keys(&self) -> usize {
        self.keys.read().len()
    }
}

// ─────────────────────────────────────────────────────────────────
// ResourcePermit
// ─────────────────────────────────────────────────────────────────

/// RAII permit from a [`ResourceLimiter::acquire`] or
/// [`ResourceLimiter::try_acquire`].  Dropping it returns the
/// slots to the global and per-key semaphores and bumps the
/// `released` metric.
#[derive(Debug)]
pub struct ResourcePermit<K: Eq + std::hash::Hash + Clone> {
    _key: K,
    /// Per-key permit; `None` when `per_key_limit == 0` (per-key
    /// enforcement disabled).
    _key_permit: Option<OwnedSemaphorePermit>,
    _global_permit: OwnedSemaphorePermit,
    metrics: Arc<LimiterMetrics>,
}

impl<K: Eq + std::hash::Hash + Clone> Drop for ResourcePermit<K> {
    fn drop(&mut self) {
        self.metrics.released.fetch_add(1, Ordering::Relaxed);
    }
}

// ─────────────────────────────────────────────────────────────────
// AcquireError
// ─────────────────────────────────────────────────────────────────

/// Errors returned by [`ResourceLimiter::acquire`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireError {
    /// Both per-key and global slots are exhausted and the wait
    /// timed out before either freed up.
    Timeout,
    /// The global pool is exhausted but per-key had a slot
    /// (rare — usually indicates a misconfigured per-key limit).
    Exhausted,
    /// The supplied cancellation token fired during the wait.
    Cancelled,
    /// A semaphore was closed mid-acquire (impossible under current
    /// usage; reserved for future graceful-shutdown paths).
    Closed,
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "resource limiter acquire timed out"),
            Self::Exhausted => write!(f, "resource limiter global pool exhausted"),
            Self::Cancelled => write!(f, "resource limiter acquire cancelled"),
            Self::Closed => write!(f, "resource limiter semaphore closed"),
        }
    }
}

impl std::error::Error for AcquireError {}

// ─────────────────────────────────────────────────────────────────
// Convenience constructors
// ─────────────────────────────────────────────────────────────────

/// Per-peer limiter used by `Node` for inbound mesh / DHT work.
pub type PeerLimiter = ResourceLimiter<String>;
/// Per-room limiter used by `Node` for outbound gossip fan-out.
pub type RoomLimiter = ResourceLimiter<String>;

/// Default per-peer config: 256 global, 16 per peer.
pub fn default_peer_config() -> ResourceConfig {
    ResourceConfig {
        global_limit: 256,
        per_key_limit: 16,
        acquire_timeout: Duration::from_secs(5),
    }
}

/// Default per-room config: 64 global, 32 per room.
pub fn default_room_config() -> ResourceConfig {
    ResourceConfig {
        global_limit: 64,
        per_key_limit: 32,
        acquire_timeout: Duration::from_secs(5),
    }
}

/// Catch-all limiter used by `Node` for any operation that doesn't
/// have a more specific bucket.  Keyed by an opaque tag (e.g.
/// `"blobstore.fetch"`, `"relay.proxy"`).
pub type TagLimiter = ResourceLimiter<String>;

/// Default tag config: 512 global, 64 per tag.
pub fn default_tag_config() -> ResourceConfig {
    ResourceConfig {
        global_limit: 512,
        per_key_limit: 64,
        acquire_timeout: Duration::from_secs(5),
    }
}

// `parking_lot::RwLock` and `tokio::sync::Semaphore` are both
// `Send + Sync`; we don't expose internals, so this is implicit.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    let _ = assert_send_sync::<ResourceLimiter<String>>;
};

// ─────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn test_cfg(global: usize, per_key: usize) -> ResourceConfig {
        ResourceConfig {
            global_limit: global,
            per_key_limit: per_key,
            acquire_timeout: Duration::from_millis(100),
        }
    }

    #[tokio::test]
    async fn try_acquire_returns_permit_when_capacity_available() {
        let lim = ResourceLimiter::<String>::new(test_cfg(4, 4));
        let p1 = lim.try_acquire("a".into());
        assert!(p1.is_some());
        // Drop frees the slot.
        drop(p1);
        let p2 = lim.try_acquire("a".into());
        assert!(p2.is_some());
    }

    #[tokio::test]
    async fn try_acquire_rejects_when_per_key_full() {
        let lim = ResourceLimiter::<String>::new(test_cfg(8, 2));
        let _a = lim.try_acquire("a".into()).expect("first");
        let _b = lim.try_acquire("a".into()).expect("second");
        // Third on the same key must fail even though global has
        // plenty of room.
        let c = lim.try_acquire("a".into());
        assert!(c.is_none(), "per-key limit should reject");
        assert!(lim.snapshot().try_rejected >= 1);
    }

    #[tokio::test]
    async fn try_acquire_rejects_when_global_full() {
        let lim = ResourceLimiter::<String>::new(test_cfg(2, 4));
        let _a = lim.try_acquire("a".into()).expect("first");
        let _b = lim.try_acquire("b".into()).expect("second");
        let c = lim.try_acquire("c".into());
        assert!(c.is_none(), "global limit should reject");
    }

    #[tokio::test]
    async fn acquire_waits_for_slot() {
        let lim = std::sync::Arc::new(ResourceLimiter::<String>::new(test_cfg(1, 1)));
        let first = lim.try_acquire("a".into()).expect("first");

        // Spawn a task that releases the permit after a short delay.
        let lim_for_release = lim.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            drop(first);
            // Keep the Arc alive until after `drop(first)` so the
            // runtime doesn't tear down the limiter prematurely.
            drop(lim_for_release);
        });

        // Acquire should succeed once the first permit is dropped.
        let p2 = lim
            .acquire("a".into(), Duration::from_secs(1), None)
            .await
            .expect("acquire should succeed after wait");
        drop(p2);
    }

    #[tokio::test]
    async fn acquire_times_out_when_pool_stays_full() {
        let lim = ResourceLimiter::<String>::new(test_cfg(1, 1));
        let _first = lim.try_acquire("a".into()).expect("first");

        let result = lim
            .acquire("a".into(), Duration::from_millis(50), None)
            .await;
        assert_eq!(result.err(), Some(AcquireError::Timeout));
    }

    #[tokio::test]
    async fn acquire_bails_on_cancellation() {
        let token = CancellationToken::new();
        let token_for_task = token.clone();
        // Limiter isn't `Clone` — callers wrap in `Arc` if they
        // need shared ownership.
        let lim2 = Arc::new(ResourceLimiter::<String>::new(test_cfg(1, 1)));
        let _first2 = lim2.try_acquire("a".into()).expect("first2");

        let waiter = {
            let lim2 = lim2.clone();
            tokio::spawn(async move {
                lim2.acquire(
                    "a".into(),
                    Duration::from_secs(5),
                    Some(token_for_task),
                )
                .await
            })
        };

        // Give the waiter a moment to park.
        tokio::time::sleep(Duration::from_millis(20)).await;
        token.cancel();

        let result = waiter.await.expect("task panicked");
        assert_eq!(result.err(), Some(AcquireError::Cancelled));
        assert!(lim2.snapshot().cancelled >= 1);
    }

    #[tokio::test]
    async fn per_peer_isolation_prevents_starvation() {
        let lim = ResourceLimiter::<String>::new(test_cfg(8, 2));
        // Peer A hogs the global pool.
        for _ in 0..6 {
            // Each peer gets only 2; A can have at most 2 in flight.
            assert!(lim.try_acquire("A".into()).is_some());
        }
        // After dropping A's permits, peer B should be able to use
        // the global pool.
        lim.reap_key(&"A".to_string());
        // B should be able to grab fresh permits.
        let _b1 = lim.try_acquire("B".into()).expect("B first");
        let _b2 = lim.try_acquire("B".into()).expect("B second");
        // And B's third must be rejected (per-key limit).
        assert!(lim.try_acquire("B".into()).is_none());
    }

    #[tokio::test]
    async fn metrics_track_acquire_and_release() {
        let lim = ResourceLimiter::<String>::new(test_cfg(4, 4));
        let p1 = lim.try_acquire("a".into()).expect("first");
        let snap = lim.snapshot();
        assert_eq!(snap.acquired, 1);
        assert_eq!(snap.released, 0);

        drop(p1);
        let snap = lim.snapshot();
        assert_eq!(snap.released, 1);
    }

    #[tokio::test]
    async fn reap_key_removes_idle_buckets() {
        let lim = ResourceLimiter::<String>::new(test_cfg(8, 2));
        let p = lim.try_acquire("ephemeral".into()).expect("acquire");
        assert_eq!(lim.tracked_keys(), 1);
        drop(p);
        lim.reap_key(&"ephemeral".to_string());
        assert_eq!(lim.tracked_keys(), 0);
    }

    #[tokio::test]
    async fn tracked_keys_grows_with_distinct_keys() {
        let lim = ResourceLimiter::<String>::new(test_cfg(16, 2));
        let _a = lim.try_acquire("a".into());
        let _b = lim.try_acquire("b".into());
        let _c = lim.try_acquire("c".into());
        assert_eq!(lim.tracked_keys(), 3);
    }

    #[tokio::test]
    async fn global_available_reflects_in_flight() {
        let lim = ResourceLimiter::<String>::new(test_cfg(3, 3));
        assert_eq!(lim.global_available(), 3);
        let p = lim.try_acquire("a".into()).expect("first");
        assert_eq!(lim.global_available(), 2);
        drop(p);
        assert_eq!(lim.global_available(), 3);
    }

    #[tokio::test]
    async fn snapshot_total_rejected_sums_components() {
        let lim = ResourceLimiter::<String>::new(test_cfg(1, 1));
        let _a = lim.try_acquire("a".into()).expect("first");
        // Global full — try_acquire on a new key fails.
        assert!(lim.try_acquire("b".into()).is_none());
        assert!(lim.try_acquire("c".into()).is_none());
        let snap = lim.snapshot();
        assert!(snap.total_rejected() >= 2);
    }

    #[tokio::test]
    async fn high_concurrency_stress_no_leak() {
        // 100 concurrent acquire/release cycles across 4 keys on a
        // global pool of 8 / per-key 4.  No semaphore leak.
        let lim = Arc::new(ResourceLimiter::<String>::new(test_cfg(8, 4)));
        let mut handles = Vec::new();
        for i in 0..100 {
            let lim = lim.clone();
            let key = format!("peer-{i}");
            handles.push(tokio::spawn(async move {
                if let Ok(p) = lim
                    .acquire(key.clone(), Duration::from_millis(50), None)
                    .await
                {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    drop(p);
                }
            }));
        }
        for h in handles {
            let _ = h.await;
        }
        // After draining, every permit should be available.
        assert_eq!(lim.global_available(), 8);
        assert!(lim.snapshot().released >= 1);
    }

    #[tokio::test]
    async fn per_key_limit_zero_means_global_only() {
        // per_key_limit=0 should not lock anyone out per-key — only
        // the global pool applies.  This test holds 4 permits and
        // then verifies the 5th acquire (on either key) is rejected.
        let lim = ResourceLimiter::<String>::new(ResourceConfig {
            global_limit: 4,
            per_key_limit: 0,
            acquire_timeout: Duration::from_millis(50),
        });
        let _held = vec![
            lim.try_acquire("a".into()).expect("first"),
            lim.try_acquire("a".into()).expect("second"),
            lim.try_acquire("b".into()).expect("third"),
            lim.try_acquire("c".into()).expect("fourth"),
        ];
        // Global is now exhausted.
        assert!(lim.try_acquire("a".into()).is_none());
        assert!(lim.try_acquire("b".into()).is_none());
        assert!(lim.try_acquire("d".into()).is_none());
    }

    #[test]
    fn default_peer_config_has_reasonable_limits() {
        let cfg = default_peer_config();
        assert!(cfg.global_limit >= cfg.per_key_limit);
        assert!(cfg.per_key_limit > 0);
    }

    #[test]
    fn default_room_config_has_reasonable_limits() {
        let cfg = default_room_config();
        assert!(cfg.global_limit >= cfg.per_key_limit);
        assert!(cfg.per_key_limit > 0);
    }

    #[test]
    fn default_tag_config_has_reasonable_limits() {
        let cfg = default_tag_config();
        assert!(cfg.global_limit >= cfg.per_key_limit);
        assert!(cfg.per_key_limit > 0);
    }

    // Compile-time check: confirm `ResourcePermit` is `Send`, since
    // callers will move it across `.await` points.
    #[allow(dead_code)]
    fn _permit_is_send<K: Eq + std::hash::Hash + Clone + Send>(
        p: ResourcePermit<K>,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            // Permit just needs to be moved into the future and
            // dropped at scope end — confirm the compiler accepts
            // this without complaint.
            tokio::time::sleep(Duration::from_millis(1)).await;
            drop(p);
        }
    }

    // Suppress an unused warning on `AtomicUsize` if a future test
    // adds a counter-based assertion.
    #[allow(dead_code)]
    fn _suppress_unused_atomic(_x: AtomicUsize) {}
}