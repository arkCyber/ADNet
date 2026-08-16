//! Type-safe labels for metrics.
//!
//! Prometheus labels are `(name, value)` pairs attached to a metric
//! instance. A3Net restricts labels to a small, pre-registered
//! label set per metric to bound cardinality (the V8 lesson
//! learned from `DiscoveryDiagnostics::MAX_PROVENANCE_BUCKETS`).
//!
//! ## Design
//!
//! - [`Label`] is a single `(name, value)` pair. Cheap to clone.
//! - [`LabelSet`] is an **ordered** `Vec<Label>` used as the
//!   key for a labelled metric variant. Two `LabelSet`s are equal
//!   iff they have the same labels in the same order. The order
//!   invariant lets us skip a `HashMap<String, String>` inside the
//!   hot path — `PartialEq` is a linear scan over typically <8
//!   labels, which is faster than a hash for tiny collections.
//! - Label *names* must be valid Prometheus identifier characters
//!   (regex `[a-zA-Z_][a-zA-Z0-9_]*`). `Label::new` enforces this
//!   by returning `None` for malformed input. Callers that need a
//!   hard error use [`Label::new_checked`].
//!
//! [`Label`]: crate::labels::Label
//! [`LabelSet`]: crate::labels::LabelSet

use serde::{Deserialize, Serialize};

/// Single label `(name, value)` pair.
///
/// `value` is a UTF-8 string. Empty values are allowed (Prometheus
/// permits them) but discouraged.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    pub value: String,
}

impl Label {
    /// Construct a label. Returns `None` if `name` is not a valid
    /// Prometheus identifier.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Option<Self> {
        let name = name.into();
        if !is_valid_label_name(&name) {
            return None;
        }
        Some(Self {
            name,
            value: value.into(),
        })
    }

    /// Like [`Label::new`] but returns a typed error so the caller
    /// can surface a structured diagnostic.
    pub fn new_checked(
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, LabelError> {
        let name = name.into();
        if !is_valid_label_name(&name) {
            return Err(LabelError::InvalidName(name));
        }
        Ok(Self {
            name,
            value: value.into(),
        })
    }
}

/// Ordered, deduplicated set of labels.
///
/// `LabelSet` is the key for a labelled metric variant (one
/// counter value per `(metric_name, label_set)` tuple). Order is
/// significant: two `LabelSet`s are equal iff the labels are in
/// the same order. This is enforced by [`LabelSet::new`] which
/// sorts the input on construction.
///
/// `LabelSet` is **deduplicated**: if the input contains two
/// labels with the same name, the **later** one wins (consistent
/// with how `tracing` handles duplicate fields). `Vec::dedup_by`
/// keeps the first occurrence, so we have to filter manually.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct LabelSet {
    inner: Vec<Label>,
}

impl LabelSet {
    /// Empty label set. Equivalent to a metric with no labels.
    pub const EMPTY: LabelSet = LabelSet { inner: Vec::new() };

    /// Build a `LabelSet` from a `(name, value)` list. Order is
    /// normalised (sorted by name); duplicates are dropped
    /// (last-wins). Names that are not valid Prometheus
    /// identifiers cause the entire call to return `Err`.
    pub fn new(labels: impl IntoIterator<Item = (String, String)>) -> Result<Self, LabelError> {
        let mut labels: Vec<Label> = labels
            .into_iter()
            .map(|(name, value)| Label::new_checked(name, value))
            .collect::<Result<_, _>>()?;
        labels.sort_by(|a, b| a.name.cmp(&b.name));
        // Last-wins dedup: walk from the end backwards,
        // dropping any earlier label whose name matches a
        // later one. After dedup we re-sort to keep the
        // order invariant.
        labels = dedup_last_wins(labels);
        Ok(Self { inner: labels })
    }

    /// Borrow the underlying labels as a slice.
    pub fn as_slice(&self) -> &[Label] {
        &self.inner
    }

    /// Number of labels.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// True when the set is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Render as the Prometheus `{name="value",name2="value2"}`
    /// string. Returns an empty string when the set is empty.
    ///
    /// Note: when the set is non-empty, the rendered form
    /// **always** starts with `{` and ends with `}` — there is
    /// no way to produce `{...}` without the curly braces.
    /// Callers that want to splice label content into a
    /// histogram bucket line should use the bare content
    /// (`{name="value"}`) and wrap it themselves — see
    /// [`HistogramSnapshot::render_prometheus`](crate::histogram::HistogramSnapshot::render_prometheus).
    pub fn render(&self) -> String {
        if self.inner.is_empty() {
            return String::new();
        }
        let mut out = String::with_capacity(64);
        out.push('{');
        for (i, l) in self.inner.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            // Per the Prometheus exposition format spec
            // (https://prometheus.io/docs/instrumenting/exposition_formats/),
            // labels are rendered as `name="value"`. The
            // name is **not** quoted — it's a bare
            // identifier — only the value is quoted.
            out.push_str(&l.name);
            out.push('=');
            out.push('"');
            escape_label_value(&mut out, &l.value);
            out.push('"');
        }
        out.push('}');
        out
    }

    /// Render the *inner* label content (`name="value",...`)
    /// without the surrounding `{` / `}`. Used by the
    /// histogram bucket exporter to splice extra labels
    /// (`le=...`) before the closing brace.
    pub fn render_inner(&self) -> String {
        let mut out = String::with_capacity(64);
        for (i, l) in self.inner.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&l.name);
            out.push('=');
            out.push('"');
            escape_label_value(&mut out, &l.value);
            out.push('"');
        }
        out
    }
}

/// Error returned when a label name is malformed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LabelError {
    #[error("invalid Prometheus label name: {0:?} (must match [a-zA-Z_][a-zA-Z0-9_]*)")]
    InvalidName(String),
}

/// Cheap syntactic check: `[a-zA-Z_][a-zA-Z0-9_]*`.
///
/// A3Net restricts label cardinality to keep the in-process
/// registry from growing without bound; an upstream that emits
/// `topic={random-uuid}` as a label would slowly OOM the metrics
/// surface. Names that are not valid Prometheus identifiers are
/// rejected at construction time so the cardinality limit is
/// structural, not a runtime cap.
pub(crate) fn is_valid_label_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
    }
    true
}

/// Escape a label value per the Prometheus text format spec.
/// Quotes the value (the caller is responsible for emitting the
/// surrounding `"`) and replaces `\\`, `\n`, and `"` with their
/// backslash-escaped form.
pub(crate) fn escape_label_value(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '\n' => out.push_str(r"\n"),
            '"' => out.push_str(r#"\""#),
            other => out.push(other),
        }
    }
}

/// Drop earlier labels whose name is shadowed by a later one,
/// preserving sort order. Equivalent to `vec.dedup_by(|a, b|
/// a.name == b.name)` **with last-wins semantics**.
/// `Vec::dedup_by` keeps the *first* occurrence, but the
/// documented contract of `LabelSet::new` is last-wins, so we
/// filter manually.
fn dedup_last_wins(mut labels: Vec<Label>) -> Vec<Label> {
    // Walk in reverse, recording the set of names we've
    // already seen; drop any earlier label whose name is
    // already in that set.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut kept: Vec<Label> = Vec::with_capacity(labels.len());
    while let Some(l) = labels.pop() {
        if seen.insert(l.name.clone()) {
            kept.push(l);
        }
    }
    kept.reverse();
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_label_names_accepted() {
        for name in [
            "ok",
            "_ok",
            "transport",
            "topic_id",
            "A1",
            "_",
            "a_b_c_d_e_f_g_h_i_j",
        ] {
            let l = Label::new(name, "v").unwrap_or_else(|| panic!("{name} should be valid"));
            assert_eq!(l.name, name);
        }
    }

    #[test]
    fn invalid_label_names_rejected() {
        for name in ["", "1abc", "a-b", "a b", "a.b", "a:b", "héllo"] {
            assert!(
                Label::new(name, "v").is_none(),
                "{name:?} should be rejected"
            );
        }
    }

    #[test]
    fn label_set_sorts_and_dedupes() {
        let s = LabelSet::new([
            ("beta".into(), "2".into()),
            ("alpha".into(), "1".into()),
            ("alpha".into(), "1-dup".into()),
        ])
        .expect("valid names");
        // Order: alpha, beta. Duplicate dropped (last-wins: "1-dup").
        assert_eq!(s.as_slice().len(), 2);
        assert_eq!(s.as_slice()[0].name, "alpha");
        assert_eq!(s.as_slice()[0].value, "1-dup");
        assert_eq!(s.as_slice()[1].name, "beta");
        assert_eq!(s.as_slice()[1].value, "2");
    }

    #[test]
    fn empty_label_set_renders_empty_string() {
        assert_eq!(LabelSet::default().render(), "");
        assert_eq!(LabelSet::EMPTY.render(), "");
    }

    #[test]
    fn non_empty_label_set_renders_prometheus_form() {
        let s = LabelSet::new([
            ("topic".into(), "lobby".into()),
            ("kind".into(), "in".into()),
        ])
        .unwrap();
        // Per the Prometheus exposition format spec, label
        // names are bare identifiers (no quotes), and the
        // `=` sits between two literal `"` characters that
        // delimit the value.
        assert_eq!(s.render(), r#"{kind="in",topic="lobby"}"#);
    }

    #[test]
    fn label_value_escaping() {
        // Input string contains one backslash, one
        // newline, and one quote — characters that must
        // be escaped per the Prometheus text format spec.
        // The Rust literal `"a\\b\nc\"d"` decodes to the
        // 10-char sequence `a \ \ b \ n c \ " d`.
        let input = "a\\b\nc\"d";
        let s = LabelSet::new([("msg".into(), input.to_string())]).unwrap();
        // Escape rules: backslash → `\\`, newline → `\n`,
        // quote → `\"`. The name `msg` is bare (no quotes);
        // the value gets its backslashes doubled.
        assert_eq!(s.render(), r#"{msg="a\\b\nc\"d"}"#);
    }

    #[test]
    fn invalid_label_name_in_set_fails_atomically() {
        let err =
            LabelSet::new([("ok".into(), "v".into()), ("1bad".into(), "v".into())]).unwrap_err();
        assert_eq!(err, LabelError::InvalidName("1bad".into()));
    }
}
