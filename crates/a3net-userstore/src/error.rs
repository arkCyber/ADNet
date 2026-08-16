//! Error types for `a3net-userstore`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum UserStoreError {
    /// A required argument is missing or malformed.
    #[error("invalid parameter `{parameter}`: {reason}")]
    InvalidParameter { parameter: String, reason: String },

    /// SQLite / fs IO failure.
    #[error("io error during `{operation}`: {reason}")]
    Io { operation: String, reason: String },

    /// Mutex poisoning / lock acquisition.
    #[error("lock error: {reason}")]
    Lock { reason: String },

    /// Serde_json / bincode failure.
    #[error("serialization error during `{operation}`: {reason}")]
    Serialization { operation: String, reason: String },

    /// A referenced user / device / key does not exist.
    #[error("not found: {kind} `{id}`")]
    NotFound { kind: &'static str, id: String },

    /// A user / device / key already exists.
    #[error("already exists: {kind} `{id}`")]
    AlreadyExists { kind: &'static str, id: String },
}

pub type UserStoreResult<T> = Result<T, UserStoreError>;

impl UserStoreError {
    pub fn code(&self) -> &'static str {
        match self {
            UserStoreError::InvalidParameter { .. } => "US-001",
            UserStoreError::Io { .. } => "US-002",
            UserStoreError::Lock { .. } => "US-003",
            UserStoreError::Serialization { .. } => "US-004",
            UserStoreError::NotFound { .. } => "US-NF",
            UserStoreError::AlreadyExists { .. } => "US-AE",
        }
    }
}

impl From<rusqlite::Error> for UserStoreError {
    fn from(err: rusqlite::Error) -> Self {
        UserStoreError::Io {
            operation: "sqlite".to_string(),
            reason: err.to_string(),
        }
    }
}

impl From<serde_json::Error> for UserStoreError {
    fn from(err: serde_json::Error) -> Self {
        UserStoreError::Serialization {
            operation: "json".to_string(),
            reason: err.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_prefix() {
        assert!(UserStoreError::InvalidParameter {
            parameter: "x".into(),
            reason: "y".into()
        }
        .code()
        .starts_with("US-"));
    }
}