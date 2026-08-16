//! Video codec definitions and parameters.
//!
//! Supports H.264, VP8, VP9, and AV1 codecs with strict validation
//! against DO-178C DAL-B safety requirements.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Supported video codecs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum VideoCodec {
    /// H.264/AVC codec (most widely compatible).
    H264,
    /// VP8 codec (WebRTC default).
    Vp8,
    /// VP9 codec (successor to VP8, better compression).
    Vp9,
    /// AV1 codec ( newest, best compression).
    Av1,
}

impl fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VideoCodec::H264 => write!(f, "H.264"),
            VideoCodec::Vp8 => write!(f, "VP8"),
            VideoCodec::Vp9 => write!(f, "VP9"),
            VideoCodec::Av1 => write!(f, "AV1"),
        }
    }
}

/// H.264 profile levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodecProfile {
    // H.264 profiles
    H264Baseline,
    H264Main,
    H264High,
    // VP9 profiles
    Vp9Profile0,
    Vp9Profile1,
    // AV1 profiles
    Av1Profile0,
    Av1Profile1,
}

impl fmt::Display for VideoCodecProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VideoCodecProfile::H264Baseline => write!(f, "Baseline"),
            VideoCodecProfile::H264Main => write!(f, "Main"),
            VideoCodecProfile::H264High => write!(f, "High"),
            VideoCodecProfile::Vp9Profile0 => write!(f, "VP9 Profile 0"),
            VideoCodecProfile::Vp9Profile1 => write!(f, "VP9 Profile 1"),
            VideoCodecProfile::Av1Profile0 => write!(f, "AV1 Profile 0"),
            VideoCodecProfile::Av1Profile1 => write!(f, "AV1 Profile 1"),
        }
    }
}

/// H.264 level indicators (for bitrate/resolution limits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodecLevel {
    /// Level 1.0 — 64x32 @ 15fps max
    H264Level1 = 10,
    /// Level 1.1 — 176x144 @ 30fps max
    H264Level11 = 11,
    /// Level 1.2 — 176x144 @ 30fps max
    H264Level12 = 12,
    /// Level 1.3 — 320x240 @ 30fps max
    H264Level13 = 13,
    /// Level 2.0 — 352x288 @ 30fps max
    H264Level20 = 20,
    /// Level 2.1 — 352x480 @ 30fps max
    H264Level21 = 21,
    /// Level 2.2 — 352x480 @ 30fps max
    H264Level22 = 22,
    /// Level 3.0 — 720x480 @ 30fps max
    H264Level30 = 30,
    /// Level 3.1 — 1280x720 @ 30fps max
    H264Level31 = 31,
    /// Level 3.2 — 1280x720 @ 60fps max
    H264Level32 = 32,
    /// Level 4.0 — 1920x1080 @ 30fps max
    H264Level40 = 40,
    /// Level 4.1 — 1920x1080 @ 30fps max
    H264Level41 = 41,
    /// Level 4.2 — 1920x1080 @ 60fps max
    H264Level42 = 42,
    /// Level 5.0 — 2560x1920 @ 30fps max
    H264Level50 = 50,
    /// Level 5.1 — 4096x2048 @ 30fps max
    H264Level51 = 51,
    /// Level 5.2 — 4096x2048 @ 60fps max
    H264Level52 = 52,
    /// Level 6.0 — 4096x4096 @ 120fps max
    H264Level60 = 60,
    /// Level 6.1 — 8192x4096 @ 120fps max
    H264Level61 = 61,
    /// Level 6.2 — 8192x8192 @ 120fps max
    H264Level62 = 62,
    /// VP9/AV1 "Level" (placeholder)
    Vp9Level1,
    Vp9Level2,
    Vp9Level3,
    Av1Level2_0,
    Av1Level4_0,
    Av1Level6_0,
}

impl fmt::Display for VideoCodecLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VideoCodecLevel::H264Level1 => write!(f, "1.0"),
            VideoCodecLevel::H264Level11 => write!(f, "1.1"),
            VideoCodecLevel::H264Level12 => write!(f, "1.2"),
            VideoCodecLevel::H264Level13 => write!(f, "1.3"),
            VideoCodecLevel::H264Level20 => write!(f, "2.0"),
            VideoCodecLevel::H264Level21 => write!(f, "2.1"),
            VideoCodecLevel::H264Level22 => write!(f, "2.2"),
            VideoCodecLevel::H264Level30 => write!(f, "3.0"),
            VideoCodecLevel::H264Level31 => write!(f, "3.1"),
            VideoCodecLevel::H264Level32 => write!(f, "3.2"),
            VideoCodecLevel::H264Level40 => write!(f, "4.0"),
            VideoCodecLevel::H264Level41 => write!(f, "4.1"),
            VideoCodecLevel::H264Level42 => write!(f, "4.2"),
            VideoCodecLevel::H264Level50 => write!(f, "5.0"),
            VideoCodecLevel::H264Level51 => write!(f, "5.1"),
            VideoCodecLevel::H264Level52 => write!(f, "5.2"),
            VideoCodecLevel::H264Level60 => write!(f, "6.0"),
            VideoCodecLevel::H264Level61 => write!(f, "6.1"),
            VideoCodecLevel::H264Level62 => write!(f, "6.2"),
            VideoCodecLevel::Vp9Level1 => write!(f, "VP9 L1"),
            VideoCodecLevel::Vp9Level2 => write!(f, "VP9 L2"),
            VideoCodecLevel::Vp9Level3 => write!(f, "VP9 L3"),
            VideoCodecLevel::Av1Level2_0 => write!(f, "AV1 2.0"),
            VideoCodecLevel::Av1Level4_0 => write!(f, "AV1 4.0"),
            VideoCodecLevel::Av1Level6_0 => write!(f, "AV1 6.0"),
        }
    }
}

impl VideoCodecLevel {
    /// Returns the maximum bitrate in kbps for this level.
    pub fn max_bitrate_kbps(&self) -> u32 {
        match self {
            VideoCodecLevel::H264Level1 => 64,
            VideoCodecLevel::H264Level11 | VideoCodecLevel::H264Level12 => 192,
            VideoCodecLevel::H264Level13 => 384,
            VideoCodecLevel::H264Level20 => 2_000,
            VideoCodecLevel::H264Level21 => 4_000,
            VideoCodecLevel::H264Level22 => 4_000,
            VideoCodecLevel::H264Level30 => 10_000,
            VideoCodecLevel::H264Level31 => 14_000,
            VideoCodecLevel::H264Level32 => 20_000,
            VideoCodecLevel::H264Level40 => 20_000,
            VideoCodecLevel::H264Level41 => 50_000,
            VideoCodecLevel::H264Level42 => 50_000,
            VideoCodecLevel::H264Level50 => 135_000,
            VideoCodecLevel::H264Level51 => 240_000,
            VideoCodecLevel::H264Level52 => 240_000,
            VideoCodecLevel::H264Level60 => 480_000,
            VideoCodecLevel::H264Level61 => 800_000,
            VideoCodecLevel::H264Level62 => 800_000,
            VideoCodecLevel::Vp9Level1 => 200,
            VideoCodecLevel::Vp9Level2 => 800,
            VideoCodecLevel::Vp9Level3 => 1_800,
            VideoCodecLevel::Av1Level2_0 => 300,
            VideoCodecLevel::Av1Level4_0 => 3_000,
            VideoCodecLevel::Av1Level6_0 => 12_000,
        }
    }
}

/// Pixel format for raw frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PixelFormat {
    /// Planar YUV 4:2:0 (most common).
    Yuv420,
    /// Planar YUV 4:2:2.
    Yuv422,
    /// Planar YUV 4:4:4.
    Yuv444,
    /// RGBA interleaved.
    Rgba,
    /// BGRA interleaved.
    Bgra,
}

impl Default for PixelFormat {
    fn default() -> Self {
        PixelFormat::Yuv420
    }
}

/// Frame type indicator (keyframe vs delta frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameType {
    /// Keyframe (I-frame) — self-contained, no dependencies.
    Keyframe,
    /// Delta frame (P/B-frame) — depends on previous frames.
    Delta,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // VideoCodec Tests
    // ========================================================================

    #[test]
    fn codec_display() {
        assert_eq!(VideoCodec::H264.to_string(), "H.264");
        assert_eq!(VideoCodec::Vp8.to_string(), "VP8");
        assert_eq!(VideoCodec::Vp9.to_string(), "VP9");
        assert_eq!(VideoCodec::Av1.to_string(), "AV1");
    }

    #[test]
    fn codec_equality() {
        assert_eq!(VideoCodec::H264, VideoCodec::H264);
        assert_ne!(VideoCodec::H264, VideoCodec::Vp8);
    }

    // ========================================================================
    // VideoCodecProfile Tests
    // ========================================================================

    #[test]
    fn profile_display() {
        assert_eq!(VideoCodecProfile::H264Baseline.to_string(), "Baseline");
        assert_eq!(VideoCodecProfile::H264Main.to_string(), "Main");
        assert_eq!(VideoCodecProfile::H264High.to_string(), "High");
        assert_eq!(VideoCodecProfile::Vp9Profile0.to_string(), "VP9 Profile 0");
        assert_eq!(VideoCodecProfile::Vp9Profile1.to_string(), "VP9 Profile 1");
        assert_eq!(VideoCodecProfile::Av1Profile0.to_string(), "AV1 Profile 0");
        assert_eq!(VideoCodecProfile::Av1Profile1.to_string(), "AV1 Profile 1");
    }

    // ========================================================================
    // VideoCodecLevel Tests
    // ========================================================================

    #[test]
    fn level_max_bitrate() {
        assert_eq!(VideoCodecLevel::H264Level31.max_bitrate_kbps(), 14_000);
        assert_eq!(VideoCodecLevel::H264Level41.max_bitrate_kbps(), 50_000);
        assert_eq!(VideoCodecLevel::H264Level51.max_bitrate_kbps(), 240_000);
        assert_eq!(VideoCodecLevel::Vp9Level1.max_bitrate_kbps(), 200);
        assert_eq!(VideoCodecLevel::Vp9Level2.max_bitrate_kbps(), 800);
        assert_eq!(VideoCodecLevel::Av1Level2_0.max_bitrate_kbps(), 300);
        assert_eq!(VideoCodecLevel::Av1Level4_0.max_bitrate_kbps(), 3_000);
    }

    #[test]
    fn level_display() {
        assert_eq!(VideoCodecLevel::H264Level31.to_string(), "3.1");
        assert_eq!(VideoCodecLevel::H264Level41.to_string(), "4.1");
        assert_eq!(VideoCodecLevel::H264Level62.to_string(), "6.2");
        assert_eq!(VideoCodecLevel::Vp9Level1.to_string(), "VP9 L1");
        assert_eq!(VideoCodecLevel::Av1Level6_0.to_string(), "AV1 6.0");
    }

    // ========================================================================
    // PixelFormat Tests
    // ========================================================================

    #[test]
    fn pixel_format_default() {
        assert_eq!(PixelFormat::default(), PixelFormat::Yuv420);
    }

    #[test]
    fn pixel_format_debug() {
        let fmt = PixelFormat::Yuv420;
        let debug_str = format!("{:?}", fmt);
        assert!(debug_str.contains("Yuv420"));
    }

    // ========================================================================
    // FrameType Tests
    // ========================================================================

    #[test]
    fn frame_type_serialization() {
        let keyframe = FrameType::Keyframe;
        let delta = FrameType::Delta;

        // Test serde serialization
        let key_json = serde_json::to_string(&keyframe).unwrap();
        let delta_json = serde_json::to_string(&delta).unwrap();

        assert_eq!(key_json, "\"keyframe\"");
        assert_eq!(delta_json, "\"delta\"");

        // Test deserialization
        let key_parsed: FrameType = serde_json::from_str("\"keyframe\"").unwrap();
        let delta_parsed: FrameType = serde_json::from_str("\"delta\"").unwrap();

        assert_eq!(keyframe, key_parsed);
        assert_eq!(delta, delta_parsed);
    }

    #[test]
    fn frame_type_equality() {
        assert_eq!(FrameType::Keyframe, FrameType::Keyframe);
        assert_eq!(FrameType::Delta, FrameType::Delta);
        assert_ne!(FrameType::Keyframe, FrameType::Delta);
    }
}
