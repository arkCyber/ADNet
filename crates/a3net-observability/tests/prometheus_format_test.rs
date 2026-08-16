//! Snapshot tests for the Prometheus text format output.
//!
//! These tests pin the byte-exact output of
//! [`PrometheusExporter::render`] for a known metric state.
//! Any change to the format (e.g. label ordering, bucket
//! rendering, value formatting) will break these tests — that
//! is intentional. The text format is a wire contract; a
//! silent change would surprise downstream scrapers.

use a3net_observability::labels::LabelSet;
use a3net_observability::prometheus::PrometheusExporter;
use a3net_observability::registry::Registry;

#[test]
fn snapshot_counter_only() {
    let reg = Registry::default();
    let c = reg.register_counter("a3net_test_total", "test counter");
    c.inc_by(42);
    let out = PrometheusExporter::new(&reg).render_to_string();
    let expected = "\
# HELP a3net_test_total test counter
# TYPE a3net_test_total counter
a3net_test_total 42
";
    assert_eq!(out, expected);
}

#[test]
fn snapshot_counter_with_labels() {
    let reg = Registry::default();
    let c = reg.register_counter("a3net_req_total", "requests");
    let l1 = LabelSet::new([("topic".into(), "lobby".into())]).unwrap();
    let l2 = LabelSet::new([("topic".into(), "files".into())]).unwrap();
    c.inc_by(10);
    c.inc_labels(&l1);
    c.inc_labels(&l1);
    c.inc_labels(&l2);
    let out = PrometheusExporter::new(&reg).render_to_string();
    let expected = "\
# HELP a3net_req_total requests
# TYPE a3net_req_total counter
a3net_req_total 10
a3net_req_total{topic=\"files\"} 1
a3net_req_total{topic=\"lobby\"} 2
";
    // Labeled variants are sorted by name (same name -> insertion
    // order); LabelSet sorts labels by name too, so the
    // rendered order is stable.
    assert_eq!(out, expected);
}

#[test]
fn snapshot_gauge() {
    let reg = Registry::default();
    let g = reg.register_gauge("a3net_g", "test gauge");
    g.set(-5);
    let out = PrometheusExporter::new(&reg).render_to_string();
    let expected = "\
# HELP a3net_g test gauge
# TYPE a3net_g gauge
a3net_g -5
";
    assert_eq!(out, expected);
}

#[test]
fn snapshot_histogram_basic() {
    let reg = Registry::default();
    let h = reg.register_histogram("a3net_h_seconds", "test histogram");
    h.observe(0.0005);
    h.observe(0.05);
    h.observe(5.0);
    let out = PrometheusExporter::new(&reg).render_to_string();
    // Cumulative bucket counts:
    //   0.001: 1   (0.0005 only)
    //   0.01:  1
    //   0.1:   2   (0.0005, 0.05)
    //   0.5:   2
    //   1:     2
    //   5:     3
    //   10:    3
    //   +Inf:  3
    let expected = "\
# HELP a3net_h_seconds test histogram
# TYPE a3net_h_seconds histogram
a3net_h_seconds_bucket{le=\"0.001\"} 1
a3net_h_seconds_bucket{le=\"0.01\"} 1
a3net_h_seconds_bucket{le=\"0.1\"} 2
a3net_h_seconds_bucket{le=\"0.5\"} 2
a3net_h_seconds_bucket{le=\"1\"} 2
a3net_h_seconds_bucket{le=\"5\"} 3
a3net_h_seconds_bucket{le=\"10\"} 3
a3net_h_seconds_bucket{le=\"+Inf\"} 3
a3net_h_seconds_count 3
a3net_h_seconds_sum 5.0505
";
    assert_eq!(out, expected);
}

#[test]
fn snapshot_histogram_with_labels() {
    let reg = Registry::default();
    let h = reg.register_histogram("a3net_h_seconds", "test");
    let l = LabelSet::new([("topic".into(), "lobby".into())]).unwrap();
    h.observe_labels(&l, 0.5);
    let out = PrometheusExporter::new(&reg).render_to_string();
    // Both unlabeled and labeled variants are emitted. The
    // unlabeled one is all-zero because no `observe` was
    // called on it.
    let expected = "\
# HELP a3net_h_seconds test
# TYPE a3net_h_seconds histogram
a3net_h_seconds_bucket{le=\"0.001\"} 0
a3net_h_seconds_bucket{le=\"0.01\"} 0
a3net_h_seconds_bucket{le=\"0.1\"} 0
a3net_h_seconds_bucket{le=\"0.5\"} 0
a3net_h_seconds_bucket{le=\"1\"} 0
a3net_h_seconds_bucket{le=\"5\"} 0
a3net_h_seconds_bucket{le=\"10\"} 0
a3net_h_seconds_bucket{le=\"+Inf\"} 0
a3net_h_seconds_count 0
a3net_h_seconds_sum 0
a3net_h_seconds_bucket{topic=\"lobby\",le=\"0.001\"} 0
a3net_h_seconds_bucket{topic=\"lobby\",le=\"0.01\"} 0
a3net_h_seconds_bucket{topic=\"lobby\",le=\"0.1\"} 0
a3net_h_seconds_bucket{topic=\"lobby\",le=\"0.5\"} 1
a3net_h_seconds_bucket{topic=\"lobby\",le=\"1\"} 1
a3net_h_seconds_bucket{topic=\"lobby\",le=\"5\"} 1
a3net_h_seconds_bucket{topic=\"lobby\",le=\"10\"} 1
a3net_h_seconds_bucket{topic=\"lobby\",le=\"+Inf\"} 1
a3net_h_seconds_count{topic=\"lobby\"} 1
a3net_h_seconds_sum{topic=\"lobby\"} 0.5
";
    assert_eq!(out, expected);
}

#[test]
fn snapshot_multiple_kinds_sorted_by_name() {
    let reg = Registry::default();
    let g = reg.register_gauge("zzz", "z");
    let c = reg.register_counter("aaa", "a");
    let h = reg.register_histogram("mmm", "m");
    c.inc();
    g.set(3);
    h.observe(0.01);
    let out = PrometheusExporter::new(&reg).render_to_string();
    let a_pos = out.find("aaa ").expect("aaa present");
    let m_pos = out.find("mmm ").expect("mmm present");
    let z_pos = out.find("zzz ").expect("zzz present");
    assert!(a_pos < m_pos);
    assert!(m_pos < z_pos);
    // The HELP / TYPE lines are colocated with each metric's
    // first sample line.
    assert!(out.contains("# TYPE aaa counter"));
    assert!(out.contains("# TYPE mmm histogram"));
    assert!(out.contains("# TYPE zzz gauge"));
}

#[test]
fn snapshot_label_value_with_special_chars_is_escaped() {
    let reg = Registry::default();
    let c = reg.register_counter("c", "help");
    let l = LabelSet::new([("msg".into(), "a\\b\nc\"d".into())]).unwrap();
    c.inc_labels(&l);
    let out = PrometheusExporter::new(&reg).render_to_string();
    // Input is `a \ \ b \ n c \ " d` (10 chars). The
    // escape function doubles the backslashes and
    // prefixes `n` and `"` with a backslash, yielding
    // `a \ \ \ \ b \ n c \ \ " d` (15 chars).
    assert!(out.contains(r#"c{msg="a\\b\nc\"d"} 1"#));
}

#[test]
fn snapshot_empty_registry_renders_empty_string() {
    let reg = Registry::default();
    let out = PrometheusExporter::new(&reg).render_to_string();
    assert_eq!(out, "");
}

#[test]
fn snapshot_is_deterministic_across_calls() {
    // Two consecutive `render` calls with the same metric
    // state must produce byte-identical output.
    let reg = Registry::default();
    let c = reg.register_counter("c", "help");
    c.inc_by(7);
    let l = LabelSet::new([("k".into(), "v".into())]).unwrap();
    c.inc_labels(&l);
    let exporter = PrometheusExporter::new(&reg);
    let a = exporter.render_to_string();
    let b = exporter.render_to_string();
    assert_eq!(a, b);
}

#[test]
fn snapshot_handles_histogram_zero_observations() {
    // A histogram that has never been observed must still
    // emit all bucket lines, all zero, plus _count 0 and
    // _sum 0. This is the "no data" state.
    let reg = Registry::default();
    let _h = reg.register_histogram("h", "help");
    let out = PrometheusExporter::new(&reg).render_to_string();
    assert!(out.contains("h_count 0"));
    assert!(out.contains("h_sum 0"));
    // 8 bucket lines, all zero.
    for le in &["0.001", "0.01", "0.1", "0.5", "1", "5", "10", "+Inf"] {
        assert!(
            out.contains(&format!("h_bucket{{le=\"{le}\"}} 0")),
            "missing zero bucket for le={le}: {out}"
        );
    }
}
