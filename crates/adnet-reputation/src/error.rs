//! Error model for the reputation subsystem.
//!
//! All public APIs return [`ReputationResult<T>`]. Errors are
//! intentionally coarse — the goal is to give callers enough context
//! to decide whether to retry, ignore, or escalate, not to be a full
//! error tree.

use thiserror::Error;

/// Convenient `Result<T, ReputationError>` alias.
pub type ReputationResult<T> = std::result::Result<T, ReputationError>;

/// All errors surfaced from `adnet-reputation`.
#[derive(Debug, Error)]
pub enum ReputationError {
    /// A supplied NodeId is malformed.
    #[error("invalid node id: {0}")]
    InvalidNodeId(String),

    /// A supplied TopicId is malformed.
    #[error("invalid topic id: {0}")]
    InvalidTopic(String),

    /// The on-disk snapshot file failed to parse.
    #[error("malformed snapshot at {path}: {reason}")]
    MalformedSnapshot {
        /// Path of the malformed file.
        path: String,
        /// Description of what went wrong.
        reason: String,
    },

    /// The on-disk JSONL log failed to parse at a specific line.
    #[error("malformed delta at {path}:{line}: {reason}")]
    MalformedDelta {
        /// Path of the log file.
        path: String,
        /// 1-indexed line number.
        line: u64,
        /// Description of what went wrong.
        reason: String,
    },

    /// Underlying I/O failure (disk full, permission denied, …).
    #[error("i/o: {context}: {source}")]
    Io {
        /// What we were trying to do (read/write/rename/…).
        context: String,
        /// Inner error message.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// A score update produced a non-finite value (NaN / Inf) — the
    /// caller passed parameters that mathematically explode. This is
    /// a programming error and is therefore reported as an error
    /// rather than silently clamping.
    #[error("non-finite score after apply: peer={peer}, event={event}")]
    NonFiniteScore {
        /// Hex NodeId of the offending peer.
        peer: String,
        /// The event variant that triggered the explosion.
        event: String,
    },

    /// The decay task was asked to do something impossible (negative
    /// period, etc).
    #[error("invalid decay config: {0}")]
    InvalidDecayConfig(String),

    /// Persistence path does not exist and could not be created.
    #[error("storage path unavailable: {0}")]
    StorageUnavailable(String),
}

impl ReputationError {
    /// Classify into the [`adnet_error::ErrorKind`] taxonomy for
    /// uniform logging / Prometheus labelling. Requires the
    /// `metrics` feature.
    #[cfg(feature = "metrics")]
    pub fn classify(&self) -> adnet_error::ErrorKind {
        use adnet_error::ErrorKind;
        match self {
            Self::InvalidNodeId(_) | Self::InvalidTopic(_) => ErrorKind::BadRequest,
            Self::MalformedSnapshot { .. }
            | Self::MalformedDelta { .. }
            | Self::NonFiniteScore { .. }
            | Self::InvalidDecayConfig(_) => ErrorKind::Internal,
            Self::Io { .. } | Self::StorageUnavailable(_) => ErrorKind::Internal,
        }
    }
}
