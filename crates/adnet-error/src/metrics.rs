//! Optional metrics emission for `AdnetErrorReport`.
//!
//! Lives behind the `metrics` feature so the default build
//! of `adnet-error` is cycle-free: `adnet-observability`
//! can depend on `adnet-error` without forming a cycle.
//!
//! When the feature is enabled, [`counter_inc_from_report`]
//! bumps the `adnet_error_total` Prometheus counter with
//! labels `(code, kind, crate, severity)`.

use std::sync::OnceLock;

use adnet_observability::metrics::Counter;
use adnet_observability::registry::GLOBAL;

use crate::AdnetErrorReport;

/// Lazily-registered handle to the global
/// `adnet_error_total` counter.
///
/// `adnet-observability::registry::GLOBAL.register_counter`
/// panics on duplicate names, so we cannot call it on every
/// `emit()`. The `OnceLock` ensures the counter is
/// registered exactly once per process; subsequent calls
/// only carry the `Arc` out. We hold the `Arc<Counter>`
/// (the public, clone-able handle returned by
/// `register_counter`) rather than the bare `Counter`
/// because `Counter` is not `Clone`.
static ERROR_TOTAL: OnceLock<std::sync::Arc<Counter>> = OnceLock::new();

fn error_total() -> std::sync::Arc<Counter> {
    ERROR_TOTAL
        .get_or_init(|| {
            GLOBAL.register_counter(
                "adnet_error_total",
                "Total ADNet errors emitted via AdnetErrorReport::emit",
            )
        })
        .clone()
}

/// Bump the `adnet_error_total` counter. Idempotent — the
/// underlying `Registry` deduplicates by name so multiple
/// calls with the same labels go through the same counter.
///
/// `LabelSet::new` can fail if a label name is empty or the
/// set is over the size limit. We log a `warn!` in that
/// case so operators can see when an error report is being
/// dropped from the metric stream (rather than silently
/// vanishing).
pub fn counter_inc_from_report(report: &AdnetErrorReport) {
    use adnet_observability::labels::LabelSet;

    let pairs: Vec<(String, String)> = Vec::with_capacity(4)
        .into_iter()
        .chain(std::iter::once((
            "code".to_string(),
            report.code.clone(),
        )))
        .chain(std::iter::once((
            "kind".to_string(),
            report.kind.as_str().to_string(),
        )))
        .chain(std::iter::once((
            "crate".to_string(),
            report
                .details
                .get("crate")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
        )))
        .chain(std::iter::once((
            "severity".to_string(),
            report.severity.as_str().to_string(),
        )))
        .collect();

    match LabelSet::new(pairs) {
        Ok(labels) => error_total().inc_labels(&labels),
        Err(e) => {
            // Drop the report from the metric stream rather
            // than crash — observability is best-effort. We
            // still surface the failure so a misconfigured
            // label set doesn't go unnoticed.
            tracing::warn!(
                error = %e,
                code = %report.code,
                "adnet_error_total: failed to build LabelSet; counter not incremented",
            );
        }
    }
}

/// Test-only helper. Returns the value of the
/// `adnet_error_total` counter for the supplied labels, or
/// `0` if the counter has not been registered yet. Lets the
/// `metrics` feature tests assert that `emit()` actually
/// bumped the counter instead of just confirming it did not
/// panic.
#[cfg(test)]
pub fn read_counter(code: &str, kind: &str, crate_name: &str, severity: &str) -> u64 {
    use adnet_observability::labels::LabelSet;

    let pairs = vec![
        ("code".to_string(), code.to_string()),
        ("kind".to_string(), kind.to_string()),
        ("crate".to_string(), crate_name.to_string()),
        ("severity".to_string(), severity.to_string()),
    ];
    let Ok(labels) = LabelSet::new(pairs) else {
        return 0;
    };
    error_total()
        .labeled_snapshot()
        .iter()
        .find(|(l, _)| l.as_slice() == labels.as_slice())
        .map(|(_, v)| *v)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorKind, Severity};

    #[test]
    fn emit_bumps_counter_visible_via_read_counter() {
        // Snapshot the counter values before this report to
        // avoid coupling to other tests in the suite that
        // also call `emit()`. We pick a unique code so the
        // lookup is unambiguous.
        let code = "TST-METRIC-001";
        let before = read_counter(code, "not_found", "test-metric", "warn");

        let report = AdnetErrorReport::new(
            code,
            ErrorKind::NotFound,
            Severity::Warn,
            "test emit",
            "test-metric",
        );
        report.emit();

        let after = read_counter(code, "not_found", "test-metric", "warn");
        assert_eq!(
            after,
            before + 1,
            "counter should have advanced by exactly 1 (before={before}, after={after})"
        );
    }

    #[test]
    fn read_counter_returns_zero_for_unknown_labels() {
        // A code we never emit should not have a label set
        // in the registry — `read_counter` returns 0.
        let v = read_counter("TST-METRIC-NEVER", "other", "x", "info");
        assert_eq!(v, 0);
    }

    #[test]
    fn emit_is_idempotent_under_repeated_calls() {
        // `register_counter` panics on duplicate names, so
        // the most basic property we need is that `emit()`
        // does not re-register. Three back-to-back calls
        // exercise this without crashing.
        let code = "TST-METRIC-IDEMPOTENT";
        let before = read_counter(code, "timeout", "test-metric", "warn");
        for _ in 0..3 {
            let r = AdnetErrorReport::new(
                code,
                ErrorKind::Timeout,
                Severity::Warn,
                "idempotent",
                "test-metric",
            );
            r.emit();
        }
        let after = read_counter(code, "timeout", "test-metric", "warn");
        assert_eq!(after, before + 3);
    }
}
