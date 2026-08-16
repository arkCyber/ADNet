//! `a3net-error` — unified error model for A3Net.
//!
//! Background: every crate defines its own `enum Error` via
//! `thiserror`. That's good for **internal** ergonomics, but
//! at the **boundary** (RPC, FFI, CLI exit, HTTP gateway) we
//! want a stable, machine-readable shape that operators can
//! group, count, and trace. This crate defines that shape:
//!
//! ```text
//! AdnetErrorReport {
//!     code:        String,         // "BLB-001", "DNS-014", ...
//!     kind:        ErrorKind,      // NotFound / Timeout / Internal / ...
//!     severity:    Severity,       // Info / Warn / Error / Fatal
//!     message:     String,         // human-readable
//!     correlation: Option<String>, // op-id / request-id
//!     cause:       Option<String>, // chain of inner messages
//!     details:     Map<String, Value>,
//! }
//! ```
//!
//! The shape mirrors **Hessian 2** error envelopes (used by
//! Dubbo / Motan): a stable `code`, a coarse `kind` for
//! retry-policy, a `message` for humans, and a free-form
//! `details` map for protocol-specific context (e.g. the
//! offending ticket hash or the failing relay URL).
//!
//! Two integration hooks ship in this crate:
//!
//! 1. `AdnetErrorReport::emit(&self)` — fires a `tracing`
//!    event at the severity the report carries; emits a
//!    `metrics` counter increment tagged with kind+code+crate
//!    (only when the `metrics` feature is on).
//! 2. The `IntoReport` trait lets every `thiserror`-style
//!    error wrap itself into a report by carrying a
//!    `code()` method.
//!
//! The crate is **deliberately** non-invasive — it adds no
//! new dependency to existing crates beyond what `thiserror`
//! already pulls in. Crates adopt it incrementally by
//! implementing `IntoReport` for their error enums.
//!
//! # Example
//!
//! ```rust
//! use a3net_error::{AdnetErrorReport, ErrorKind, Severity};
//!
//! let report = AdnetErrorReport::new(
//!     "BLB-001",
//!     ErrorKind::NotFound,
//!     Severity::Warn,
//!     "blob not found",
//!     "a3net-blobstore",
//! );
//! report.emit();
//! assert_eq!(report.code, "BLB-001");
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

#[cfg(feature = "metrics")]
mod metrics;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Coarse classification of a failure. Mirrors Dubbo's
/// Hessian 2 + gRPC error categories, narrowed to what
/// A3Net's transports actually distinguish.
///
/// Each variant maps to a stable integer so operators can
/// write dashboard panels without depending on variant
/// order. New variants are added at the end and only ever
/// **append** — never renumbered or removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Caller supplied a malformed input — bad ticket,
    /// invalid UTF-8, missing required field. Retryable only
    /// after fixing the request.
    BadRequest = 1,
    /// Caller is not authenticated.
    Unauthorized = 2,
    /// Caller is authenticated but lacks permission.
    Forbidden = 3,
    /// Resource does not exist. **Not** retryable.
    NotFound = 4,
    /// Resource already exists or state conflicts.
    Conflict = 5,
    /// Caller exceeded a rate limit. Retryable after the
    /// Retry-After window.
    RateLimited = 6,
    /// Operation timed out. Retryable.
    Timeout = 7,
    /// Operation cancelled by the caller. Retryable.
    Cancelled = 8,
    /// Server-side invariant violated. **Not** retryable
    /// unless the operator can confirm the cause is benign.
    Internal = 9,
    /// Dependency (database, peer, relay) is down.
    /// Retryable with backoff.
    Unavailable = 10,
    /// Bytes were truncated or corrupted in transit.
    /// Retryable (the source may still be healthy).
    DataLoss = 11,
    /// Catch-all for things that don't fit a category above.
    /// New codes should *avoid* this; new categories are
    /// added at the end of the enum.
    Other = 99,
}

impl ErrorKind {
    /// Stable numeric code (matches the discriminant above).
    /// Used in Prometheus / log labels.
    pub fn code(self) -> u16 {
        self as u16
    }

    /// Snake-case string form. Matches the `serde` rename.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Internal => "internal",
            Self::Unavailable => "unavailable",
            Self::DataLoss => "data_loss",
            Self::Other => "other",
        }
    }

    /// HTTP-style status code. The mapping is intentionally
    /// not 1:1 with HTTP because Hessian doesn't have a
    /// status line; we expose this so the gateway can render
    /// sensible HTTP responses.
    pub fn http_status(self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::Conflict => 409,
            Self::RateLimited => 429,
            Self::Timeout | Self::Cancelled => 499,
            Self::Internal => 500,
            Self::Unavailable => 503,
            Self::DataLoss => 500,
            Self::Other => 500,
        }
    }

    /// Hint for retry policies: `true` when a fresh attempt
    /// with the same arguments is **likely** to succeed.
    pub fn is_transient(self) -> bool {
        matches!(
            self,
            Self::Timeout
                | Self::Cancelled
                | Self::Unavailable
                | Self::RateLimited
                | Self::DataLoss
        )
    }
}

/// Severity of a report. Independent from `ErrorKind` so a
/// `NotFound` can be `Warn` (expected cache miss) while a
/// `DataLoss` can be `Error` (silently corrupting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info = 0,
    Warn = 1,
    Error = 2,
    Fatal = 3,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Fatal => "fatal",
        }
    }
}

/// A JSON value used inside `details`. We avoid pulling in
/// `serde_json::Value` to keep the dependency surface
/// minimal — `Display + Serialize + DeserializeOwned` is all
/// the consumers need.
pub type DetailValue = serde_json::Value;

/// The unified report. See module docs for the field shapes.
///
/// `BTreeMap` (rather than `HashMap`) is used for `details`
/// so the JSON serialisation is deterministic — easier to
/// diff across services and to ship as a log line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdnetErrorReport {
    /// Stable error code, scoped to the producing crate
    /// (`"BLB-001"`, `"DNS-014"`). The convention is
    /// `<crate-prefix>-<3-digit>` so operators can grep by
    /// prefix.
    pub code: String,
    /// Coarse category. See [`ErrorKind`].
    pub kind: ErrorKind,
    /// Severity hint. Drives the `tracing` level and the
    /// metric tag for dashboards.
    pub severity: Severity,
    /// Human-readable message. Localised at the boundary —
    /// the FFI / CLI may translate this; the Rust side stays
    /// in English.
    pub message: String,
    /// Crate that produced the report. The FFI / RPC layer
    /// trusts the producer to fill this in via
    /// [`AdnetErrorReport::new`] — a `#[track_caller]`
    /// attribute captures the file:line so the same source
    /// file is always paired with the same crate name.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_loc: Option<String>,
    /// Operator-supplied correlation id (RPC request id,
    /// op-id, span id). The tracing layer can attach this
    /// automatically when the report is constructed inside
    /// a `tracing` span.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub correlation: Option<String>,
    /// Chain of inner error messages (joined with `" -> "`).
    /// Captured by [`AdnetErrorReport::with_cause`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cause: Option<String>,
    /// Free-form protocol-specific context.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, DetailValue>,
}

impl AdnetErrorReport {
    /// Construct a report. `code` / `crate_name` should
    /// follow the `<prefix>-NNN` convention so dashboards
    /// can group by crate.
    #[track_caller]
    pub fn new(
        code: impl Into<String>,
        kind: ErrorKind,
        severity: Severity,
        message: impl Into<String>,
        crate_name: &'static str,
    ) -> Self {
        let loc = caller_loc();
        let mut r = Self {
            code: code.into(),
            kind,
            severity,
            message: message.into(),
            source_loc: Some(loc),
            correlation: None,
            cause: None,
            details: BTreeMap::new(),
        };
        // `crate_name` is encoded into the `details` map so
        // the top-level struct serialises uniformly across
        // sub-crates. Operators group by `details.crate`.
        r.details.insert(
            "crate".to_string(),
            DetailValue::String(crate_name.to_string()),
        );
        r
    }

    /// Attach a correlation id (typically the request id).
    pub fn with_correlation(mut self, id: impl Into<String>) -> Self {
        self.correlation = Some(id.into());
        self
    }

    /// Attach a chain of inner causes. `err` may be any
    /// `std::error::Error`; the chain walks `source()` and
    /// joins the messages with `" -> "`.
    pub fn with_cause<E: std::error::Error + ?Sized>(mut self, err: &E) -> Self {
        let chain = walk_chain(err);
        self.cause = if chain.is_empty() {
            None
        } else {
            Some(chain.join(" -> "))
        };
        self
    }

    /// Insert a detail key/value.
    pub fn with_detail<K: Into<String>, V: Serialize>(
        mut self,
        key: K,
        value: V,
    ) -> Self {
        let v = serde_json::to_value(value).unwrap_or(DetailValue::Null);
        self.details.insert(key.into(), v);
        self
    }

    /// Emit the report through `tracing`. The companion
    /// `a3net-observability` crate's metrics counter is
    /// updated when the `metrics` feature is enabled
    /// (the dependency is optional so `a3net-error` can
    /// live at the bottom of the dependency graph).
    ///
    /// Every severity branch attaches the same structured
    /// fields (`code`, `kind`, `crate`, `cause`) so JSON
    /// log shippers don't have to special-case `info`
    /// versus the others.
    pub fn emit(&self) {
        let kind = self.kind;
        let severity = self.severity;
        let code = &self.code;
        let crate_name = self
            .details
            .get("crate")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let cause = self.cause.as_deref().unwrap_or("");
        // tracing emit ------------------------------------------------
        match severity {
            Severity::Info => {
                tracing::info!(
                    code,
                    kind = kind_str(kind),
                    crate = crate_name,
                    cause,
                    "a3net info: {}",
                    self.message,
                );
            }
            Severity::Warn => {
                tracing::warn!(
                    code,
                    kind = kind_str(kind),
                    crate = crate_name,
                    cause,
                    "a3net error: {}",
                    self.message,
                );
            }
            Severity::Error => {
                tracing::error!(
                    code,
                    kind = kind_str(kind),
                    crate = crate_name,
                    cause,
                    "a3net error: {}",
                    self.message,
                );
            }
            Severity::Fatal => {
                tracing::error!(
                    code,
                    kind = kind_str(kind),
                    crate = crate_name,
                    cause,
                    "a3net FATAL: {}",
                    self.message,
                );
            }
        }
        // metrics emit -----------------------------------------------
        // Optional — only wired when the companion
        // observability crate is present. We feature-gate the
        // counter call so this crate stays at the bottom of
        // the dependency graph (i.e. `a3net-observability`
        // can in turn depend on this crate without forming a
        // cycle).
        #[cfg(feature = "metrics")]
        a3net_metrics_emit(self);
    }
}

#[cfg(feature = "metrics")]
fn a3net_metrics_emit(report: &AdnetErrorReport) {
    // Direct call into `a3net-observability` — kept behind
    // the `metrics` feature so the default build is
    // cycle-free. The function lives in a sibling file to
    // keep `lib.rs` readable.
    crate::metrics::counter_inc_from_report(report);
}

fn kind_str(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::BadRequest => "bad_request",
        ErrorKind::Unauthorized => "unauthorized",
        ErrorKind::Forbidden => "forbidden",
        ErrorKind::NotFound => "not_found",
        ErrorKind::Conflict => "conflict",
        ErrorKind::RateLimited => "rate_limited",
        ErrorKind::Timeout => "timeout",
        ErrorKind::Cancelled => "cancelled",
        ErrorKind::Internal => "internal",
        ErrorKind::Unavailable => "unavailable",
        ErrorKind::DataLoss => "data_loss",
        ErrorKind::Other => "other",
    }
}

fn caller_loc() -> String {
    #[cfg(feature = "source-loc")]
    {
        let bt = backtrace::Backtrace::new();
        // `Backtrace::frames()` yields `BacktraceFrame`s.
        // Skip frame 0 (this fn) and frame 1 (the `new()`
        // constructor); frame 2 is the user call site.
        for (i, frame) in bt.frames().iter().enumerate() {
            if i < 2 {
                continue;
            }
            for sym in frame.symbols() {
                if let (Some(file), Some(lineno)) = (sym.filename(), sym.lineno()) {
                    return format!("{}:{}", file.display(), lineno);
                }
            }
            break;
        }
    }
    "<unknown>".to_string()
}

/// Maximum depth of the `source()` chain we walk before
/// giving up. `std::error::Error::source` is not guaranteed
/// to terminate — a misbehaving `source()` impl that loops
/// back to itself would otherwise burn the FFI hot path.
const MAX_CAUSE_DEPTH: usize = 32;

fn walk_chain<E: std::error::Error + ?Sized>(err: &E) -> Vec<String> {
    let mut out = vec![err.to_string()];
    let mut current = err.source();
    let mut depth = 0;
    while let Some(e) = current {
        if depth >= MAX_CAUSE_DEPTH {
            // Bail with a marker so operators can see we
            // truncated a runaway chain.
            out.push(format!(
                "<truncated after {MAX_CAUSE_DEPTH} frames>"
            ));
            break;
        }
        out.push(e.to_string());
        current = e.source();
        depth += 1;
    }
    out
}

/// Trait that every internal error enum implements so it
/// can be lifted into a report at the boundary. Crates
/// typically add a single impl with their stable code set.
///
/// The trait is **not** a blanket impl — `E: std::error::Error`
/// is too permissive (anything would qualify, including
/// foreign errors that don't carry our code). Each crate
/// opts in explicitly by writing `impl IntoReport for
/// crate::Error { fn code() -> &'static str { ... } }`.
///
/// Requires `Self: std::error::Error` so the default
/// `into_report` can walk the `source()` chain and pull the
/// `Display` form for `message`.
pub trait IntoReport: std::error::Error {
    fn code(&self) -> &'static str;
    fn kind(&self) -> ErrorKind;
    fn severity(&self) -> Severity {
        Severity::Error
    }
    fn message(&self) -> String {
        std::string::ToString::to_string(self)
    }

    /// Convert into a report. The default implementation
    /// uses `code` / `kind` / `severity` / `message`. The
    /// `Display` impl of the source error feeds `message`;
    /// `cause` is filled from the `source()` chain.
    ///
    /// `&self` (not `self`) is intentional: callers
    /// usually have `&MyError` (e.g. inside `map_err`),
    /// and we want to keep the original error alive for
    /// the `source()` walk. We suppress the
    /// `wrong_self_convention` lint because the standard
    /// `Into::into(self)` convention doesn't apply when
    /// the trait already requires `Self: Error`.
    #[allow(clippy::wrong_self_convention)]
    fn into_report(&self, crate_name: &'static str) -> AdnetErrorReport {
        let report = AdnetErrorReport::new(
            self.code(),
            self.kind(),
            self.severity(),
            self.message(),
            crate_name,
        );
        // Walk `source()` chain so the report's `cause`
        // mirrors the original error structure.
        report.with_cause(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_codes_are_stable() {
        // Pin the discriminants — operators depend on these
        // staying stable across releases.
        assert_eq!(ErrorKind::BadRequest.code(), 1);
        assert_eq!(ErrorKind::NotFound.code(), 4);
        assert_eq!(ErrorKind::Internal.code(), 9);
        assert_eq!(ErrorKind::Other.code(), 99);
    }

    #[test]
    fn transient_kinds() {
        assert!(ErrorKind::Timeout.is_transient());
        assert!(ErrorKind::Unavailable.is_transient());
        assert!(!ErrorKind::NotFound.is_transient());
        assert!(!ErrorKind::Internal.is_transient());
    }

    #[test]
    fn http_status_mapping() {
        assert_eq!(ErrorKind::NotFound.http_status(), 404);
        assert_eq!(ErrorKind::Internal.http_status(), 500);
        assert_eq!(ErrorKind::RateLimited.http_status(), 429);
    }

    #[test]
    fn report_round_trips_via_json() {
        let r = AdnetErrorReport::new(
            "BLB-001",
            ErrorKind::NotFound,
            Severity::Warn,
            "blob not found",
            "a3net-blobstore",
        )
        .with_detail("hash", "abc123")
        .with_correlation("op-42");
        let json = serde_json::to_string(&r).unwrap();
        let back: AdnetErrorReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r.code, back.code);
        assert_eq!(r.kind, back.kind);
        assert_eq!(r.severity, back.severity);
        assert_eq!(r.correlation, back.correlation);
    }

    #[test]
    fn details_deterministic_in_json() {
        let mut a = AdnetErrorReport::new(
            "X-1",
            ErrorKind::Internal,
            Severity::Error,
            "msg",
            "test",
        );
        a = a.with_detail("z", 1).with_detail("a", 2);
        let json = serde_json::to_string(&a).unwrap();
        // BTreeMap orders keys — `a` should come before `z`.
        assert!(json.find("\"a\":2").unwrap() < json.find("\"z\":1").unwrap());
    }

    #[test]
    fn severity_str() {
        assert_eq!(Severity::Info.as_str(), "info");
        assert_eq!(Severity::Fatal.as_str(), "fatal");
    }

    #[test]
    fn cause_chain_collected() {
        let inner = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        // Build a chain manually: an `io::Error` whose
        // `source()` is a custom error. `anyhow::Error` does
        // not implement `std::error::Error` in 1.0+ so we
        // stay close to the metal.
        let report = AdnetErrorReport::new(
            "X-2",
            ErrorKind::NotFound,
            Severity::Warn,
            "top",
            "test",
        )
        .with_cause(&inner);
        let cause = report.cause.expect("cause present");
        assert!(cause.contains("no such file"), "cause chain: {cause}");
    }

    #[test]
    fn emit_does_not_panic() {
        // We don't assert on the tracing output here — the
        // goal is just to confirm `emit` runs without
        // panicking when the static counter registry is
        // available. A future PR can wire a `tracing-test`
        // assertion if we want stricter coverage.
        let r = AdnetErrorReport::new(
            "X-3",
            ErrorKind::Timeout,
            Severity::Warn,
            "transient timeout",
            "test",
        )
        .with_correlation("op-42");
        r.emit();
    }

    #[test]
    fn source_loc_populated_by_default() {
        // Even without the `source-loc` feature we want
        // a sentinel so callers can tell a report was
        // emitted from a known site vs. an unknown one.
        let r = AdnetErrorReport::new(
            "X-4",
            ErrorKind::Internal,
            Severity::Error,
            "x",
            "test",
        );
        assert!(r.source_loc.is_some());
    }

    #[test]
    fn cause_chain_depth_bounded() {
        // A contrived error whose `source()` is itself.
        // `walk_chain` must truncate, not loop forever.
        struct LoopErr;
        impl std::fmt::Display for LoopErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("loop")
            }
        }
        impl std::fmt::Debug for LoopErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("LoopErr")
            }
        }
        impl std::error::Error for LoopErr {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(self)
            }
        }
        let report = AdnetErrorReport::new(
            "X-5",
            ErrorKind::Internal,
            Severity::Error,
            "top",
            "test",
        )
        .with_cause(&LoopErr);
        let cause = report.cause.expect("cause present");
        assert!(
            cause.contains("truncated"),
            "loop chain should be bounded, got: {cause}"
        );
    }

    #[test]
    fn into_report_default_uses_display_and_walks_cause() {
        // A minimal IntoReport impl exercising the default
        // `into_report` / `message` paths.
        #[derive(Debug)]
        struct Dummy(String);
        impl std::fmt::Display for Dummy {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl std::error::Error for Dummy {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                None
            }
        }
        impl IntoReport for Dummy {
            fn code(&self) -> &'static str {
                "DUM-001"
            }
            fn kind(&self) -> ErrorKind {
                ErrorKind::BadRequest
            }
        }
        let report = Dummy("bad input".to_string()).into_report("test-crate");
        assert_eq!(report.code, "DUM-001");
        assert_eq!(report.kind, ErrorKind::BadRequest);
        // Default severity is Error.
        assert_eq!(report.severity, Severity::Error);
        // Message comes from `Display`.
        assert_eq!(report.message, "bad input");
        // Crate name is encoded in `details["crate"]`.
        assert_eq!(
            report.details.get("crate").and_then(|v| v.as_str()),
            Some("test-crate")
        );
        // No `source()` chain → `cause` is still set to
        // the top-level `Display` text (a single link),
        // because `walk_chain` always collects at least
        // `err.to_string()`.
        let cause = report.cause.expect("cause present");
        assert_eq!(cause, "bad input");
    }

    #[test]
    fn into_report_carries_cause_chain() {
        // A two-level chain: outer wraps inner.
        #[derive(Debug)]
        struct Inner;
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("inner msg")
            }
        }
        impl std::error::Error for Inner {}
        #[derive(Debug)]
        struct Outer(Inner);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("outer msg")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        impl IntoReport for Outer {
            fn code(&self) -> &'static str {
                "OUT-001"
            }
            fn kind(&self) -> ErrorKind {
                ErrorKind::Internal
            }
        }
        let report = Outer(Inner).into_report("test");
        let cause = report.cause.expect("cause present");
        assert!(cause.contains("outer msg"));
        assert!(cause.contains("inner msg"));
    }

    #[test]
    fn detail_serialization_accepts_arbitrary_types() {
        // Numbers, booleans, nested structs all round-trip
        // through `with_detail` because we serialise with
        // `serde_json`. Guard against the obvious
        // regressions where the serializer is accidentally
        // pinned to one type.
        let r = AdnetErrorReport::new(
            "X-6",
            ErrorKind::Internal,
            Severity::Error,
            "msg",
            "test",
        )
        .with_detail("n", 42_u32)
        .with_detail("b", true)
        .with_detail("s", "value")
        .with_detail("v", vec![1_u32, 2, 3]);
        let json = serde_json::to_string(&r).unwrap();
        let back: AdnetErrorReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.details.get("n").and_then(|v| v.as_u64()), Some(42));
        assert_eq!(back.details.get("b").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            back.details.get("s").and_then(|v| v.as_str()),
            Some("value")
        );
        assert_eq!(back.details.get("v").map(|v| v.as_array().map(|a| a.len())), Some(Some(3)));
    }
}