//! P1-5 PoC: ResourceLimiter substrate under realistic pressure.
//!
//! Goal — verify the limiter's runtime behavior matches the design
//! intent at the substrate level (no `a3net-node` / `a3net-mesh`
//! dependency).  These tests use a separately-constructed
//! `ResourceLimiter<String>` to exercise:
//!
//! 1. **Peer flood** — many concurrent acquires from one peer key
//!    must respect the per-key cap.
//! 2. **Global budget** — acquires spread across many peers must
//!    respect the global cap.
//! 3. **Cancellation race** — flood + token cancel mid-flight must
//!    release queued acquires with `AcquireError::Cancelled`.
//!
//! These are intentionally *substrate-only* — the Node-level
//! integration tests live in `crates/a3net-node/tests/resource_limiter_integration.rs`
//! and `resource_limiter_poc.rs`.  Keeping the substrate PoC in
//! `a3net-resilience/tests/` avoids the `a3net-node` build
//! dependency for these checks (some P1-N branches are mid-refactor
//! and may be temporarily uncompilable).

use std::sync::Arc;
use std::time::Duration;

use a3net_resilience::{
    AcquireError, CancellationToken, ResourceConfig, ResourceLimiter,
};

fn small_limiter(global: usize, per_key: usize) -> ResourceLimiter<String> {
    ResourceLimiter::new(ResourceConfig {
        global_limit: global,
        per_key_limit: per_key,
        acquire_timeout: Duration::from_millis(50),
    })
}

#[tokio::test]
async fn peer_flood_respects_per_key_cap() {
    // 256 concurrent acquires from the same peer key.  Per-key
    // cap = 16, so only 16 may be held concurrently at any
    // instant.  We use a barrier to maximize contention.
    let lim = Arc::new(small_limiter(256, 16));

    let n_tasks = 256;
    let barrier = Arc::new(tokio::sync::Barrier::new(n_tasks));
    let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let current = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(n_tasks);
    for _ in 0..n_tasks {
        let lim = lim.clone();
        let barrier = barrier.clone();
        let peak = peak.clone();
        let current = current.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            if let Some(_permit) = lim.try_acquire("flood-peer".into()) {
                let now =
                    current.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                let mut p = peak.load(std::sync::atomic::Ordering::SeqCst);
                while now > p {
                    match peak.compare_exchange(
                        p,
                        now,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    ) {
                        Ok(_) => break,
                        Err(actual) => p = actual,
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
                current.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }

    let observed_peak =
        peak.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        observed_peak <= 16,
        "per-key cap must hold (peak={observed_peak}, limit=16)"
    );
    assert!(observed_peak >= 1, "must see at least one grant");

    // Cleanup: the key semaphore was used and now sits at 16/16;
    // reaping clears the HashMap entry.
    lim.reap_key(&"flood-peer".to_string());
    assert_eq!(lim.tracked_keys(), 0);
    assert_eq!(lim.global_available(), 256);
}

#[tokio::test]
async fn global_budget_distributes_across_peers() {
    // 256 acquires spread across 32 peers → 8 each.  All 256
    // must succeed; the global pool must be exactly drained
    // during the test and fully restored afterwards.
    let lim = Arc::new(small_limiter(256, 16));

    let mut held = Vec::with_capacity(256);
    for i in 0..256 {
        let key = format!("peer-{}", i % 32);
        let permit = lim
            .try_acquire(key.clone())
            .unwrap_or_else(|| panic!("peer {key} should have a slot (i={i})"));
        held.push(permit);
    }
    assert_eq!(lim.global_available(), 0);
    let snap = lim.snapshot();
    assert_eq!(snap.acquired, 256);
    assert_eq!(snap.try_rejected, 0);

    drop(held);
    assert_eq!(lim.global_available(), 256);
    for i in 0..32 {
        lim.reap_key(&format!("peer-{i}"));
    }
    assert_eq!(lim.tracked_keys(), 0);
}

#[tokio::test]
async fn global_budget_rejects_overflow() {
    // 300 acquires spread across 30 peers (10 per peer).  Global
    // cap = 256 → 44 rejections.
    let lim = Arc::new(small_limiter(256, 16));

    let mut held: Vec<a3net_resilience::ResourcePermit<String>> = Vec::new();
    let mut rejected = 0;
    for i in 0..300 {
        let key = format!("peer-{}", i % 30);
        match lim.try_acquire(key) {
            Some(p) => held.push(p),
            None => rejected += 1,
        }
    }
    assert_eq!(held.len(), 256);
    assert_eq!(rejected, 44);
    let snap = lim.snapshot();
    assert_eq!(snap.try_rejected, 44);
    assert_eq!(snap.acquired, 256);

    drop(held);
    assert_eq!(lim.global_available(), 256);
}

#[tokio::test]
async fn cancellation_aborts_queued_acquires() {
    // Saturate per-peer budget → next acquire parks.  Fire the
    // token → parked acquire returns Cancelled, no deadlock.
    let lim = Arc::new(small_limiter(16, 16));

    let mut held = Vec::with_capacity(16);
    for _ in 0..16 {
        held.push(lim.try_acquire("cancelled-peer".into()).expect("permit"));
    }
    assert_eq!(lim.key_available(&"cancelled-peer".to_string()), 0);

    let token = CancellationToken::new();
    let token_for_task = token.clone();
    let lim_for_task = lim.clone();
    let waiter = tokio::spawn(async move {
        lim_for_task
            .acquire(
                "cancelled-peer".into(),
                Duration::from_secs(5),
                Some(token_for_task),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    token.cancel();

    let result = waiter.await.expect("task panicked");
    assert_eq!(result.err(), Some(AcquireError::Cancelled));
    assert_eq!(lim.snapshot().cancelled, 1);

    drop(held);
    assert_eq!(lim.key_available(&"cancelled-peer".to_string()), 16);
    assert_eq!(lim.global_available(), 16);
}

#[tokio::test]
async fn mixed_workload_no_deadlock() {
    // 64 long-held permits on 16 peers (4 each), 64 short-lived
    // acquires on 16 different peers, then 1 cancellation round.
    let lim = Arc::new(small_limiter(256, 16));

    let long_held: Vec<_> = (0..64)
        .map(|i| {
            lim.try_acquire(format!("long-peer-{i}"))
                .expect("long permit")
        })
        .collect();
    assert_eq!(lim.global_available(), 256 - 64);

    // Short burst.
    let burst = {
        let lim = lim.clone();
        async move {
            let mut handles = Vec::with_capacity(64);
            for i in 0..64 {
                let lim = lim.clone();
                handles.push(tokio::spawn(async move {
                    let _p = lim
                        .try_acquire(format!("burst-peer-{i}"))
                        .expect("burst permit");
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }));
            }
            for h in handles {
                let _ = h.await;
            }
        }
    };
    burst.await;

    // Cancellation round — saturate cancelled-peer's budget so
    // the waiter must park on `acquire().await`.
    let cancelled_held: Vec<_> = (0..16)
        .map(|_| {
            lim.try_acquire("cancelled-peer".into())
                .expect("cancelled permit")
        })
        .collect();
    assert_eq!(lim.key_available(&"cancelled-peer".to_string()), 0);

    let token = CancellationToken::new();
    let lim_for_task = lim.clone();
    let waiter = {
        let token = token.clone();
        tokio::spawn(async move {
            lim_for_task
                .acquire(
                    "cancelled-peer".into(),
                    Duration::from_secs(5),
                    Some(token),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    token.cancel();
    assert_eq!(
        waiter.await.expect("task").err(),
        Some(AcquireError::Cancelled)
    );
    drop(cancelled_held);

    drop(long_held);
    let snap = lim.snapshot();
    assert!(snap.acquired >= 64);
    assert!(snap.released >= 64);
    assert!(snap.cancelled >= 1);

    assert_eq!(lim.global_available(), 256);
}

#[tokio::test]
async fn huge_flood_does_not_leak() {
    // 1000 acquires across 200 peers → 5 per peer (under per-key
    // cap).  Under multi-thread scheduling some acquires may
    // temporarily hit the global cap and be rejected; the test
    // asserts that *every successful acquire releases cleanly*
    // and the final state is consistent (no leaked semaphore
    // slots, no orphaned HashMap entries beyond what reaping
    // would clear).  Tests for any subtle leak in the
    // `HashMap` / `RwLock` / `Semaphore` plumbing.
    let lim = Arc::new(small_limiter(256, 16));

    let mut handles = Vec::with_capacity(1000);
    for i in 0..1000 {
        let lim = lim.clone();
        handles.push(tokio::spawn(async move {
            let key = format!("peer-{i}");
            // Permits are optional — global contention may force
            // some to None.  The assertion that matters is the
            // post-drop state, not the per-task success rate.
            let _p = lim.try_acquire(key);
            tokio::time::sleep(Duration::from_micros(100)).await;
        }));
    }
    for h in handles {
        let _ = h.await;
    }

    // After every permit (whether Some or None) is dropped, the
    // global pool must be fully restored.
    assert_eq!(lim.global_available(), 256);

    let snap = lim.snapshot();
    // acquired == released (every successful permit was dropped).
    assert_eq!(
        snap.acquired, snap.released,
        "every acquire must release (acquired={}, released={})",
        snap.acquired, snap.released
    );
    // We expect some acquired (the test would be vacuous otherwise).
    assert!(snap.acquired >= 256, "must see at least global-cap grants");

    // Reap all distinct keys (1000 in this case since each task
    // touches a unique key); the HashMap entry for each key must
    // drop to zero so `tracked_keys` returns to 0.
    for i in 0..1000 {
        lim.reap_key(&format!("peer-{i}"));
    }
    assert_eq!(lim.tracked_keys(), 0);
}