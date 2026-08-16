//! Media error types — exhaustive, mapped 1:1 to HTTP / IPC status codes.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MediaError {
    #[error("unsupported codec: {codec:?} (kind={kind:?})")]
    UnsupportedCodec { codec: String, kind: String },

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("input payload too small: {actual} bytes (need ≥ {min})")]
    InputTooSmall { actual: usize, min: usize },

    #[error("input payload too large: {actual} bytes (limit {limit})")]
    InputTooLarge { actual: u64, limit: u64 },

    #[error("decode error at byte offset {offset}: {message}")]
    DecodeError { offset: u64, message: String },

    #[error("truncated frame: expected {expected} bytes, got {actual}")]
    TruncatedFrame { expected: usize, actual: usize },

    #[error("length-prefix overflow: {actual} exceeds payload {payload}")]
    LengthPrefixOverflow { actual: u64, payload: u64 },

    #[error("invalid length-prefix for kind {0}: zero-length frame rejected")]
    InvalidLp(u8),

    #[error("segment out of range: index {index} ≥ {count}")]
    SegmentOutOfRange { index: usize, count: usize },

    #[error("manifest hash mismatch: expected {expected}, got {actual}")]
    ManifestHashMismatch { expected: String, actual: String },

    #[error("quarantined segment: {cid}")]
    Quarantined { cid: String },

    /// H-10: the segment-index sidecar is missing an entry that
    /// the manifest references. This is structurally different
    /// from `Quarantined` (where the blobstore knows about the
    /// segment but the bytes are corrupt) and tells the operator
    /// that the local sidecar has been corrupted and must be
    /// rebuilt.
    #[error("media-segments.json index missing entry: {missing}")]
    IndexCorrupt { missing: String },

    #[error("clock skew: local {local} vs media {media} drift {drift_ms} ms exceeds limit")]
    ClockSkew { local: i64, media: i64, drift_ms: i64 },

    #[error("duration mismatch: declared {declared} ms, computed {computed} ms")]
    DurationMismatch { declared: u64, computed: u64 },

    #[error("frame count mismatch: declared {declared}, computed {computed}")]
    FrameCountMismatch { declared: u64, computed: u64 },

    #[error("bitrate mismatch: declared {declared} bps, computed {computed} bps")]
    BitrateMismatch { declared: u64, computed: u64 },

    #[error("audio/video drift: {drift_ms} ms exceeds {tolerance} ms")]
    AvDrift { drift_ms: i64, tolerance: i64 },

    #[error("io: {0}")]
    Io(String),

    #[error("serialization: {0}")]
    Serialization(String),
}

pub type MediaResult<T> = Result<T, MediaError>;

impl From<std::io::Error> for MediaError {
    fn from(e: std::io::Error) -> Self {
        MediaError::Io(e.to_string())
    }
}

impl From<bincode::Error> for MediaError {
    fn from(e: bincode::Error) -> Self {
        MediaError::Serialization(e.to_string())
    }
}

impl From<serde_json::Error> for MediaError {
    fn from(e: serde_json::Error) -> Self {
        MediaError::Serialization(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_io_error_maps_to_io_variant() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing file");
        let m: MediaError = io_err.into();
        assert!(matches!(m, MediaError::Io(_)));
        // The display impl should include the inner io error string.
        assert!(m.to_string().contains("missing file"));
    }

    #[test]
    fn from_serde_json_error_maps_to_serialization_variant() {
        // A trailing comma is invalid JSON.
        let result: Result<serde_json::Value, _> = serde_json::from_str("{\"a\":}");
        let parse_err = result.unwrap_err();
        let m: MediaError = parse_err.into();
        assert!(matches!(m, MediaError::Serialization(_)));
    }

    #[test]
    fn from_bincode_error_maps_to_serialization_variant() {
        // bincode deserialization of an empty buffer into a `u32` should fail.
        let result: Result<u32, _> = bincode::deserialize(&[]);
        let bin_err = result.unwrap_err();
        let m: MediaError = bin_err.into();
        assert!(matches!(m, MediaError::Serialization(_)));
    }

    #[test]
    fn error_display_strings_are_meaningful() {
        // Every variant should produce a non-empty Display string that
        // includes a recognizable substring so log-grepping stays useful.
        let cases: Vec<(MediaError, &[&str])> = vec![
            (
                MediaError::UnsupportedCodec {
                    codec: "vp9".into(),
                    kind: "video".into(),
                },
                &["vp9", "video"],
            ),
            (
                MediaError::InvalidConfig("bad bitrate".into()),
                &["bad bitrate"],
            ),
            (
                MediaError::InputTooSmall {
                    actual: 1,
                    min: 100,
                },
                &["1", "100"],
            ),
            (
                MediaError::InputTooLarge {
                    actual: 10_000,
                    limit: 5_000,
                },
                &["10000", "5000"],
            ),
            (
                MediaError::DecodeError {
                    offset: 42,
                    message: "bad".into(),
                },
                &["42", "bad"],
            ),
            (
                MediaError::TruncatedFrame {
                    expected: 10,
                    actual: 5,
                },
                &["10", "5"],
            ),
            (
                MediaError::LengthPrefixOverflow {
                    actual: 999,
                    payload: 100,
                },
                &["999", "100"],
            ),
            (MediaError::InvalidLp(7), &["7"]),
            (
                MediaError::SegmentOutOfRange {
                    index: 9,
                    count: 9,
                },
                &["9"],
            ),
            (
                MediaError::ManifestHashMismatch {
                    expected: "aaa".into(),
                    actual: "bbb".into(),
                },
                &["aaa", "bbb"],
            ),
            (
                MediaError::Quarantined {
                    cid: "cid-x".into(),
                },
                &["cid-x"],
            ),
            (
                MediaError::ClockSkew {
                    local: 100,
                    media: 200,
                    drift_ms: 100,
                },
                &["100", "200"],
            ),
            (
                MediaError::DurationMismatch {
                    declared: 1000,
                    computed: 999,
                },
                &["1000", "999"],
            ),
            (
                MediaError::FrameCountMismatch {
                    declared: 30,
                    computed: 29,
                },
                &["30", "29"],
            ),
            (
                MediaError::BitrateMismatch {
                    declared: 1_000_000,
                    computed: 999_999,
                },
                &["1000000", "999999"],
            ),
            (
                MediaError::AvDrift {
                    drift_ms: 75,
                    tolerance: 50,
                },
                &["75", "50"],
            ),
            (MediaError::Io("disk".into()), &["disk"]),
            (
                MediaError::Serialization("bad json".into()),
                &["bad json"],
            ),
        ];

        for (err, needles) in cases {
            let s = err.to_string();
            for n in needles {
                assert!(
                    s.contains(n),
                    "error {err:?} should contain {n:?} in display, got: {s}"
                );
            }
        }
    }

    #[test]
    fn errors_implement_eq() {
        // PartialEq + Eq is asserted in the derive, but we also want a
        // runtime smoke test so future refactors that drop the bound
        // surface as a test failure instead of a compile error in a
        // downstream crate.
        let a = MediaError::InvalidLp(3);
        let b = MediaError::InvalidLp(3);
        let c = MediaError::InvalidLp(4);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
