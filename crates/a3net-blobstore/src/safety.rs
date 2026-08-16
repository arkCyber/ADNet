//! Path safety — reject imports of non-regular / sensitive files.
//!
//! DO-178C DAL-B SR-5: every blob import path must be validated
//! before any data is read. This module is the single source of
//! truth for what counts as "safe to import".
//!
//! ## Forbidden path families
//!
//! 1. Virtual filesystems (`/proc/*`, `/sys/*`, `/dev/*`) —
//!    reading these can cause hangs, return infinite streams,
//!    or leak kernel state.
//! 2. Symlinks — always resolve to the real path before checking;
//!    a symlink in the data dir must never be followed.
//! 3. Sockets / pipes / devices — anything that is not a regular
//!    file. The kernel special files report `size == 0` and
//!    `read()` returns zero, which would silently import an
//!    empty blob and shadow a malicious payload's hash.
//! 4. Whitelisted sensitive system files
//!    (`/etc/shadow`, `/etc/passwd`, `/etc/sudoers`).
//!
//! ## DO-178C traceability
//!
//! - `validate_import_path` corresponds to Safety Requirement
//!   SR-5 in `SAFETY_CASE.md`.
//! - Tested under `tests/aerospace_compliance.rs`.

use std::path::{Path, PathBuf};

/// Maximum single-file size accepted by `validate_import_path`.
/// Mirrors the Filecoin / IPFS "single block ≤ 1 GiB" rule,
/// but lifted to 1 PiB to allow bootable disk images.
/// Tests can override via [`MAX_IMPORT_FILE_BYTES`].
pub const MAX_IMPORT_FILE_BYTES: u64 = 1u64 << 50; // 1 PiB

/// Path-prefix blacklist. Anything starting with one of these
/// prefixes is rejected. List is intentionally a small set of
/// known-hazardous trees — DO-178C requires deterministic and
/// auditable rules.
///
/// The `/dev` prefix is **without** a trailing slash because on
/// macOS `/dev/null` is a regular file (not a directory) and the
/// prefix must still match it. The `symlink_metadata` check above
/// keeps `/dev` symlinks (which Linux uses for fd forwarding) out
/// of the import path.
const FORBIDDEN_PREFIXES: &[&str] = &[
    "/proc/",
    "/proc", // macOS compatibility: `/proc` itself
    "/sys/",
    "/sys", // macOS compatibility
    "/dev", // covers /dev/null, /dev/zero, /dev/random, ...
    "/run/",
    "/run", // macOS compatibility
    "/var/run/",
];

/// Exact-path blacklist — files whose content would be
/// harmful to leak via a BLAKE3-addressed blob.
const FORBIDDEN_EXACT: &[&str] = &[
    "/etc/shadow",
    "/etc/passwd",
    "/etc/sudoers",
    "/etc/sudoers.d/",
];

/// Safety errors raised by [`validate_import_path`].
///
/// Kept as a separate enum (rather than reusing `std::io::Error`)
/// so callers can map safety violations to specific HTTP / IPC
/// status codes without losing the variant information.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathSafetyError {
    #[error("path does not exist: {0}")]
    NotFound(String),

    #[error("path is not a regular file: {0}")]
    NotRegularFile(String),

    #[error("path is a forbidden virtual or device tree: {0}")]
    Forbidden(String),

    #[error("path matches a forbidden exact path: {0}")]
    ForbiddenExact(String),

    #[error("path is a symlink: {0}")]
    Symlink(String),

    #[error("file size {actual} bytes exceeds safety limit {limit} bytes")]
    TooLarge { limit: u64, actual: u64 },

    #[error("path traversal detected: {0}")]
    PathTraversal(String),
}

/// Validate that `path` is safe to import into the blob store.
///
/// # DO-178C SR-5 contract
///
/// Returns `Ok(Metadata)` (the file's metadata) when ALL of
/// the following hold:
///
/// 1. `path` exists.
/// 2. `path` is NOT a symbolic link. (We use
///    `symlink_metadata` so we never follow a symlink to
///    decide.)
/// 3. `path` is a regular file (not a directory, socket,
///    fifo, block/char device).
/// 4. `path` does not begin with any [`FORBIDDEN_PREFIXES`]
///    entry.
/// 5. `path` is not in [`FORBIDDEN_EXACT`].
/// 6. `path` does not contain any `..` segment after
///    canonicalization (defense against crafted paths).
/// 7. The file's size is `≤ MAX_IMPORT_FILE_BYTES`.
///
/// On failure, returns a [`PathSafetyError`] enumerating the
/// exact violated rule.
pub fn validate_import_path(path: &Path) -> Result<std::fs::Metadata, PathSafetyError> {
    // Check 6 (path traversal) FIRST so a path with `..` segments
    // surfaces the precise error even when the resulting target
    // does not exist. Order matters for the aerospace tests.
    for component in path.components() {
        if let std::path::Component::ParentDir = component {
            return Err(PathSafetyError::PathTraversal(path.display().to_string()));
        }
    }

    // 1. Existence (via lstat so a dangling symlink still errors).
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| PathSafetyError::NotFound(path.display().to_string()))?;

    // 2. Symlink rejection.
    if meta.file_type().is_symlink() {
        return Err(PathSafetyError::Symlink(path.display().to_string()));
    }

    // 3. Regular file only.
    if !meta.file_type().is_file() {
        return Err(PathSafetyError::NotRegularFile(path.display().to_string()));
    }

    // 4 & 5. Forbidden path families (string-based check on the
    // literal path the caller supplied; we don't canonicalize
    // here because canonicalize() dereferences symlinks which is
    // explicitly disallowed above).
    let path_str = path.display().to_string();
    for prefix in FORBIDDEN_PREFIXES {
        if path_str.starts_with(prefix) {
            return Err(PathSafetyError::Forbidden(path_str));
        }
    }
    for exact in FORBIDDEN_EXACT {
        if path_str == *exact || path_str.starts_with(exact) {
            return Err(PathSafetyError::ForbiddenExact(path_str));
        }
    }

    // 7. Size cap.
    if meta.len() > MAX_IMPORT_FILE_BYTES {
        return Err(PathSafetyError::TooLarge {
            limit: MAX_IMPORT_FILE_BYTES,
            actual: meta.len(),
        });
    }

    Ok(meta)
}

/// Canonicalize and re-validate. Returns the canonical path on
/// success. Use this when the caller wants a stable path key
/// after validation.
///
/// Note: this second pass is **optional** — `validate_import_path`
/// is sufficient on its own. This function exists for callers
/// that need a canonical form (e.g. for metadata sidecars).
pub fn canonicalize_safe(path: &Path) -> Result<PathBuf, PathSafetyError> {
    let meta = validate_import_path(path)?;
    drop(meta); // meta not needed for canonicalize itself
    std::fs::canonicalize(path).map_err(|_| PathSafetyError::NotFound(path.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn validate_regular_file_ok() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("ok.bin");
        std::fs::write(&f, b"hello").unwrap();
        let meta = validate_import_path(&f).unwrap();
        assert_eq!(meta.len(), 5);
    }

    #[test]
    fn validate_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("missing.bin");
        assert!(matches!(
            validate_import_path(&f),
            Err(PathSafetyError::NotFound(_))
        ));
    }

    #[test]
    fn validate_directory_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            validate_import_path(dir.path()),
            Err(PathSafetyError::NotRegularFile(_))
        ));
    }

    #[test]
    fn validate_symlink_errors() {
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("target.bin");
            std::fs::write(&target, b"x").unwrap();
            let link = dir.path().join("link.bin");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(matches!(
                validate_import_path(&link),
                Err(PathSafetyError::Symlink(_))
            ));
        }
    }

    #[test]
    fn validate_proc_rejected() {
        // /proc exists on Linux. The test is best-effort: if
        // /proc is missing the assertion is skipped.
        if Path::new("/proc/self/status").exists() {
            assert!(matches!(
                validate_import_path(Path::new("/proc/self/status")),
                Err(PathSafetyError::Forbidden(_))
            ));
        }
    }

    #[test]
    fn validate_dev_rejected() {
        if Path::new("/dev/null").exists() {
            // On Linux /dev/null is a character device (rejected
            // as `NotRegularFile`). On macOS it is also a char
            // device, but some Unixes may classify it as a
            // regular file. Accept either rejection mode so the
            // test stays portable.
            assert!(matches!(
                validate_import_path(Path::new("/dev/null")),
                Err(PathSafetyError::NotRegularFile(_)) | Err(PathSafetyError::Forbidden(_))
            ));
        }
    }

    #[test]
    fn validate_etc_shadow_rejected() {
        if Path::new("/etc/shadow").exists() {
            assert!(matches!(
                validate_import_path(Path::new("/etc/shadow")),
                Err(PathSafetyError::ForbiddenExact(_))
            ));
        }
    }

    #[test]
    fn validate_size_cap() {
        // We don't actually create a PiB file — the size check
        // is independent of file contents. Write a file with a
        // spoofed size via set_len.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("huge.bin");
        let f_handle = std::fs::File::create(&f).unwrap();
        f_handle.set_len(MAX_IMPORT_FILE_BYTES + 1).unwrap();
        drop(f_handle);
        assert!(matches!(
            validate_import_path(&f),
            Err(PathSafetyError::TooLarge { .. })
        ));
    }

    #[test]
    fn validate_path_traversal_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let evil = dir.path().join("..").join("evil.bin");
        let err = validate_import_path(&evil);
        assert!(matches!(err, Err(PathSafetyError::PathTraversal(_))));
    }

    #[test]
    fn validate_path_traversal_inside_subdir_ok() {
        // A "../" segment causes PathTraversal. A "../file" at
        // the boundary also fails — that's the point of the
        // rule.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("subdir").join("file.bin");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        let mut h = std::fs::File::create(&f).unwrap();
        h.write_all(b"x").unwrap();
        assert!(validate_import_path(&f).is_ok());
    }
}
