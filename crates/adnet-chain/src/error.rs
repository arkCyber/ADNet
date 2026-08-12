//! Error type for the blockchain-node scaffold.

use thiserror::Error;

pub type ChainResult<T> = std::result::Result<T, ChainError>;

#[derive(Debug, Error)]
pub enum ChainError {
    #[error("chain node is not configured: {0}")]
    NotConfigured(String),

    #[error("chain node is already running")]
    AlreadyRunning,

    #[error("chain node is not running")]
    NotRunning,

    #[error("unsupported chain kind: {0}")]
    UnsupportedChain(String),

    #[error("not yet implemented: {0}")]
    Unimplemented(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_chain_includes_name() {
        let e = ChainError::UnsupportedChain("foochain".into());
        assert!(e.to_string().contains("foochain"));
    }
}
