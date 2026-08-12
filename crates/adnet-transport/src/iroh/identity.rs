//! Persistent Ed25519 identity for iroh endpoints.
//!
//! An iroh endpoint is authenticated by an Ed25519 [`iroh::SecretKey`].
//! This module makes that key the durable identity root instead of treating an
//! arbitrary ADNet `NodeId` or a native-QUIC certificate fingerprint as an
//! iroh identity.
//!
//! # Safety properties
//!
//! - Exactly 32 secret bytes are stored; malformed files are rejected.
//! - Existing symlinks and non-regular files are rejected (fail closed).
//! - On Unix, key files must be owner-only and new files are created as `0600`.
//! - Creation is atomic and race-safe: a unique temporary file is fsynced and
//!   installed using a hard link, so concurrent creators cannot overwrite the
//!   winning identity.
//! - Secret byte buffers are zeroized after use.

#![cfg(feature = "iroh")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use adnet_types::NodeId;
use zeroize::Zeroize;

use crate::traits::{TransportError, TransportResult};

/// Default filename below an ADNet node's data directory.
pub const IROH_SECRET_KEY_FILE: &str = "iroh_secret_key";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Durable Ed25519 identity used to construct an iroh endpoint.
#[derive(Clone)]
pub struct IrohIdentity {
    secret_key: iroh::SecretKey,
    path: PathBuf,
}

impl std::fmt::Debug for IrohIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IrohIdentity")
            .field("endpoint_id", &self.endpoint_id())
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl IrohIdentity {
    /// Load an existing identity, rejecting malformed or insecure files.
    pub fn load(path: impl AsRef<Path>) -> TransportResult<Self> {
        let path = path.as_ref();
        validate_identity_file(path)?;

        let mut bytes = std::fs::read(path).map_err(|e| {
            TransportError::Identity(format!("read iroh identity {}: {e}", path.display()))
        })?;
        if bytes.len() != 32 {
            let actual = bytes.len();
            bytes.zeroize();
            return Err(TransportError::Identity(format!(
                "iroh identity {} must contain exactly 32 bytes, got {actual}",
                path.display()
            )));
        }

        let mut secret = [0u8; 32];
        secret.copy_from_slice(&bytes);
        bytes.zeroize();
        let secret_key = iroh::SecretKey::from_bytes(&secret);
        secret.zeroize();

        Ok(Self {
            secret_key,
            path: path.to_path_buf(),
        })
    }

    /// Load `data_dir/iroh_secret_key`, or create it atomically.
    ///
    /// Concurrent callers converge on exactly one identity. A caller that loses
    /// the installation race discards its generated key and loads the winner.
    pub fn load_or_create(data_dir: impl AsRef<Path>) -> TransportResult<Self> {
        let data_dir = data_dir.as_ref();
        create_private_dir(data_dir)?;
        let path = data_dir.join(IROH_SECRET_KEY_FILE);

        match std::fs::symlink_metadata(&path) {
            Ok(_) => return Self::load(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(TransportError::Identity(format!(
                    "inspect iroh identity {}: {e}",
                    path.display()
                )));
            }
        }

        let generated = iroh::SecretKey::generate();
        let mut secret = generated.to_bytes();
        let tmp = unique_temp_path(data_dir);
        if let Err(e) = write_new_private_file(&tmp, &secret) {
            secret.zeroize();
            return Err(e);
        }
        secret.zeroize();

        match std::fs::hard_link(&tmp, &path) {
            Ok(()) => {
                remove_temp(&tmp);
                sync_directory(data_dir)?;
                // Read back from the durable representation, validating both
                // bytes and permissions rather than trusting the prior write.
                Self::load(path)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                remove_temp(&tmp);
                Self::load(path)
            }
            Err(e) => {
                remove_temp(&tmp);
                Err(TransportError::Identity(format!(
                    "install iroh identity {}: {e}",
                    path.display()
                )))
            }
        }
    }

    /// Wrap a caller-managed `iroh::SecretKey` without writing to disk.
    /// Useful for tests, for fixtures, and for callers that store the
    /// secret elsewhere (e.g. an HSM, KMS, or encrypted volume). The
    /// resulting identity's `endpoint_id` equals `secret.public()` and
    /// its `path()` returns `None`.
    pub fn from_secret(secret_key: iroh::SecretKey) -> Self {
        Self {
            secret_key,
            path: PathBuf::new(),
        }
    }

    /// Endpoint id derived from the persistent Ed25519 secret.
    pub fn endpoint_id(&self) -> iroh::EndpointId {
        self.secret_key.public()
    }

    /// ADNet `NodeId` containing the exact EndpointId public-key bytes.
    pub fn node_id(&self) -> NodeId {
        super::public_key_to_node_id(&self.endpoint_id())
    }

    /// Location of the persistent key file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Clone the zeroizing secret-key handle for an endpoint builder.
    pub fn secret_key(&self) -> iroh::SecretKey {
        self.secret_key.clone()
    }
}

fn unique_temp_path(dir: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(
        ".{IROH_SECRET_KEY_FILE}.{}.{}.tmp",
        std::process::id(),
        counter
    ))
}

fn create_private_dir(path: &Path) -> TransportResult<()> {
    std::fs::create_dir_all(path).map_err(|e| {
        TransportError::Identity(format!("create identity directory {}: {e}", path.display()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            TransportError::Identity(format!(
                "set identity directory permissions {}: {e}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

fn write_new_private_file(path: &Path, bytes: &[u8; 32]) -> TransportResult<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path).map_err(|e| {
        TransportError::Identity(format!("create identity temp file {}: {e}", path.display()))
    })?;
    file.write_all(bytes).map_err(|e| {
        TransportError::Identity(format!("write identity temp file {}: {e}", path.display()))
    })?;
    file.sync_all().map_err(|e| {
        TransportError::Identity(format!("sync identity temp file {}: {e}", path.display()))
    })
}

fn validate_identity_file(path: &Path) -> TransportResult<()> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        TransportError::Identity(format!("inspect iroh identity {}: {e}", path.display()))
    })?;
    if !meta.file_type().is_file() || meta.file_type().is_symlink() {
        return Err(TransportError::Identity(format!(
            "iroh identity {} must be a regular non-symlink file",
            path.display()
        )));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(TransportError::Identity(format!(
                "iroh identity {} has insecure permissions {mode:o}; expected owner-only access",
                path.display()
            )));
        }
    }
    Ok(())
}

fn remove_temp(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), "failed to remove identity temp file: {e}");
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> TransportResult<()> {
    let dir = std::fs::File::open(path).map_err(|e| {
        TransportError::Identity(format!("open identity directory {}: {e}", path.display()))
    })?;
    dir.sync_all().map_err(|e| {
        TransportError::Identity(format!("sync identity directory {}: {e}", path.display()))
    })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> TransportResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_across_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let first = IrohIdentity::load_or_create(dir.path()).unwrap();
        let second = IrohIdentity::load_or_create(dir.path()).unwrap();
        assert_eq!(first.endpoint_id(), second.endpoint_id());
        assert_eq!(first.node_id().as_bytes(), first.endpoint_id().as_bytes());
        assert_eq!(std::fs::metadata(first.path()).unwrap().len(), 32);
    }

    #[test]
    fn malformed_identity_fails_closed_without_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(IROH_SECRET_KEY_FILE);
        std::fs::write(&path, b"truncated").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let err = IrohIdentity::load_or_create(dir.path()).unwrap_err();
        assert!(matches!(err, TransportError::Identity(_)));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn concurrent_creators_converge_on_one_endpoint_id() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::sync::Arc::new(dir.path().to_path_buf());
        let mut workers = Vec::new();
        for _ in 0..16 {
            let root = root.clone();
            workers.push(std::thread::spawn(move || {
                IrohIdentity::load_or_create(root.as_path())
                    .unwrap()
                    .endpoint_id()
            }));
        }
        let ids: Vec<_> = workers.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(ids.iter().all(|id| *id == ids[0]));
        let leftovers = std::fs::read_dir(root.as_path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[cfg(unix)]
    #[test]
    fn insecure_permissions_are_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let identity = IrohIdentity::load_or_create(dir.path()).unwrap();
        std::fs::set_permissions(identity.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = IrohIdentity::load(identity.path()).unwrap_err();
        assert!(matches!(err, TransportError::Identity(_)));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_identity_is_rejected() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, [7u8; 32]).unwrap();
        let path = dir.path().join(IROH_SECRET_KEY_FILE);
        symlink(&target, &path).unwrap();
        let err = IrohIdentity::load_or_create(dir.path()).unwrap_err();
        assert!(matches!(err, TransportError::Identity(_)));
    }
}
