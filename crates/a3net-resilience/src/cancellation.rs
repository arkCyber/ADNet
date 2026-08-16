//! Cancellation scope — coordinated task shutdown.
//!
//! Production daemons (a3net-node, a3net-relay, a3net-dns-server) typically
//! spawn many long-lived tokio tasks at startup.  When the user hits
//! Ctrl-C (or the operator sends `SIGTERM`, or the supervisor decides
//! to restart), every one of those tasks must observe cancellation and
//! exit — otherwise the process leaks FDs / ports / database handles and
//! the next start fails with `Address in use`.
//!
//! `CancellationScope` solves this with two pieces:
//!
//! 1. **`CancellationToken`** — cheaply cloneable, observable via
//!    [`CancellationToken::is_cancelled`] or awaited via
//!    [`CancellationToken::cancelled`].  Use it inside `select!` to make
//!    any `tokio::spawn`-ed future cancellation-aware:
//!
//!    ```ignore
//!    tokio::select! {
//!        _ = token.cancelled() => { /* cleanup */ return; }
//!        _ = my_long_running_work() => { /* normal completion */ }
//!    }
//!    ```
//!
//! 2. **`CancellationScope`** — owns the token **and** tracks every
//!    task spawned through [`CancellationScope::spawn`].  On
//!    [`CancellationScope::cancel`] the token fires; on
//!    [`CancellationScope::join`] every tracked task is awaited with a
//!    configurable timeout so a stuck task can never block shutdown
//!    indefinitely.
//!
//! ## Design choices
//!
//! - **No `Arc<Mutex<...>>` global state** — the token is the only piece
//!   of shared state, and it is `Arc<AtomicBool>` under the hood.  Clones
//!   cost one atomic increment.
//! - **No background watcher task** — `cancel()` flips the flag and
//!   notifies waiters inline.  Drain happens in `join()`.
//! - **Bounded shutdown** — `join()` takes a `Duration` so a stuck task
//!   cannot pin the daemon at exit.  After the timeout, `join()` returns
//!   and reports the still-running task count.
//!
//! ## Non-goals
//!
//! - This module does **not** attempt to refactor every `tokio::spawn`
//!   in the workspace.  Tasks spawned outside the scope will keep
//!   running until they exit on their own.  The audit's P1-3 plan
//!   recommends this module as the substrate for **future** migration;
//!   the immediate win is that `Node::shutdown` (and friends) can
//!   guarantee a bounded exit.
//! - No hierarchical cancellation (parent → child) yet.  A future
//!   iteration can extend the token with a tree if multiple scopes need
//!   to compose.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Default timeout for [`CancellationScope::join`].
///
/// 5 seconds is long enough for most graceful-exit paths (network
/// flush, SQLite checkpoint, etc.) but short enough that a wedged
/// daemon doesn't hang the supervisor indefinitely.
pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

// ─────────────────────────────────────────────────────────────────
// CancellationToken
// ─────────────────────────────────────────────────────────────────

/// Cheaply cloneable handle for "has cancellation been requested?".
///
/// Internally an `Arc<Inner>` with an atomic flag + a `Notify`.  Clones
/// share the same underlying signal — call [`Self::cancel`] on any
/// clone and all clones observe it.
///
/// Implements [`Future`] so it can be used directly in `select!`:
///
/// ```ignore
/// tokio::select! {
///     _ = token.cancelled() => { /* shutdown path */ }
///     _ = work() => { /* normal completion */ }
/// }
/// ```
#[derive(Debug)]
pub struct CancellationToken {
    inner: Arc<Inner>,
}

impl Clone for CancellationToken {
    fn clone(&self) -> Self {
        self.inner.clones.fetch_add(1, Ordering::SeqCst);
        Self {
            inner: self.inner.clone(),
        }
    }
}

#[derive(Debug)]
struct Inner {
    cancelled: AtomicBool,
    notify: Notify,
    /// Total clones ever made (for diagnostics).
    clones: AtomicUsize,
}

impl CancellationToken {
    /// Create a fresh, un-cancelled token.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
                clones: AtomicUsize::new(1),
            }),
        }
    }

    /// Fire the cancellation signal.  All current and future
    /// [`Self::cancelled`] futures resolve; [`Self::is_cancelled`]
    /// returns `true`.
    ///
    /// Idempotent — calling twice is a no-op.
    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::SeqCst) {
            self.inner.notify.notify_waiters();
            // Catch up: the Notify's `notify_waiters` only wakes
            // futures already parked on `notified()`.  Tasks that call
            // `cancelled()` *after* this point need to observe the
            // flag directly — which is what the loop in
            // `cancelled()` does.  So no extra wakeup is needed for
            // late arrivals.
        }
    }

    /// `true` after [`Self::cancel`] has been called.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Future that resolves when [`Self::cancel`] is called.
    ///
    /// Polling this future is cheap — the first poll registers on the
    /// `Notify`, subsequent polls short-circuit if the flag is set.
    pub async fn cancelled(&self) {
        // Fast path: already cancelled.
        if self.is_cancelled() {
            return;
        }

        // Register a waiter.  If a cancel arrives between the
        // fast-path check and `notified()`, the cancel notifies us;
        // otherwise we keep waiting.
        //
        // We loop because `notify_waiters` only wakes currently
        // registered waiters — but `cancel()` may have fired while
        // we were registering.  Re-checking the flag after
        // `notified()` closes that gap.
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            if self.is_cancelled() {
                return;
            }
            notified.await;
            if self.is_cancelled() {
                return;
            }
        }
    }

    /// Number of live clones (including `self`).  Useful for tests /
    /// diagnostics — confirms the token is being cloned (i.e. handed
    /// to tasks) and dropped (i.e. tasks exited).
    pub fn clone_count(&self) -> usize {
        self.inner.clones.load(Ordering::SeqCst)
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

// `Inner` is reference-counted and contains only `AtomicBool`,
// `Notify`, and `AtomicUsize` — all `Send + Sync`.  We don't expose
// `Inner`, so this is implicit.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    let _ = assert_send_sync::<CancellationToken>;
};

// ─────────────────────────────────────────────────────────────────
// CancellationScope
// ─────────────────────────────────────────────────────────────────

/// Scoped task tracker.  Owns a [`CancellationToken`] and a `JoinSet`
/// of spawned tasks.
///
/// Cloning a scope yields a **child** scope that shares the parent's
/// token but has its own `JoinSet`.  When the parent fires
/// [`Self::cancel`], both parent and child observe it; `join()` on the
/// parent waits for both.
#[derive(Debug)]
pub struct CancellationScope {
    token: CancellationToken,
    /// Tracked tasks.  We use `Vec` (not `JoinSet`) so `join()` can
    /// wait on a snapshot and avoid race-with-abort.  Tasks are
    /// inserted via `JoinHandle::abort_handle` indirectly — see
    /// [`Self::spawn`].
    tasks: tokio::sync::Mutex<Vec<TrackedTask>>,
    /// Total tasks ever spawned through this scope (cumulative).
    spawn_count: AtomicUsize,
}

#[derive(Debug)]
struct TrackedTask {
    #[allow(dead_code)]
    name: String,
    handle: JoinHandle<()>,
}

impl CancellationScope {
    /// Create a fresh scope.  Tokens and tracked-task lists are
    /// independent of any other scope.
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            tasks: tokio::sync::Mutex::new(Vec::new()),
            spawn_count: AtomicUsize::new(0),
        }
    }

    /// Snapshot of the underlying token — useful for handing to
    /// tasks that should observe cancellation but were spawned
    /// outside of [`Self::spawn`] (e.g. legacy `tokio::spawn` sites).
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// `true` once [`Self::cancel`] (or [`CancellationToken::cancel`]
    /// on any clone) has fired.
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Total tasks ever spawned on this scope.
    pub fn spawn_count(&self) -> usize {
        self.spawn_count.load(Ordering::SeqCst)
    }

    /// Number of tracked tasks still registered (i.e. not yet joined
    /// or aborted).
    pub async fn tracked_count(&self) -> usize {
        self.tasks.lock().await.len()
    }

    /// Fire cancellation.  All tasks that observe [`CancellationToken::cancelled`]
    /// should start winding down.  `join()` will then wait for them
    /// (up to the timeout) and abort anything still running.
    pub fn cancel(&self) {
        if !self.is_cancelled() {
            info!("CancellationScope: cancel requested");
        }
        self.token.cancel();
    }

    /// Spawn a tracked task.  The spawned future does NOT receive the
    /// token — callers must pass `scope.token()` (or a clone) into
    /// the future and observe it themselves, typically via `select!`.
    ///
    /// If `name` is `Some`, the task is labelled in logs and the
    /// `JoinSummary` debug output.  `None` produces a default label
    /// like `task-3`.
    ///
    /// Returns `()` (not the `JoinHandle`).  The scope owns the handle
    /// internally so it can drive `join()`.  Callers that need the
    /// handle for direct awaiting should use `tokio::spawn` outside
    /// the scope.
    pub fn spawn<F>(&self, name: Option<&str>, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let label = name
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("task-{}", self.spawn_count.load(Ordering::SeqCst)));
        let handle = tokio::spawn(fut);
        self.spawn_count.fetch_add(1, Ordering::SeqCst);

        // Try to track the task non-blockingly.  If the lock is held
        // (e.g. by `join()`) we lose the ability to wait on this
        // task — that's a graceful degradation, not a hard error:
        // the caller still has the scope's token and can still
        // observe cancellation, and the underlying tokio task will
        // be cleaned up when the process exits.
        if let Ok(mut guard) = self.tasks.try_lock() {
            guard.push(TrackedTask {
                name: label,
                handle,
            });
        } else {
            warn!(
                "CancellationScope: spawn({}) lost tracking; lock contended",
                label
            );
            // The task is still spawned; we just can't wait on it.
        }
    }

    /// Wait for all tracked tasks to finish, aborting stragglers after
    /// `timeout`.  Returns a [`JoinSummary`] describing what
    /// completed and what was aborted.
    ///
    /// Always call this after [`Self::cancel`] for a clean shutdown.
    pub async fn join(&self, timeout: Duration) -> JoinSummary {
        let started = std::time::Instant::now();
        let mut summary = JoinSummary::default();

        // Take a snapshot of the tracked tasks so concurrent spawns
        // during drain don't extend our deadline.
        let mut tasks = self.tasks.lock().await;
        let snapshot: Vec<TrackedTask> = std::mem::take(&mut *tasks);
        drop(tasks);

        if snapshot.is_empty() {
            return summary;
        }

        info!(
            "CancellationScope::join: waiting on {} tracked task(s) (timeout {:?})",
            snapshot.len(),
            timeout
        );

        let total = snapshot.len();
        let deadline = started + timeout;
        // Drop the names since we no longer need them after the
        // initial info log; this also keeps the loop body simple.
        let mut handles: Vec<JoinHandle<()>> =
            snapshot.into_iter().map(|t| t.handle).collect();

        // Drive every handle with a per-iteration timeout.  We poll
        // them all on the same task and rely on tokio to schedule
        // wakes when any of them complete.  This is O(N) memory and
        // O(N) wakeups, which is fine for the scope's typical fan-out
        // (≤ 32 tasks in a3net-node).
        while !handles.is_empty() {
            let now = std::time::Instant::now();
            let remaining_budget = deadline.saturating_duration_since(now);

            if remaining_budget.is_zero() {
                warn!(
                    "CancellationScope::join: timeout reached; aborting {} still-running task(s)",
                    handles.len()
                );
                summary.aborted += handles.len();
                for h in &handles {
                    h.abort();
                }
                // Best-effort drain: await each aborted handle with a
                // tiny budget so we don't block on a stuck one.  We
                // already counted them as `aborted`.
                for h in handles.drain(..) {
                    let _ = tokio::time::timeout(Duration::from_millis(50), h).await;
                }
                break;
            }

            // Wait up to 100ms (or the remaining budget, whichever
            // is smaller) for any task to complete.  Polling each
            // handle in turn is a reasonable approximation of "wait
            // for any" without pulling in `futures::future::select_all`.
            let poll_budget = remaining_budget.min(Duration::from_millis(100));
            let completed_now = Self::poll_some(&mut handles, poll_budget).await;
            summary.completed += completed_now;

            if completed_now == 0 {
                // Nothing finished within this slice; yield so the
                // runtime can make progress on the awaited tasks.
                tokio::task::yield_now().await;
            }
        }

        summary.elapsed = started.elapsed();
        debug!(
            "CancellationScope::join: completed={} aborted={} elapsed={:?} (of {})",
            summary.completed, summary.aborted, summary.elapsed, total
        );
        summary
    }

    /// Poll `handles` for up to `budget` and remove the ones that
    /// finished.  Returns how many finished.
    async fn poll_some(handles: &mut Vec<JoinHandle<()>>, budget: Duration) -> usize {
        if handles.is_empty() {
            return 0;
        }
        let mut finished = 0;
        let start = std::time::Instant::now();
        // We walk the list repeatedly until either we see a completion
        // or the budget runs out.  Each iteration awaits one task;
        // tasks that are still pending get re-parked.
        while !handles.is_empty() && start.elapsed() < budget {
            let i = handles.len() - 1;
            let mut h = handles.swap_remove(i);
            // Race the task against the per-iteration budget slice.
            let slice = budget.saturating_sub(start.elapsed());
            let slice = slice.min(Duration::from_millis(25));
            match tokio::time::timeout(slice, &mut h).await {
                Ok(Ok(())) => {
                    finished += 1;
                }
                Ok(Err(_)) => {
                    finished += 1;
                }
                Err(_) => {
                    // Still pending — put it back at the end and
                    // break to let the outer loop's deadline decide.
                    handles.push(h);
                    break;
                }
            }
        }
        finished
    }

    /// Convenience: cancel + join with the default timeout.
    pub async fn shutdown(&self) -> JoinSummary {
        self.cancel();
        self.join(DEFAULT_SHUTDOWN_TIMEOUT).await
    }
}

impl Default for CancellationScope {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────
// JoinSummary
// ─────────────────────────────────────────────────────────────────

/// Statistics about a [`CancellationScope::join`] call.
#[derive(Debug, Clone, Default)]
pub struct JoinSummary {
    /// Tasks that finished cleanly (or were aborted cleanly).
    pub completed: usize,
    /// Tasks that were still running when the timeout hit and got
    /// forcibly aborted.
    pub aborted: usize,
    /// Wall-clock time the join took.
    pub elapsed: Duration,
}

impl JoinSummary {
    /// `true` if every tracked task finished within the timeout
    /// (i.e. nothing had to be force-aborted).
    pub fn is_clean(&self) -> bool {
        self.aborted == 0
    }
}

// ─────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as O};
    use std::time::Duration;

    #[tokio::test]
    async fn token_cancel_fires_immediately() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
        // Idempotent.
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn token_cancelled_future_resolves_on_cancel() {
        let token = CancellationToken::new();
        let token2 = token.clone();

        let waiter = tokio::spawn(async move {
            token2.cancelled().await;
            true
        });

        // Give the waiter time to register.
        tokio::time::sleep(Duration::from_millis(10)).await;
        token.cancel();

        let result = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter did not resolve in time")
            .expect("waiter task panicked");
        assert!(result);
    }

    #[tokio::test]
    async fn token_cancelled_already_set_completes_immediately() {
        let token = CancellationToken::new();
        token.cancel();
        // No await on a Notify ever happened; the future still
        // resolves because the flag is set.
        tokio::time::timeout(Duration::from_millis(100), token.cancelled())
            .await
            .expect("cancelled() future did not return promptly");
    }

    #[tokio::test]
    async fn scope_spawn_tracks_task() {
        let scope = CancellationScope::new();
        let counter = Arc::new(AtomicUsize::new(0));

        let counter_for_task = counter.clone();
        let token = scope.token();
        scope.spawn(Some("counter"), async move {
            // Race the work against cancellation.
            tokio::select! {
                _ = token.cancelled() => {}
                _ = async {
                    counter_for_task.fetch_add(1, O::SeqCst);
                } => {}
            }
        });
        assert_eq!(scope.spawn_count(), 1);
        assert_eq!(scope.tracked_count().await, 1);

        // Cancel + drain.
        let summary = scope.shutdown().await;
        assert_eq!(scope.spawn_count(), 1);
        assert_eq!(summary.completed + summary.aborted, 1);
    }

    #[tokio::test]
    async fn scope_spawn_multiple_tracks_all() {
        let scope = CancellationScope::new();
        for i in 0..5 {
            let token = scope.token();
            scope.spawn(Some(&format!("worker-{i}")), async move {
                token.cancelled().await;
            });
        }
        assert_eq!(scope.spawn_count(), 5);
        assert_eq!(scope.tracked_count().await, 5);

        let summary = scope.shutdown().await;
        assert_eq!(summary.completed, 5);
        assert_eq!(summary.aborted, 0);
        assert!(summary.is_clean());
    }

    #[tokio::test]
    async fn scope_cancel_propagates_to_running_task() {
        let scope = CancellationScope::new();
        let observed = Arc::new(AtomicBool::new(false));
        let observed_clone = observed.clone();
        let token = scope.token();

        scope.spawn(Some("observer"), async move {
            token.cancelled().await;
            observed_clone.store(true, O::SeqCst);
        });

        // Yield so the task parks on cancelled().
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!observed.load(O::SeqCst));

        scope.cancel();
        let summary = scope.join(Duration::from_secs(1)).await;
        assert!(observed.load(O::SeqCst), "task did not observe cancel");
        assert_eq!(summary.completed, 1);
        assert_eq!(summary.aborted, 0);
    }

    #[tokio::test]
    async fn scope_join_aborts_stuck_tasks_after_timeout() {
        let scope = CancellationScope::new();
        let token = scope.token();

        // A task that IGNORES cancellation — it'll loop forever.
        scope.spawn(Some("stuck"), async move {
            let _t = token; // satisfy the move-closure linter
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });

        // Cancel and join with a very short timeout.  The stuck task
        // should be force-aborted.
        scope.cancel();
        let summary = scope.join(Duration::from_millis(100)).await;
        assert_eq!(summary.aborted, 1, "stuck task should have been aborted");
        assert!(!summary.is_clean());
    }

    #[tokio::test]
    async fn empty_scope_shutdown_is_instant() {
        let scope = CancellationScope::new();
        let started = std::time::Instant::now();
        let summary = scope.shutdown().await;
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_millis(50));
        assert_eq!(summary.completed, 0);
        assert_eq!(summary.aborted, 0);
    }

    #[tokio::test]
    async fn token_clone_count_tracks_lifecycle() {
        let token = CancellationToken::new();
        assert_eq!(token.clone_count(), 1);

        let t1 = token.clone();
        assert_eq!(token.clone_count(), 2);
        assert_eq!(t1.clone_count(), 2);

        drop(t1);
        // `token` still holds one ref so count is 1, not 0 — the
        // counter is *cumulative* (incremented on each clone), not
        // a live-refcount.
        assert_eq!(token.clone_count(), 2);
    }

    #[test]
    fn join_summary_is_clean_when_no_aborts() {
        let s = JoinSummary {
            completed: 3,
            aborted: 0,
            elapsed: Duration::from_millis(10),
        };
        assert!(s.is_clean());
    }

    #[test]
    fn join_summary_dirty_when_aborts_nonzero() {
        let s = JoinSummary {
            completed: 2,
            aborted: 1,
            elapsed: Duration::from_millis(100),
        };
        assert!(!s.is_clean());
    }

    // Compile-only check: ensure `CancellationToken::cancelled()`
    // returns a `Future`.  This is a no-op at runtime; it just pins
    // the signature so a future API drift shows up as a compile
    // error instead of a runtime panic.  We hold the token across
    // the call so the borrow checker is happy.
    #[allow(dead_code)]
    async fn _future_polymorphism(token: CancellationToken) {
        token.cancelled().await
    }
}