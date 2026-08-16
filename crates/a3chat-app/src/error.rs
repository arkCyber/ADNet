//! App-layer error type. `From` bridges from [`a3chat_core::A3chatError`]
//! so `?`-conversion is ergonomic.

use thiserror::Error;

/// Result alias used across `a3chat-app`.
pub type AppResult<T> = std::result::Result<T, AppError>;

/// Errors raised by the service layer.
#[derive(Debug, Error)]
pub enum AppError {
    /// Bubbled up from the domain types.
    #[error("domain error: {0}")]
    Domain(String),

    /// Bubbled up from the persistence layer (`a3net-chatstore`).
    #[error("storage error: {0}")]
    Storage(String),

    /// Bubbled up from the crypto layer.
    #[error("crypto error: {0}")]
    Crypto(String),

    /// The service was called before it was initialised.
    #[error("not initialised: {0}")]
    NotInitialised(&'static str),

    /// Operation forbidden by ACL (group membership, blocklist, …).
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// Duplicate resource (already-friend, already-member, …).
    #[error("conflict: {0}")]
    Conflict(String),

    /// Network/transport failure talking to a peer or upstream.
    /// Kept distinct from `Internal` so the RPC layer can return
    /// a structured error code (`RpcError::NetworkError`) instead
    /// of the catch-all `-32603`.
    #[error("network error: {0}")]
    Network(String),

    /// Upstream returned a non-success response (HTTP non-2xx,
    /// malformed JSON-RPC payload, …). Distinct from `Network`
    /// because the underlying transport was healthy.
    #[error("rpc error: {0}")]
    Rpc(String),

    /// Catch-all — prefer a dedicated variant.
    #[error("internal: {0}")]
    Internal(String),
}

impl From<a3chat_core::error::A3chatError> for AppError {
    fn from(e: a3chat_core::error::A3chatError) -> Self {
        use a3chat_core::error::A3chatError as D;
        match e {
            D::NotFound(_) => AppError::Domain(e.to_string()),
            D::PermissionDenied(_) => AppError::Forbidden(e.to_string()),
            D::InvalidInput(_) => AppError::Domain(e.to_string()),
            D::CryptoError(_) => AppError::Crypto(e.to_string()),
            D::StorageError(_) => AppError::Storage(e.to_string()),
            D::RpcError(_) => AppError::Rpc(e.to_string()),
            D::NetworkError(_) => AppError::Network(e.to_string()),
            D::Internal(_) => AppError::Internal(e.to_string()),
        }
    }
}

/// Inverse of `From<A3chatError>`. Used by service-level
/// dispatchers (e.g. `profile_service::dispatch`) that already
/// work in `AppResult<T>` and need to return
/// `Result<serde_json::Value, A3chatError>` to satisfy the
/// `A3chatApp::dispatch` signature.
pub fn app_to_domain(e: AppError) -> a3chat_core::error::A3chatError {
    use a3chat_core::error::A3chatError as D;
    match e {
        AppError::Domain(_) => D::InvalidInput(e.to_string()),
        AppError::Forbidden(_) => D::PermissionDenied(e.to_string()),
        AppError::Crypto(_) => D::CryptoError(e.to_string()),
        AppError::Storage(_) => D::StorageError(e.to_string()),
        AppError::Rpc(_) => D::RpcError(e.to_string()),
        AppError::Network(_) => D::NetworkError(e.to_string()),
        AppError::Conflict(_) => D::InvalidInput(e.to_string()),
        AppError::NotInitialised(_) => D::Internal(e.to_string()),
        AppError::Internal(_) => D::Internal(e.to_string()),
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Storage(e.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Internal(format!("serde_json: {e}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Storage(format!("io: {e}"))
    }
}

impl From<a3net_chatstore::error::ChatStoreError> for AppError {
    fn from(e: a3net_chatstore::error::ChatStoreError) -> Self {
        use a3net_chatstore::error::ChatStoreError as C;
        match e {
            C::Lock => AppError::Internal("chatstore lock poisoned".into()),
            C::Sqlite(e) => AppError::Storage(e.to_string()),
            C::Json(e) => AppError::Storage(format!("json: {e}")),
            C::Io(e) => AppError::Storage(format!("io: {e}")),
            C::InvalidTrustLevel { level } => {
                AppError::Domain(format!("invalid trust level {level}"))
            }
            C::InvalidId(m) => AppError::Domain(m),
            other => AppError::Storage(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::error::A3chatError;

    #[test]
    fn domain_not_found_maps_to_domain_variant() {
        let e: AppError = A3chatError::NotFound("foo".into()).into();
        assert!(matches!(e, AppError::Domain(_)));
    }

    #[test]
    fn domain_permission_maps_to_forbidden() {
        let e: AppError = A3chatError::PermissionDenied("nope".into()).into();
        assert!(matches!(e, AppError::Forbidden(_)));
    }

    #[test]
    fn domain_crypto_maps_to_crypto() {
        let e: AppError = A3chatError::CryptoError("e".into()).into();
        assert!(matches!(e, AppError::Crypto(_)));
    }

    #[test]
    fn domain_internal_maps_to_internal() {
        let e: AppError = A3chatError::Internal("oops".into()).into();
        assert!(matches!(e, AppError::Internal(_)));
    }

    #[test]
    fn rusqlite_error_maps_to_storage() {
        let e: AppError = rusqlite::Error::QueryReturnedNoRows.into();
        assert!(matches!(e, AppError::Storage(_)));
    }

    #[test]
    fn io_error_maps_to_storage() {
        let io = std::io::Error::other("disk full");
        let e: AppError = io.into();
        assert!(matches!(e, AppError::Storage(_)));
    }

    #[test]
    fn serde_error_maps_to_internal() {
        // Construct a malformed JSON to force an error.
        let r: std::result::Result<i32, serde_json::Error> = serde_json::from_str("not json");
        let err = r.unwrap_err();
        let e: AppError = err.into();
        assert!(matches!(e, AppError::Internal(_)));
    }

    #[test]
    fn domain_rpc_error_maps_to_rpc_variant() {
        let e: AppError = A3chatError::RpcError("upstream 502".into()).into();
        assert!(matches!(e, AppError::Rpc(_)));
    }

    #[test]
    fn domain_network_error_maps_to_network_variant() {
        let e: AppError = A3chatError::NetworkError("connection reset".into()).into();
        assert!(matches!(e, AppError::Network(_)));
    }

    #[test]
    fn domain_storage_error_maps_to_storage_variant() {
        let e: AppError = A3chatError::StorageError("disk full".into()).into();
        assert!(matches!(e, AppError::Storage(_)));
    }

    #[test]
    fn domain_invalid_input_maps_to_domain_variant() {
        let e: AppError = A3chatError::InvalidInput("bad name".into()).into();
        assert!(matches!(e, AppError::Domain(_)));
    }
}
