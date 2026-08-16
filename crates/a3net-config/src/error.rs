//! Configuration error types.
//!
//! DO-178C SR-8: Error handling for configuration system.

use thiserror::Error;

/// Configuration system errors.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Failed to read configuration file.
    #[error("failed to read config file: {0}")]
    Io(#[from] std::io::Error),

    /// Failed to parse configuration (invalid JSON, YAML, etc.).
    #[error("failed to parse config: {0}")]
    Parse(#[from] serde_json::Error),

    /// Configuration validation failed.
    #[error("validation failed: {0}")]
    Validation(String),

    /// Configuration key not found.
    #[error("key not found: {0}")]
    KeyNotFound(String),

    /// Configuration file not found.
    #[error("config file not found: {0}")]
    FileNotFound(String),

    /// Invalid configuration value type.
    #[error("invalid type for key '{key}': expected {expected}, got {actual}")]
    InvalidType {
        key: String,
        expected: &'static str,
        actual: &'static str,
    },

    /// Hot reload failed.
    #[error("hot reload error: {0}")]
    HotReload(String),

    /// Schema violation.
    #[error("schema violation at '{path}': {message}")]
    SchemaViolation { path: String, message: String },

    /// Environment variable expansion failed.
    #[error("environment variable expansion failed: {0}")]
    EnvExpansion(String),

    /// Watcher error (file system events).
    #[error("config watcher error: {0}")]
    Watcher(String),
}

/// Result type alias for configuration operations.
pub type ConfigResult<T> = Result<T, ConfigError>;

impl ConfigError {
    /// Returns true if this error indicates the config file was not found.
    pub fn is_not_found(&self) -> bool {
        matches!(self, ConfigError::FileNotFound(_))
    }

    /// Returns true if this error indicates a validation failure.
    pub fn is_validation(&self) -> bool {
        matches!(self, ConfigError::Validation(_) | ConfigError::SchemaViolation { .. })
    }

    /// Returns the key path if this error is related to a specific key.
    pub fn key_path(&self) -> Option<&str> {
        match self {
            ConfigError::KeyNotFound(key) => Some(key),
            ConfigError::InvalidType { key, .. } => Some(key),
            ConfigError::SchemaViolation { path, .. } => Some(path),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Smoke tests for `ConfigError`'s helper methods.
    //!
    //! Behavioural invariants tested:
    //! - `is_not_found` only matches `FileNotFound`.
    //! - `is_validation` matches both `Validation` and `SchemaViolation`.
    //! - `key_path` returns the underlying key for the three variants
    //!   that carry a key, and `None` for unrelated variants.
    //! - `Display` strings are stable enough that doc links will not
    //!   break silently — pinned here to catch a regression in the
    //!   `thiserror` `#[error = "..."]` strings.

    use super::*;

    #[test]
    fn is_not_found_only_matches_filenotfound() {
        assert!(ConfigError::FileNotFound("a".into()).is_not_found());
        assert!(!ConfigError::Validation("a".into()).is_not_found());
        assert!(!ConfigError::KeyNotFound("a".into()).is_not_found());
        assert!(!ConfigError::HotReload("a".into()).is_not_found());
        assert!(!ConfigError::SchemaViolation {
            path: "p".into(),
            message: "m".into()
        }
        .is_not_found());
    }

    #[test]
    fn is_validation_matches_both_variants() {
        assert!(ConfigError::Validation("too short".into()).is_validation());
        assert!(ConfigError::SchemaViolation {
            path: "host".into(),
            message: "must be a string".into(),
        }
        .is_validation());
        assert!(!ConfigError::FileNotFound("a".into()).is_validation());
        assert!(!ConfigError::KeyNotFound("a".into()).is_validation());
        assert!(!ConfigError::HotReload("a".into()).is_validation());
        assert!(!ConfigError::Watcher("a".into()).is_validation());
    }

    #[test]
    fn key_path_returns_underlying_key_when_present() {
        assert_eq!(ConfigError::KeyNotFound("host".into()).key_path(), Some("host"));
        assert_eq!(
            ConfigError::InvalidType {
                key: "port".into(),
                expected: "int",
                actual: "string",
            }
            .key_path(),
            Some("port")
        );
        assert_eq!(
            ConfigError::SchemaViolation {
                path: "server.port".into(),
                message: "out of range".into(),
            }
            .key_path(),
            Some("server.port")
        );
    }

    #[test]
    fn key_path_is_none_for_unrelated_variants() {
        assert_eq!(ConfigError::FileNotFound("a".into()).key_path(), None);
        assert_eq!(ConfigError::Validation("a".into()).key_path(), None);
        assert_eq!(ConfigError::HotReload("a".into()).key_path(), None);
        assert_eq!(ConfigError::Watcher("a".into()).key_path(), None);
        assert_eq!(ConfigError::EnvExpansion("a".into()).key_path(), None);
    }

    #[test]
    fn display_strings_are_stable() {
        // These text strings are referenced from CLI error messages
        // and from the README. A regression here should fail loud.
        let s = format!("{}", ConfigError::FileNotFound("config.json".into()));
        assert!(s.contains("config.json"), "Display lost the path: {s}");
        let s = format!(
            "{}",
            ConfigError::InvalidType {
                key: "port".into(),
                expected: "int",
                actual: "string",
            }
        );
        assert!(s.contains("port"));
        assert!(s.contains("int"));
        assert!(s.contains("string"));
    }

    #[test]
    fn from_io_error_preserves_source() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let ce: ConfigError = io.into();
        assert!(matches!(ce, ConfigError::Io(_)));
    }

    #[test]
    fn from_serde_error_preserves_source() {
        let bad_json = "{ not valid json";
        let se: serde_json::Error = serde_json::from_str::<serde_json::Value>(bad_json)
            .unwrap_err();
        let ce: ConfigError = se.into();
        assert!(matches!(ce, ConfigError::Parse(_)));
    }
}
