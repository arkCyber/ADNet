//! Bridge error types for a3net-eliza-bridge.

use thiserror::Error;

/// Unified error type for all bridge operations.
#[derive(Error, Debug)]
pub enum BridgeError {
    #[error("identity error: {0}")]
    Identity(String),

    #[error("chat store error: {0}")]
    ChatStore(String),

    #[error("news service error: {0}")]
    NewsService(String),

    #[error("gossip error: {0}")]
    Gossip(String),

    #[error("agent error: {0}")]
    Agent(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("not connected to A3Net network")]
    NotConnected,

    #[error("agent not authenticated: {0}")]
    NotAuthenticated(String),

    #[error("invalid message format: {0}")]
    InvalidMessage(String),

    #[error("contact not found: {0}")]
    ContactNotFound(String),

    #[error("room not found: {0}")]
    RoomNotFound(String),

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("timeout after {0}s")]
    Timeout(u64),

    #[error("subscription cancelled")]
    Cancelled,

    #[error("rate limited: {0}")]
    RateLimited(String),

    #[error("signature verification failed: {0}")]
    SignatureInvalid(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("duplicate operation: {0}")]
    Duplicate(String),

    #[error("resource not found: {0}")]
    NotFound(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl BridgeError {
    /// `true` if the error is transient and the operation could be
    /// retried safely.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            BridgeError::Network(_)
                | BridgeError::Timeout(_)
                | BridgeError::Gossip(_)
                | BridgeError::NotConnected
        )
    }

    /// `true` if the error reflects a client-side mistake (no retry).
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            BridgeError::InvalidMessage(_)
                | BridgeError::PermissionDenied(_)
                | BridgeError::NotFound(_)
                | BridgeError::SignatureInvalid(_)
                | BridgeError::Duplicate(_)
        )
    }
}

impl From<anyhow::Error> for BridgeError {
    fn from(e: anyhow::Error) -> Self {
        BridgeError::Internal(e.to_string())
    }
}

impl From<a3net_types::error::AdnetError> for BridgeError {
    fn from(e: a3net_types::error::AdnetError) -> Self {
        BridgeError::Internal(e.to_string())
    }
}

/// Result type alias for bridge operations.
pub type BridgeResult<T> = Result<T, BridgeError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// `Display` is used for both user-facing errors and `assert_eq!`
    /// debugging. Verify each variant produces the documented prefix.
    #[test]
    fn display_messages_for_each_variant() {
        let cases: Vec<(BridgeError, &str)> = vec![
            (BridgeError::Identity("bad".into()), "identity error: bad"),
            (BridgeError::ChatStore("missing".into()), "chat store error: missing"),
            (BridgeError::NewsService("rate".into()), "news service error: rate"),
            (BridgeError::Gossip("dropped".into()), "gossip error: dropped"),
            (BridgeError::Agent("no brain".into()), "agent error: no brain"),
            (
                BridgeError::NotConnected,
                "not connected to A3Net network",
            ),
            (
                BridgeError::NotAuthenticated("anonymous".into()),
                "agent not authenticated: anonymous",
            ),
            (
                BridgeError::InvalidMessage("empty".into()),
                "invalid message format: empty",
            ),
            (
                BridgeError::ContactNotFound("0xdead".into()),
                "contact not found: 0xdead",
            ),
            (
                BridgeError::RoomNotFound("general".into()),
                "room not found: general",
            ),
            (
                BridgeError::PermissionDenied("blocked".into()),
                "permission denied: blocked",
            ),
            (BridgeError::Timeout(7), "timeout after 7s"),
            (BridgeError::Cancelled, "subscription cancelled"),
            (
                BridgeError::RateLimited("wait".into()),
                "rate limited: wait",
            ),
            (
                BridgeError::SignatureInvalid("bad sig".into()),
                "signature verification failed: bad sig",
            ),
            (
                BridgeError::Network("timeout".into()),
                "network error: timeout",
            ),
            (
                BridgeError::Duplicate("already".into()),
                "duplicate operation: already",
            ),
            (BridgeError::NotFound("file".into()), "resource not found: file"),
            (
                BridgeError::Internal("panic".into()),
                "internal error: panic",
            ),
        ];

        for (err, expected) in cases {
            assert_eq!(err.to_string(), expected, "variant mismatch");
        }
    }

    #[test]
    fn display_for_serde_json_error() {
        let err: BridgeError = serde_json::from_str::<u32>("not-a-number")
            .err()
            .map(BridgeError::Serialization)
            .expect("json error");
        assert!(err.to_string().starts_with("serialization error:"));
    }

    #[test]
    fn display_for_io_error() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err = BridgeError::Io(io);
        assert!(err.to_string().starts_with("io error:"));
    }

    #[test]
    fn from_anyhow_error_wraps_as_internal() {
        let err: BridgeError = anyhow::anyhow!("boom").into();
        match err {
            BridgeError::Internal(msg) => assert_eq!(msg, "boom"),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    #[test]
    fn from_a3net_error_wraps_as_internal() {
        let a3net_err = a3net_types::error::AdnetError::Validation("oops".into());
        let err: BridgeError = a3net_err.into();
        match err {
            BridgeError::Internal(msg) => assert!(msg.contains("oops")),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    #[test]
    fn is_retryable_for_transient_variants() {
        assert!(BridgeError::Network("x".into()).is_retryable());
        assert!(BridgeError::Timeout(1).is_retryable());
        assert!(BridgeError::Gossip("x".into()).is_retryable());
        assert!(BridgeError::NotConnected.is_retryable());
    }

    #[test]
    fn is_not_retryable_for_other_variants() {
        let others: Vec<BridgeError> = vec![
            BridgeError::Identity("x".into()),
            BridgeError::ChatStore("x".into()),
            BridgeError::NewsService("x".into()),
            BridgeError::Agent("x".into()),
            BridgeError::NotAuthenticated("x".into()),
            BridgeError::InvalidMessage("x".into()),
            BridgeError::ContactNotFound("x".into()),
            BridgeError::RoomNotFound("x".into()),
            BridgeError::PermissionDenied("x".into()),
            BridgeError::Cancelled,
            BridgeError::RateLimited("x".into()),
            BridgeError::SignatureInvalid("x".into()),
            BridgeError::Duplicate("x".into()),
            BridgeError::NotFound("x".into()),
            BridgeError::Internal("x".into()),
        ];
        for err in others {
            assert!(!err.is_retryable(), "{err:?} should not be retryable");
        }
    }

    #[test]
    fn is_permanent_for_permanent_variants() {
        assert!(BridgeError::InvalidMessage("x".into()).is_permanent());
        assert!(BridgeError::PermissionDenied("x".into()).is_permanent());
        assert!(BridgeError::NotFound("x".into()).is_permanent());
        assert!(BridgeError::SignatureInvalid("x".into()).is_permanent());
        assert!(BridgeError::Duplicate("x".into()).is_permanent());
    }

    #[test]
    fn is_not_permanent_for_other_variants() {
        let others: Vec<BridgeError> = vec![
            BridgeError::Identity("x".into()),
            BridgeError::ChatStore("x".into()),
            BridgeError::NewsService("x".into()),
            BridgeError::Gossip("x".into()),
            BridgeError::Agent("x".into()),
            BridgeError::NotConnected,
            BridgeError::NotAuthenticated("x".into()),
            BridgeError::ContactNotFound("x".into()),
            BridgeError::RoomNotFound("x".into()),
            BridgeError::Timeout(1),
            BridgeError::Cancelled,
            BridgeError::RateLimited("x".into()),
            BridgeError::Network("x".into()),
            BridgeError::Internal("x".into()),
        ];
        for err in others {
            assert!(!err.is_permanent(), "{err:?} should not be permanent");
        }
    }

    #[test]
    fn error_is_send_sync() {
        fn assert_send<T: Send + Sync>() {}
        assert_send::<BridgeError>();
    }

    #[test]
    fn debug_includes_variant_and_payload() {
        let err = BridgeError::RateLimited("foo".into());
        let dbg = format!("{err:?}");
        assert!(dbg.contains("RateLimited"));
        assert!(dbg.contains("foo"));
    }
}