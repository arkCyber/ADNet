//! Error types for the hole-punch planner.
//!
//! Two layers:
//!
//! - [`HolePunchError`] — the public error surface. Mapped to
//!   `anyhow::Error` at the boundary via `From<HolePunchError> for
//!   anyhow::Error` so callers can `?` the planner.
//! - Internal fallible functions bubble up one of the variants
//!   below; the planner accepts any `Result<_, HolePunchError>` from
//!   a [`HolePunchResolver`](super::strategy::HolePunchResolver).
//!
//! All errors carry enough context for the [`HolePunchOutcome`]
//! diagnostics to surface the failure reason without a separate
//! "why" handler.
//!
//! [`HolePunchOutcome`]: super::diagnostics::HolePunchOutcome

use std::time::Duration;

use a3net_types::NodeId;
use serde::{Deserialize, Serialize};

use thiserror::Error;

/// Result alias used by the planner.
pub type HolePunchResult<T> = Result<T, HolePunchError>;

/// Errors raised by the planner or any [`HolePunchResolver`].
///
/// The variants are intentionally coarse — we want operators to
/// tell noise (cancelled, timeout) from real failures (network,
/// wire-format). Sub-categorisation (e.g. "DNS NXDOMAIN" vs "DNS
/// refused") belongs in the per-strategy telemetry, not in the
/// top-level error.
///
/// [`HolePunchResolver`]: super::strategy::HolePunchResolver
#[derive(Debug, Clone, Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HolePunchError {
    /// The planner was cancelled (`AbortHandle::abort`) before any
    /// strategy produced a result. This is the **expected** path
    /// when a winning strategy surfaces while siblings are still
    /// running — they are cancelled, not failed.
    #[error("hole-punch attempt cancelled")]
    Cancelled,

    /// All configured strategies exhausted the operator-supplied
    /// budget without resolving a non-empty address. The carried
    /// `Duration` is the actual elapsed time so the operator can
    /// decide whether to retry with a larger budget.
    #[error("all hole-punch strategies exhausted after {elapsed:?}")]
    StrategiesExhausted {
        elapsed: Duration,
        /// Total number of strategies that ran.
        attempted: usize,
    },

    /// The supplied [`NodeId`] is invalid (e.g. wrong length for
    /// the target's underlying crypto curve). The planner surfaces
    /// this **before** spawning any strategy so the caller doesn't
    /// pay for a timeout round before learning the input is bogus.
    #[error("invalid target node id: {0}")]
    InvalidNodeId(String),

    /// The underlying resolver (e.g. DHT, Pkarr relay) returned a
    /// transient I/O error. The message is the inner library's
    /// `Display`, untouched, so operators can grep their existing
    /// pkarr / DNS error logs.
    #[error("resolver I/O error: {0}")]
    ResolverIo(String),

    /// The resolver returned a result that failed wire-format
    /// validation (e.g. a Pkarr packet with an invalid signature,
    /// a ticket whose `node_id` doesn't match the target). This is
    /// treated as a **failure** rather than a "not found" because
    /// bytes that look like addressing data but cannot be decoded
    /// are usually a security-critical signal (mis-routed or
    /// tampered relay).
    #[error("resolver returned invalid wire-format data: {0}")]
    InvalidWireFormat(String),

    /// The caller's configuration is self-contradictory (e.g. zero
    /// strategies, negative timeout). The planner catches this at
    /// `HolePunchPlanner::new` time so the error lands before any
    /// tokio task is spawned.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// Catch-all for unexpected internal failures. The planner
    /// wraps these in `Internal` rather than letting them bubble
    /// to the caller so the public error surface stays stable.
    #[error("internal: {0}")]
    Internal(String),
}

impl HolePunchError {
    /// Convenience: build a `StrategiesExhausted` error from the
    /// elapsed time and the number of strategies that ran.
    pub fn exhausted(elapsed: Duration, attempted: usize) -> Self {
        Self::StrategiesExhausted { elapsed, attempted }
    }

    /// True when this error is the "expected" cancellation path —
    /// sibling strategies should treat it as a non-error.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, HolePunchError::Cancelled)
    }

    /// True when this error represents a hard failure (network /
    /// wire-format / config) that should be surfaced to the
    /// operator. The opposite of `is_cancelled`.
    pub fn is_hard_failure(&self) -> bool {
        !self.is_cancelled()
    }

    /// Light classification used by the [`HolePunchOutcome`]
    /// snapshot. Keeps the public `String` short so the
    /// `/discovery` JSON stays readable.
    ///
    /// [`HolePunchOutcome`]: super::diagnostics::HolePunchOutcome
    pub fn classify(&self) -> &'static str {
        match self {
            HolePunchError::Cancelled => "cancelled",
            HolePunchError::StrategiesExhausted { .. } => "exhausted",
            HolePunchError::InvalidNodeId(_) => "invalid_node_id",
            HolePunchError::ResolverIo(_) => "resolver_io",
            HolePunchError::InvalidWireFormat(_) => "invalid_wire_format",
            HolePunchError::InvalidConfig(_) => "invalid_config",
            HolePunchError::Internal(_) => "internal",
        }
    }
}

/// Helper: derive a [`HolePunchError::InvalidNodeId`] from a
/// `NodeId` whose bytes don't fit the target's curve. Centralised
/// so the error message stays consistent across the planner and
/// downstream callers.
pub fn invalid_node_id(node_id: NodeId, why: impl std::fmt::Display) -> HolePunchError {
    HolePunchError::InvalidNodeId(format!("{node_id}: {why}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_not_a_hard_failure() {
        let e = HolePunchError::Cancelled;
        assert!(e.is_cancelled());
        assert!(!e.is_hard_failure());
    }

    #[test]
    fn io_is_a_hard_failure_and_classifies() {
        let e = HolePunchError::ResolverIo("dns timeout".into());
        assert!(e.is_hard_failure());
        assert_eq!(e.classify(), "resolver_io");
    }

    #[test]
    fn exhausted_error_classifies_and_carries_count() {
        let e = HolePunchError::exhausted(Duration::from_millis(1500), 3);
        assert!(e.is_hard_failure());
        assert_eq!(e.classify(), "exhausted");
        match e {
            HolePunchError::StrategiesExhausted { elapsed, attempted } => {
                assert_eq!(elapsed, Duration::from_millis(1500));
                assert_eq!(attempted, 3);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn invalid_config_is_hard() {
        let e = HolePunchError::InvalidConfig("no strategies".into());
        assert!(e.is_hard_failure());
        assert_eq!(e.classify(), "invalid_config");
    }

    #[test]
    fn invalid_node_id_helper() {
        let id = NodeId::from_bytes(&[0u8; 32]).expect("32 bytes is valid");
        let e = invalid_node_id(id, "wrong curve");
        assert!(e.to_string().contains("wrong curve"));
    }
}
