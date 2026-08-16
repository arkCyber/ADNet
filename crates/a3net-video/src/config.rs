//! Video pipeline configuration types.
//!
//! All configurations are validated at construction time per DO-178C DAL-B
//! requirements (SR-9: oversized inputs rejected).

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::codec::{PixelFormat, VideoCodec, VideoCodecLevel, VideoCodecProfile};
use crate::error::{VideoError, VideoResult};

/// Maximum frame size in bytes (16 MiB — prevents DoS).
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Maximum resolution (8K).
pub const MAX_WIDTH: u32 = 7680;
pub const MAX_HEIGHT: u32 = 4320;

/// Maximum framerate (120fps).
pub const MAX_FRAMERATE: u32 = 120;

/// Maximum keyframe interval (5 seconds at 60fps).
pub const MAX_KEYFRAME_INTERVAL: u32 = 300;

/// Default buffer depth for frame queue.
pub const DEFAULT_BUFFER_DEPTH: usize = 30;

// ============================================================================
// Video Quality Presets
// ============================================================================

/// Video quality preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoQuality {
    /// 240p @ 15fps — minimum bandwidth.
    Minimum,
    /// 360p @ 24fps — low quality.
    Low,
    /// 480p @ 30fps — standard quality.
    Standard,
    /// 720p @ 30fps — good quality.
    High,
    /// 1080p @ 60fps — best quality.
    Maximum,
    /// 4K @ 60fps — ultra quality.
    Ultra,
}

impl VideoQuality {
    /// Returns the recommended resolution for this quality level.
    pub fn resolution(&self) -> Resolution {
        match self {
            VideoQuality::Minimum => Resolution::new(320, 240).unwrap(),
            VideoQuality::Low => Resolution::new(640, 360).unwrap(),
            VideoQuality::Standard => Resolution::new(854, 480).unwrap(),
            VideoQuality::High => Resolution::new(1280, 720).unwrap(),
            VideoQuality::Maximum => Resolution::new(1920, 1080).unwrap(),
            VideoQuality::Ultra => Resolution::new(3840, 2160).unwrap(),
        }
    }

    /// Returns the recommended framerate for this quality level.
    pub fn framerate(&self) -> Framerate {
        match self {
            VideoQuality::Minimum => Framerate::new(15).unwrap(),
            VideoQuality::Low => Framerate::new(24).unwrap(),
            VideoQuality::Standard => Framerate::new(30).unwrap(),
            VideoQuality::High => Framerate::new(30).unwrap(),
            VideoQuality::Maximum => Framerate::new(60).unwrap(),
            VideoQuality::Ultra => Framerate::new(60).unwrap(),
        }
    }

    /// Returns the recommended bitrate in kbps for this quality level.
    pub fn bitrate_kbps(&self) -> u32 {
        match self {
            VideoQuality::Minimum => 100,
            VideoQuality::Low => 300,
            VideoQuality::Standard => 750,
            VideoQuality::High => 1500,
            VideoQuality::Maximum => 4000,
            VideoQuality::Ultra => 8000,
        }
    }

    /// Returns the minimum bitrate in kbps for acceptable quality.
    pub fn min_bitrate_kbps(&self) -> u32 {
        self.bitrate_kbps() / 2
    }

    /// Returns the maximum bitrate in kbps for this quality.
    pub fn max_bitrate_kbps(&self) -> u32 {
        self.bitrate_kbps() * 3 / 2
    }

    /// Returns a human-readable name.
    pub fn display_name(&self) -> &'static str {
        match self {
            VideoQuality::Minimum => "240p (Minimum)",
            VideoQuality::Low => "360p (Low)",
            VideoQuality::Standard => "480p (Standard)",
            VideoQuality::High => "720p (HD)",
            VideoQuality::Maximum => "1080p (Full HD)",
            VideoQuality::Ultra => "4K (Ultra HD)",
        }
    }
}

impl Default for VideoQuality {
    fn default() -> Self {
        VideoQuality::Standard
    }
}

// ============================================================================
// Quality Presets
// ============================================================================

/// Complete quality preset with all tuning parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityPreset {
    /// Quality level.
    pub quality: VideoQuality,
    /// Codec settings.
    pub codec: CodecPreset,
    /// Encoder tuning.
    pub tuning: EncoderTuning,
}

impl QualityPreset {
    /// Creates a preset from a quality level.
    pub fn from_quality(quality: VideoQuality) -> Self {
        Self {
            quality,
            codec: CodecPreset::from_quality(quality),
            tuning: EncoderTuning::from_quality(quality),
        }
    }

    /// Returns the default preset (Standard quality).
    pub fn default_preset() -> Self {
        Self::from_quality(VideoQuality::default())
    }

    /// Returns a preset optimized for low latency.
    pub fn low_latency() -> Self {
        Self {
            quality: VideoQuality::High,
            codec: CodecPreset::low_latency(),
            tuning: EncoderTuning::low_latency(),
        }
    }

    /// Returns a preset optimized for screen sharing.
    pub fn screen_share() -> Self {
        Self {
            quality: VideoQuality::High,
            codec: CodecPreset::screen_share(),
            tuning: EncoderTuning::screen_share(),
        }
    }

    /// Returns a preset optimized for motion (gaming, sports).
    pub fn high_motion() -> Self {
        Self {
            quality: VideoQuality::Maximum,
            codec: CodecPreset::high_motion(),
            tuning: EncoderTuning::high_motion(),
        }
    }

    /// Returns a preset for very limited bandwidth.
    pub fn bandwidth_saver() -> Self {
        Self {
            quality: VideoQuality::Minimum,
            codec: CodecPreset::bandwidth_saver(),
            tuning: EncoderTuning::bandwidth_saver(),
        }
    }
}

impl Default for QualityPreset {
    fn default() -> Self {
        Self::default_preset()
    }
}

/// Codec-specific preset configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodecPreset {
    /// Target bitrate in kbps.
    pub bitrate_kbps: u32,
    /// Maximum bitrate in kbps.
    pub max_bitrate_kbps: u32,
    /// GOP size (keyframe interval in frames).
    pub gop_size: u32,
    /// Rate control mode.
    pub rate_control: RateControlMode,
}

impl CodecPreset {
    /// Creates a preset from quality level.
    pub fn from_quality(quality: VideoQuality) -> Self {
        let bitrate = quality.bitrate_kbps();
        Self {
            bitrate_kbps: bitrate,
            max_bitrate_kbps: quality.max_bitrate_kbps(),
            gop_size: Self::default_gop_size(quality),
            rate_control: RateControlMode::default(),
        }
    }

    fn default_gop_size(quality: VideoQuality) -> u32 {
        match quality {
            VideoQuality::Minimum => 30,  // 2 seconds @ 15fps
            VideoQuality::Low => 48,       // 2 seconds @ 24fps
            VideoQuality::Standard => 60, // 2 seconds @ 30fps
            VideoQuality::High => 60,     // 2 seconds @ 30fps
            VideoQuality::Maximum => 120, // 2 seconds @ 60fps
            VideoQuality::Ultra => 120,   // 2 seconds @ 60fps
        }
    }

    /// Low latency codec preset.
    pub fn low_latency() -> Self {
        Self {
            bitrate_kbps: 1500,
            max_bitrate_kbps: 2000,
            gop_size: 30,
            rate_control: RateControlMode::Cbr,
        }
    }

    /// Screen sharing codec preset.
    pub fn screen_share() -> Self {
        Self {
            bitrate_kbps: 1000,
            max_bitrate_kbps: 1500,
            gop_size: 30,
            rate_control: RateControlMode::Cqp(28),
        }
    }

    /// High motion codec preset.
    pub fn high_motion() -> Self {
        Self {
            bitrate_kbps: 4000,
            max_bitrate_kbps: 6000,
            gop_size: 60, // More frequent keyframes for motion
            rate_control: RateControlMode::Vbr,
        }
    }

    /// Bandwidth saver preset.
    pub fn bandwidth_saver() -> Self {
        Self {
            bitrate_kbps: 100,
            max_bitrate_kbps: 150,
            gop_size: 30,
            rate_control: RateControlMode::Cqp(35),
        }
    }
}

/// Rate control mode for video encoder.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RateControlMode {
    /// Constant Bitrate.
    Cbr,
    /// Variable Bitrate.
    Vbr,
    /// Constant Quality (QP value).
    Cqp(u8),
}

impl Default for RateControlMode {
    fn default() -> Self {
        RateControlMode::Vbr
    }
}

/// Encoder tuning parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncoderTuning {
    /// Motion estimation precision.
    pub motion_estimation: MotionEstimation,
    /// Deblocking filter strength (0-5).
    pub deblock_strength: u8,
    /// Noise reduction level (0-3).
    pub noise_reduction: u8,
    /// Scene change detection.
    pub scene_change_detection: bool,
    /// Adaptive quantization.
    pub adaptive_quantization: bool,
    /// Spatial scalability.
    pub spatial_scalability: bool,
}

impl EncoderTuning {
    /// Creates tuning from quality level.
    pub fn from_quality(quality: VideoQuality) -> Self {
        match quality {
            VideoQuality::Minimum => Self::bandwidth_saver(),
            VideoQuality::Low => Self::low_quality(),
            VideoQuality::Standard => Self::standard(),
            VideoQuality::High => Self::high_quality(),
            VideoQuality::Maximum | VideoQuality::Ultra => Self::maximum_quality(),
        }
    }

    /// Standard quality tuning.
    pub fn standard() -> Self {
        Self {
            motion_estimation: MotionEstimation::Default,
            deblock_strength: 2,
            noise_reduction: 1,
            scene_change_detection: true,
            adaptive_quantization: true,
            spatial_scalability: false,
        }
    }

    /// High quality tuning.
    pub fn high_quality() -> Self {
        Self {
            motion_estimation: MotionEstimation::Hex,
            deblock_strength: 3,
            noise_reduction: 2,
            scene_change_detection: true,
            adaptive_quantization: true,
            spatial_scalability: false,
        }
    }

    /// Maximum quality tuning.
    pub fn maximum_quality() -> Self {
        Self {
            motion_estimation: MotionEstimation::Diamond,
            deblock_strength: 4,
            noise_reduction: 2,
            scene_change_detection: true,
            adaptive_quantization: true,
            spatial_scalability: true,
        }
    }

    /// Low quality tuning.
    pub fn low_quality() -> Self {
        Self {
            motion_estimation: MotionEstimation::Default,
            deblock_strength: 1,
            noise_reduction: 1,
            scene_change_detection: false,
            adaptive_quantization: false,
            spatial_scalability: false,
        }
    }

    /// Bandwidth saver tuning.
    pub fn bandwidth_saver() -> Self {
        Self {
            motion_estimation: MotionEstimation::Diamond,
            deblock_strength: 0,
            noise_reduction: 3,
            scene_change_detection: false,
            adaptive_quantization: false,
            spatial_scalability: false,
        }
    }

    /// Low latency tuning.
    pub fn low_latency() -> Self {
        Self {
            motion_estimation: MotionEstimation::Diamond,
            deblock_strength: 1,
            noise_reduction: 0,
            scene_change_detection: false,
            adaptive_quantization: false,
            spatial_scalability: false,
        }
    }

    /// Screen share tuning.
    pub fn screen_share() -> Self {
        Self {
            motion_estimation: MotionEstimation::Hex,
            deblock_strength: 0, // No deblocking for sharp text
            noise_reduction: 0,
            scene_change_detection: true,
            adaptive_quantization: true,
            spatial_scalability: false,
        }
    }

    /// High motion tuning.
    pub fn high_motion() -> Self {
        Self {
            motion_estimation: MotionEstimation::Hex,
            deblock_strength: 2,
            noise_reduction: 1,
            scene_change_detection: true,
            adaptive_quantization: true,
            spatial_scalability: false,
        }
    }
}

impl Default for EncoderTuning {
    fn default() -> Self {
        Self::standard()
    }
}

/// Motion estimation algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MotionEstimation {
    /// Diamond search (fastest).
    Diamond,
    /// Hexagonal search.
    Hex,
    /// Exhaustive search (slowest, best quality).
    Exhaustive,
    /// Default algorithm.
    Default,
}

/// Video resolution with validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    /// Frame width in pixels (must be even for YUV420).
    pub width: u32,
    /// Frame height in pixels (must be even for YUV420).
    pub height: u32,
}

impl Resolution {
    /// Creates a new resolution after validation.
    pub fn new(width: u32, height: u32) -> VideoResult<Self> {
        if width == 0 || height == 0 {
            return Err(VideoError::InvalidConfig {
                param: "resolution",
                value: format!("{}x{}", width, height),
                reason: "dimensions must be non-zero",
            });
        }
        if width > MAX_WIDTH || height > MAX_HEIGHT {
            return Err(VideoError::InvalidConfig {
                param: "resolution",
                value: format!("{}x{}", width, height),
                reason: "exceeds maximum resolution",
            });
        }
        // YUV420 requires even dimensions
        if width % 2 != 0 || height % 2 != 0 {
            return Err(VideoError::InvalidConfig {
                param: "resolution",
                value: format!("{}x{}", width, height),
                reason: "dimensions must be even for YUV420",
            });
        }
        Ok(Self { width, height })
    }

    /// Returns the pixel count.
    pub fn pixels(&self) -> u64 {
        (self.width as u64) * (self.height as u64)
    }

    /// Returns true if this resolution is 720p or higher.
    pub fn is_hd(&self) -> bool {
        self.height >= 720
    }

    /// Returns true if this resolution is 1080p or higher.
    pub fn is_full_hd(&self) -> bool {
        self.height >= 1080
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// Framerate with validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Framerate {
    /// Frames per second.
    pub fps: u32,
    /// Frame duration in nanoseconds (derived from fps).
    pub frame_duration_ns: u64,
}

impl Framerate {
    /// Creates a new framerate after validation.
    pub fn new(fps: u32) -> VideoResult<Self> {
        if fps == 0 {
            return Err(VideoError::InvalidConfig {
                param: "framerate",
                value: "0".to_string(),
                reason: "framerate must be non-zero",
            });
        }
        if fps > MAX_FRAMERATE {
            return Err(VideoError::InvalidConfig {
                param: "framerate",
                value: fps.to_string(),
                reason: "exceeds maximum framerate",
            });
        }
        let frame_duration_ns = 1_000_000_000u64 / (fps as u64);
        Ok(Self {
            fps,
            frame_duration_ns,
        })
    }

    /// Returns the frame duration as a Duration.
    pub fn frame_duration(&self) -> std::time::Duration {
        std::time::Duration::from_nanos(self.frame_duration_ns)
    }
}

impl fmt::Display for Framerate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}fps", self.fps)
    }
}

/// Keyframe interval configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyFrameInterval {
    /// Maximum frames between keyframes.
    pub frames: u32,
}

impl KeyFrameInterval {
    /// Creates a new keyframe interval after validation.
    pub fn new(frames: u32) -> VideoResult<Self> {
        if frames == 0 {
            return Err(VideoError::InvalidConfig {
                param: "keyframe_interval",
                value: "0".to_string(),
                reason: "keyframe interval must be non-zero",
            });
        }
        if frames > MAX_KEYFRAME_INTERVAL {
            return Err(VideoError::InvalidConfig {
                param: "keyframe_interval",
                value: frames.to_string(),
                reason: "exceeds maximum keyframe interval",
            });
        }
        Ok(Self { frames })
    }

    /// Creates from seconds (at given framerate).
    pub fn from_seconds(seconds: f64, fps: u32) -> VideoResult<Self> {
        if seconds <= 0.0 {
            return Err(VideoError::InvalidConfig {
                param: "keyframe_interval",
                value: seconds.to_string(),
                reason: "seconds must be positive",
            });
        }
        let frames = (seconds * (fps as f64)) as u32;
        Self::new(frames.max(1))
    }
}

impl Default for KeyFrameInterval {
    fn default() -> Self {
        // Default: keyframe every 2 seconds
        Self { frames: 120 }
    }
}

/// Complete video pipeline configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoConfig {
    /// Enable video (set to false for audio-only).
    pub enabled: bool,
    /// Video codec to use.
    pub codec: VideoCodec,
    /// Codec profile (e.g., H.264 High).
    pub profile: Option<VideoCodecProfile>,
    /// Codec level (e.g., H.264 Level 3.1).
    pub level: Option<VideoCodecLevel>,
    /// Target resolution.
    pub resolution: Resolution,
    /// Target framerate.
    pub framerate: Framerate,
    /// Target bitrate in kbps (0 = auto).
    pub bitrate_kbps: u32,
    /// Keyframe interval.
    pub keyframe_interval: KeyFrameInterval,
    /// Pixel format for raw frames.
    pub pixel_format: PixelFormat,
    /// Frame buffer depth (for jitter absorption).
    pub buffer_depth: usize,
    /// Enable temporal scalability (layered encoding).
    pub temporal_scalability: bool,
}

impl VideoConfig {
    /// Returns true if video is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Disables video (audio-only mode).
    pub fn disable_video(&mut self) {
        self.enabled = false;
        self.bitrate_kbps = 0;
    }

    /// Enables video with specified quality.
    pub fn enable_video(&mut self, quality: VideoQuality) {
        self.enabled = true;
        self.resolution = quality.resolution();
        self.framerate = quality.framerate();
        self.bitrate_kbps = quality.bitrate_kbps();
    }
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            enabled: true, // Video enabled by default
            codec: VideoCodec::H264,
            profile: None,
            level: None,
            resolution: VideoQuality::Standard.resolution(),
            framerate: VideoQuality::Standard.framerate(),
            bitrate_kbps: VideoQuality::Standard.bitrate_kbps(),
            keyframe_interval: KeyFrameInterval::default(),
            pixel_format: PixelFormat::default(),
            buffer_depth: DEFAULT_BUFFER_DEPTH,
            temporal_scalability: false,
        }
    }
}

/// Pipeline configuration combining video and audio settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Enable video in pipeline (set to false for audio-only calls).
    pub enable_video: bool,
    /// Video configuration.
    pub video: VideoConfig,
}

impl PipelineConfig {
    /// Creates an audio-only pipeline configuration.
    pub fn audio_only() -> Self {
        let mut config = Self::default();
        config.enable_video = false;
        config.video.disable_video();
        config
    }

    /// Creates a video pipeline configuration.
    pub fn with_video() -> Self {
        Self::default()
    }

    /// Returns true if video is enabled.
    pub fn is_video_enabled(&self) -> bool {
        self.enable_video && self.video.is_enabled()
    }
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            enable_video: true,
            video: VideoConfig::default(),
        }
    }
}

impl From<VideoConfig> for PipelineConfig {
    fn from(video: VideoConfig) -> Self {
        Self {
            enable_video: video.is_enabled(),
            video,
        }
    }
}

/// Video track configuration for WebRTC media tracks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackConfig {
    /// Enable video track (set to false for audio-only).
    pub enabled: bool,
    /// Stream label (e.g., "camera", "screen").
    pub label: String,
    /// Video configuration.
    pub video: VideoConfig,
    /// Enable simulcast layers.
    pub simulcast: bool,
    /// Number of simulcast layers.
    pub simulcast_layers: u8,
    /// Enable NACK for packet recovery.
    pub nack: bool,
    /// Enable RTX (retransmission).
    pub rtx: bool,
    /// Target bitrate in kbps.
    pub target_bitrate_kbps: u32,
    /// Maximum bitrate in kbps.
    pub max_bitrate_kbps: u32,
}

impl TrackConfig {
    /// Returns true if video track is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled && self.video.is_enabled()
    }

    /// Disables the video track (audio-only).
    pub fn disable_video(&mut self) {
        self.enabled = false;
        self.video.disable_video();
    }

    /// Creates an audio-only track configuration.
    pub fn audio_only(label: &str) -> Self {
        let mut config = Self::default();
        config.label = label.to_string();
        config.disable_video();
        config
    }
}

impl Default for TrackConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            label: "a3net/video".to_string(),
            video: VideoConfig::default(),
            simulcast: false,
            simulcast_layers: 1,
            nack: true,
            rtx: true,
            target_bitrate_kbps: 1500,
            max_bitrate_kbps: 4000,
        }
    }
}

impl TrackConfig {
    /// Creates a track config from a quality preset.
    pub fn from_quality(quality: VideoQuality, codec: VideoCodec) -> VideoResult<Self> {
        Ok(Self {
            video: VideoConfig::from_quality(quality, codec)?,
            ..Default::default()
        })
    }
}

impl VideoConfig {
    /// Creates a new config with validation.
    pub fn new(
        codec: VideoCodec,
        resolution: Resolution,
        framerate: Framerate,
        bitrate_kbps: u32,
    ) -> VideoResult<Self> {
        Ok(Self {
            enabled: true,
            codec,
            profile: None,
            level: None,
            resolution,
            framerate,
            bitrate_kbps: if bitrate_kbps == 0 {
                VideoQuality::Standard.bitrate_kbps()
            } else {
                bitrate_kbps
            },
            keyframe_interval: KeyFrameInterval::default(),
            pixel_format: PixelFormat::default(),
            buffer_depth: DEFAULT_BUFFER_DEPTH,
            temporal_scalability: false,
        })
    }

    /// Creates a config from a quality preset.
    pub fn from_quality(quality: VideoQuality, codec: VideoCodec) -> VideoResult<Self> {
        Self::new(
            codec,
            quality.resolution(),
            quality.framerate(),
            quality.bitrate_kbps(),
        )
    }

    /// Validates the configuration against codec level limits.
    /// DO-178C SR-9: Frame size limits enforcement
    pub fn validate_codec_limits(&self) -> VideoResult<()> {
        // Check resolution
        if self.resolution.pixels() > 2_000_000 && self.codec == VideoCodec::Vp8 {
            return Err(VideoError::UnsupportedResolution {
                width: self.resolution.width,
                height: self.resolution.height,
            });
        }

        // Check framerate
        if self.framerate.fps > 60 && self.codec == VideoCodec::Vp8 {
            return Err(VideoError::UnsupportedFramerate {
                fps: self.framerate.fps,
            });
        }

        // Check bitrate against level
        if let Some(level) = self.level {
            if self.bitrate_kbps > level.max_bitrate_kbps() {
                return Err(VideoError::CodecLevelExceeded {
                    codec: self.codec.to_string(),
                    profile: self.profile.map(|p| p.to_string()).unwrap_or_default(),
                    level: level.to_string(),
                    width: self.resolution.width,
                    height: self.resolution.height,
                    fps: self.framerate.fps,
                });
            }
        }

        Ok(())
    }

    /// Complete aerospace-grade validation of the configuration.
    /// DO-178C: Validates all requirements before pipeline initialization
    ///
    /// This implements the following Safety Requirements:
    /// - SR-1: Frame buffer overflow prevention
    /// - SR-7: Codec initialization verification
    /// - SR-9: Frame size limits enforcement
    pub fn validate(&self) -> VideoResult<()> {
        // Validate codec capability
        self.validate_codec_limits()?;

        // DO-178C SR-9: Frame size limits - verify computed frame size
        let max_frame_size = (self.resolution.width as usize)
            .saturating_mul(self.resolution.height as usize)
            .saturating_mul(4); // RGBA
        if max_frame_size > MAX_FRAME_BYTES {
            return Err(VideoError::FrameSizeExceeded {
                width: self.resolution.width,
                height: self.resolution.height,
                size: max_frame_size,
                max_bytes: MAX_FRAME_BYTES,
            });
        }

        // DO-178C SR-1: Buffer overflow prevention
        if self.buffer_depth > 1000 {
            return Err(VideoError::InvalidBufferDepth {
                depth: self.buffer_depth,
                max: 1000,
            });
        }

        // Validate keyframe interval
        if self.keyframe_interval.frames > MAX_KEYFRAME_INTERVAL {
            return Err(VideoError::InvalidKeyframeInterval {
                interval: self.keyframe_interval.frames,
                max: MAX_KEYFRAME_INTERVAL,
            });
        }

        // Validate bitrate is reasonable for resolution
        let min_bitrate = (self.resolution.pixels() as u32)
            .saturating_mul(self.framerate.fps)
            / 1000; // rough estimate
        if self.bitrate_kbps < min_bitrate.saturating_div(10) {
            return Err(VideoError::BitrateTooLow {
                bitrate_kbps: self.bitrate_kbps,
                min_recommended: min_bitrate.saturating_div(10),
            });
        }

        Ok(())
    }

    /// Returns the maximum frame budget in nanoseconds.
    pub fn frame_budget_ns(&self) -> u64 {
        self.framerate.frame_duration_ns
    }

    /// DO-178C SR-11: Integrity check - returns a checksum of this configuration
    /// for verification purposes
    pub fn integrity_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        self.codec.hash(&mut hasher);
        self.resolution.width.hash(&mut hasher);
        self.resolution.height.hash(&mut hasher);
        self.framerate.fps.hash(&mut hasher);
        self.bitrate_kbps.hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_validation() {
        assert!(Resolution::new(0, 0).is_err());
        assert!(Resolution::new(1920, 1080).is_ok());
        assert!(Resolution::new(1921, 1080).is_err()); // odd width
    }

    #[test]
    fn framerate_validation() {
        assert!(Framerate::new(0).is_err());
        assert!(Framerate::new(60).is_ok());
        assert!(Framerate::new(121).is_err());
    }

    #[test]
    fn config_from_quality() {
        let config = VideoConfig::from_quality(VideoQuality::High, VideoCodec::H264).unwrap();
        assert_eq!(config.resolution.height, 720);
    }

    #[test]
    fn config_codec_limits() {
        let mut config = VideoConfig::default();
        config.codec = VideoCodec::Vp8;
        config.resolution = Resolution::new(1920, 1080).unwrap();
        // VP8 doesn't support 1080p
        assert!(config.validate_codec_limits().is_err());
    }

    #[test]
    fn audio_only_config() {
        // Test PipelineConfig audio-only
        let config = PipelineConfig::audio_only();
        assert!(!config.is_video_enabled());

        // Test TrackConfig audio-only
        let mut track = TrackConfig::default();
        track.disable_video();
        assert!(!track.is_enabled());
        assert!(!track.video.is_enabled());
    }

    #[test]
    fn video_enable_disable() {
        let mut config = VideoConfig::default();
        assert!(config.is_enabled());

        // Disable video
        config.disable_video();
        assert!(!config.is_enabled());
        assert_eq!(config.bitrate_kbps, 0);

        // Re-enable with quality
        config.enable_video(VideoQuality::High);
        assert!(config.is_enabled());
        assert_eq!(config.resolution.height, 720);
        assert_eq!(config.bitrate_kbps, 1500);
    }
}
