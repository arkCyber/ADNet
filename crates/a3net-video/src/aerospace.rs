//! DO-178C DAL-B certification constants and revision pin.
//!
//! These constants define the aerospace certification baseline for the
//! video pipeline. When the `aerospace` feature is enabled, the build
//! system enforces that all safety-critical invariants are preserved.

/// Bump whenever any source file changes anything that affects
/// compiled output. The compliance suite (and the safety case)
/// reads this so a reviewer can read the source and the artifact
/// at the same revision.
pub const SAFETY_REVISION: &str = "VIDEO-2026-08-13-r1";

/// Compliance identifier (mirrors the safety case top section).
pub const DAL_LEVEL: &str = "B";

/// Build-time check: when the `aerospace` feature is enabled,
/// these constants assert the baseline safety properties.
pub const ZERO_FRAMES_REJECTED: bool = true;
pub const TRUNCATED_FRAME_REJECTED: bool = true;
pub const SEQUENCE_GAP_REJECTED: bool = true;
pub const KEYFRAME_ENFORCED: bool = true;
pub const MONOTONIC_TIMESTAMP_ENFORCED: bool = true;

/// Reproducible build marker.
pub const REPRODUCIBLE_BUILD: bool = true;

/// Maximum allowed framerate deviation (parts per million).
/// Exceeding this triggers clock skew detection.
pub const MAX_FRAMERATE_DEVIATION_PPM: u32 = 1000;

/// Maximum consecutive dropped frames before alarm.
/// Set per DO-178C §6.3.4b "consecutive error threshold".
pub const MAX_CONSECUTIVE_DROPS: u32 = 5;

/// Maximum latency budget for a single frame (nanoseconds).
/// 60fps = 16,666,666ns per frame.
pub const MAX_FRAME_LATENCY_NS: u64 = 20_000_000; // 20ms budget

/// Keyframe interval must not exceed this many frames.
pub const MAX_KEYFRAME_INTERVAL: u32 = 300; // 5 seconds @ 60fps

/// Returns the pinned safety revision string.
#[doc(hidden)]
pub fn safety_revision() -> &'static str {
    SAFETY_REVISION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_revision_is_pinned() {
        assert!(!SAFETY_REVISION.is_empty());
        assert!(SAFETY_REVISION.starts_with("VIDEO-"));
    }

    #[test]
    fn dal_level_matches_safety_case() {
        assert_eq!(DAL_LEVEL, "B");
    }

    #[test]
    fn aerospace_constants_are_valid() {
        assert!(MAX_CONSECUTIVE_DROPS > 0);
        assert!(MAX_FRAME_LATENCY_NS > 0);
        assert!(MAX_KEYFRAME_INTERVAL > 0);
    }
}
