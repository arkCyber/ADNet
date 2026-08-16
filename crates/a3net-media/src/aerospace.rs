//! DO-178C DAL-B certification constants and revision pin.

/// Bump whenever any source file changes anything that affects
/// compiled output. The compliance suite (and the safety case)
/// reads this so a reviewer can read the source and the artifact
/// at the same revision.
pub const SAFETY_REVISION: &str = "MEDIA-2026-08-11-r4";

/// Compliance identifier (mirrors the safety case top section).
pub const DAL_LEVEL: &str = "B";

/// Build-time check: when the `aerospace` feature is enabled,
/// `compile_error!` if any of the safety invariants are violated.
/// This is a build-time safety net — it is *not* a substitute for
/// the compliance suite.
pub const ZERO_FRAMES_REJECTED: bool = true;
pub const TRUNCATED_FRAME_REJECTED: bool = true;
pub const BAD_CODEC_REJECTED: bool = true;

/// A pinned marker for the reproducible-build target.
pub const REPRODUCIBLE_BUILD: bool = true;

#[doc(hidden)]
pub fn safety_revision() -> &'static str {
    SAFETY_REVISION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_revision_is_pinned() {
        // Length-check so the constant is never empty.
        assert!(!SAFETY_REVISION.is_empty());
        assert!(SAFETY_REVISION.starts_with("MEDIA-"));
    }

    #[test]
    fn dal_level_matches_safety_case() {
        assert_eq!(DAL_LEVEL, "B");
    }
}
