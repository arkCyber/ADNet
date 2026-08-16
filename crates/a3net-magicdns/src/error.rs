//! Error type for the magic DNS resolver.

use thiserror::Error;

use crate::MAX_NAME_LEN;

pub type MagicResult<T> = std::result::Result<T, MagicError>;

#[derive(Debug, Error)]
pub enum MagicError {
    #[error("malformed name: {0}")]
    MalformedName(String),

    #[error("name exceeds MAX_NAME_LEN ({MAX_NAME_LEN} bytes): {actual}")]
    NameTooLong { actual: usize },

    #[error("empty name")]
    Empty,

    #[error("unknown network: {0}")]
    UnknownNetwork(String),

    #[error("unknown hostname: {0} in {1}")]
    UnknownHost(String, String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_name_includes_reason() {
        let e = MagicError::MalformedName("empty segment".into());
        assert!(e.to_string().contains("malformed"));
    }

    #[test]
    fn unknown_network_includes_label() {
        let e = MagicError::UnknownNetwork("ghost".into());
        assert!(e.to_string().contains("ghost"));
    }

    #[test]
    fn unknown_host_includes_both() {
        let e = MagicError::UnknownHost("alice".into(), "gaming".into());
        let s = e.to_string();
        assert!(s.contains("alice"));
        assert!(s.contains("gaming"));
    }
}
