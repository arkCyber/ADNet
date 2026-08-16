//! Unified error types for the security crate.

use thiserror::Error;

/// Unified result type for security operations.
pub type SecurityResult<T> = Result<T, SecurityError>;

/// Comprehensive error types for security operations.
#[derive(Error, Debug, Clone)]
pub enum SecurityError {
    #[error("Access denied: {reason}")]
    AccessDenied { reason: String },

    #[error("Authentication failed: {reason}")]
    AuthenticationFailed { reason: String },

    #[error("Invalid credentials: {reason}")]
    InvalidCredentials { reason: String },

    #[error("Session error: {reason}")]
    SessionError { reason: String },

    #[error("Session expired")]
    SessionExpired,

    #[error("Session not found: {id}")]
    SessionNotFound { id: String },

    #[error("Key error: {reason}")]
    KeyError { reason: String },

    #[error("Key not found: {id}")]
    KeyNotFound { id: String },

    #[error("Key rotation failed: {reason}")]
    KeyRotationFailed { reason: String },

    #[error("Encryption failed: {reason}")]
    EncryptionFailed { reason: String },

    #[error("Decryption failed: {reason}")]
    DecryptionFailed { reason: String },

    #[error("Intrusion detected: {threat_type:?}")]
    IntrusionDetected { threat_type: String },

    #[error("Anomaly detected: score {score}")]
    AnomalyDetected { score: f64 },

    #[error("Audit failure: {reason}")]
    AuditFailure { reason: String },

    #[error("Invalid configuration: {reason}")]
    InvalidConfig { reason: String },

    #[error("Serialization error: {reason}")]
    SerializationError { reason: String },

    #[error("Internal error: {reason}")]
    Internal { reason: String },
}

impl SecurityError {
    /// Returns true if the error is retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            SecurityError::SessionExpired
                | SecurityError::SessionError { .. }
                | SecurityError::AuditFailure { .. }
        )
    }

    /// Returns true if the error indicates a security threat.
    pub fn is_security_threat(&self) -> bool {
        matches!(
            self,
            SecurityError::AuthenticationFailed { .. }
                | SecurityError::InvalidCredentials { .. }
                | SecurityError::IntrusionDetected { .. }
                | SecurityError::AnomalyDetected { .. }
        )
    }

    /// Returns a severity level for the error.
    pub fn severity(&self) -> super::audit::AuditSeverity {
        use super::audit::AuditSeverity;
        match self {
            SecurityError::AccessDenied { .. } => AuditSeverity::Warning,
            SecurityError::AuthenticationFailed { .. } => AuditSeverity::Warning,
            SecurityError::InvalidCredentials { .. } => AuditSeverity::Warning,
            SecurityError::SessionExpired => AuditSeverity::Info,
            SecurityError::SessionError { .. } => AuditSeverity::Warning,
            SecurityError::SessionNotFound { .. } => AuditSeverity::Info,
            SecurityError::KeyError { .. } => AuditSeverity::Error,
            SecurityError::KeyNotFound { .. } => AuditSeverity::Warning,
            SecurityError::KeyRotationFailed { .. } => AuditSeverity::Error,
            SecurityError::EncryptionFailed { .. } => AuditSeverity::Error,
            SecurityError::DecryptionFailed { .. } => AuditSeverity::Error,
            SecurityError::IntrusionDetected { .. } => AuditSeverity::Critical,
            SecurityError::AnomalyDetected { .. } => AuditSeverity::Warning,
            SecurityError::AuditFailure { .. } => AuditSeverity::Error,
            SecurityError::InvalidConfig { .. } => AuditSeverity::Error,
            SecurityError::SerializationError { .. } => AuditSeverity::Error,
            SecurityError::Internal { .. } => AuditSeverity::Error,
        }
    }
}
