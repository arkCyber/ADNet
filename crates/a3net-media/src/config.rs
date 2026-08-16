//! Media configuration — every limit is a typed constant so the
//! safety case can point at it by name.

use crate::error::{MediaError, MediaResult};
use serde::{Deserialize, Serialize};

/// Maximum raw payload size accepted by the ingester. 1 GiB.
/// Mirrors the blobstore's "single-block" cap.
pub const MAX_MEDIA_BYTES: u64 = 1u64 << 30;

/// Maximum segment duration. 6 s. Long enough for short-video
/// keyframes, short enough to keep retransmit cost bounded.
pub const MAX_SEGMENT_DURATION_MS: u64 = 6_000;

/// Minimum segment duration. 500 ms. Below this the manifest
/// overhead dominates the per-segment download.
pub const MIN_SEGMENT_DURATION_MS: u64 = 500;

/// Maximum variant ladder height. 4 (e.g. 240p / 480p / 720p / 1080p).
pub const MAX_VARIANT_COUNT: usize = 4;

/// Maximum frame rate (FPS). 120 (slow-motion ceiling).
pub const MAX_FRAME_RATE: u32 = 120;

/// Minimum frame rate. 1 (still images encoded as video).
pub const MIN_FRAME_RATE: u32 = 1;

/// Maximum number of segments per variant. ~ 16 hours at MAX.
pub const MAX_SEGMENTS_PER_VARIANT: usize = 9_600;

/// Maximum audio sample rate (Hz). 48 kHz.
pub const MAX_AUDIO_SAMPLE_RATE: u32 = 48_000;

/// Maximum audio channels. 8 (7.1 surround).
pub const MAX_AUDIO_CHANNELS: u8 = 8;

/// AV-drift tolerance in ms. A/V that drifts more than this is
/// rejected at verify time (SR-7).
pub const AV_DRIFT_TOLERANCE_MS: i64 = 50;

/// Clock-skew tolerance in ms. Computed vs declared media creation
/// time.
pub const CLOCK_SKEW_TOLERANCE_MS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantSpec {
    pub label: String,
    pub width: u32,
    pub height: u32,
    pub bitrate_kbps: u32,
}

impl VariantSpec {
    pub fn validate(&self) -> MediaResult<()> {
        if self.label.is_empty() || self.label.len() > 16 {
            return Err(MediaError::InvalidConfig(format!(
                "variant label '{}' must be 1..=16 chars",
                self.label
            )));
        }
        if self.width == 0 || self.height == 0 {
            return Err(MediaError::InvalidConfig(format!(
                "variant '{}' has zero dimension {}x{}",
                self.label, self.width, self.height
            )));
        }
        if self.width > 7680 || self.height > 4320 {
            return Err(MediaError::InvalidConfig(format!(
                "variant '{}' exceeds 8K ({}x{})",
                self.label, self.width, self.height
            )));
        }
        if self.bitrate_kbps == 0 || self.bitrate_kbps > 200_000 {
            return Err(MediaError::InvalidConfig(format!(
                "variant '{}' bitrate {} kbps out of range",
                self.label, self.bitrate_kbps
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantLadder(pub Vec<VariantSpec>);

impl VariantLadder {
    pub fn new(specs: Vec<VariantSpec>) -> MediaResult<Self> {
        if specs.is_empty() {
            return Err(MediaError::InvalidConfig(
                "variant ladder must contain at least one entry".into(),
            ));
        }
        if specs.len() > MAX_VARIANT_COUNT {
            return Err(MediaError::InvalidConfig(format!(
                "variant ladder has {} entries, max {}",
                specs.len(),
                MAX_VARIANT_COUNT
            )));
        }
        for s in &specs {
            s.validate()?;
        }
        // Detect duplicate labels.
        let mut seen = std::collections::BTreeSet::new();
        for s in &specs {
            if !seen.insert(s.label.clone()) {
                return Err(MediaError::InvalidConfig(format!(
                    "duplicate variant label '{}'",
                    s.label
                )));
            }
        }
        Ok(Self(specs))
    }

    /// Default H.264/AVC ladder used for short-video.
    pub fn default_short_video() -> Self {
        Self(vec![
            VariantSpec { label: "240p".into(), width: 426, height: 240, bitrate_kbps: 400 },
            VariantSpec { label: "480p".into(), width: 854, height: 480, bitrate_kbps: 1_000 },
            VariantSpec { label: "720p".into(), width: 1280, height: 720, bitrate_kbps: 2_500 },
            VariantSpec { label: "1080p".into(), width: 1920, height: 1080, bitrate_kbps: 5_000 },
        ])
    }

    pub fn iter(&self) -> impl Iterator<Item = &VariantSpec> {
        self.0.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmenterConfig {
    pub target_duration_ms: u64,
}

impl SegmenterConfig {
    pub fn validate(&self) -> MediaResult<()> {
        if self.target_duration_ms < MIN_SEGMENT_DURATION_MS
            || self.target_duration_ms > MAX_SEGMENT_DURATION_MS
        {
            return Err(MediaError::InvalidConfig(format!(
                "segment duration {} ms out of [{}, {}]",
                self.target_duration_ms, MIN_SEGMENT_DURATION_MS, MAX_SEGMENT_DURATION_MS
            )));
        }
        Ok(())
    }

    pub fn default_short_video() -> Self {
        Self { target_duration_ms: 2_000 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaConfig {
    pub ladder: VariantLadder,
    pub segmenter: SegmenterConfig,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub clock_skew_tolerance_ms: i64,
    pub av_drift_tolerance_ms: i64,
}

impl MediaConfig {
    pub fn validate(&self) -> MediaResult<()> {
        self.ladder.0.iter().try_for_each(|v| v.validate())?;
        self.segmenter.validate()?;
        if self.audio_sample_rate < 8_000 || self.audio_sample_rate > MAX_AUDIO_SAMPLE_RATE {
            return Err(MediaError::InvalidConfig(format!(
                "audio sample rate {} Hz out of range",
                self.audio_sample_rate
            )));
        }
        if self.audio_channels == 0 || self.audio_channels > MAX_AUDIO_CHANNELS {
            return Err(MediaError::InvalidConfig(format!(
                "audio channels {} out of range",
                self.audio_channels
            )));
        }
        Ok(())
    }

    pub fn default_short_video() -> Self {
        Self {
            ladder: VariantLadder::default_short_video(),
            segmenter: SegmenterConfig::default_short_video(),
            audio_sample_rate: 48_000,
            audio_channels: 2,
            clock_skew_tolerance_ms: CLOCK_SKEW_TOLERANCE_MS,
            av_drift_tolerance_ms: AV_DRIFT_TOLERANCE_MS,
        }
    }
}

#[derive(Debug, Error)]
#[error("{0}")]
pub struct MediaConfigError(pub MediaError);

impl From<MediaError> for MediaConfigError {
    fn from(e: MediaError) -> Self {
        MediaConfigError(e)
    }
}

use thiserror::Error;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::MediaError;

    // ---- constants --------------------------------------------------------

    #[test]
    fn constants_have_expected_values() {
        // Pin the safety-case constants so a stray edit is caught at CI time.
        assert_eq!(MAX_MEDIA_BYTES, 1u64 << 30);
        assert_eq!(MAX_SEGMENT_DURATION_MS, 6_000);
        assert_eq!(MIN_SEGMENT_DURATION_MS, 500);
        assert_eq!(MAX_VARIANT_COUNT, 4);
        assert_eq!(MAX_FRAME_RATE, 120);
        assert_eq!(MIN_FRAME_RATE, 1);
        assert_eq!(MAX_SEGMENTS_PER_VARIANT, 9_600);
        assert_eq!(MAX_AUDIO_SAMPLE_RATE, 48_000);
        assert_eq!(MAX_AUDIO_CHANNELS, 8);
        assert_eq!(AV_DRIFT_TOLERANCE_MS, 50);
        assert_eq!(CLOCK_SKEW_TOLERANCE_MS, 24 * 60 * 60 * 1_000);
    }

    // ---- VariantSpec::validate -------------------------------------------

    #[test]
    fn variant_spec_validate_ok() {
        let v = VariantSpec {
            label: "720p".into(),
            width: 1280,
            height: 720,
            bitrate_kbps: 2_500,
        };
        assert!(v.validate().is_ok());
    }

    #[test]
    fn variant_spec_validate_empty_label() {
        let v = VariantSpec {
            label: "".into(),
            width: 1280,
            height: 720,
            bitrate_kbps: 2_500,
        };
        let err = v.validate().unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn variant_spec_validate_label_too_long() {
        let v = VariantSpec {
            label: "a".repeat(17),
            width: 1280,
            height: 720,
            bitrate_kbps: 2_500,
        };
        let err = v.validate().unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn variant_spec_validate_zero_dimension() {
        let v = VariantSpec {
            label: "v".into(),
            width: 0,
            height: 720,
            bitrate_kbps: 2_500,
        };
        assert!(v.validate().is_err());
        let v = VariantSpec {
            label: "v".into(),
            width: 1280,
            height: 0,
            bitrate_kbps: 2_500,
        };
        assert!(v.validate().is_err());
    }

    #[test]
    fn variant_spec_validate_above_8k() {
        let v = VariantSpec {
            label: "8k+".into(),
            width: 7690,
            height: 4320,
            bitrate_kbps: 2_500,
        };
        assert!(v.validate().is_err());
        let v = VariantSpec {
            label: "8k+".into(),
            width: 7680,
            height: 4330,
            bitrate_kbps: 2_500,
        };
        assert!(v.validate().is_err());
    }

    #[test]
    fn variant_spec_validate_max_dimensions_ok() {
        // 7680x4320 = 8K, exactly at the limit.
        let v = VariantSpec {
            label: "8k".into(),
            width: 7680,
            height: 4320,
            bitrate_kbps: 200_000,
        };
        assert!(v.validate().is_ok());
    }

    #[test]
    fn variant_spec_validate_zero_bitrate() {
        let v = VariantSpec {
            label: "v".into(),
            width: 1280,
            height: 720,
            bitrate_kbps: 0,
        };
        assert!(v.validate().is_err());
    }

    #[test]
    fn variant_spec_validate_oversized_bitrate() {
        let v = VariantSpec {
            label: "v".into(),
            width: 1280,
            height: 720,
            bitrate_kbps: 200_001,
        };
        assert!(v.validate().is_err());
    }

    // ---- VariantLadder::new ----------------------------------------------

    #[test]
    fn variant_ladder_new_ok() {
        let ladder = VariantLadder::new(vec![VariantSpec {
            label: "720p".into(),
            width: 1280,
            height: 720,
            bitrate_kbps: 2_500,
        }])
        .unwrap();
        assert_eq!(ladder.0.len(), 1);
    }

    #[test]
    fn variant_ladder_new_empty_rejected() {
        let err = VariantLadder::new(vec![]).unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn variant_ladder_new_too_many_rejected() {
        let specs: Vec<VariantSpec> = (0..=MAX_VARIANT_COUNT)
            .map(|i| VariantSpec {
                label: format!("v{i}"),
                width: 640,
                height: 360,
                bitrate_kbps: 1_000,
            })
            .collect();
        let err = VariantLadder::new(specs).unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn variant_ladder_new_duplicate_label_rejected() {
        let specs = vec![
            VariantSpec {
                label: "dup".into(),
                width: 1280,
                height: 720,
                bitrate_kbps: 2_500,
            },
            VariantSpec {
                label: "dup".into(),
                width: 1920,
                height: 1080,
                bitrate_kbps: 5_000,
            },
        ];
        let err = VariantLadder::new(specs).unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn variant_ladder_new_propagates_invalid_spec() {
        // 17-char label inside the ladder.
        let specs = vec![VariantSpec {
            label: "a".repeat(17),
            width: 1280,
            height: 720,
            bitrate_kbps: 2_500,
        }];
        let err = VariantLadder::new(specs).unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    // ---- VariantLadder::default_short_video ------------------------------

    #[test]
    fn variant_ladder_default_short_video_is_valid() {
        let ladder = VariantLadder::default_short_video();
        assert!(!ladder.0.is_empty());
        // Every entry should round-trip through `new`.
        let rebuilt = VariantLadder::new(ladder.0.clone()).unwrap();
        assert_eq!(rebuilt.0.len(), ladder.0.len());
    }

    // ---- VariantLadder::iter ---------------------------------------------

    #[test]
    fn variant_ladder_iter_visits_every_entry() {
        let ladder = VariantLadder::default_short_video();
        let n = ladder.iter().count();
        assert_eq!(n, ladder.0.len());
    }

    // ---- SegmenterConfig::validate / default_short_video -----------------

    #[test]
    fn segmenter_config_validate_min_max() {
        assert!(SegmenterConfig {
            target_duration_ms: MIN_SEGMENT_DURATION_MS,
        }
        .validate()
        .is_ok());
        assert!(SegmenterConfig {
            target_duration_ms: MAX_SEGMENT_DURATION_MS,
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn segmenter_config_validate_below_min() {
        let cfg = SegmenterConfig {
            target_duration_ms: MIN_SEGMENT_DURATION_MS - 1,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn segmenter_config_validate_above_max() {
        let cfg = SegmenterConfig {
            target_duration_ms: MAX_SEGMENT_DURATION_MS + 1,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn segmenter_config_default_short_video_is_valid() {
        let cfg = SegmenterConfig::default_short_video();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.target_duration_ms, 2_000);
    }

    // ---- MediaConfig::validate / default_short_video ---------------------

    #[test]
    fn media_config_default_short_video_is_valid() {
        let cfg = MediaConfig::default_short_video();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn media_config_validate_propagates_segmenter_error() {
        let mut cfg = MediaConfig::default_short_video();
        cfg.segmenter.target_duration_ms = 0; // below MIN
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn media_config_validate_low_audio_sample_rate() {
        let mut cfg = MediaConfig::default_short_video();
        cfg.audio_sample_rate = 4_000;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn media_config_validate_high_audio_sample_rate() {
        let mut cfg = MediaConfig::default_short_video();
        cfg.audio_sample_rate = MAX_AUDIO_SAMPLE_RATE + 1;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn media_config_validate_zero_audio_channels() {
        let mut cfg = MediaConfig::default_short_video();
        cfg.audio_channels = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn media_config_validate_too_many_audio_channels() {
        let mut cfg = MediaConfig::default_short_video();
        cfg.audio_channels = MAX_AUDIO_CHANNELS + 1;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn media_config_validate_max_audio_channels_ok() {
        let mut cfg = MediaConfig::default_short_video();
        cfg.audio_channels = MAX_AUDIO_CHANNELS;
        cfg.audio_sample_rate = MAX_AUDIO_SAMPLE_RATE;
        assert!(cfg.validate().is_ok());
    }

    // ---- MediaConfigError -------------------------------------------------

    #[test]
    fn media_config_error_from_media_error() {
        let original = MediaError::InvalidConfig("test".into());
        let wrapped: MediaConfigError = original.into();
        // Display should surface the underlying error message.
        assert!(wrapped.to_string().contains("test"));
    }

    #[test]
    fn media_config_error_display_includes_message() {
        let err = MediaConfigError(MediaError::TruncatedFrame {
            expected: 10,
            actual: 5,
        });
        let s = err.to_string();
        assert!(s.contains("truncated") || s.contains("expected"));
    }

    // ---- serde round-trip -------------------------------------------------

    #[test]
    fn media_config_serde_round_trip() {
        let cfg = MediaConfig::default_short_video();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: MediaConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn variant_ladder_serde_round_trip() {
        let ladder = VariantLadder::default_short_video();
        let json = serde_json::to_string(&ladder).unwrap();
        let back: VariantLadder = serde_json::from_str(&json).unwrap();
        assert_eq!(ladder.0.len(), back.0.len());
        assert_eq!(ladder.0[0].label, back.0[0].label);
    }
}
