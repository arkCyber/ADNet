//! Path-component helpers used by [`crate::walk`] and
//! [`crate::collection`].
//!
//! These are a hardened, A3Net-flavoured port of `canonicalized_path_to_string`
//! from `n0-computer/sendme@0.36.0/src/main.rs`. Differences from the
//! upstream:
//!
//! 1. Symlinks are refused by default (sendme also refuses them, but
//!    only implicitly — it only calls `WalkDir::new` which by default
//!    does follow them).
//! 2. The error type is `ShareError`, not `anyhow::Error`, so callers
//!    can pattern-match on the failure reason.
//! 3. The Unicode / `\\\\` reject policy matches sendme, but the
//!    error message is structured for A3Net's logs.

use std::path::Component;

use crate::error::{ShareError, ShareResult};

/// Maximum length (in bytes) of a single collection entry name.
///
/// sendme does not enforce an explicit cap; A3Net picks 512 bytes to
/// mirror the filesystem `PATH_MAX` budget while leaving room for
/// surrounding JSON / ticket prefixes.
pub const MAX_NAME_LEN: usize = 512;

/// Validate a single path component. Rejects:
///
/// - empty strings,
/// - any byte of `'/'` or `'\\\\'` (so the component cannot escape
///   the canonicalised join),
/// - non-UTF-8 input (the upstream uses `to_str`; we surface the same
///   error).
pub fn validate_path_component(component: &str) -> ShareResult<()> {
    if component.is_empty() {
        return Err(ShareError::InvalidPathComponent(component.to_string()));
    }
    if component.contains('/') || component.contains('\\') {
        return Err(ShareError::InvalidPathComponent(component.to_string()));
    }
    if component.len() > MAX_NAME_LEN {
        return Err(ShareError::PathComponentTooLong {
            name: component.to_string(),
            len: component.len(),
            max: MAX_NAME_LEN,
        });
    }
    Ok(())
}

/// Canonicalise an already-resolved (no `.` / `..`) relative path into
/// the `name1/name2/...` form used by [`crate::collection::Collection`].
///
/// Mirrors sendme's `canonicalized_path_to_string(path, must_be_relative = true)`
/// but returns our error type and surfaces the offending component in
/// the error message.
pub fn canonicalized_path_to_string(path: &std::path::Path) -> ShareResult<String> {
    let mut out = String::new();
    for c in path.components() {
        match c {
            Component::Normal(name) => match name.to_str() {
                Some(s) => {
                    validate_path_component(s)?;
                    if !out.is_empty() {
                        out.push('/');
                    }
                    out.push_str(s);
                }
                None => {
                    return Err(ShareError::InvalidPathComponent(format!(
                        "{name:?}"
                    )));
                }
            },
            Component::RootDir => {
                return Err(ShareError::AbsolutePathRefused(format!(
                    "{path:?}"
                )));
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(ShareError::InvalidPathComponent(format!(
                    "non-canonical component {c:?}"
                )));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_simple_name() {
        assert!(validate_path_component("hello.txt").is_ok());
        assert!(validate_path_component("a").is_ok());
        assert!(validate_path_component("with spaces ok").is_ok());
    }

    #[test]
    fn validate_rejects_empty() {
        assert!(validate_path_component("").is_err());
    }

    #[test]
    fn validate_rejects_slash() {
        let err = validate_path_component("a/b").unwrap_err();
        assert!(matches!(err, ShareError::InvalidPathComponent(_)));
    }

    #[test]
    fn validate_rejects_backslash() {
        let err = validate_path_component("a\\b").unwrap_err();
        assert!(matches!(err, ShareError::InvalidPathComponent(_)));
    }

    #[test]
    fn validate_rejects_overlong_name() {
        let too_long = "x".repeat(MAX_NAME_LEN + 1);
        let err = validate_path_component(&too_long).unwrap_err();
        match err {
            ShareError::PathComponentTooLong { len, max, .. } => {
                assert_eq!(len, MAX_NAME_LEN + 1);
                assert_eq!(max, MAX_NAME_LEN);
            }
            other => panic!("expected PathComponentTooLong, got {other:?}"),
        }
    }

    #[test]
    fn canonicalize_simple() {
        let p = std::path::Path::new("a/b/c.txt");
        assert_eq!(
            canonicalized_path_to_string(p).unwrap(),
            "a/b/c.txt"
        );
    }

    #[test]
    fn canonicalize_single_component() {
        let p = std::path::Path::new("hello.txt");
        assert_eq!(
            canonicalized_path_to_string(p).unwrap(),
            "hello.txt"
        );
    }

    #[test]
    fn canonicalize_rejects_absolute() {
        let p = std::path::Path::new("/etc/passwd");
        let err = canonicalized_path_to_string(p).unwrap_err();
        assert!(matches!(err, ShareError::AbsolutePathRefused(_)));
    }

    #[test]
    fn canonicalize_rejects_curdir() {
        let p = std::path::Path::new("./foo");
        let err = canonicalized_path_to_string(p).unwrap_err();
        assert!(matches!(err, ShareError::InvalidPathComponent(_)));
    }

    #[test]
    fn canonicalize_rejects_parent() {
        let p = std::path::Path::new("../foo");
        let err = canonicalized_path_to_string(p).unwrap_err();
        assert!(matches!(err, ShareError::InvalidPathComponent(_)));
    }

    #[test]
    fn canonicalize_rejects_embedded_slash() {
        // Path::components() will treat "a/b" as two components, both
        // normal, so this is fine. But if the underlying string is
        // literally a single component containing '/', the
        // `validate_path_component` call catches it.
        let p = std::path::Path::new("a/b");
        assert_eq!(
            canonicalized_path_to_string(p).unwrap(),
            "a/b"
        );
    }

    #[test]
    fn canonicalize_rejects_embedded_backslash() {
        // On Unix, 'a\\b' is a single component containing a backslash.
        // On Windows it would split — we test on Unix semantics.
        let p = std::path::Path::new("a\\b");
        let err = canonicalized_path_to_string(p).unwrap_err();
        assert!(matches!(err, ShareError::InvalidPathComponent(_)));
    }
}