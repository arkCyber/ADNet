//! Prometheus text format exporter.
//!
//! Renders a [`Registry`] to the standard Prometheus text
//! exposition format (`text/plain; version=0.0.4`).
//!
//! ## Output format
//!
//! Each metric emits:
//!
//! ```text
//! # HELP <name> <help text>
//! # TYPE <name> <counter|gauge|histogram>
//! <name> <value>
//! <name>{<label>=<value>,...} <value>
//! ...
//! ```
//!
//! The format spec lives at
//! <https://prometheus.io/docs/instrumenting/exposition_formats/>
//! We follow the **0.0.4** text format (no protobuf, no
//! OpenMetrics). All ADNet metrics are exposed in 0.0.4
//! because that's what every Prometheus-compatible scraper
//! (Prometheus, VictoriaMetrics, Grafana Agent, Mimir) accepts.
//!
//! ## Sorting
//!
//! The exporter sorts metrics by name before emitting so the
//! output is deterministic — the snapshot tests rely on this
//! for byte-exact comparison.

use crate::registry::Registry;

/// Output of a [`PrometheusExporter::render`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrometheusOutput {
    /// The serialized text. Always UTF-8; line endings are LF.
    pub body: String,
    /// `Content-Type` header value suitable for an HTTP response.
    pub content_type: &'static str,
}

impl PrometheusOutput {
    /// Borrow the rendered text.
    pub fn text(&self) -> &str {
        &self.body
    }

    /// Into the inner text.
    pub fn into_string(self) -> String {
        self.body
    }
}

/// Render a [`Registry`] to Prometheus text format.
#[derive(Debug)]
pub struct PrometheusExporter<'a> {
    registry: &'a Registry,
}

impl<'a> PrometheusExporter<'a> {
    /// Construct an exporter over `registry`. Cheap; just borrows.
    pub fn new(registry: &'a Registry) -> Self {
        Self { registry }
    }

    /// Render the registry to a [`PrometheusOutput`]. The
    /// output is sorted by metric name so two calls with the
    /// same metric state produce byte-identical output
    /// (modulo label ordering inside the metric, which is
    /// also sorted — see [`LabelSet::new`](crate::labels::LabelSet::new)).
    pub fn render(&self) -> PrometheusOutput {
        let snapshot = self.registry.snapshot();
        let mut out = String::with_capacity(2048);
        for metric in snapshot.sorted() {
            // `Metric::render_prometheus` already includes the
            // `# HELP` and `# TYPE` lines plus the sample
            // lines, so we can just append.
            out.push_str(&metric.render_prometheus());
        }
        PrometheusOutput {
            body: out,
            content_type: "text/plain; version=0.0.4; charset=utf-8",
        }
    }

    /// Render the registry and write directly into a `String`.
    /// Equivalent to `self.render().into_string()` but avoids
    /// the intermediate `PrometheusOutput` allocation.
    pub fn render_to_string(&self) -> String {
        self.render().body
    }

    /// Render the registry into a pre-allocated `String` buffer.
    /// Useful for callers that already have a `String` ready
    /// (e.g. an HTTP response body being built up).
    pub fn render_into(&self, buf: &mut String) {
        let snapshot = self.registry.snapshot();
        for metric in snapshot.sorted() {
            buf.push_str(&metric.render_prometheus());
        }
    }
}

/// Helper: write a single sample line to `out` in Prometheus
/// format. Currently exposed for documentation / future use;
/// the in-tree exporters build the lines inline. Kept
/// `pub(crate)` to avoid the dead-code warning.
#[allow(dead_code)]
pub(crate) fn write_sample_line(out: &mut String, name: &str, labels_str: &str, value_str: &str) {
    // `name` is not validated here — registration enforces
    // it; the renderer trusts the registry.
    out.push_str(name);
    if !labels_str.is_empty() {
        out.push_str(labels_str);
    }
    out.push(' ');
    out.push_str(value_str);
    out.push('\n');
}

/// Format an integer counter / gauge value. The Prometheus text
/// format accepts plain integers for both `int` and `float`
/// metrics, so we use the same `Display` impl for everything
/// except f64. Kept `pub(crate)` to avoid the dead-code warning.
#[allow(dead_code)]
pub(crate) fn format_int_value(v: u64) -> String {
    v.to_string()
}

impl std::fmt::Display for PrometheusOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::LabelSet;
    use crate::metrics::Counter;

    #[test]
    fn render_empty_registry_produces_empty_body() {
        let reg = Registry::default();
        let out = PrometheusExporter::new(&reg).render();
        assert_eq!(out.text(), "");
        // Content type is still the Prometheus one.
        assert!(out.content_type.starts_with("text/plain"));
    }

    #[test]
    fn render_counter_emits_help_type_and_value() {
        let reg = Registry::default();
        let c = reg.register_counter("adnet_test_total", "test help");
        c.inc_by(7);
        let out = PrometheusExporter::new(&reg).render();
        let text = out.text();
        assert!(text.contains("# HELP adnet_test_total test help"));
        assert!(text.contains("# TYPE adnet_test_total counter"));
        assert!(text.contains("adnet_test_total 7"));
    }

    #[test]
    fn render_multiple_metrics_is_sorted() {
        let reg = Registry::default();
        let _ = reg.register_counter("zzz_total", "z");
        let _ = reg.register_counter("aaa_total", "a");
        let _ = reg.register_gauge("mmm", "m");
        let out = PrometheusExporter::new(&reg).render();
        let text = out.text();
        // Names must appear in sorted order: aaa, mmm, zzz.
        let a_pos = text.find("aaa_total").expect("aaa present");
        let m_pos = text.find("mmm ").expect("mmm present");
        let z_pos = text.find("zzz_total").expect("zzz present");
        assert!(a_pos < m_pos, "aaa must come before mmm");
        assert!(m_pos < z_pos, "mmm must come before zzz");
    }

    #[test]
    fn render_labeled_counter_emits_labels() {
        let reg = Registry::default();
        let c = reg.register_counter("c", "help");
        let l = LabelSet::new([("topic".into(), "lobby".into())]).unwrap();
        c.inc_labels(&l);
        c.inc_labels(&l);
        let out = PrometheusExporter::new(&reg).render();
        let text = out.text();
        assert!(text.contains(r#"c{topic="lobby"} 2"#));
    }

    #[test]
    fn render_histogram_emits_buckets_count_sum() {
        let reg = Registry::default();
        let h = reg.register_histogram("h_seconds", "test histogram");
        h.observe(0.05);
        h.observe(5.0);
        let out = PrometheusExporter::new(&reg).render();
        let text = out.text();
        assert!(text.contains("# TYPE h_seconds histogram"));
        assert!(text.contains(r#"h_seconds_bucket{le="0.1"} 1"#));
        assert!(text.contains(r#"h_seconds_bucket{le="+Inf"} 2"#));
        assert!(text.contains("h_seconds_count 2"));
        assert!(text.contains("h_seconds_sum 5.05"));
    }

    #[test]
    fn write_sample_line_handles_empty_labels() {
        let mut out = String::new();
        write_sample_line(&mut out, "n", "", "42");
        assert_eq!(out, "n 42\n");
    }

    #[test]
    fn write_sample_line_handles_nonempty_labels() {
        let mut out = String::new();
        write_sample_line(&mut out, "n", r#"{a="1"}"#, "42");
        assert_eq!(out, "n{a=\"1\"} 42\n");
    }

    #[test]
    fn format_int_value_basic() {
        assert_eq!(format_int_value(0), "0");
        assert_eq!(format_int_value(1234567890), "1234567890");
        assert_eq!(format_int_value(u64::MAX), u64::MAX.to_string());
    }

    #[test]
    fn render_to_string_matches_render() {
        let reg = Registry::default();
        let c: std::sync::Arc<Counter> = reg.register_counter("c", "help");
        c.inc();
        let exporter = PrometheusExporter::new(&reg);
        assert_eq!(exporter.render_to_string(), exporter.render().body);
    }

    #[test]
    fn render_into_appends_to_existing_buffer() {
        let reg = Registry::default();
        let _ = reg.register_counter("c", "help");
        let mut buf = String::from("prelude\n");
        PrometheusExporter::new(&reg).render_into(&mut buf);
        assert!(buf.starts_with("prelude\n"));
        assert!(buf.contains("c 0"));
    }
}
