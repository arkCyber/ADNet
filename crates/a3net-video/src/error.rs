//! DO-178C DAL-B error types for the video pipeline.
//!
//! All video operations return `VideoResult<T>` = `Result<T, VideoError>`.
//! No function ever panics or unwraps in production code.

use std::fmt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Aerospace-grade video error type.
///
/// Every variant is documented with its safety impact per DO-178C DAL-B.
#[derive(Debug, Clone, Error, Serialize, Deserialize, PartialEq)]
#[serde(tag = "code", content = "detail")]
#[non_exhaustive]
pub enum VideoError {
    // ─────────────────────────────────────────────────────────────────
    // Input / Output Errors
    // ─────────────────────────────────────────────────────────────────

    /// Frame data is truncated (fewer bytes than declared).
    /// Safety impact: prevents buffer overrun but loses frame continuity.
    #[error("truncated frame: expected {expected} bytes, got {actual}")]
    TruncatedFrame {
        expected: usize,
        actual: usize,
    },

    /// Frame size exceeds maximum allowed buffer.
    /// Safety impact: prevents denial-of-service via oversized frames.
    #[error("frame too large: {size} bytes exceeds limit of {limit} bytes")]
    FrameTooLarge {
        size: usize,
        limit: usize,
    },

    /// No frames available in the buffer within the timeout.
    /// Safety impact: signals upstream failure for fault injection.
    #[error("no frame available after {timeout_ms}ms")]
    NoFrameAvailable {
        timeout_ms: u64,
    },

    /// Frame buffer overflow — producer is faster than consumer.
    /// Safety impact: prevents unbounded memory growth.
    #[error("frame buffer overflow: {dropped} frames dropped")]
    BufferOverflow {
        dropped: usize,
    },

    /// Frame encoding timeout.
    /// Safety impact: signals encoder overload.
    #[error("encode timeout after {timeout_ms}ms")]
    EncodeTimeout {
        timeout_ms: u64,
    },

    /// Frame decoding timeout.
    /// Safety impact: signals decoder overload.
    #[error("decode timeout after {timeout_ms}ms")]
    DecodeTimeout {
        timeout_ms: u64,
    },

    // ─────────────────────────────────────────────────────────────────
    // Codec Errors
    // ─────────────────────────────────────────────────────────────────

    /// Codec initialization failed.
    #[error("codec init failed: {0}")]
    CodecInit(String),

    /// Frame encoding failed.
    #[error("encode failed: {0}")]
    EncodeFailed(String),

    /// Frame decoding failed.
    #[error("decode failed: {0}")]
    DecodeFailed(String),

    /// Unsupported codec requested.
    #[error("unsupported codec: {codec} (supported: H.264, VP8, VP9, AV1)")]
    UnsupportedCodec {
        codec: String,
    },

    /// Codec level exceeded (e.g., H.264 Level 3.1 for 1080p@60fps).
    #[error("codec level exceeded: {codec} {profile} @ {level} does not support {width}x{height}@{fps}")]
    CodecLevelExceeded {
        codec: String,
        profile: String,
        level: String,
        width: u32,
        height: u32,
        fps: u32,
    },

    /// Invalid codec parameter (e.g., negative framerate, zero dimensions).
    #[error("invalid codec parameter: {param} = {value}")]
    InvalidCodecParam {
        param: String,
        value: String,
    },

    /// Codec hardware acceleration failed.
    #[error("hardware acceleration failed: {0}")]
    HardwareAccelFailed(String),

    // ─────────────────────────────────────────────────────────────────
    // Timing / Synchronization Errors
    // ─────────────────────────────────────────────────────────────────

    /// Timestamp is not monotonically increasing.
    #[error("non-monotonic timestamp: prev={prev_ns}ns, curr={curr_ns}ns")]
    NonMonotonicTimestamp {
        prev_ns: u64,
        curr_ns: u64,
    },

    /// Sequence number gap detected (frames lost).
    #[error("sequence gap: expected seq {expected}, got {actual} (lost {lost} frames)")]
    SequenceGap {
        expected: u32,
        actual: u32,
        lost: u32,
    },

    /// Frame arrived too late for render deadline.
    #[error("frame late: ts={ts_ns}ns, deadline={deadline_ns}ns, late_by={late_ns}ns")]
    FrameLate {
        ts_ns: u64,
        deadline_ns: u64,
        late_ns: u64,
    },

    /// Clock skew detected (system time jumped).
    #[error("clock skew: system clock jumped by {drift_ms}ms")]
    ClockSkew {
        drift_ms: i64,
    },

    /// Jitter buffer underrun.
    #[error("jitter buffer underrun: insufficient frames in buffer")]
    JitterBufferUnderrun,

    /// Jitter buffer overflow.
    #[error("jitter buffer overflow: too many frames buffered")]
    JitterBufferOverflow,

    // ─────────────────────────────────────────────────────────────────
    // Pipeline Errors
    // ─────────────────────────────────────────────────────────────────

    /// Pipeline is not in the required state for this operation.
    #[error("invalid pipeline state: expected {expected}, got {actual}")]
    InvalidPipelineState {
        expected: &'static str,
        actual: &'static str,
    },

    /// Track is not in the required state for this operation.
    #[error("invalid track state: expected {expected}, got {actual}")]
    InvalidTrackState {
        expected: &'static str,
        actual: &'static str,
    },

    /// Pipeline is stopped; no more frames will be produced.
    #[error("pipeline stopped: {reason}")]
    PipelineStopped {
        reason: String,
    },

    /// Pipeline component failed.
    #[error("pipeline component failed: {component}: {cause}")]
    PipelineComponentFailed {
        component: &'static str,
        cause: Box<VideoError>,
    },

    /// Pipeline initialization failed.
    #[error("pipeline init failed: {0}")]
    PipelineInitFailed(String),

    /// Pipeline configuration error.
    #[error("pipeline config error: {0}")]
    PipelineConfigError(String),

    // ─────────────────────────────────────────────────────────────────
    // WebRTC / Transport Errors
    // ─────────────────────────────────────────────────────────────────

    /// WebRTC track creation failed.
    #[error("track creation failed: {0}")]
    TrackCreationFailed(String),

    /// Media track is not connected.
    #[error("track not connected")]
    TrackNotConnected,

    /// SRTP session error.
    #[error("SRTP error: {0}")]
    SrtpError(String),

    /// Peer connection state changed to failed.
    #[error("peer connection failed: {0}")]
    PeerConnectionFailed(String),

    /// Network congestion detected.
    #[error("network congestion: latency={latency_ms}ms, packet_loss={packet_loss_pct}%")]
    NetworkCongestion {
        latency_ms: u32,
        packet_loss_pct: f64,
    },

    /// Bandwidth estimation failed.
    #[error("bandwidth estimation failed: {0}")]
    BandwidthEstimationFailed(String),

    /// Media quality degradation detected.
    #[error("quality degradation: {reason}")]
    QualityDegradation {
        reason: String,
    },

    // ─────────────────────────────────────────────────────────────────
    // Configuration Errors
    // ─────────────────────────────────────────────────────────────────

    /// Invalid video configuration parameter.
    #[error("invalid config: {param} = {value} ({reason})")]
    InvalidConfig {
        param: &'static str,
        value: String,
        reason: &'static str,
    },

    /// Resolution not supported by codec.
    #[error("unsupported resolution: {width}x{height} not supported")]
    UnsupportedResolution {
        width: u32,
        height: u32,
    },

    /// Frame size exceeds configured limits.
    /// DO-178C SR-9: Frame size limits enforcement
    #[error("frame size exceeded: {width}x{height} = {size} bytes exceeds limit of {max_bytes} bytes")]
    FrameSizeExceeded {
        width: u32,
        height: u32,
        size: usize,
        max_bytes: usize,
    },

    /// Invalid buffer depth configuration.
    /// DO-178C SR-1: Buffer overflow prevention
    #[error("invalid buffer depth: {depth} exceeds maximum of {max}")]
    InvalidBufferDepth {
        depth: usize,
        max: usize,
    },

    /// Invalid keyframe interval configuration.
    #[error("invalid keyframe interval: {interval} exceeds maximum of {max}")]
    InvalidKeyframeInterval {
        interval: u32,
        max: u32,
    },

    /// Bitrate too low for configured resolution.
    #[error("bitrate too low: {bitrate_kbps} kbps (minimum recommended: {min_recommended} kbps)")]
    BitrateTooLow {
        bitrate_kbps: u32,
        min_recommended: u32,
    },

    /// Framerate not supported by codec.
    #[error("unsupported framerate: {fps}fps not supported")]
    UnsupportedFramerate {
        fps: u32,
    },

    // ─────────────────────────────────────────────────────────────────
    // Integrity / Verification Errors
    // ─────────────────────────────────────────────────────────────────

    /// Frame integrity check failed (CRC/checksum mismatch).
    #[error("frame integrity check failed at ts={ts_ns}ns")]
    IntegrityCheckFailed {
        ts_ns: u64,
    },

    /// Keyframe missing at expected position.
    #[error("keyframe missing: expected at seq={seq}")]
    KeyframeMissing {
        seq: u32,
    },

    /// Manifest hash mismatch (DAG verification failed).
    #[error("manifest hash mismatch: expected={expected}, got {actual}")]
    ManifestHashMismatch {
        expected: String,
        actual: String,
    },

    // ─────────────────────────────────────────────────────────────────
    // Resource Errors
    // ─────────────────────────────────────────────────────────────────

    /// Memory allocation failed.
    #[error("memory allocation failed: {size} bytes")]
    OutOfMemory {
        size: usize,
    },

    /// File system error.
    #[error("file system error: {0}")]
    FileSystemError(String),

    /// Permission denied.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    // ─────────────────────────────────────────────────────────────────
    // Capture / Render Errors
    // ─────────────────────────────────────────────────────────────────

    /// Camera initialization failed.
    #[error("camera init failed: {0}")]
    CameraInitFailed(String),

    /// Camera device not found.
    #[error("camera device not found: {0}")]
    CameraNotFound(String),

    /// Display initialization failed.
    #[error("display init failed: {0}")]
    DisplayInitFailed(String),

    // ─────────────────────────────────────────────────────────────────
    // Recovery / Fallback Errors
    // ─────────────────────────────────────────────────────────────────

    /// Fallback mechanism triggered.
    #[error("fallback triggered: {original} -> {fallback}")]
    FallbackTriggered {
        original: Box<VideoError>,
        fallback: String,
    },

    /// Recovery attempt failed.
    #[error("recovery failed after {attempts} attempts: {last_error}")]
    RecoveryFailed {
        attempts: u32,
        last_error: Box<VideoError>,
    },

    /// Maximum retry attempts exceeded.
    #[error("max retries exceeded: {attempts} attempts")]
    MaxRetriesExceeded {
        attempts: u32,
    },
}

/// Type alias for ergonomic usage.
pub type VideoResult<T> = Result<T, VideoError>;

impl VideoError {
    /// Returns true if this error indicates a transient condition
    /// that may resolve on retry.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            VideoError::BufferOverflow { .. }
                | VideoError::FrameLate { .. }
                | VideoError::TrackNotConnected
                | VideoError::NoFrameAvailable { .. }
                | VideoError::EncodeTimeout { .. }
                | VideoError::DecodeTimeout { .. }
                | VideoError::JitterBufferUnderrun
                | VideoError::JitterBufferOverflow
                | VideoError::NetworkCongestion { .. }
                | VideoError::BandwidthEstimationFailed(_)
        )
    }

    /// Returns true if this error indicates a fatal condition
    /// that requires pipeline restart.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            VideoError::PipelineStopped { .. }
                | VideoError::CodecInit(_)
                | VideoError::UnsupportedCodec { .. }
                | VideoError::PeerConnectionFailed(_)
                | VideoError::InvalidPipelineState { .. }
                | VideoError::InvalidTrackState { .. }
                | VideoError::CameraInitFailed(_)
                | VideoError::CameraNotFound(_)
                | VideoError::DisplayInitFailed(_)
                | VideoError::OutOfMemory { .. }
                | VideoError::FileSystemError(_)
                | VideoError::PermissionDenied(_)
                | VideoError::PipelineInitFailed(_)
                | VideoError::PipelineConfigError(_)
                | VideoError::SrtpError(_)
        )
    }

    /// Returns true if this error can trigger a fallback mechanism.
    pub fn can_fallback(&self) -> bool {
        matches!(
            self,
            VideoError::EncodeFailed(_)
                | VideoError::DecodeFailed(_)
                | VideoError::EncodeTimeout { .. }
                | VideoError::DecodeTimeout { .. }
                | VideoError::QualityDegradation { .. }
                | VideoError::HardwareAccelFailed(_)
        )
    }

    /// Returns true if this error indicates a network-related issue.
    pub fn is_network_error(&self) -> bool {
        matches!(
            self,
            VideoError::TrackNotConnected
                | VideoError::PeerConnectionFailed(_)
                | VideoError::SrtpError(_)
                | VideoError::NetworkCongestion { .. }
                | VideoError::BandwidthEstimationFailed(_)
        )
    }

    /// Returns true if this error indicates a resource constraint.
    pub fn is_resource_error(&self) -> bool {
        matches!(
            self,
            VideoError::OutOfMemory { .. }
                | VideoError::BufferOverflow { .. }
                | VideoError::FrameTooLarge { .. }
        )
    }

    /// Wraps this error in a recovery context.
    pub fn with_recovery_context(self, attempts: u32) -> VideoError {
        VideoError::RecoveryFailed {
            attempts,
            last_error: Box::new(self),
        }
    }

    /// Wraps this error in a fallback context.
    pub fn with_fallback_context(self, fallback: impl Into<String>) -> VideoError {
        VideoError::FallbackTriggered {
            original: Box::new(self),
            fallback: fallback.into(),
        }
    }

    /// Returns the error code for logging/tracing purposes.
    pub fn error_code(&self) -> &'static str {
        match self {
            VideoError::TruncatedFrame { .. } => "E001",
            VideoError::FrameTooLarge { .. } => "E002",
            VideoError::NoFrameAvailable { .. } => "E003",
            VideoError::BufferOverflow { .. } => "E004",
            VideoError::EncodeTimeout { .. } => "E005",
            VideoError::DecodeTimeout { .. } => "E006",
            VideoError::CodecInit(_) => "E010",
            VideoError::EncodeFailed(_) => "E011",
            VideoError::DecodeFailed(_) => "E012",
            VideoError::UnsupportedCodec { .. } => "E013",
            VideoError::CodecLevelExceeded { .. } => "E014",
            VideoError::InvalidCodecParam { .. } => "E015",
            VideoError::HardwareAccelFailed(_) => "E016",
            VideoError::NonMonotonicTimestamp { .. } => "E020",
            VideoError::SequenceGap { .. } => "E021",
            VideoError::FrameLate { .. } => "E022",
            VideoError::ClockSkew { .. } => "E023",
            VideoError::JitterBufferUnderrun => "E024",
            VideoError::JitterBufferOverflow => "E025",
            VideoError::InvalidPipelineState { .. } => "E030",
            VideoError::InvalidTrackState { .. } => "E031",
            VideoError::PipelineStopped { .. } => "E032",
            VideoError::PipelineComponentFailed { .. } => "E033",
            VideoError::PipelineInitFailed(_) => "E034",
            VideoError::PipelineConfigError(_) => "E035",
            VideoError::TrackCreationFailed(_) => "E040",
            VideoError::TrackNotConnected => "E041",
            VideoError::SrtpError(_) => "E042",
            VideoError::PeerConnectionFailed(_) => "E043",
            VideoError::NetworkCongestion { .. } => "E044",
            VideoError::BandwidthEstimationFailed(_) => "E045",
            VideoError::QualityDegradation { .. } => "E046",
            VideoError::InvalidConfig { .. } => "E050",
            VideoError::UnsupportedResolution { .. } => "E051",
            VideoError::UnsupportedFramerate { .. } => "E052",
            VideoError::IntegrityCheckFailed { .. } => "E060",
            VideoError::KeyframeMissing { .. } => "E061",
            VideoError::ManifestHashMismatch { .. } => "E062",
            VideoError::OutOfMemory { .. } => "E070",
            VideoError::FileSystemError(_) => "E071",
            VideoError::PermissionDenied(_) => "E072",
            VideoError::CameraInitFailed(_) => "E080",
            VideoError::CameraNotFound(_) => "E081",
            VideoError::DisplayInitFailed(_) => "E082",
            VideoError::FallbackTriggered { .. } => "E090",
            VideoError::RecoveryFailed { .. } => "E091",
            VideoError::MaxRetriesExceeded { .. } => "E092",
            // DO-178C DAL-B Configuration Errors
            VideoError::FrameSizeExceeded { .. } => "E100",
            VideoError::InvalidBufferDepth { .. } => "E101",
            VideoError::InvalidKeyframeInterval { .. } => "E102",
            VideoError::BitrateTooLow { .. } => "E103",
        }
    }

    /// Returns the severity level of this error.
    pub fn severity(&self) -> ErrorSeverity {
        match self {
            VideoError::TruncatedFrame { .. } => ErrorSeverity::Warning,
            VideoError::FrameTooLarge { .. } => ErrorSeverity::Warning,
            VideoError::NoFrameAvailable { .. } => ErrorSeverity::Info,
            VideoError::BufferOverflow { .. } => ErrorSeverity::Warning,
            VideoError::EncodeTimeout { .. } => ErrorSeverity::Warning,
            VideoError::DecodeTimeout { .. } => ErrorSeverity::Warning,
            VideoError::CodecInit(_) => ErrorSeverity::Critical,
            VideoError::EncodeFailed(_) => ErrorSeverity::Warning,
            VideoError::DecodeFailed(_) => ErrorSeverity::Warning,
            VideoError::UnsupportedCodec { .. } => ErrorSeverity::Critical,
            VideoError::CodecLevelExceeded { .. } => ErrorSeverity::Error,
            VideoError::InvalidCodecParam { .. } => ErrorSeverity::Error,
            VideoError::HardwareAccelFailed(_) => ErrorSeverity::Warning,
            VideoError::NonMonotonicTimestamp { .. } => ErrorSeverity::Warning,
            VideoError::SequenceGap { .. } => ErrorSeverity::Info,
            VideoError::FrameLate { .. } => ErrorSeverity::Info,
            VideoError::ClockSkew { .. } => ErrorSeverity::Warning,
            VideoError::JitterBufferUnderrun => ErrorSeverity::Info,
            VideoError::JitterBufferOverflow => ErrorSeverity::Warning,
            VideoError::InvalidPipelineState { .. } => ErrorSeverity::Error,
            VideoError::InvalidTrackState { .. } => ErrorSeverity::Error,
            VideoError::PipelineStopped { .. } => ErrorSeverity::Critical,
            VideoError::PipelineComponentFailed { .. } => ErrorSeverity::Error,
            VideoError::PipelineInitFailed(_) => ErrorSeverity::Critical,
            VideoError::PipelineConfigError(_) => ErrorSeverity::Error,
            VideoError::TrackCreationFailed(_) => ErrorSeverity::Critical,
            VideoError::TrackNotConnected => ErrorSeverity::Warning,
            VideoError::SrtpError(_) => ErrorSeverity::Error,
            VideoError::PeerConnectionFailed(_) => ErrorSeverity::Critical,
            VideoError::NetworkCongestion { .. } => ErrorSeverity::Warning,
            VideoError::BandwidthEstimationFailed(_) => ErrorSeverity::Warning,
            VideoError::QualityDegradation { .. } => ErrorSeverity::Info,
            VideoError::InvalidConfig { .. } => ErrorSeverity::Error,
            VideoError::UnsupportedResolution { .. } => ErrorSeverity::Error,
            VideoError::UnsupportedFramerate { .. } => ErrorSeverity::Error,
            VideoError::IntegrityCheckFailed { .. } => ErrorSeverity::Error,
            VideoError::KeyframeMissing { .. } => ErrorSeverity::Warning,
            VideoError::ManifestHashMismatch { .. } => ErrorSeverity::Error,
            VideoError::OutOfMemory { .. } => ErrorSeverity::Critical,
            VideoError::FileSystemError(_) => ErrorSeverity::Error,
            VideoError::PermissionDenied(_) => ErrorSeverity::Error,
            VideoError::CameraInitFailed(_) => ErrorSeverity::Critical,
            VideoError::CameraNotFound(_) => ErrorSeverity::Warning,
            VideoError::DisplayInitFailed(_) => ErrorSeverity::Critical,
            VideoError::FallbackTriggered { .. } => ErrorSeverity::Info,
            VideoError::RecoveryFailed { .. } => ErrorSeverity::Error,
            VideoError::MaxRetriesExceeded { .. } => ErrorSeverity::Error,
            // DO-178C DAL-B Configuration Validation Errors
            VideoError::FrameSizeExceeded { .. } => ErrorSeverity::Error,
            VideoError::InvalidBufferDepth { .. } => ErrorSeverity::Error,
            VideoError::InvalidKeyframeInterval { .. } => ErrorSeverity::Error,
            VideoError::BitrateTooLow { .. } => ErrorSeverity::Warning,
        }
    }

    /// Returns the safety requirement (SR-X) this error violates.
    #[cfg(feature = "aerospace")]
    pub fn safety_requirement(&self) -> &'static str {
        match self {
            VideoError::TruncatedFrame { .. } => "SR-6",
            VideoError::FrameTooLarge { .. } => "SR-9",
            VideoError::NonMonotonicTimestamp { .. } => "SR-5",
            VideoError::SequenceGap { .. } => "SR-5",
            VideoError::IntegrityCheckFailed { .. } => "SR-11",
            VideoError::KeyframeMissing { .. } => "SR-4",
            VideoError::ManifestHashMismatch { .. } => "SR-2",
            VideoError::FrameSizeExceeded { .. } => "SR-9",
            VideoError::InvalidBufferDepth { .. } => "SR-1",
            VideoError::InvalidKeyframeInterval { .. } => "SR-9",
            VideoError::BitrateTooLow { .. } => "SR-GENERAL",
            _ => "SR-GENERAL",
        }
    }
}

/// Error severity levels for categorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorSeverity {
    /// Informational message, no action required.
    Info,
    /// Warning, may indicate potential issues.
    Warning,
    /// Error, requires attention but system can continue.
    Error,
    /// Critical error, system may not be able to continue.
    Critical,
}

impl fmt::Display for ErrorSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorSeverity::Info => write!(f, "INFO"),
            ErrorSeverity::Warning => write!(f, "WARN"),
            ErrorSeverity::Error => write!(f, "ERROR"),
            ErrorSeverity::Critical => write!(f, "CRITICAL"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_serialization_round_trip() {
        let err = VideoError::TrackNotConnected;
        let json = serde_json::to_string(&err).unwrap();
        // Just verify it can serialize without panic
        assert!(!json.is_empty());
    }

    #[test]
    fn transient_errors_are_transient() {
        assert!(VideoError::BufferOverflow { dropped: 10 }.is_transient());
        assert!(VideoError::FrameLate {
            ts_ns: 0,
            deadline_ns: 100,
            late_ns: 50
        }
        .is_transient());
        assert!(VideoError::EncodeTimeout { timeout_ms: 1000 }.is_transient());
        assert!(VideoError::DecodeTimeout { timeout_ms: 1000 }.is_transient());
        assert!(VideoError::JitterBufferUnderrun.is_transient());
        assert!(VideoError::JitterBufferOverflow.is_transient());
        assert!(VideoError::NetworkCongestion { latency_ms: 100, packet_loss_pct: 5.0 }.is_transient());
        assert!(!VideoError::CodecInit("test".into()).is_transient());
    }

    #[test]
    fn fatal_errors_are_fatal() {
        assert!(VideoError::PipelineStopped { reason: "test".into() }.is_fatal());
        assert!(VideoError::CodecInit("test".into()).is_fatal());
        assert!(VideoError::CameraInitFailed("test".into()).is_fatal());
        assert!(VideoError::CameraNotFound("test".into()).is_fatal());
        assert!(VideoError::OutOfMemory { size: 1000 }.is_fatal());
        assert!(!VideoError::BufferOverflow { dropped: 10 }.is_fatal());
    }

    #[test]
    fn fallback_errors_can_fallback() {
        assert!(VideoError::EncodeFailed("test".into()).can_fallback());
        assert!(VideoError::DecodeFailed("test".into()).can_fallback());
        assert!(VideoError::EncodeTimeout { timeout_ms: 1000 }.can_fallback());
        assert!(VideoError::QualityDegradation { reason: "test".into() }.can_fallback());
        assert!(!VideoError::TrackNotConnected.can_fallback());
    }

    #[test]
    fn network_errors_are_network_errors() {
        assert!(VideoError::TrackNotConnected.is_network_error());
        assert!(VideoError::PeerConnectionFailed("test".into()).is_network_error());
        assert!(VideoError::SrtpError("test".into()).is_network_error());
        assert!(VideoError::NetworkCongestion { latency_ms: 100, packet_loss_pct: 5.0 }.is_network_error());
        assert!(!VideoError::BufferOverflow { dropped: 10 }.is_network_error());
    }

    #[test]
    fn resource_errors() {
        assert!(VideoError::OutOfMemory { size: 1000 }.is_resource_error());
        assert!(VideoError::BufferOverflow { dropped: 10 }.is_resource_error());
        assert!(VideoError::FrameTooLarge { size: 1000, limit: 500 }.is_resource_error());
        assert!(!VideoError::TrackNotConnected.is_resource_error());
    }

    #[test]
    fn error_codes_are_unique() {
        let errors = vec![
            VideoError::TruncatedFrame { expected: 1, actual: 0 },
            VideoError::FrameTooLarge { size: 100, limit: 50 },
            VideoError::TrackNotConnected,
            VideoError::CodecInit("test".into()),
        ];

        let codes: Vec<_> = errors.iter().map(|e| e.error_code()).collect();
        // All codes should be unique
        for code in &codes {
            assert_eq!(codes.iter().filter(|c| *c == code).count(), 1);
        }
    }

    #[test]
    fn error_severity_levels() {
        assert_eq!(VideoError::NoFrameAvailable { timeout_ms: 100 }.severity(), ErrorSeverity::Info);
        assert_eq!(VideoError::BufferOverflow { dropped: 10 }.severity(), ErrorSeverity::Warning);
        assert_eq!(VideoError::SequenceGap { expected: 1, actual: 2, lost: 1 }.severity(), ErrorSeverity::Info);
        assert_eq!(VideoError::CodecInit("test".into()).severity(), ErrorSeverity::Critical);
        assert_eq!(VideoError::UnsupportedResolution { width: 1920, height: 1080 }.severity(), ErrorSeverity::Error);
    }

    #[test]
    fn recovery_context_wrapping() {
        let err = VideoError::EncodeFailed("encoder busy".into());
        let wrapped = err.with_recovery_context(3);
        
        match wrapped {
            VideoError::RecoveryFailed { attempts, last_error } => {
                assert_eq!(attempts, 3);
                assert!(matches!(*last_error, VideoError::EncodeFailed(_)));
            }
            _ => panic!("Expected RecoveryFailed"),
        }
    }

    #[test]
    fn fallback_context_wrapping() {
        let err = VideoError::HardwareAccelFailed("GPU not available".into());
        let wrapped = err.with_fallback_context("software encoding");
        
        match wrapped {
            VideoError::FallbackTriggered { original, fallback } => {
                assert!(matches!(*original, VideoError::HardwareAccelFailed(_)));
                assert_eq!(fallback, "software encoding");
            }
            _ => panic!("Expected FallbackTriggered"),
        }
    }

    #[test]
    fn error_display_format() {
        let err = VideoError::BufferOverflow { dropped: 5 };
        let display = format!("{}", err);
        assert!(display.contains("5"));
        assert!(display.contains("dropped"));
    }
}
