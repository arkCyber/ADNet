//! Logging utilities for video pipeline debugging.
//!
//! Provides structured logging for:
//! - Bandwidth changes and quality transitions
//! - Frame encoding/decoding statistics
//! - Pipeline state changes
//! - Error conditions

use std::fmt;

/// Video module identifier for log filtering.
pub const VIDEO_MODULE: &str = "a3net::video";

/// Log levels for video operations.
#[derive(Debug, Clone, Copy)]
pub enum VideoLogLevel {
    /// Debug: detailed frame-level logging.
    Debug,
    /// Info: state changes and quality transitions.
    Info,
    /// Warn: degraded conditions but recoverable.
    Warn,
    /// Error: failures requiring attention.
    Error,
}

impl VideoLogLevel {
    /// Returns the corresponding tracing level.
    pub fn to_tracing_level(&self) -> tracing::Level {
        match self {
            VideoLogLevel::Debug => tracing::Level::DEBUG,
            VideoLogLevel::Info => tracing::Level::INFO,
            VideoLogLevel::Warn => tracing::Level::WARN,
            VideoLogLevel::Error => tracing::Level::ERROR,
        }
    }
}

/// Video event for logging.
#[derive(Debug, Clone)]
pub enum VideoEvent {
    /// Pipeline state change.
    PipelineStateChange { from: &'static str, to: &'static str },
    /// Quality level change.
    QualityChange { from: &'static str, to: &'static str, reason: &'static str },
    /// Bandwidth update.
    BandwidthUpdate { bandwidth_kbps: u32, packet_loss_pct: f64 },
    /// Frame encoded.
    FrameEncoded { frame_id: u64, size_bytes: usize, is_keyframe: bool },
    /// Frame decoded.
    FrameDecoded { frame_id: u64, size_bytes: usize, decode_time_ms: u64 },
    /// Frame dropped.
    FrameDropped { frame_id: u64, reason: &'static str },
    /// Bitrate adjustment.
    BitrateChange { from_kbps: u32, to_kbps: u32 },
    /// Error occurred.
    Error { code: &'static str, message: String },
    /// Audio-only mode activated.
    AudioOnlyActivated { reason: &'static str },
    /// Image fallback mode activated.
    ImageFallbackActivated { quality: u8, interval_ms: u64 },
}

impl fmt::Display for VideoEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VideoEvent::PipelineStateChange { from, to } => {
                write!(f, "Pipeline: {} -> {}", from, to)
            }
            VideoEvent::QualityChange { from, to, reason } => {
                write!(f, "Quality: {} -> {} ({})", from, to, reason)
            }
            VideoEvent::BandwidthUpdate { bandwidth_kbps, packet_loss_pct } => {
                write!(f, "Bandwidth: {} kbps, {:.1}% loss", bandwidth_kbps, packet_loss_pct)
            }
            VideoEvent::FrameEncoded { frame_id, size_bytes, is_keyframe } => {
                let frame_type = if *is_keyframe { "KEY" } else { "DELTA" };
                write!(f, "Encoded #{} ({}): {} bytes", frame_id, frame_type, size_bytes)
            }
            VideoEvent::FrameDecoded { frame_id, size_bytes, decode_time_ms } => {
                write!(f, "Decoded #{}: {} bytes in {}ms", frame_id, size_bytes, decode_time_ms)
            }
            VideoEvent::FrameDropped { frame_id, reason } => {
                write!(f, "Dropped #{}: {}", frame_id, reason)
            }
            VideoEvent::BitrateChange { from_kbps, to_kbps } => {
                write!(f, "Bitrate: {} -> {} kbps", from_kbps, to_kbps)
            }
            VideoEvent::Error { code, message } => {
                write!(f, "Error [{}]: {}", code, message)
            }
            VideoEvent::AudioOnlyActivated { reason } => {
                write!(f, "Audio-only mode: {}", reason)
            }
            VideoEvent::ImageFallbackActivated { quality, interval_ms } => {
                write!(f, "Image fallback: quality={}, interval={}ms", quality, interval_ms)
            }
        }
    }
}

/// Video logger for structured logging.
pub struct VideoLogger {
    module: &'static str,
}

impl VideoLogger {
    /// Creates a new video logger.
    pub fn new(module: &'static str) -> Self {
        Self { module }
    }

    /// Logs a video event at the specified level.
    pub fn log(&self, level: VideoLogLevel, event: &VideoEvent) {
        match level {
            VideoLogLevel::Debug => tracing::debug!(target: "a3net::video", event = %event, "video event"),
            VideoLogLevel::Info => tracing::info!(target: "a3net::video", event = %event, "video event"),
            VideoLogLevel::Warn => tracing::warn!(target: "a3net::video", event = %event, "video event"),
            VideoLogLevel::Error => tracing::error!(target: "a3net::video", event = %event, "video event"),
        }
    }

    /// Logs at debug level.
    pub fn debug(&self, msg: impl AsRef<str>) {
        tracing::debug!(target: "a3net::video", "{}", msg.as_ref());
    }

    /// Logs at info level.
    pub fn info(&self, msg: impl AsRef<str>) {
        tracing::info!(target: "a3net::video", "{}", msg.as_ref());
    }

    /// Logs at warn level.
    pub fn warn(&self, msg: impl AsRef<str>) {
        tracing::warn!(target: "a3net::video", "{}", msg.as_ref());
    }

    /// Logs at error level.
    pub fn error(&self, msg: impl AsRef<str>) {
        tracing::error!(target: "a3net::video", "{}", msg.as_ref());
    }

    /// Logs bandwidth update.
    pub fn bandwidth(&self, bandwidth_kbps: u32, packet_loss_pct: f64, latency_ms: u32) {
        tracing::info!(
            target: "a3net::video",
            bandwidth_kbps,
            packet_loss_pct,
            latency_ms,
            "network metrics"
        );
    }

    /// Logs quality transition.
    pub fn quality_transition(&self, from: &'static str, to: &'static str, reason: &'static str) {
        self.log(VideoLogLevel::Info, &VideoEvent::QualityChange { from, to, reason });
    }

    /// Logs frame metrics.
    pub fn frame_metrics(&self, frame_id: u64, size_bytes: usize, encode_time_us: u64) {
        tracing::trace!(
            target: "a3net::video",
            frame_id,
            size_bytes,
            encode_time_us,
            "frame metrics"
        );
    }

    /// Logs error with context.
    pub fn log_error(&self, code: &'static str, message: impl Into<String>) {
        self.log(VideoLogLevel::Error, &VideoEvent::Error {
            code,
            message: message.into(),
        });
    }
}

impl Default for VideoLogger {
    fn default() -> Self {
        Self::new(VIDEO_MODULE)
    }
}

// ============================================================================
// Convenience functions (available at crate root)
// ============================================================================

/// Logs a debug message with video context.
pub fn video_debug(msg: impl AsRef<str>) {
    tracing::debug!(target: "a3net::video", "{}", msg.as_ref());
}

/// Logs an info message with video context.
pub fn video_info(msg: impl AsRef<str>) {
    tracing::info!(target: "a3net::video", "{}", msg.as_ref());
}

/// Logs a warning with video context.
pub fn video_warn(msg: impl AsRef<str>) {
    tracing::warn!(target: "a3net::video", "{}", msg.as_ref());
}

/// Logs an error with video context.
pub fn video_error(msg: impl AsRef<str>) {
    tracing::error!(target: "a3net::video", "{}", msg.as_ref());
}

/// Logs pipeline state change.
pub fn video_state_change(from: &'static str, to: &'static str) {
    tracing::info!(
        target: "a3net::video",
        from = from,
        to = to,
        "pipeline state change"
    );
}

/// Logs quality change with reason.
pub fn video_quality_change(from: &'static str, to: &'static str, reason: &'static str) {
    tracing::info!(
        target: "a3net::video",
        from = from,
        to = to,
        reason = reason,
        "quality change"
    );
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_event_display() {
        let event = VideoEvent::BandwidthUpdate {
            bandwidth_kbps: 1000,
            packet_loss_pct: 2.5,
        };
        assert!(event.to_string().contains("1000"));
        assert!(event.to_string().contains("2.5"));
    }

    #[test]
    fn test_video_event_display_keyframe() {
        let event = VideoEvent::FrameEncoded {
            frame_id: 123,
            size_bytes: 5000,
            is_keyframe: true,
        };
        let s = event.to_string();
        assert!(s.contains("123"));
        assert!(s.contains("KEY"));
    }

    #[test]
    fn test_log_level_conversion() {
        assert_eq!(
            VideoLogLevel::Debug.to_tracing_level(),
            tracing::Level::DEBUG
        );
        assert_eq!(
            VideoLogLevel::Info.to_tracing_level(),
            tracing::Level::INFO
        );
        assert_eq!(
            VideoLogLevel::Warn.to_tracing_level(),
            tracing::Level::WARN
        );
        assert_eq!(
            VideoLogLevel::Error.to_tracing_level(),
            tracing::Level::ERROR
        );
    }

    #[test]
    fn test_logger_creation() {
        let logger = VideoLogger::new("test::module");
        logger.debug("test debug");
        logger.info("test info");
        logger.warn("test warning");
        logger.error("test error");
    }

    #[test]
    fn test_convenience_functions() {
        video_debug("debug message");
        video_info("info message");
        video_warn("warning message");
        video_error("error message");
        video_state_change("Idle", "Running");
        video_quality_change("Low", "High", "bandwidth increase");
    }
}
