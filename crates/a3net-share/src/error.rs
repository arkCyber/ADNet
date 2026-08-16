//! Errors surfaced from `a3net-share`.

use thiserror::Error;

/// Crate-wide result alias.
pub type ShareResult<T> = std::result::Result<T, ShareError>;

/// Errors that can occur while preparing, sharing, or receiving a
/// file/directory blob.
#[derive(Debug, Error)]
pub enum ShareError {
    #[error("path does not exist: {0}")]
    PathNotFound(String),

    #[error("path is not a regular file or directory: {0}")]
    NotFileOrDir(String),

    #[error("invalid path component: {0:?} (must not contain '/' or '\\\\', must be valid UTF-8)")]
    InvalidPathComponent(String),

    #[error("path component too long: {name:?} ({len} bytes, max {max})")]
    PathComponentTooLong {
        name: String,
        len: usize,
        max: usize,
    },

    #[error("symlink refused: {0:?} (use WalkOptions::allow_symlinks = true to opt in)")]
    SymlinkRefused(String),

    #[error("absolute path refused: {0:?} (use a path relative to the share root)")]
    AbsolutePathRefused(String),

    #[error("collection name collided after canonicalisation: {0:?}")]
    DuplicateName(String),

    #[error("collection exceeds MAX_COLLECTION_ENTRIES ({max}) — got {got}")]
    CollectionTooLarge { got: usize, max: usize },

    #[error("collection entry name exceeds MAX_NAME_LEN ({max}) — got {got}")]
    NameTooLong { got: usize, max: usize },

    #[error("invalid ticket: {0}")]
    InvalidTicket(String),

    #[error("invalid manifest hash: expected {expected}, got {actual}")]
    ManifestMismatch { expected: String, actual: String },

    #[error("backend error: {0}")]
    Backend(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<ShareError> for std::io::Error {
    fn from(e: ShareError) -> Self {
        match e {
            ShareError::Io(inner) => inner,
            other => std::io::Error::new(std::io::ErrorKind::Other, other.to_string()),
        }
    }
}