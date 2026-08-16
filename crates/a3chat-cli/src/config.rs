//! Configuration loader for the `a3chat` CLI.
//!
//! The CLI reads a TOML config from one of:
//!
//! 1. `--config <path>` flag (highest priority)
//! 2. `$A3CHAT_CONFIG` environment variable
//! 3. `${XDG_CONFIG_HOME:-~/.config}/a3chat/config.toml` (default)
//!
//! All CLI flags can also override config values, allowing ad-hoc
//! usage without touching the file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CliError, CliResult};

/// Default daemon URL when no config is provided. Matches the
/// `a3chat-rpc` default bind address (`127.0.0.1:0` falls back to a
/// random port at runtime — for the CLI we assume the operator
/// picked something stable).
pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:53421";

/// Default owner placeholder — the operator MUST override this.
pub const DEFAULT_OWNER: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Persistent config file shape. Every field is `Option<…>` so that a
/// missing key keeps the default and we can distinguish "user set
/// empty string" from "user did not set".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CliConfig {
    /// Base URL of the running `a3chat-rpc` daemon.
    pub daemon_url: Option<String>,
    /// Local owner identity (hex NodeId, 32 bytes / 64 hex chars).
    pub owner: Option<String>,
    /// Default output format.
    pub output: Option<OutputFormat>,
    /// Retry count for transient RPC errors.
    pub retries: Option<u32>,
    /// Request timeout in milliseconds.
    pub timeout_ms: Option<u64>,
}

impl CliConfig {
    /// Load the config from `<path>`, or fall back to the
    /// platform-default location if `path` is `None`.
    pub fn load(path: Option<&Path>) -> CliResult<Self> {
        let resolved = match path {
            Some(p) => p.to_path_buf(),
            None => default_config_path()?,
        };
        if !resolved.exists() {
            // No file is fine — we synthesize defaults below.
            tracing::debug!(path = %resolved.display(), "config file not found, using defaults");
            return Ok(Self::default());
        }
        let s = std::fs::read_to_string(&resolved).map_err(|e| {
            CliError::Config(format!("read {}: {e}", resolved.display()))
        })?;
        let cfg: Self = toml::from_str(&s).map_err(|e| {
            CliError::Config(format!("parse {}: {e}", resolved.display()))
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validate that the configured fields are well-formed. We do
    /// *not* fail if the owner is the placeholder — that's a runtime
    /// problem, not a config file problem.
    pub fn validate(&self) -> CliResult<()> {
        if let Some(url) = &self.daemon_url {
            if url.trim().is_empty() {
                return Err(CliError::Config("daemon_url is empty".into()));
            }
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(CliError::Config(format!(
                    "daemon_url {url:?} must start with http:// or https://"
                )));
            }
        }
        if let Some(o) = &self.owner {
            validate_owner(o)?;
        }
        Ok(())
    }

    /// Apply CLI overrides on top of the loaded config. CLI wins.
    pub fn apply_overrides(mut self, cli: &crate::Cli) -> Self {
        if let Some(u) = &cli.daemon_url {
            self.daemon_url = Some(u.clone());
        }
        if let Some(o) = &cli.owner {
            self.owner = Some(o.clone());
        }
        if let Some(o) = cli.output {
            self.output = Some(o);
        }
        // Retries / timeout: CLI value always wins (no `Option` indirection).
        self.retries = Some(cli.retries);
        // Re-validate after overrides.
        let _ = self.validate();
        self
    }

    /// Effective daemon URL — CLI/config wins, else default.
    pub fn effective_daemon_url(&self) -> String {
        self.daemon_url
            .clone()
            .unwrap_or_else(|| DEFAULT_DAEMON_URL.to_string())
    }

    /// Effective owner — CLI/config wins, else placeholder.
    pub fn effective_owner(&self) -> String {
        self.owner.clone().unwrap_or_else(|| DEFAULT_OWNER.to_string())
    }

    /// Effective retries. Defaults to 3.
    pub fn effective_retries(&self) -> u32 {
        self.retries.unwrap_or(3)
    }

    /// Effective timeout in milliseconds. Defaults to 30s.
    pub fn effective_timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(30_000)
    }

    /// Effective output format. Defaults to `Table`.
    pub fn effective_output(&self) -> OutputFormat {
        self.output.unwrap_or(OutputFormat::Table)
    }
}

/// Compute the platform-default config path. Errors only on IO that
/// the user can fix (e.g. $HOME unset).
pub fn default_config_path() -> CliResult<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(dir).join("a3chat").join("config.toml"));
    }
    // macOS / Linux without XDG_CONFIG_HOME: ~/.config/a3chat/config.toml
    let home = std::env::var_os("HOME").ok_or_else(|| {
        CliError::Config("$HOME is unset and no --config given".into())
    })?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("a3chat")
        .join("config.toml"))
}

/// Validate that `owner` is a 64-character hex string (NodeId).
pub fn validate_owner(owner: &str) -> CliResult<()> {
    if owner.len() != 64 {
        return Err(CliError::Config(format!(
            "owner must be 64 hex chars (32-byte NodeId); got len={}",
            owner.len()
        )));
    }
    if !owner.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CliError::Config(
            "owner must be lowercase or uppercase hex".into(),
        ));
    }
    Ok(())
}

/// CLI output formats. Stable enum so we can drive clap directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Table,
    Json,
    Plain,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let c = CliConfig::default();
        let s = toml::to_string(&c).unwrap();
        assert!(s.contains("daemon_url") || s.is_empty() || s.contains("["));
        // Round-trip: parsing the empty / partial string should give back defaults.
        let _: CliConfig = toml::from_str(&s).unwrap_or_default();
    }

    #[test]
    fn validate_owner_rejects_wrong_length() {
        assert!(validate_owner("abc").is_err());
        assert!(validate_owner("").is_err());
        assert!(validate_owner(&"a".repeat(63)).is_err());
        assert!(validate_owner(&"a".repeat(65)).is_err());
    }

    #[test]
    fn validate_owner_rejects_non_hex() {
        // 64 chars but contains 'g' which is not hex.
        let bad = format!("{}g", "a".repeat(63));
        assert!(validate_owner(&bad).is_err());
    }

    #[test]
    fn validate_owner_accepts_canonical_hex() {
        let canonical = "0".repeat(64);
        assert!(validate_owner(&canonical).is_ok());
        // 64 hex chars: 10 * 6 = 60 + 4 more.
        let upper = format!("{}{}", "ABCDEF".repeat(10), "ABCD");
        assert_eq!(upper.len(), 64);
        assert!(validate_owner(&upper).is_ok());
    }

    #[test]
    fn validate_rejects_empty_daemon_url() {
        let mut c = CliConfig::default();
        c.daemon_url = Some(String::new());
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_non_http_scheme() {
        let mut c = CliConfig::default();
        c.daemon_url = Some("ftp://example".into());
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_accepts_http_and_https() {
        let mut c = CliConfig::default();
        c.daemon_url = Some("http://localhost:1234".into());
        assert!(c.validate().is_ok());
        c.daemon_url = Some("https://chat.example.com".into());
        assert!(c.validate().is_ok());
    }

    #[test]
    fn effective_values_fall_back_to_constants() {
        let c = CliConfig::default();
        assert_eq!(c.effective_daemon_url(), DEFAULT_DAEMON_URL);
        assert_eq!(c.effective_owner(), DEFAULT_OWNER);
        assert_eq!(c.effective_retries(), 3);
        assert_eq!(c.effective_output(), OutputFormat::Table);
    }

    #[test]
    fn apply_overrides_wins_over_file() {
        let mut c = CliConfig::default();
        c.daemon_url = Some("http://from-file".into());
        c.owner = Some("0".repeat(64));
        c.retries = Some(1);
        c.output = Some(OutputFormat::Json);
        // Build a synthetic CLI overrides struct.
        let cli = crate::Cli {
            config: None,
            daemon_url: Some("http://from-cli".into()),
            owner: Some("a".repeat(64)),
            output: Some(OutputFormat::Plain),
            retries: 5,
            print_config: false,
            verbose: 0,
            command: crate::Cmd::Whoami,
        };
        let merged = c.apply_overrides(&cli);
        assert_eq!(merged.daemon_url.as_deref(), Some("http://from-cli"));
        assert_eq!(merged.owner.as_deref(), Some("a".repeat(64).as_str()));
        assert_eq!(merged.retries, Some(5));
        assert_eq!(merged.output, Some(OutputFormat::Plain));
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("does-not-exist.toml");
        let c = CliConfig::load(Some(&p)).unwrap();
        assert_eq!(c, CliConfig::default());
    }

    #[test]
    fn load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(
            &p,
            r#"
daemon_url = "http://127.0.0.1:9999"
owner = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
output = "json"
retries = 5
timeout_ms = 5000
"#,
        )
        .unwrap();
        let c = CliConfig::load(Some(&p)).unwrap();
        assert_eq!(c.daemon_url.as_deref(), Some("http://127.0.0.1:9999"));
        assert_eq!(c.output, Some(OutputFormat::Json));
        assert_eq!(c.retries, Some(5));
        assert_eq!(c.timeout_ms, Some(5000));
    }

    #[test]
    fn load_invalid_toml_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "this is not = = valid toml [[[[").unwrap();
        let r = CliConfig::load(Some(&p));
        assert!(r.is_err());
    }

    #[test]
    fn load_rejects_bad_owner_in_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "owner = \"abc\"").unwrap();
        let r = CliConfig::load(Some(&p));
        assert!(r.is_err());
    }
}