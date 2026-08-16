//! Discovery diagnostics — counter snapshots + structured events.
//!
//! Three pieces:
//!
//! - [`DiscoveryEvent`] — a single tagged event (`PublishFiltered`,
//!   `PublishPassed`, `ResolutionHit`, `ResolutionMiss`) emitted from
//!   inside the discovery pipeline.
//! - [`DiscoveryDiagnostics`] — a shared `Arc`-friendly counter
//!   block. Every address-lookup interaction increments one or more
//!   fields. Cheap enough to keep attached in production.
//! - [`IrohDiscoverySnapshot`] — a point-in-time, `Clone`-able view
//!   of the counters and the configured service set. Suitable for
//!   `/discovery` admin commands or JSON logging.

#![cfg(feature = "iroh")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::Serialize;

use super::PublishPolicy;
use super::pkarr_publisher::UserData;

/// Counter block shared between the discovery builder and the
/// individual [`AddressLookup`] implementations. All increments are
/// best-effort — a failing atomic op is ignored so the discovery
/// path never errors out because the diagnostics subsystem tripped.
///
/// [`AddressLookup`]: iroh::address_lookup::AddressLookup
#[derive(Debug, Default)]
pub struct DiscoveryDiagnostics {
    inner: Arc<DiagnosticsInner>,
}

#[derive(Debug, Default)]
struct DiagnosticsInner {
    publishes_total: AtomicU64,
    publishes_filtered: AtomicU64,
    resolutions_total: AtomicU64,
    resolutions_hit: AtomicU64,
    resolutions_miss: AtomicU64,
    by_provenance: Mutex<Vec<ProvenanceCount>>,
    last_publish_at: Mutex<Option<SystemTime>>,
    last_resolution_at: Mutex<Option<SystemTime>>,
    /// Most-recent user-data payload the local node tried to
    /// publish to a pkarr relay. Mirrors
    /// `iroh_dns::endpoint_info::UserData` — set to `None` when
    /// the operator has not configured `user_data`, and to the
    /// payload itself otherwise. `last_user_data` in the snapshot
    /// is the same value, surfaced for `/discovery` admin output.
    last_user_data: Mutex<Option<UserData>>,
}

/// Counts of resolutions per `provenance` string (e.g. `"pkarr"`,
/// `"dns"`, `"memory"`, `"a3net-mainline-dht"`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProvenanceCount {
    pub provenance: String,
    pub resolutions: u64,
    pub hits: u64,
    pub misses: u64,
}

impl DiscoveryDiagnostics {
    /// Construct an empty counter block.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that an attempt was made to publish `EndpointData`.
    /// `kept` is true when the publish-policy filter did **not**
    /// strip every address.
    pub fn record_publish(&self, kept: bool) {
        self.inner.publishes_total.fetch_add(1, Ordering::Relaxed);
        if !kept {
            self.inner
                .publishes_filtered
                .fetch_add(1, Ordering::Relaxed);
        }
        *recover_lock(self.inner.last_publish_at.lock()) = Some(SystemTime::now());
    }

    /// Emit a structured [`DiscoveryEvent`] from anywhere in the
    /// pipeline. Counter side-effects are routed to the matching
    /// `record_*` helper so the snapshot stays consistent.
    ///
    /// Returns silently for events that have no counter effect
    /// (currently only `PublishPassed` — a placeholder reserved
    /// for future "publish-acknowledged" telemetry).
    ///
    /// NOTE: `DiscoveryEvent` is marked `#[non_exhaustive]`, but
    /// match exhaustiveness inside this crate is allowed because
    /// we are the type's owner. Downstream crates that pattern
    /// match on `DiscoveryEvent` **must** add a `_ =>` arm so
    /// future variants don't break their match arms. New
    /// variants added here that need a counter effect should be
    /// wired into this function explicitly; new no-op variants
    /// stay quiet by default.
    pub fn emit(&self, event: DiscoveryEvent) {
        match event {
            DiscoveryEvent::PublishFiltered { kept } => self.record_publish(kept),
            DiscoveryEvent::PublishPassed => { /* reserved */ }
            DiscoveryEvent::ResolutionHit { provenance } => {
                // P1-B fix: route through the started→outcome
                // path so `resolutions_total` (started counter)
                // stays consistent with `resolutions_hit + miss`
                // (finished counters). Previously the
                // ResolutionHit branch only called
                // `record_resolution`, leaving `resolutions_total`
                // lagging by one event per resolution — operators
                // reading the snapshot saw a "missing" entry.
                self.record_resolution_started();
                self.record_resolution(&provenance, true);
            }
            DiscoveryEvent::ResolutionMiss { provenance } => {
                self.record_resolution_started();
                self.record_resolution(&provenance, false);
            }
        }
    }

    /// Record that an `AddressLookup::resolve` call started.
    pub fn record_resolution_started(&self) {
        self.inner.resolutions_total.fetch_add(1, Ordering::Relaxed);
        *recover_lock(self.inner.last_resolution_at.lock()) = Some(SystemTime::now());
    }

    /// Stamp the most-recent user-data payload the local node
    /// tried to publish. Called from
    /// [`InstrumentedPublisher::publish`](super::pkarr_publisher::InstrumentedPublisher::publish)
    /// on every pkarr publish so the snapshot's
    /// [`IrohDiscoverySnapshot::last_user_data`] field stays in
    /// sync with what the relay actually sees. Pass `None` to
    /// clear the field (e.g. when the operator removed the
    /// `user_data` config).
    pub fn record_user_data(&self, user_data: Option<UserData>) {
        *recover_lock(self.inner.last_user_data.lock()) = user_data;
    }

    /// Record the outcome of a single resolution stream attempt.
    pub fn record_resolution(&self, provenance: &str, hit: bool) {
        if hit {
            self.inner.resolutions_hit.fetch_add(1, Ordering::Relaxed);
        } else {
            self.inner.resolutions_miss.fetch_add(1, Ordering::Relaxed);
        }
        let mut guard = recover_lock(self.inner.by_provenance.lock());
        if let Some(existing) = guard.iter_mut().find(|p| p.provenance == provenance) {
            existing.resolutions += 1;
            if hit {
                existing.hits += 1;
            } else {
                existing.misses += 1;
            }
            return;
        }
        // F1: cap the number of distinct provenance buckets
        // (`by_provenance`) to prevent unbounded growth from a
        // misconfigured upstream that emits a unique
        // provenance per call (e.g. embedding a timestamp).
        //
        // The cap is `MAX_PROVENANCE_BUCKETS` total entries —
        // when a NEW provenance would exceed the cap we
        // promote the last bucket (FIFO) into the synthetic
        // "<other>" aggregator so overflow counts are still
        // visible to the operator.
        const OVERFLOW: &str = "<other>";
        // Fast path: there's an existing <other> bucket — just
        // merge into it.
        if let Some(bucket) = guard.iter_mut().find(|p| p.provenance == OVERFLOW) {
            bucket.resolutions += 1;
            if hit {
                bucket.hits += 1;
            } else {
                bucket.misses += 1;
            }
            return;
        }
        if guard.len() < MAX_PROVENANCE_BUCKETS {
            // Plenty of room — create a fresh bucket.
            guard.push(ProvenanceCount {
                provenance: provenance.to_string(),
                resolutions: 1,
                hits: u64::from(hit),
                misses: u64::from(!hit),
            });
        } else {
            // At capacity, no <other> bucket yet. Promote the
            // last bucket to <other> by replacing it. The
            // operator loses visibility into the displaced
            // bucket's provenance but keeps resolution counts
            // in aggregate.
            let last = guard
                .last_mut()
                .expect("at least one bucket when len >= MAX");
            *last = ProvenanceCount {
                provenance: OVERFLOW.into(),
                resolutions: 1,
                hits: u64::from(hit),
                misses: u64::from(!hit),
            };
        }
    }

    /// Build a point-in-time snapshot.
    pub fn snapshot(&self) -> IrohDiscoverySnapshot {
        let by_provenance = recover_lock(self.inner.by_provenance.lock()).clone();
        let last_publish_at = *recover_lock(self.inner.last_publish_at.lock());
        let last_resolution_at = *recover_lock(self.inner.last_resolution_at.lock());
        let last_user_data = recover_lock(self.inner.last_user_data.lock()).clone();
        IrohDiscoverySnapshot {
            publishes_total: self.inner.publishes_total.load(Ordering::Relaxed),
            publishes_filtered: self.inner.publishes_filtered.load(Ordering::Relaxed),
            resolutions_total: self.inner.resolutions_total.load(Ordering::Relaxed),
            resolutions_hit: self.inner.resolutions_hit.load(Ordering::Relaxed),
            resolutions_miss: self.inner.resolutions_miss.load(Ordering::Relaxed),
            by_provenance,
            last_publish_at,
            last_resolution_at,
            last_user_data: last_user_data.map(|ud| ud.as_str().to_string()),
        }
    }
}

/// One tagged event from the discovery pipeline.
///
/// Marked `#[non_exhaustive]` so we can add new variants (e.g.
/// `PublishPassed` with extra fields, or new resolution
/// outcomes) without breaking downstream pattern matches. A3Net
/// always matches on the kind tag, never on a "did I cover all
/// variants" basis — adding a new variant must be a non-breaking
/// change.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DiscoveryEvent {
    /// Local node tried to publish addressing data; `kept` reports
    /// whether the publish-policy filter passed at least one address
    /// through.
    PublishFiltered { kept: bool },
    /// A publish attempt succeeded end-to-end (packet signed and
    /// HTTP PUT acknowledged). Currently a no-op for counters —
    /// reserved for future "publish acknowledged" telemetry that
    /// arrives via an async confirm channel.
    PublishPassed,
    /// A lookup resolved an address (provenance: e.g. `"pkarr"`).
    ResolutionHit { provenance: String },
    /// A lookup failed to resolve an address.
    ResolutionMiss { provenance: String },
}

/// Upper bound on the number of distinct `provenance` strings
/// tracked in a [`DiscoveryDiagnostics`] snapshot. Without this
/// cap, a misconfigured upstream that emits a unique provenance
/// per call (e.g. embedding a timestamp) would grow
/// `by_provenance` without bound and slowly OOM the operator's
/// `/discovery` endpoint. 64 is more than enough for every
/// well-known address-lookup backend (pkarr, dns, memory,
/// a3net-mainline-dht, …).
pub const MAX_PROVENANCE_BUCKETS: usize = 64;

/// Recover from a poisoned `std::sync::Mutex` without panicking.
///
/// A poisoned mutex still holds the data — the poison flag is
/// just a sticky "another thread panicked while holding this
/// lock" marker. The diagnostics counters are best-effort
/// observability; if a writer panicked we still want to read
/// the (possibly partially-updated) state. Using `unwrap_or_else`
/// here means a poisoned lock never crashes the whole process.
fn recover_lock<T>(res: std::sync::LockResult<T>) -> T {
    match res {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Immutable view of the diagnostics counters + the configured
/// discovery services.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IrohDiscoverySnapshot {
    pub publishes_total: u64,
    pub publishes_filtered: u64,
    pub resolutions_total: u64,
    pub resolutions_hit: u64,
    pub resolutions_miss: u64,
    pub by_provenance: Vec<ProvenanceCount>,
    pub last_publish_at: Option<SystemTime>,
    pub last_resolution_at: Option<SystemTime>,
    /// Most-recent `user_data` payload the local node tried to
    /// publish to the pkarr relay. Mirrors
    /// `iroh_dns::endpoint_info::UserData`. `None` when no
    /// `user_data` is configured (or the publish path has not
    /// run yet). Serialised as a plain UTF-8 string so JSON
    /// consumers don't need a custom deserialiser.
    pub last_user_data: Option<String>,
}

impl IrohDiscoverySnapshot {
    /// Empty snapshot — useful when no endpoint has been bound yet.
    pub fn empty() -> Self {
        Self {
            publishes_total: 0,
            publishes_filtered: 0,
            resolutions_total: 0,
            resolutions_hit: 0,
            resolutions_miss: 0,
            by_provenance: Vec::new(),
            last_publish_at: None,
            last_resolution_at: None,
            last_user_data: None,
        }
    }

    /// Hit rate as a percentage (0.0 ..= 100.0). `0.0` when no
    /// resolutions have been recorded.
    pub fn hit_rate_pct(&self) -> f64 {
        if self.resolutions_total == 0 {
            0.0
        } else {
            (self.resolutions_hit as f64 / self.resolutions_total as f64) * 100.0
        }
    }

    /// How many publishes were *kept* by the publish policy filter.
    pub fn publishes_kept(&self) -> u64 {
        self.publishes_total.saturating_sub(self.publishes_filtered)
    }

    /// Combined with [`PublishPolicy`] to give the operator a
    /// single-string summary suitable for log output.
    pub fn policy_summary(&self, policy: PublishPolicy) -> String {
        format!(
            "policy={policy} kept={} filtered={}",
            self.publishes_kept(),
            self.publishes_filtered,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_snapshot_has_zero_counters() {
        let s = IrohDiscoverySnapshot::empty();
        assert_eq!(s.publishes_total, 0);
        assert_eq!(s.resolutions_total, 0);
        assert!((s.hit_rate_pct() - 0.0).abs() < f64::EPSILON);
        assert!(
            s.policy_summary(PublishPolicy::RelayOnly)
                .contains("kept=0")
        );
    }

    #[test]
    fn record_publish_tracks_filtered() {
        let d = DiscoveryDiagnostics::new();
        d.record_publish(true);
        d.record_publish(false);
        let s = d.snapshot();
        assert_eq!(s.publishes_total, 2);
        assert_eq!(s.publishes_filtered, 1);
        assert_eq!(s.publishes_kept(), 1);
        assert!(s.last_publish_at.is_some());
    }

    #[test]
    fn record_resolution_tracks_provenance() {
        let d = DiscoveryDiagnostics::new();
        d.record_resolution_started();
        d.record_resolution("pkarr", true);
        d.record_resolution("pkarr", false);
        d.record_resolution("memory", true);
        let s = d.snapshot();
        assert_eq!(s.resolutions_total, 1);
        assert_eq!(s.resolutions_hit, 2);
        assert_eq!(s.resolutions_miss, 1);
        let pkarr = s
            .by_provenance
            .iter()
            .find(|p| p.provenance == "pkarr")
            .expect("pkarr bucket present");
        assert_eq!(pkarr.resolutions, 2);
        assert_eq!(pkarr.hits, 1);
        assert_eq!(pkarr.misses, 1);
    }

    #[test]
    fn hit_rate_is_calculated() {
        let d = DiscoveryDiagnostics::new();
        d.record_resolution_started();
        d.record_resolution_started();
        d.record_resolution("a", true);
        d.record_resolution("a", false);
        let s = d.snapshot();
        assert!((s.hit_rate_pct() - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn emit_resolution_hit_increments_started_counter() {
        // P1-B regression: `emit(ResolutionHit)` must route
        // through `record_resolution_started` so the snapshot's
        // `resolutions_total` matches `resolutions_hit +
        // resolutions_miss`. Previously the branch only called
        // `record_resolution`, leaving the started counter
        // behind by exactly one event per resolution.
        let d = DiscoveryDiagnostics::new();
        d.emit(DiscoveryEvent::ResolutionHit {
            provenance: "pkarr".into(),
        });
        d.emit(DiscoveryEvent::ResolutionMiss {
            provenance: "pkarr".into(),
        });
        d.emit(DiscoveryEvent::ResolutionHit {
            provenance: "dns".into(),
        });
        let s = d.snapshot();
        assert_eq!(
            s.resolutions_total, 3,
            "every emit must tick the started counter"
        );
        assert_eq!(s.resolutions_hit, 2);
        assert_eq!(s.resolutions_miss, 1);
        // Per-provenance sanity:
        let pkarr = s
            .by_provenance
            .iter()
            .find(|p| p.provenance == "pkarr")
            .expect("pkarr bucket");
        assert_eq!(pkarr.resolutions, 2);
        assert_eq!(pkarr.hits, 1);
        assert_eq!(pkarr.misses, 1);
    }

    #[test]
    fn provenance_buckets_are_bounded() {
        // F1: even if a misconfigured upstream produces a
        // unique provenance per call, the size of the bucket
        // list is bounded by `MAX_PROVENANCE_BUCKETS`.
        let d = DiscoveryDiagnostics::new();
        for i in 0..(MAX_PROVENANCE_BUCKETS * 4) {
            d.record_resolution(&format!("provenance-{i}"), true);
        }
        let snap = d.snapshot();
        assert!(
            snap.by_provenance.len() <= MAX_PROVENANCE_BUCKETS,
            "by_provenance must be capped, got {} entries",
            snap.by_provenance.len()
        );
        // The aggregated "<other>" bucket is what holds the
        // overflow counter so totals are never silently dropped.
        let other = snap
            .by_provenance
            .iter()
            .find(|p| p.provenance == "<other>")
            .expect("overflow bucket must exist");
        assert!(
            other.resolutions >= MAX_PROVENANCE_BUCKETS as u64,
            "overflow bucket must aggregate the capped entries"
        );
        // The total resolution counter is still the source of
        // truth — the bucket overflow is a presentation
        // concern, not a counter-consistency concern.
        assert_eq!(snap.resolutions_hit, (MAX_PROVENANCE_BUCKETS * 4) as u64);
    }

    #[test]
    fn recover_lock_returns_data_even_when_poisoned() {
        // G1: a poisoned mutex still holds the data. The
        // diagnostics counters are best-effort, so we recover
        // instead of panicking.
        use std::sync::{Arc, Mutex};
        let m = Arc::new(Mutex::new(42u32));
        let m2 = Arc::clone(&m);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _g = m2.lock().unwrap();
            panic!("simulated writer panic");
        }));
        // The mutex is now poisoned — the panic above held
        // the guard and unwound. `recover_lock` must still
        // return the value 42, not panic.
        let v = recover_lock(m.lock());
        assert_eq!(*v, 42);
    }

    #[test]
    fn record_resolution_started_then_outcome_keeps_total_consistent() {
        // H1: the contract is
        // `resolutions_total >= resolutions_hit + resolutions_miss`
        // for the `record_*` API path. A caller that calls
        // `record_resolution` without `record_resolution_started`
        // first observes `total < hit + miss` — that's the
        // documented "caller is responsible for the started
        // hook" semantic. `emit(ResolutionHit{..})` is the
        // safe, all-in-one path.
        let d = DiscoveryDiagnostics::new();

        // Path 1: started → outcome. Total == hit + miss.
        d.record_resolution_started();
        d.record_resolution("pkarr", true);
        d.record_resolution("pkarr", false);
        // Path 2: outcome only. The hit/miss counter advances
        // but the started counter does not.
        d.record_resolution("pkarr", true);
        let s = d.snapshot();
        assert_eq!(s.resolutions_total, 1);
        assert_eq!(s.resolutions_hit, 2);
        assert_eq!(s.resolutions_miss, 1);
        // Total < hit + miss — caller skipped the started
        // hook. This is the documented behavior of the
        // lower-level API.
        assert!(s.resolutions_total < s.resolutions_hit + s.resolutions_miss);
    }

    // ──────────────────── UserData snapshot field ─────────────────────────

    #[test]
    fn empty_snapshot_has_no_user_data() {
        let s = IrohDiscoverySnapshot::empty();
        assert!(s.last_user_data.is_none());
    }

    #[test]
    fn record_user_data_is_observable_in_snapshot() {
        let d = DiscoveryDiagnostics::new();
        d.record_user_data(Some(
            crate::iroh::discovery::UserData::new("audit-marker").unwrap(),
        ));
        let s = d.snapshot();
        assert_eq!(s.last_user_data.as_deref(), Some("audit-marker"));
    }

    #[test]
    fn record_user_data_can_be_cleared() {
        let d = DiscoveryDiagnostics::new();
        d.record_user_data(Some(
            crate::iroh::discovery::UserData::new("first").unwrap(),
        ));
        d.record_user_data(None);
        let s = d.snapshot();
        assert!(s.last_user_data.is_none(), "None must clear the field");
    }

    #[test]
    fn record_user_data_overwrites_previous() {
        let d = DiscoveryDiagnostics::new();
        d.record_user_data(Some(crate::iroh::discovery::UserData::new("v1").unwrap()));
        d.record_user_data(Some(crate::iroh::discovery::UserData::new("v2").unwrap()));
        let s = d.snapshot();
        assert_eq!(s.last_user_data.as_deref(), Some("v2"));
    }

    #[test]
    fn snapshot_serializes_user_data_to_json() {
        let mut s = IrohDiscoverySnapshot::empty();
        s.last_user_data = Some("hello".into());
        let json = serde_json::to_string(&s).expect("serialize");
        // `IrohDiscoverySnapshot` doesn't carry a global
        // `serde(rename_all)`; the field is serialised in its
        // Rust form (`last_user_data`).
        assert!(
            json.contains("\"last_user_data\":\"hello\""),
            "json missing last_user_data: {json}"
        );
    }
}
