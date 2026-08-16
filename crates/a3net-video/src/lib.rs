//! `a3net-video` — aerospace-grade real-time video streaming for A3Net.
//!
//! DO-178C DAL-B. Provides end-to-end video pipeline: capture → encode →
//! stream → decode → render, integrated with WebRTC SRTP media tracks.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                         Video Pipeline                               │
//! ├─────────────────────────────────────────────────────────────────────┤
//! │                                                                      │
//! │   Camera/Screen                                                      │
//! │      │                                                               │
//! │      ▼  capture                                                      │
//! │   RawFrame { width, height, timestamp, pixel_data }                 │
//! │      │                                                               │
//! │      ▼  encode                                                       │
//! │   EncodedFrame { codec, keyframe, data, timestamp, seq }             │
//! │      │                                                               │
//! │      ▼  stream (via WebRTC SRTP)                                     │
//! │   MediaTrack ──── WebRTC PeerConnection ──── Peer                   │
//! │      │                                                               │
//! │      ▼  decode                                                       │
//! │   EncodedFrame                                                       │
//! │      │                                                               │
//! │      ▼  render                                                       │
//! │   RawFrame                                                           │
//! │      │                                                               │
//! │      ▼  display                                                      │
//! │   [Render Target]                                                    │
//! │                                                                      │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Safety Invariants (DO-178C DAL-B)
//!
//! - Every public function returns `Result<_, VideoError>` and never `unwrap()`s.
//! - Frame buffers carry explicit length prefixes so truncated streams raise
//!   `VideoError::TruncatedFrame` rather than buffer overruns.
//! - All timestamps are monotonic `u64` nanoseconds since epoch.
//! - Sequence numbers are `u32` wrapping integers; gap detection is explicit.
//! - Keyframes are mandatory every `KeyFrameInterval` frames.
//!
//! ## Feature Flags
//!
//! - `capture` — cross-platform video capture (V4L2, AVFoundation, MediaFoundation, WASM)
//! - `h264` — H.264 codec (decoder always available, encoder via `h264-write`)
//! - `vpx` — VP8/VP9 codec support
//! - `image-processing` — image manipulation utilities
//! - `hwaccel` — hardware acceleration hints (software fallback always present)
//! - `crossbeam` — low-latency crossbeam channels for frame passing
//! - `metrics` — statistics and metrics collection
//! - `aerospace` — DO-178C DAL-B compliance suite (SR-1..SR-12)
//!
//! ## Platform Support
//!
//! The `capture` feature enables platform-specific video capture:
//!
//! | Platform | Backend | Feature Flag |
//! |----------|---------|--------------|
//! | Linux | V4L2 | `capture` |
//! | macOS | AVFoundation | `capture` |
//! | Windows | MediaFoundation | `capture` |
//! | WebAssembly | MediaDevices | `capture` |
//! | Other | Software Generator | (always available) |

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod aerospace;
pub mod adaptive;
pub mod audio_first;
pub mod bandwidth;
pub mod capture;
pub mod codec;
pub mod config;
pub mod error;
pub mod frame;
pub mod jitter_buffer;
pub mod log;
pub mod pipeline;
pub mod preflight;
pub mod resilience;
pub mod stats;
pub mod track;

#[cfg(feature = "image-processing")]
pub mod image_utils;

#[cfg(feature = "h264")]
pub mod h264;

#[cfg(feature = "vpx")]
pub mod vpx;

// Re-exports for ergonomic top-level usage.
pub use adaptive::{AdaptiveBitrateController, BandwidthEstimator, NetworkQuality, QualityLevel};
pub use bandwidth::{BandwidthManager, BandwidthManagerConfig, BandwidthManagerRunner, BandwidthStats, NetworkState};
pub use audio_first::{
    AudioFirstManager, AudioRequirements, BandwidthPolicy, EscalationManager, EscalationLevel,
    EscalationAction, ImageFallbackManager, ImageFallbackConfig, ImageSnapshot,
    MediaPriority, PriorityFrame, PriorityQueue, StatusBroadcaster, StatusMessage,
    StatusType, VisualFallbackMode,
};
pub use capture::{CaptureFactory, Platform, VideoCapture, current_platform};
pub use codec::{FrameType, VideoCodec, VideoCodecProfile, VideoCodecLevel};
pub use config::{
    CodecPreset, EncoderTuning, Framerate, KeyFrameInterval, MotionEstimation,
    PipelineConfig, QualityPreset, RateControlMode, Resolution, TrackConfig,
    VideoConfig, VideoQuality,
};
pub use log::{
    video_debug, video_error, video_info, video_quality_change, video_state_change,
    video_warn, VideoEvent, VideoLogLevel, VideoLogger,
};
pub use error::{VideoError, VideoResult, ErrorSeverity};
pub use frame::{Frame, FrameId, RawFrame, EncodedFrame, VideoFrame};
pub use jitter_buffer::{JitterBuffer, JitterBufferMode, JitterBufferStats, GapDetector, FrameReorder};
pub use pipeline::{VideoPipeline, PipelineEvent, PipelineState};
pub use preflight::{
    run_preflight_checks, run_interactive_preflight, PreFlightChecker,
    PreFlightReport, DeviceCheckResult, CheckStatus,
};
pub use resilience::{
    CircuitBreaker, CircuitState, HealthMonitor, HealthStatus, HealthMetrics, 
    RateLimiter, RetryStrategy, RecoveryConfig, RecoveryState, RecoveryResult, ErrorRecovery,
    FallbackStrategy,
};
pub use stats::{VideoStats, StreamStats, QualityMetrics, FrameTimings};
pub use track::{VideoTrack, TrackEvent, MediaTrackHandle};
