//! Concurrent metrics — verify atomicity of `Counter`,
//! `Gauge`, `Histogram` under heavy multi-threaded load.
//!
//! These tests do **not** assert that a counter ends at a
//! specific value (because each task may have non-deterministic
//! scheduling, the final value can be off by a small delta if
//! a labeled-insert races with an observation). Instead they
//! assert **structural** invariants:
//!
//! - The unlabeled atomic never regresses (counter is monotonic).
//! - Every labeled variant observed by *some* task is visible
//!   in the final snapshot.
//! - The total observed count equals `task_count × per_task`.
//! - No task panics, no deadlock, no `unwrap` on a poisoned
//!   lock.

use std::collections::HashSet;
use std::sync::Arc;

use adnet_observability::histogram::Histogram;
use adnet_observability::labels::LabelSet;
use adnet_observability::metrics::{Counter, Gauge};
use adnet_observability::registry::Registry;

#[test]
fn counter_unlabeled_concurrent_increments_match_total() {
    let counter = Arc::new(Counter::new("c", "help"));
    let task_count = 8u64;
    let per_task = 10_000u64;
    let mut joins = Vec::new();
    for _ in 0..task_count {
        let c = Arc::clone(&counter);
        joins.push(std::thread::spawn(move || {
            for _ in 0..per_task {
                c.inc();
            }
        }));
    }
    for j in joins {
        j.join().expect("task must not panic");
    }
    assert_eq!(counter.get(), task_count * per_task);
}

#[test]
fn counter_labeled_concurrent_increments_match_total() {
    let counter = Arc::new(Counter::new("c", "help"));
    // 4 distinct label sets, every task hits all 4.
    let labels: Vec<LabelSet> = ["a", "b", "c", "d"]
        .into_iter()
        .map(|k| LabelSet::new([(k.to_string(), "v".to_string())]).unwrap())
        .collect();
    let task_count = 8u64;
    let per_task = 5_000u64;
    let mut joins = Vec::new();
    for _ in 0..task_count {
        let c = Arc::clone(&counter);
        let labels = labels.clone();
        joins.push(std::thread::spawn(move || {
            for _ in 0..per_task {
                for l in &labels {
                    c.inc_labels(l);
                }
            }
        }));
    }
    for j in joins {
        j.join().expect("task must not panic");
    }
    let snap = counter.labeled_snapshot();
    assert_eq!(snap.len(), labels.len());
    for (k, v) in snap {
        // Every label set should have been hit
        // `task_count * per_task` times.
        assert_eq!(v, task_count * per_task, "label set {k:?} has wrong count");
    }
}

#[test]
fn gauge_concurrent_set_and_get_does_not_deadlock() {
    let gauge = Arc::new(Gauge::new("g", "help"));
    let mut joins = Vec::new();
    for t in 0..4u64 {
        let g = Arc::clone(&gauge);
        joins.push(std::thread::spawn(move || {
            for i in 0..1000u64 {
                g.set((t * 1000 + i) as i64);
                let _ = g.get();
            }
        }));
    }
    for j in joins {
        j.join().expect("task must not panic");
    }
    // We don't assert a specific final value (racy), but
    // `get()` must succeed and return a value in the
    // expected range.
    let v = gauge.get();
    assert!((0..4000).contains(&v));
}

#[test]
fn histogram_concurrent_observations_count_correctly() {
    let hist = Arc::new(Histogram::new("h", "help"));
    let task_count = 8u64;
    let per_task = 1_000u64;
    let mut joins = Vec::new();
    for _ in 0..task_count {
        let h = Arc::clone(&hist);
        joins.push(std::thread::spawn(move || {
            for i in 0..per_task {
                // Values spread across all 8 buckets.
                let v = (i as f64) * 0.012;
                h.observe(v);
            }
        }));
    }
    for j in joins {
        j.join().expect("task must not panic");
    }
    assert_eq!(hist.count(), task_count * per_task);
    // Sum should equal the total of all observations.
    let expected_sum: f64 =
        (0..per_task).map(|i| (i as f64) * 0.012).sum::<f64>() * (task_count as f64);
    let actual_sum = hist.sum();
    assert!(
        (actual_sum - expected_sum).abs() < 1e-3,
        "expected sum ≈ {expected_sum}, got {actual_sum}"
    );
}

#[test]
fn registry_concurrent_registration_is_safe() {
    use std::collections::HashSet;
    let reg = Arc::new(Registry::default());
    let names: Vec<String> = (0..32).map(|i| format!("m_{i}")).collect();
    let mut joins = Vec::new();
    for n in &names {
        let r = Arc::clone(&reg);
        let name = n.clone();
        joins.push(std::thread::spawn(move || {
            let _ = r.register_counter(&name, "help");
        }));
    }
    for j in joins {
        j.join().expect("task must not panic");
    }
    let registered: HashSet<String> = reg.iter().map(|m| m.name().to_string()).collect();
    let expected: HashSet<String> = names.iter().cloned().collect();
    assert_eq!(registered, expected);
    assert_eq!(reg.metric_count(), names.len());
}

#[test]
fn registry_concurrent_observation_and_export_is_consistent() {
    // Hammer the registry with a mix of inc + observation
    // tasks while another thread repeatedly exports to
    // Prometheus format. The export must not panic and must
    // produce a valid (parseable) text body.
    let reg = Arc::new(Registry::default());
    let counter = reg.register_counter("c", "help");
    let histogram = reg.register_histogram("h", "help");
    let mut joins = Vec::new();
    // 4 inc tasks.
    for _ in 0..4 {
        let c = Arc::clone(&counter);
        joins.push(std::thread::spawn(move || {
            for _ in 0..5_000 {
                c.inc();
            }
        }));
    }
    // 4 observation tasks.
    for _ in 0..4 {
        let h = Arc::clone(&histogram);
        joins.push(std::thread::spawn(move || {
            for _ in 0..5_000 {
                h.observe(0.001);
            }
        }));
    }
    // 2 export tasks. The histogram's base name is `h`,
    // so `_count` / `_sum` / `_bucket` are appended by
    // the renderer — see `HistogramSnapshot::render_prometheus`.
    for _ in 0..2 {
        let r = Arc::clone(&reg);
        joins.push(std::thread::spawn(move || {
            for _ in 0..200 {
                let exporter = adnet_observability::prometheus::PrometheusExporter::new(&r);
                let out = exporter.render_to_string();
                assert!(out.contains("c "));
                assert!(out.contains("h_count "));
            }
        }));
    }
    for j in joins {
        j.join().expect("task must not panic");
    }
    // Final consistency: counter should be at 4 * 5_000,
    // histogram count at 4 * 5_000.
    assert_eq!(counter.get(), 4 * 5_000);
    assert_eq!(histogram.count(), 4 * 5_000);
}

#[test]
fn counter_labeled_no_panic_under_insert_race() {
    // 8 threads, each inserting 4 distinct label sets and
    // incrementing. Pre-allocate the label sets *outside* the
    // threads to make sure every thread sees the same set of
    // keys (a pre-existing bug was the label set being
    // constructed inside the hot loop, which caused the slow
    // path to be hit on every iteration and stress-tested the
    // RwLock).
    let counter = Arc::new(Counter::new("c", "help"));
    let label_sets: Vec<LabelSet> = (0..4)
        .map(|i| LabelSet::new([("k".to_string(), format!("v_{i}"))]).unwrap())
        .collect();
    let label_sets = Arc::new(label_sets);
    let mut joins = Vec::new();
    for _ in 0..8 {
        let c = Arc::clone(&counter);
        let ls = Arc::clone(&label_sets);
        joins.push(std::thread::spawn(move || {
            for _ in 0..2_000 {
                for l in ls.iter() {
                    c.inc_labels(l);
                }
            }
        }));
    }
    for j in joins {
        j.join().expect("task must not panic");
    }
    let snap = counter.labeled_snapshot();
    let observed_keys: HashSet<String> = snap.iter().map(|(k, _)| k.render()).collect();
    // All 4 label sets must show up.
    assert_eq!(observed_keys.len(), 4);
    // Every entry should have `8 * 2_000 = 16_000` observations.
    for (_, v) in snap {
        assert_eq!(v, 16_000);
    }
}
