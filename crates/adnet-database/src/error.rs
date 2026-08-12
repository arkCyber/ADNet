//! Database error types.

use thiserror::Error;

/// Result type for database operations.
pub type DatabaseResult<T> = Result<T, DatabaseError>;

/// Database error types.
#[derive(Error, Debug)]
pub enum DatabaseError {
    #[error("Connection error: {reason}")]
    Connection { reason: String },

    #[error("Query error: {reason}")]
    Query { reason: String },

    #[error("Transaction error: {reason}")]
    Transaction { reason: String },

    #[error("Migration error: {reason}")]
    Migration { reason: String },

    #[error("Pool error: {reason}")]
    Pool { reason: String },

    #[error("Not found: {entity} with id {id}")]
    NotFound { entity: String, id: String },

    #[error("Constraint violation: {field} - {reason}")]
    ConstraintViolation { field: String, reason: String },

    #[error("Serialization error: {reason}")]
    Serialization { reason: String },

    #[error("Configuration error: {reason}")]
    Config { reason: String },

    #[error("Unknown error: {reason}")]
    Unknown { reason: String },
}

impl DatabaseError {
    /// Check if this is a "not found" error.
    pub fn is_not_found(&self) -> bool {
        matches!(self, DatabaseError::NotFound { .. })
    }

    /// Check if this is a constraint violation.
    pub fn is_constraint_violation(&self) -> bool {
        matches!(self, DatabaseError::ConstraintViolation { .. })
    }

    /// Check if this is retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            DatabaseError::Connection { .. }
                | DatabaseError::Pool { .. }
        )
    }
}
