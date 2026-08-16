//! Media codecs — exhaustive enums, no string ambiguity.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VideoCodec {
    /// H.264 / AVC. The mobile default.
    H264,
    /// H.265 / HEVC. Higher compression, royalty encumbered.
    H265,
    /// AV1. Royalty-free, modern.
    Av1,
    /// VP9. Royalty-free, YouTube-friendly.
    Vp9,
}

impl VideoCodec {
    pub fn as_str(self) -> &'static str {
        match self {
            VideoCodec::H264 => "h264",
            VideoCodec::H265 => "h265",
            VideoCodec::Av1 => "av1",
            VideoCodec::Vp9 => "vp9",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "h264" | "avc" | "avc1" => Some(VideoCodec::H264),
            "h265" | "hevc" | "hev1" => Some(VideoCodec::H265),
            "av1" | "av01" => Some(VideoCodec::Av1),
            "vp9" | "vp09" => Some(VideoCodec::Vp9),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioCodec {
    /// AAC LC. Mobile default.
    Aac,
    /// Opus. Modern, low-latency.
    Opus,
    /// MP3. Legacy fallback.
    Mp3,
    /// FLAC. Lossless.
    Flac,
}

impl AudioCodec {
    pub fn as_str(self) -> &'static str {
        match self {
            AudioCodec::Aac => "aac",
            AudioCodec::Opus => "opus",
            AudioCodec::Mp3 => "mp3",
            AudioCodec::Flac => "flac",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "aac" | "mp4a" => Some(AudioCodec::Aac),
            "opus" => Some(AudioCodec::Opus),
            "mp3" => Some(AudioCodec::Mp3),
            "flac" => Some(AudioCodec::Flac),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaKind {
    Video,
    Audio,
    Image,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SampleFormat {
    /// Signed 16-bit little-endian PCM.
    S16,
    /// Signed 24-bit little-endian PCM.
    S24,
    /// 32-bit floating-point PCM.
    F32,
}

impl SampleFormat {
    pub fn bytes_per_sample(self) -> u8 {
        match self {
            SampleFormat::S16 => 2,
            SampleFormat::S24 => 3,
            SampleFormat::F32 => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_codec_round_trip() {
        for c in [VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1, VideoCodec::Vp9] {
            assert_eq!(VideoCodec::from_str(c.as_str()), Some(c));
        }
    }

    #[test]
    fn audio_codec_round_trip() {
        for c in [AudioCodec::Aac, AudioCodec::Opus, AudioCodec::Mp3, AudioCodec::Flac] {
            assert_eq!(AudioCodec::from_str(c.as_str()), Some(c));
        }
    }

    #[test]
    fn video_codec_unknown_is_none() {
        assert_eq!(VideoCodec::from_str("mpeg2"), None);
    }

    #[test]
    fn audio_codec_unknown_is_none() {
        assert_eq!(AudioCodec::from_str("wma"), None);
    }

    #[test]
    fn sample_format_bytes_per_sample() {
        assert_eq!(SampleFormat::S16.bytes_per_sample(), 2);
        assert_eq!(SampleFormat::S24.bytes_per_sample(), 3);
        assert_eq!(SampleFormat::F32.bytes_per_sample(), 4);
    }
}
