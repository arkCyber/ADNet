//! Persistent-key resolution helpers for `adnet-ssh`.
//!
//! iroh-ssh stores its server key at `~/.ssh/irohssh_ed25519` and
//! its service-mode key at `~/.ssh/irohssh_service_ed25519`. ADNet
//! already mints a single persistent Ed25519 identity at
//! `<data-dir>/iroh_secret_key` via
//! [`adnet_transport::iroh::IrohIdentity::load_or_create`], and we
//! reuse it. The end result is identical:
//!
//! - Server endpoint id is stable across restarts.
//! - No second key file lives next to the canonical iroh identity.
//! - The same key is shared with the `adnet/frame/1` ALPN, the
//!   blob store, and the gossip layer (because the iroh endpoint
//!   is the same one the rest of the ADNet runtime is using).
//!
//! The functions in this module are intentionally tiny: they
//! either resolve to the persistent identity or to an ephemeral
//! in-memory key (matching iroh-ssh's `server` vs `server --persist`
//! distinction).
//!
//! # Authorized-keys and trusted peers
//!
//! The [`AuthorizedKeys`] type parses `~/.ssh/authorized_keys` (the
//! standard OpenSSH format) so callers can restrict which public keys
//! are permitted to open tunnels to this node. In ADNet's trust model
//! the *peer* presents an Ed25519 public key derived from their
//! persistent identity; the server verifies that key against this
//! file before accepting the tunnel.
//!
//! Additionally, [`TrustedPeers`] tracks the set of endpoint IDs that
//! are allowed to connect even without a matching `authorized_keys`
//! entry — a convenience layer for a "friends list" model where two
//! nodes mutually add each other's endpoint IDs.

use std::path::{Path, PathBuf};

#[cfg(feature = "iroh")]
use adnet_transport::iroh::IrohIdentity;

#[cfg(feature = "iroh")]
use crate::error::{SshError, SshResult};

/// Default subdirectory under `<data-dir>` for SSH-tunnel state.
///
/// Today this is unused (the persistent identity lives directly
/// under `<data-dir>`), but we keep it as a hook for future
/// auxiliary state — known-host entries, per-connection logs,
/// etc. — without breaking the public API.
pub const SSH_SUBDIR: &str = "ssh";

/// Filename of the persistent Ed25519 identity under
/// `<data-dir>`. Re-exported here from
/// `adnet_transport::iroh::IROH_SECRET_KEY_FILE` so callers don't
/// have to import the inner transport crate.
pub const IROH_SECRET_KEY_FILE: &str = "iroh_secret_key";

/// Load (or create) the durable identity for the SSH tunnel.
///
/// Returns the same identity that the rest of the ADNet runtime
/// uses, so the endpoint id printed by `adnet ssh info` matches
/// the endpoint id published by the iroh gossip / blob layer.
#[cfg(feature = "iroh")]
pub fn persistent_identity(data_dir: impl AsRef<Path>) -> SshResult<IrohIdentity> {
    let dir = data_dir.as_ref();
    IrohIdentity::load_or_create(dir).map_err(|e| SshError::Identity {
        path: dir.join(IROH_SECRET_KEY_FILE).display().to_string(),
        source: Box::new(e),
    })
}

/// Resolve the data directory for SSH-tunnel state.
///
/// - If the caller passed `--data-dir`, use it verbatim.
/// - Otherwise default to `./.adnet-data` (matching
///   `adnet-cli::Cli::data_dir`).
pub fn resolve_data_dir(cli_data_dir: Option<&str>) -> PathBuf {
    match cli_data_dir {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from("./.adnet-data"),
    }
}

// -------------------------------------------------------------------------
#[cfg(feature = "iroh")]
#[allow(dead_code)]
mod authorized_keys {
use std::collections::HashSet;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// Authorized-keys support
// -------------------------------------------------------------------------

/// Path to the `authorized_keys` file relative to the ADNet data
/// directory. Mirrors OpenSSH's `~/.ssh/authorized_keys` layout.
const AUTHORIZED_KEYS_FILE: &str = "ssh/authorized_keys";

/// Path to the trusted-peers file relative to the ADNet data
/// directory. One endpoint-id per line; empty lines and `#`-prefixed
/// comments are ignored.
const TRUSTED_PEERS_FILE: &str = "ssh/trusted_peers";

/// A parsed entry from `authorized_keys`.
///
/// SSH's `authorized_keys` format (RFC 4252 §6) is a line-based format.
/// Each line contains space-separated options, a key type, a base64-encoded
/// blob, and an optional comment. We support the `from="..."` option
/// (which restricts the connecting IP address) and the ADNet-specific
/// `from-adnet="<endpoint-id>` option (which restricts which ADNet
/// endpoint ID is permitted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedKeyEntry {
    /// Raw key type, e.g. `"ssh-ed25519"` or `"ecdsa-sha2-nistp256"`.
    pub key_type: String,
    /// Base64-encoded key blob.
    pub key_blob: String,
    /// Optional comment (usually `user@host`).
    pub comment: Option<String>,
    /// Restrict to a specific IP address or pattern (the `from` option).
    pub from_pattern: Option<String>,
    /// Restrict to a specific ADNet endpoint id (the `from-adnet` option).
    pub from_adnet_id: Option<iroh::EndpointId>,
}

/// Loaded set of authorized keys.
///
/// Parses `$DATA_DIR/ssh/authorized_keys` on construction. The file is
/// reloaded lazily on each [`check`](AuthorizedKeys::check) call so
/// edits take effect without restarting the server.
#[derive(Debug, Clone)]
pub struct AuthorizedKeys {
    /// Path to the authorized_keys file (may not exist).
    data_dir: PathBuf,
}

impl AuthorizedKeys {
    /// Construct from a data directory root. Does not check the file
    /// exists; [`check`](Self::check) returns `false` for all peers
    /// if the file is missing.
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
        }
    }

    /// Path this instance reads from.
    pub fn path(&self) -> PathBuf {
        self.data_dir.join(AUTHORIZED_KEYS_FILE)
    }

    /// Parse every line of `$data_dir/ssh/authorized_keys`.
    ///
    /// Returns an empty vector if the file does not exist or cannot be
    /// read. Parse errors are logged and skipped — a malformed line
    /// does not abort the load.
    pub fn load(&self) -> Vec<AuthorizedKeyEntry> {
        let path = self.path();
        let file = match fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                tracing::debug!("adnet-ssh: failed to open authorized_keys at {path:?}: {e}");
                return Vec::new();
            }
        };
        let mut entries = Vec::new();
        for (line_no, line) in io::BufReader::new(file).lines().enumerate() {
            match line {
                Ok(l) => match parse_one_authorized_key(&l) {
                    Some(entry) => entries.push(entry),
                    None => {
                        // Blank or comment-only line — skip silently.
                    }
                },
                Err(e) => {
                    tracing::debug!(
                        "adnet-ssh: skipping line {line_no} in authorized_keys: {e}"
                    );
                }
            }
        }
        tracing::debug!(
            "adnet-ssh: loaded {} authorized_keys entries from {:?}",
            entries.len(),
            path
        );
        entries
    }

    /// Check whether a connecting peer with the given public key blob
    /// and endpoint id is permitted by this file.
    ///
    /// Returns `true` if the peer matches any non-expired entry in the
    /// file. Returns `false` if the file is absent, empty, or contains
    /// no matching entry.
    ///
    /// The `from-adnet` option on an entry takes priority: if an entry
    /// specifies `from-adnet="<peer-id>"` and the connecting peer's
    /// endpoint id does not match, the entry is skipped even if the
    /// key blob matches.
    ///
    /// # Security notes
    ///
    /// - This checks the *key blob*, not the `authorized_keys` file
    ///   verbatim. Callers must pass the peer public key as bytes.
    /// - Expiry options (`valid-from`, `valid-to`) are not yet
    ///   implemented.
    pub fn check(&self, key_blob: &[u8], peer_endpoint_id: iroh::EndpointId) -> bool {
        let entries = self.load();
        for entry in entries {
            // `from-adnet` is an ADNet-specific restriction: skip if the
            // peer endpoint id does not match.
            if let Some(ref required) = entry.from_adnet_id
                && *required != peer_endpoint_id
            {
                continue;
            }
            // Base64-decode the stored blob and compare.
            if let Some(decoded) = base64_decode(&entry.key_blob)
                && decoded == key_blob
            {
                return true;
            }
        }
        false
    }

    /// Ensure the `ssh/` subdirectory and `authorized_keys` file
    /// exist (creating them empty if absent).
    ///
    /// Returns `Ok(())` if the directory/file are present or were
    /// created. Returns an error if directory creation fails.
    pub fn ensure(&self) -> io::Result<()> {
        let ssh_dir = self.data_dir.join("ssh");
        fs::create_dir_all(&ssh_dir)?;
        let file_path = self.path();
        if !file_path.exists() {
            fs::File::create(&file_path)?;
            // Write a header explaining the format.
            if let Ok(mut f) = fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&file_path)
            {
                let _ = writeln!(
                    f,
                    "# ADNet SSH authorized keys\n\
                    # Format: one OpenSSH authorized_keys line per peer.\n\
                    # Supported ADNet-specific options:\n\
                    #   from-adnet=<endpoint-id>  — restrict to a specific ADNet peer\n\
                    # Lines beginning with # are comments.\n"
                );
            }
        }
        Ok(())
    }

    /// Append an `authorized_keys` entry for a peer's endpoint id.
    ///
    /// Adds a line of the form:
    #[cfg_attr(docsrs, doc(cfg(feature = "iroh")))]
    /// ```ignore
    /// from-adnet=<endpoint-id> ssh-ed25519 <base64-blob> <comment>
    /// ```
    ///
    /// If the file does not exist it is created (including the `ssh/`
    /// parent directory).
    ///
    /// Returns `Ok(())` on success.
    pub fn add_peer(&self, peer_endpoint_id: iroh::EndpointId, key_blob: &[u8], comment: &str) -> io::Result<()> {
        self.ensure()?;
        let mut file = fs::OpenOptions::new().append(true).open(self.path())?;
        let blob_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, key_blob);
        writeln!(
            file,
            "from-adnet={} ssh-ed25519 {} {}",
            peer_endpoint_id,
            blob_b64,
            comment
        )?;
        tracing::info!(
            "adnet-ssh: added authorized_keys entry for peer {}",
            peer_endpoint_id
        );
        Ok(())
    }
}

/// Parse a single non-comment, non-blank line into an [`AuthorizedKeyEntry`].
pub(crate) fn parse_one_authorized_key(line: &str) -> Option<AuthorizedKeyEntry> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    // Split options from the rest at the first key-type prefix in the line.
    // SSH authorized_keys lines look like: [options] key-type base64-blob [comment]
    // where the key-type always starts with "ssh-", "ecdsa-", "sk-", or "rsa-".
    // We scan for the first occurrence of any of these prefixes and split there.
    let key_type_pos = line.find("ssh-").or_else(|| line.find("ecdsa-"))
        .or_else(|| line.find("sk-")).or_else(|| line.find("rsa-"));
    let (options_str, rest) = match key_type_pos {
        Some(pos) => (&line[..pos], line[pos..].trim_start()),
        None => (line, ""),
    };

    let mut from_pattern = None;
    let mut from_adnet_id = None;

    // Parse quoted options: name="value".
    if let Ok(quoted_re) =
        regex::Regex::new(r#"([a-zA-Z0-9._-]+)="([^"]*)""#)
    {
        for cap in quoted_re.captures_iter(options_str) {
            let name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let value = cap.get(2).map(|m| m.as_str()).unwrap_or("");
            if name == "from" {
                from_pattern = Some(value.to_string());
            } else if name == "from-adnet"
                && let Ok(id) = value.parse::<iroh::EndpointId>()
            {
                from_adnet_id = Some(id);
            }
        }
    }
    // Parse unquoted options: name=value.
    if let Ok(unquoted_re) = regex::Regex::new(r#"([a-zA-Z0-9._-]+)=([^\s,]+)"#) {
    for cap in unquoted_re.captures_iter(options_str) {
        let name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let value = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        // Skip if the value contains quotes — it should have been handled
        // by the quoted regex instead.
        if value.contains('"') {
            continue;
        }
        if name == "from" {
            from_pattern = Some(value.to_string());
        } else if name == "from-adnet"
            && let Ok(id) = value.parse::<iroh::EndpointId>()
        {
            from_adnet_id = Some(id);
        }
    }
    }

    // Parse key-type, key-blob, and comment from the remainder.
    // Split on whitespace — the first token is the key type, the second
    // is the base64 blob, everything after is the comment.
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    let key_type = tokens[0].to_string();
    let key_blob = tokens[1].to_string();
    let comment = if tokens.len() > 2 {
        Some(tokens[2..].join(" "))
    } else {
        None
    };

    Some(AuthorizedKeyEntry {
        key_type,
        key_blob,
        comment,
        from_pattern,
        from_adnet_id,
    })
}

/// Base64 decode a string, returning `None` on invalid input.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(input).ok()
}

/// Load the base64 standard engine lazily so we don't pull in the
/// `base64` crate for the no-iroh build path.
#[cfg(not(feature = "iroh"))]
fn base64_decode(_input: &str) -> Option<Vec<u8>> {
    None
}

// -------------------------------------------------------------------------
// Trusted-peers support
// -------------------------------------------------------------------------

/// A set of endpoint IDs that are permitted to connect even without
/// a matching `authorized_keys` entry.
///
/// The file is `$data_dir/ssh/trusted_peers`; one
/// [`iroh::EndpointId`] per line. Lines starting with `#` are
/// comments; blank lines are ignored.
///
/// This is a convenience layer for a "friends list" model: two nodes
/// mutually add each other's endpoint IDs to each other's trusted list,
/// and they can then open tunnels to each other without going through
/// `authorized_keys`.
///
/// Unlike [`AuthorizedKeys`], the trusted-peers file is cached after
/// the first load and refreshed periodically via [`check`](TrustedPeers::check).
///
/// Shared cheaply via `Clone` (each clone bumps the inner `Arc`).
#[derive(Debug, Clone)]
pub struct TrustedPeers {
    /// Immutable path; cloning the struct shares the same file.
    path: PathBuf,
    /// In-memory cache of the last loaded set. Refreshed lazily.
    /// Wrapped in `Arc` so clones share the same cache.
    cache: Arc<parking_lot::RwLock<HashSet<iroh::EndpointId>>>,
    /// Unix timestamp (seconds) of the last successful load.
    last_load: Arc<parking_lot::RwLock<u64>>,
}

impl TrustedPeers {
    /// Construct from a data directory root. Does not load the file
    /// until the first call to [`check`](Self::check).
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join(TRUSTED_PEERS_FILE),
            cache: Arc::new(parking_lot::RwLock::new(HashSet::new())),
            last_load: Arc::new(parking_lot::RwLock::new(0)),
        }
    }

    /// Path this instance reads from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns `true` if the given endpoint id is in the trusted-peers
    /// file. The file is reloaded if more than [`RELOAD_INTERVAL_SECS`]
    /// seconds have elapsed since the last load.
    ///
    /// Returns `false` if the file does not exist.
    pub fn check(&self, endpoint_id: iroh::EndpointId) -> bool {
        self.refresh_if_stale();
        self.cache.read().contains(&endpoint_id)
    }

    /// Force a reload of the trusted-peers file, ignoring the
    /// stale timer. Useful after an external edit.
    pub fn reload(&self) {
        self.do_load();
    }

    /// Add an endpoint id to the trusted-peers file.
    ///
    /// Creates the file (and the `ssh/` directory) if it does not
    /// exist. The new entry is appended as a single line.
    ///
    /// Returns `Ok(())` on success.
    pub fn add(&self, endpoint_id: iroh::EndpointId) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{endpoint_id}")?;
        // Update the cache immediately so the next `check` returns true.
        self.cache.write().insert(endpoint_id);
        tracing::info!(
            "adnet-ssh: added {} to trusted_peers",
            endpoint_id
        );
        Ok(())
    }

    /// Remove an endpoint id from the trusted-peers file.
    ///
    /// Rewrites the file without the given id. Returns `Ok(true)` if
    /// the id was present and removed; `Ok(false)` if it was not in
    /// the file.
    pub fn remove(&self, endpoint_id: iroh::EndpointId) -> io::Result<bool> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        let lines: Vec<&str> = contents.lines().collect();
        let found = lines.iter().any(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && !trimmed.starts_with('#')
                && trimmed.parse::<iroh::EndpointId>().is_ok_and(|id| id == endpoint_id)
        });
        if !found {
            return Ok(false);
        }
        let new_contents: String = lines
            .iter()
            .filter(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    return true;
                }
                trimmed.parse::<iroh::EndpointId>().map_or(true, |id| id != endpoint_id)
            })
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&self.path, new_contents)?;
        self.cache.write().remove(&endpoint_id);
        tracing::info!(
            "adnet-ssh: removed {} from trusted_peers",
            endpoint_id
        );
        Ok(true)
    }

    /// Returns all currently trusted endpoint ids.
    pub fn list(&self) -> Vec<iroh::EndpointId> {
        self.refresh_if_stale();
        self.cache.read().iter().copied().collect()
    }

    /// How often (in seconds) to refresh the in-memory cache from disk.
    const RELOAD_INTERVAL_SECS: u64 = 30;

    /// Check whether the cache is stale and reload if so.
    fn refresh_if_stale(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let last = *self.last_load.read();
        if now.saturating_sub(last) >= Self::RELOAD_INTERVAL_SECS {
            *self.last_load.write() = now;
            self.do_load();
        }
    }

    fn do_load(&self) {
        let file = match fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                *self.cache.write() = HashSet::new();
                return;
            }
            Err(e) => {
                tracing::debug!(
                    "adnet-ssh: failed to open trusted_peers at {:?}: {e}",
                    self.path
                );
                return;
            }
        };
        let mut set = HashSet::new();
        for line in io::BufReader::new(file).lines().map_while(Result::ok) {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Ok(id) = trimmed.parse::<iroh::EndpointId>() {
                set.insert(id);
            }
        }
        tracing::debug!(
            "adnet-ssh: loaded {} trusted peers from {:?}",
            set.len(),
            self.path
        );
        *self.cache.write() = set;
    }
}

}
#[cfg(all(test, feature = "iroh"))]
mod tests {
    use super::authorized_keys::*;

    #[test]
    fn parse_authorized_key_with_adnet_option() {
        let line =
            "from-adnet=38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac \
             ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiH \
             alice@peer";
        let entry = parse_one_authorized_key(line).unwrap();
        assert_eq!(entry.key_type, "ssh-ed25519");
        assert_eq!(entry.key_blob, "AAAAC3NzaC1lZDI1NTE5AAAAILiH");
        assert_eq!(entry.comment.as_deref(), Some("alice@peer"));
        let expected_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
            .parse()
            .unwrap();
        assert_eq!(entry.from_adnet_id, Some(expected_id));
        assert!(entry.from_pattern.is_none());
    }

    #[test]
    fn parse_authorized_key_with_from_option() {
        // from="pattern" is quoted; the comma separating options is outside.
        let line = r#"from="10.0.0.0/8", ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAC4 alice@internal"#;
        let entry = parse_one_authorized_key(line).unwrap();
        assert_eq!(entry.key_type, "ssh-ed25519");
        assert_eq!(entry.key_blob, "AAAAC3NzaC1lZDI1NTE5AAAAC4");
        assert_eq!(entry.from_pattern.as_deref(), Some("10.0.0.0/8"));
        assert!(entry.from_adnet_id.is_none());
    }

    #[test]
    fn parse_authorized_key_empty_line() {
        assert!(parse_one_authorized_key("").is_none());
        assert!(parse_one_authorized_key("   ").is_none());
        assert!(parse_one_authorized_key("# this is a comment").is_none());
    }

    #[test]
    fn parse_authorized_key_no_options() {
        let line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 comment";
        let entry = parse_one_authorized_key(line).unwrap();
        assert_eq!(entry.key_type, "ssh-ed25519");
        assert!(entry.from_pattern.is_none());
        assert!(entry.from_adnet_id.is_none());
        assert_eq!(entry.comment.as_deref(), Some("comment"));
    }

    #[test]
    fn trusted_peers_load_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let tp = TrustedPeers::new(tmp.path());
        assert!(tp.check(
            "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
                .parse()
                .unwrap()
        ) == false);
    }

    #[test]
    fn authorized_keys_load_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ak = AuthorizedKeys::new(tmp.path());
        let entries = ak.load();
        assert!(entries.is_empty(), "empty file should produce no entries");
    }

    #[test]
    fn authorized_keys_check_no_match() {
        let tmp = tempfile::tempdir().unwrap();
        let ak = AuthorizedKeys::new(tmp.path());
        let fake_key = [0u8; 32];
        let fake_ep: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
            .parse()
            .unwrap();
        assert!(!ak.check(&fake_key, fake_ep), "unknown key should not match");
    }

    #[test]
    fn authorized_keys_ensure_creates_ssh_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ak = AuthorizedKeys::new(tmp.path());
        ak.ensure().unwrap();
        assert!(tmp.path().join("ssh").is_dir(), "ssh/ directory should be created");
        assert!(ak.path().is_file(), "authorized_keys file should be created");
    }

    #[test]
    fn authorized_keys_load_with_multiple_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let ak = AuthorizedKeys::new(tmp.path());
        ak.ensure().unwrap();
        // Overwrite with two entries.
        std::fs::write(
            ak.path(),
            "ssh-ed25519 AAAAB3NzaC1yc2EAAAADAQABAAABAQ alice@host1\n\
             ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiH bob@host2\n",
        )
        .unwrap();
        let entries = ak.load();
        assert_eq!(entries.len(), 2, "should parse two entries");
        assert_eq!(entries[0].comment.as_deref(), Some("alice@host1"));
        assert_eq!(entries[1].comment.as_deref(), Some("bob@host2"));
    }

    #[test]
    fn trusted_peers_add_and_check() {
        let tmp = tempfile::tempdir().unwrap();
        let tp = TrustedPeers::new(tmp.path());
        let ep: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
            .parse()
            .unwrap();
        tp.add(ep).unwrap();
        assert!(tp.check(ep), "added peer should be trusted");
        assert!(!tp.check(
            "bb8e1a5661a6dfa9ae2dd978922f30f524f6fd8c99b3de021c53f292aae74330"
                .parse()
                .unwrap()
        ));
    }

    #[test]
    fn trusted_peers_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let tp = TrustedPeers::new(tmp.path());
        let ep: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
            .parse()
            .unwrap();
        tp.add(ep).unwrap();
        assert!(tp.remove(ep).unwrap(), "first remove should return true");
        assert!(!tp.check(ep), "check should return false after removal");
        assert!(!tp.remove(ep).unwrap(), "second remove should return false");
    }

    #[test]
    fn trusted_peers_list() {
        let tmp = tempfile::tempdir().unwrap();
        let tp = TrustedPeers::new(tmp.path());
        let ep1: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
            .parse()
            .unwrap();
        let ep2: iroh::EndpointId = "bb8e1a5661a6dfa9ae2dd978922f30f524f6fd8c99b3de021c53f292aae74330"
            .parse()
            .unwrap();
        tp.add(ep1).unwrap();
        tp.add(ep2).unwrap();
        let list = tp.list();
        assert_eq!(list.len(), 2);
        assert!(list.contains(&ep1));
        assert!(list.contains(&ep2));
    }

    #[test]
    fn authorized_keys_add_peer() {
        let tmp = tempfile::tempdir().unwrap();
        let ak = AuthorizedKeys::new(tmp.path());
        let ep: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
            .parse()
            .unwrap();
        ak.add_peer(ep, &[0xAA; 32], "test-peer").unwrap();
        let content = std::fs::read_to_string(ak.path()).unwrap();
        assert!(content.contains("from-adnet="), "should contain from-adnet option");
        assert!(content.contains("ssh-ed25519"), "should contain key type");
    }

    #[test]
    fn parse_authorized_key_with_multiple_options() {
        // from-adnet and from= together.
        let line = r#"from-adnet=38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac, from="192.168.1.0/24" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAC4 test"#;
        let entry = parse_one_authorized_key(line).unwrap();
        assert_eq!(entry.key_type, "ssh-ed25519");
        assert!(entry.from_pattern.is_some());
        assert!(entry.from_adnet_id.is_some());
    }

    #[test]
    fn authorized_keys_skip_comments_and_blanks() {
        let tmp = tempfile::tempdir().unwrap();
        let ak = AuthorizedKeys::new(tmp.path());
        ak.ensure().unwrap();
        std::fs::write(
            ak.path(),
            "# this is a comment\n\
             \n\
             ssh-ed25519 AAAAB3NzaC1yc2EAAAADAQABAAABAQ alice@host\n\
             # another comment\n",
        )
        .unwrap();
        let entries = ak.load();
        assert_eq!(entries.len(), 1, "only non-blank, non-comment lines should be parsed");
    }
}
