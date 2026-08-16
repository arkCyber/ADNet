//! Video statistics and metrics collection.
//!
//! Tracks encoding/decoding performance, quality metrics, and timing
//! information per DO-178C DAL-B requirements.

use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Video statistics snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VideoStats {
    /// Total frames encoded.
    pub frames_encoded: u64,
    /// Total frames decoded.
    pub frames_decoded: u64,
    /// Total keyframes encoded.
    pub keyframes_encoded: u64,
    /// Total frames dropped.
    pub frames_dropped: u64,
    /// Total bytes encoded.
    pub bytes_encoded: u64,
    /// Total bytes decoded.
    pub bytes_decoded: u64,
    /// Average encode time in milliseconds.
    pub avg_encode_time_ms: u64,
    /// Average decode time in milliseconds.
    pub avg_decode_time_ms: u64,
    /// Peak encode time in milliseconds.
    pub peak_encode_time_ms: u64,
    /// Peak decode time in milliseconds.
    pub peak_decode_time_ms: u64,
    /// Timestamp of last activity.
    pub last_update: u64,
}

impl VideoStats {
    /// Resets all statistics.
    pub fn reset(&mut self) {
        *self = Self::default();
        self.last_update = current_timestamp_ns();
    }

    /// Returns the total frames processed.
    pub fn total_frames(&self) -> u64 {
        self.frames_encoded + self.frames_decoded
    }

    /// Returns the total bytes transferred.
    pub fn total_bytes(&self) -> u64 {
        self.bytes_encoded + self.bytes_decoded
    }

    /// Returns the current framerate estimate (frames per second).
    pub fn estimated_fps(&self) -> f64 {
        let duration_s = self.last_update as f64 / 1_000_000_000.0;
        if duration_s > 0.0 {
            self.total_frames() as f64 / duration_s
        } else {
            0.0
        }
    }

    /// Returns the current bitrate estimate in kbps.
    pub fn estimated_bitrate_kbps(&self) -> u64 {
        let duration_s = self.last_update as f64 / 1_000_000_000.0;
        if duration_s > 0.0 {
            (self.total_bytes() as f64 * 8.0 / 1000.0 / duration_s) as u64
        } else {
            0
        }
    }

    /// Returns the effective compression ratio.
    pub fn compression_ratio(&self) -> Option<f64> {
        if self.bytes_decoded > 0 {
            Some(self.bytes_encoded as f64 / self.bytes_decoded as f64)
        } else {
            None
        }
    }
}

/// Stream-level statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamStats {
    /// Stream identifier.
    pub stream_id: String,
    /// Current state.
    pub state: StreamState,
    /// Video statistics.
    pub video: VideoStats,
    /// Quality metrics.
    pub quality: QualityMetrics,
}

impl StreamStats {
    /// Creates a new stream stats tracker.
    pub fn new(stream_id: String) -> Self {
        Self {
            stream_id,
            state: StreamState::Idle,
            video: VideoStats::default(),
            quality: QualityMetrics::default(),
        }
    }
}

/// Stream operational state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamState {
    /// Stream is idle.
    Idle,
    /// Stream is connecting.
    Connecting,
    /// Stream is active.
    Active,
    /// Stream is paused.
    Paused,
    /// Stream has ended.
    Ended,
    /// Stream has failed.
    Failed,
}

/// Quality metrics for adaptive streaming.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QualityMetrics {
    /// Actual framerate observed.
    pub actual_fps: f64,
    /// Target framerate.
    pub target_fps: u32,
    /// Actual bitrate in kbps.
    pub bitrate_kbps: u64,
    /// Target bitrate in kbps.
    pub target_bitrate_kbps: u32,
    /// Average encoding latency in milliseconds.
    pub avg_latency_ms: u64,
    /// Peak encoding latency in milliseconds.
    pub peak_latency_ms: u64,
    /// Packet loss percentage (0-100).
    pub packet_loss_pct: f64,
    /// Network jitter in milliseconds.
    pub jitter_ms: f64,
    /// Estimated bandwidth in kbps.
    pub estimated_bandwidth_kbps: u64,
    /// Quality score (0-100).
    pub quality_score: u8,
}

impl QualityMetrics {
    /// Returns true if quality is acceptable.
    pub fn is_acceptable(&self) -> bool {
        self.packet_loss_pct < 5.0 && self.jitter_ms < 50.0 && self.quality_score >= 60
    }

    /// Returns the recommended bitrate adjustment.
    pub fn recommended_bitrate_adjustment(&self) -> i32 {
        if self.packet_loss_pct > 10.0 || self.jitter_ms > 100.0 {
            -20 // Reduce by 20%
        } else if self.packet_loss_pct > 5.0 || self.jitter_ms > 50.0 {
            -10 // Reduce by 10%
        } else if self.quality_score < 70 && self.estimated_bandwidth_kbps > self.bitrate_kbps as u64 * 120 / 100 {
            10 // Increase by 10%
        } else {
            0 // No change
        }
    }
}

/// Frame timing information for latency analysis.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FrameTimings {
    /// Frame sequence number.
    pub seq: u32,
    /// Timestamp when frame was captured (ns).
    pub captured_at_ns: u64,
    /// Timestamp when frame was encoded (ns).
    pub encoded_at_ns: u64,
    /// Timestamp when frame was transmitted (ns).
    pub transmitted_at_ns: u64,
    /// Timestamp when frame was received (ns).
    pub received_at_ns: u64,
    /// Timestamp when frame was decoded (ns).
    pub decoded_at_ns: u64,
    /// Timestamp when frame was rendered (ns).
    pub rendered_at_ns: u64,
}

impl FrameTimings {
    /// Returns the end-to-end latency in milliseconds.
    pub fn end_to_end_latency_ms(&self) -> u64 {
        ((self.rendered_at_ns.saturating_sub(self.captured_at_ns)) / 1_000_000) as u64
    }

    /// Returns the network latency in milliseconds.
    pub fn network_latency_ms(&self) -> u64 {
        ((self.received_at_ns.saturating_sub(self.transmitted_at_ns)) / 1_000_000) as u64
    }

    /// Returns the encode time in milliseconds.
    pub fn encode_time_ms(&self) -> u64 {
        ((self.encoded_at_ns.saturating_sub(self.captured_at_ns)) / 1_000_000) as u64
    }

    /// Returns the decode time in milliseconds.
    pub fn decode_time_ms(&self) -> u64 {
        ((self.rendered_at_ns.saturating_sub(self.received_at_ns)) / 1_000_000) as u64
    }
}

/// Returns the current timestamp in nanoseconds since UNIX epoch.
fn current_timestamp_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // VideoStats Tests
    // ========================================================================

    #[test]
    fn video_stats_reset() {
        let mut stats = VideoStats::default();
        stats.frames_encoded = 100;
        stats.bytes_encoded = 1000;
        stats.reset();
        assert_eq!(stats.frames_encoded, 0);
    }

    #[test]
    fn video_stats_total_frames() {
        let mut stats = VideoStats::default();
        stats.frames_encoded = 100;
        stats.frames_decoded = 50;
        assert_eq!(stats.total_frames(), 150);
    }

    #[test]
    fn video_stats_total_bytes() {
        let mut stats = VideoStats::default();
        stats.bytes_encoded = 1000;
        stats.bytes_decoded = 500;
        assert_eq!(stats.total_bytes(), 1500);
    }

    #[test]
    fn video_stats_estimated_fps() {
        let mut stats = VideoStats::default();
        stats.frames_encoded = 30;
        stats.frames_decoded = 0;
        stats.last_update = 1_000_000_000; // 1 second
        let fps = stats.estimated_fps();
        assert!(fps >= 30.0);
    }

    #[test]
    fn video_stats_estimated_bitrate() {
        let mut stats = VideoStats::default();
        stats.bytes_encoded = 1_000_000; // 1 MB
        stats.bytes_decoded = 0;
        stats.last_update = 1_000_000_000; // 1 second
        let bitrate = stats.estimated_bitrate_kbps();
        assert!(bitrate >= 7000); // ~8 Mbps
    }

    #[test]
    fn video_stats_compression_ratio() {
        let mut stats = VideoStats::default();
        stats.bytes_encoded = 100;
        stats.bytes_decoded = 1000;
        assert_eq!(stats.compression_ratio(), Some(0.1));

        stats.bytes_decoded = 0;
        assert_eq!(stats.compression_ratio(), None);
    }

    #[test]
    fn video_stats_zero_duration() {
        let mut stats = VideoStats::default();
        stats.last_update = 0;
        assert_eq!(stats.estimated_fps(), 0.0);
        assert_eq!(stats.estimated_bitrate_kbps(), 0);
    }

    // ========================================================================
    // StreamStats Tests
    // ========================================================================

    #[test]
    fn stream_stats_creation() {
        let stats = StreamStats::new("test-stream".to_string());
        assert_eq!(stats.stream_id, "test-stream");
        assert_eq!(stats.state, StreamState::Idle);
    }

    #[test]
    fn stream_state_transitions() {
        assert_ne!(StreamState::Idle, StreamState::Active);
        assert_ne!(StreamState::Connecting, StreamState::Ended);
    }

    #[test]
    fn stream_state_debug() {
        let state = StreamState::Active;
        let debug_str = format!("{:?}", state);
        assert!(debug_str.contains("Active"));
    }

    // ========================================================================
    // QualityMetrics Tests
    // ========================================================================

    #[test]
    fn quality_metrics_adjustment() {
        let mut quality = QualityMetrics::default();
        quality.packet_loss_pct = 15.0;
        assert_eq!(quality.recommended_bitrate_adjustment(), -20);

        quality.packet_loss_pct = 7.0;
        assert_eq!(quality.recommended_bitrate_adjustment(), -10);

        quality.packet_loss_pct = 2.0;
        quality.jitter_ms = 30.0;
        quality.quality_score = 65;
        quality.estimated_bandwidth_kbps = 5000;
        quality.bitrate_kbps = 2000;
        assert_eq!(quality.recommended_bitrate_adjustment(), 10);

        quality.quality_score = 80;
        assert_eq!(quality.recommended_bitrate_adjustment(), 0);
    }

    #[test]
    fn quality_metrics_default() {
        let quality = QualityMetrics::default();
        assert_eq!(quality.actual_fps, 0.0);
        assert_eq!(quality.target_fps, 0);
        assert_eq!(quality.bitrate_kbps, 0);
    }

    #[test]
    fn quality_metrics_adjustment_high_loss() {
        let mut quality = QualityMetrics::default();
        quality.packet_loss_pct = 50.0;
        // High packet loss should result in significant reduction
        let adjustment = quality.recommended_bitrate_adjustment();
        assert!(adjustment < 0);
    }

    #[test]
    fn quality_metrics_adjustment_low_quality() {
        let mut quality = QualityMetrics::default();
        quality.packet_loss_pct = 0.0;
        quality.jitter_ms = 5.0;
        quality.quality_score = 30;
        quality.estimated_bandwidth_kbps = 5000;
        quality.bitrate_kbps = 500;
        // Low quality with available bandwidth should increase
        let adjustment = quality.recommended_bitrate_adjustment();
        assert!(adjustment > 0);
    }

    // ========================================================================
    // FrameTimings Tests
    // ========================================================================

    #[test]
    fn frame_timings_latency() {
        let timings = FrameTimings {
            seq: 1,
            captured_at_ns: 1000,
            encoded_at_ns: 1100,
            transmitted_at_ns: 1200,
            received_at_ns: 1300,
            decoded_at_ns: 1350,
            rendered_at_ns: 1400,
        };

        assert_eq!(timings.encode_time_ms(), 0); // < 1ms rounds to 0
        assert_eq!(timings.network_latency_ms(), 0);
        assert_eq!(timings.end_to_end_latency_ms(), 0);
    }

    #[test]
    fn frame_timings_calculation() {
        let timings = FrameTimings {
            seq: 1,
            captured_at_ns: 0,
            encoded_at_ns: 1_000_000,      // 1ms
            transmitted_at_ns: 5_000_000,   // 5ms
            received_at_ns: 10_000_000,    // 10ms
            decoded_at_ns: 11_000_000,     // 11ms
            rendered_at_ns: 12_000_000,    // 12ms
        };

        // Encode: 1ms (1_000_000 ns)
        assert_eq!(timings.encode_time_ms(), 1);

        // Network: received - transmitted = 10ms - 5ms = 5ms
        assert_eq!(timings.network_latency_ms(), 5);

        // Decode: rendered - received = 12ms - 10ms = 2ms
        assert_eq!(timings.decode_time_ms(), 2);

        // E2E: 12ms
        assert_eq!(timings.end_to_end_latency_ms(), 12);
    }

    #[test]
    fn frame_timings_serialization() {
        let timings = FrameTimings {
            seq: 42,
            captured_at_ns: 1000,
            encoded_at_ns: 2000,
            transmitted_at_ns: 3000,
            received_at_ns: 4000,
            decoded_at_ns: 5000,
            rendered_at_ns: 6000,
        };

        let json = serde_json::to_string(&timings).unwrap();
        assert!(json.contains("42"));

        let parsed: FrameTimings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.seq, 42);
    }

    // ========================================================================
    // Duration Helper Tests
    // ========================================================================

    #[test]
    fn duration_helpers() {
        let dur_100ms = Duration::from_millis(100);
        assert_eq!(dur_100ms.as_millis(), 100);

        let dur_1s = Duration::from_secs(1);
        assert_eq!(dur_1s.as_millis(), 1000);
    }
}
