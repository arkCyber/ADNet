//! CLI error type. All subcommands return [`CliResult<T>`] =
//! `Result<T, CliError>`. The top-level `run()` function maps each
//! variant to a distinct exit code so operators can react without
//! parsing human-readable strings.
//!
//! Every [`CliError`] carries an **actionable suggestion** via
//! [`CliError::suggestion()`] — DO-178C §6.3 *fail-safe*: when the
//! CLI reports an error, it also tells the operator what to do next.

use thiserror::Error;

use a3chat_core::error::{A3chatError, ErrorClass};

/// Result alias used everywhere in `a3chat-cli`.
pub type CliResult<T> = std::result::Result<T, CliError>;

/// Top-level CLI error. The mapping to `ErrorClass` mirrors
/// `a3chat_core::error::A3chatError` so operators can grep logs for
/// `error_class=transient` without learning a second taxonomy.
#[derive(Debug, Error)]
pub enum CliError {
    /// Argument validation / flag combo problem.
    #[error("usage: {0}")]
    Usage(String),

    /// Configuration file parse / validation.
    #[error("config: {0}")]
    Config(String),

    /// Filesystem I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Underlying a3chat RPC call failed.
    #[error("rpc: {0}")]
    Rpc(#[from] A3chatError),

    /// Cryptographic operation failed (e.g. snapshot hash mismatch).
    #[error("crypto: {0}")]
    Crypto(String),

    /// Catch-all for unexpected internal failures.
    #[error("internal: {0}")]
    Internal(String),
}

impl CliError {
    /// Coarse-grained class used by `lib::run` to pick an exit code.
    pub fn class(&self) -> &'static str {
        match self {
            CliError::Usage(_) => "usage",
            CliError::Config(_) => "config",
            CliError::Io(_) => "io",
            CliError::Rpc(e) => match e.error_class() {
                ErrorClass::Permanent => "rpc_permanent",
                ErrorClass::Transient => "rpc_transient",
                ErrorClass::Internal => "rpc_internal",
                ErrorClass::Security => "rpc_security",
            },
            CliError::Crypto(_) => "crypto",
            CliError::Internal(_) => "internal",
        }
    }

    /// Human-readable hint that tells the operator *what to do next*.
    /// Always present for `Usage`/`Config` errors and for the most
    /// common RPC failures.
    pub fn suggestion(&self) -> Option<&'static str> {
        match self {
            CliError::Usage(_) => Some(
                "run `a3chat <subcommand> --help` to see the full flag list",
            ),
            CliError::Config(_) => Some(
                "check `a3chat config show` to inspect the resolved config; \
                 verify --owner is a 64-char hex NodeId and --daemon-url is reachable",
            ),
            CliError::Io(_) => Some(
                "check filesystem permissions and free disk space; \
                 for permission errors on Linux, verify the data dir is owned by the running user",
            ),
            CliError::Rpc(rpc) => match rpc {
                A3chatError::NotFound(_) => Some(
                    "the resource does not exist; verify --conversation-id / --message-id / --to are correct",
                ),
                A3chatError::PermissionDenied(_) => Some(
                    "the daemon rejected the call — check group membership, contact status, or sender-only invariants",
                ),
                A3chatError::InvalidInput(_) => Some(
                    "the request payload failed validation; run with `--dry-run` to echo the envelope before sending",
                ),
                A3chatError::CryptoError(_) => Some(
                    "a cryptographic primitive failed — DO NOT retry; inspect the daemon log for the AEAD failure",
                ),
                A3chatError::StorageError(_) => Some(
                    "the local SQLite store failed; check disk pressure and restart the daemon if WAL files are corrupted",
                ),
                A3chatError::NetworkError(_) => Some(
                    "transient transport error — exit code is EX_TEMPFAIL; retry the command or check the daemon is running",
                ),
                A3chatError::RpcError(_) => Some(
                    "the daemon returned an unknown JSON-RPC error; verify the daemon version matches the CLI",
                ),
                A3chatError::Internal(_) => Some(
                    "unexpected internal failure; please open a bug report with the request_id",
                ),
            },
            CliError::Crypto(_) => Some(
                "snapshot hash mismatch — the file may be corrupt; re-request the snapshot and verify the SHA-256 sidecar",
            ),
            CliError::Internal(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_matches_error_class() {
        let e = CliError::Rpc(A3chatError::NetworkError("x".into()));
        assert_eq!(e.class(), "rpc_transient");
        let e = CliError::Rpc(A3chatError::CryptoError("x".into()));
        assert_eq!(e.class(), "rpc_security");
    }

    #[test]
    fn usage_suggestion_is_present() {
        let e = CliError::Usage("bad flag".into());
        assert!(e.suggestion().unwrap().contains("--help"));
    }

    #[test]
    fn all_rpc_variants_have_suggestions() {
        // DO-178C §6.3 — every error class must have an actionable hint.
        let cases = [
            A3chatError::NotFound("x".into()),
            A3chatError::PermissionDenied("x".into()),
            A3chatError::InvalidInput("x".into()),
            A3chatError::CryptoError("x".into()),
            A3chatError::StorageError("x".into()),
            A3chatError::NetworkError("x".into()),
            A3chatError::RpcError("x".into()),
            A3chatError::Internal("x".into()),
        ];
        for inner in cases {
            let e = CliError::Rpc(inner);
            assert!(e.suggestion().is_some(), "missing suggestion for {e}");
        }
    }

    #[test]
    fn io_suggestion_mentions_permissions() {
        let e = CliError::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "x"));
        let s = e.suggestion().unwrap();
        assert!(s.contains("permission") || s.contains("disk"));
    }
}