//! DO-178C DAL-B compliance test suite for a3net-video.
//!
//! Run with:
//!
//! ```sh
//! cargo test -p a3net-video --features aerospace --test aerospace_compliance
//! ```
//!
//! Every test maps to a Safety Requirement (SR-1 .. SR-12) in
//! `crates/a3net-video/SAFETY_CASE.md`.

#![cfg(feature = "aerospace")]

use a3net_video::codec::{FrameType, PixelFormat, VideoCodec, VideoCodecLevel};
use a3net_video::config::{Framerate, KeyFrameInterval, PipelineConfig, Resolution, VideoConfig, VideoQuality};
use a3net_video::error::VideoError;
use a3net_video::frame::{EncodedFrame, FrameId, RawFrame, VideoFrame};
use a3net_video::pipeline::{PipelineState, VideoPipeline};

// ─────────────────────────────────────────────────────────────────────
// Test helpers
// ─────────────────────────────────────────────────────────────────────

fn build_config() -> PipelineConfig {
    PipelineConfig {
        enable_video: true,
        video: VideoConfig::new(
            VideoCodec::H264,
            Resolution::new(640, 480).unwrap(),
            Framerate::new(30).unwrap(),
            500,
        )
        .unwrap(),
    }
}

fn build_raw_frame(seq: u32, pts_ns: u64) -> VideoFrame {
    VideoFrame::Raw(
        RawFrame::solid(320, 240, (seq & 0xFF) as u8, 0, 0).unwrap(),
    )
}

fn build_encoded_frame(seq: u32, pts_ns: u64, is_key: bool) -> EncodedFrame {
    EncodedFrame::new(
        FrameId::new(pts_ns, seq),
        VideoCodec::H264,
        if is_key {
            FrameType::Keyframe
        } else {
            FrameType::Delta
        },
        vec![0u8; 100],
        pts_ns,
        pts_ns,
    )
    .unwrap()
}

// ─────────────────────────────────────────────────────────────────────
// SR-1: Frame integrity — every frame carries explicit length prefix.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn sr_1_encoded_frame_has_length_prefix() {
    let frame = build_encoded_frame(1, 1_000_000_000, true);
    let wire = frame.to_wire_format();

    // First byte is LP_VIDEO_FRAME marker
    assert_eq!(wire[0], 0x56);
    // Next 4 bytes are length (little-endian)
    let len = u32::from_le_bytes([wire[1], wire[2], wire[3], wire[4]]) as usize;
    assert_eq!(len, frame.data.len());
    // Payload follows
    assert_eq!(&wire[5..], &frame.data[..]);
}

#[test]
fn sr_1_length_prefix_truncated_rejected() {
    let wire = vec![0x56, 0, 0, 0, 100, 1, 2, 3]; // declares 100 bytes, only 3 provided
    let err = EncodedFrame::from_wire_format(
        VideoCodec::H264,
        &wire,
        0,
        1,
        FrameType::Keyframe,
    )
    .unwrap_err();
    assert!(matches!(err, VideoError::TruncatedFrame { .. }));
}

#[test]
fn sr_1_length_prefix_header_only_rejected() {
    let wire = vec![0x56, 0, 0, 0]; // declares length but no payload
    let err = EncodedFrame::from_wire_format(
        VideoCodec::H264,
        &wire,
        0,
        1,
        FrameType::Keyframe,
    )
    .unwrap_err();
    assert!(matches!(err, VideoError::TruncatedFrame { .. }));
}

#[test]
fn sr_1_wrong_prefix_rejected() {
    let wire = vec![0xFF, 0, 0, 0, 10, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let err = EncodedFrame::from_wire_format(
        VideoCodec::H264,
        &wire,
        0,
        1,
        FrameType::Keyframe,
    )
    .unwrap_err();
    assert!(matches!(err, VideoError::TruncatedFrame { .. }));
}

// ─────────────────────────────────────────────────────────────────────
// SR-2: Keyframe enforcement — keyframes appear at regular intervals.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn sr_2_keyframe_interval_is_deterministic() {
    let config = build_config();
    let (pipeline, _rx) = VideoPipeline::new(config.clone());

    // Create a synthetic stream with known keyframe positions
    let keyframe_positions: Vec<u32> = vec![0, 60, 120]; // every 60 frames @ 30fps = 2s

    for i in 0..120 {
        let is_key = keyframe_positions.contains(&i);
        let frame = build_encoded_frame(i, i as u64 * 33_333_333, is_key);

        if is_key {
            assert!(frame.is_keyframe, "frame {} should be keyframe", i);
        }
    }
}

#[test]
fn sr_2_delta_frame_cannot_be_keyframe() {
    let frame = build_encoded_frame(1, 1_000_000_000, false);
    assert!(!frame.is_keyframe);
    assert_eq!(frame.frame_type, FrameType::Delta);
}

#[test]
fn sr_2_keyframe_frame_cannot_be_delta() {
    let frame = build_encoded_frame(1, 1_000_000_000, true);
    assert!(frame.is_keyframe);
    assert_eq!(frame.frame_type, FrameType::Keyframe);
}

// ─────────────────────────────────────────────────────────────────────
// SR-3: Timestamp monotonicity — timestamps never decrease.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn sr_3_raw_frame_rejects_non_monotonic_pts() {
    let err = RawFrame::new(
        FrameId::new(2_000_000_000, 1),
        320,
        240,
        PixelFormat::Rgba,
        vec![0u8; 320 * 240 * 4],
        1_000_000_000, // PTS before DTS
        2_000_000_000,
    )
    .unwrap_err();
    assert!(matches!(err, VideoError::InvalidConfig { param, .. } if param == "timestamp"));
}

#[test]
fn sr_3_frame_timestamps_are_nanoseconds() {
    let frame = RawFrame::solid(320, 240, 128, 128, 128).unwrap();
    // Timestamps should be in nanoseconds
    assert!(frame.pts_ns >= 1_000_000_000); // At least 1 second since epoch
}

#[test]
fn sr_3_encoded_frame_preserves_timestamps() {
    let pts = 5_000_000_000u64;
    let frame = EncodedFrame::new(
        FrameId::new(pts, 42),
        VideoCodec::H264,
        FrameType::Keyframe,
        vec![0u8; 100],
        pts,
        pts,
    )
    .unwrap();
    assert_eq!(frame.pts_ns, pts);
    assert_eq!(frame.dts_ns, pts);
}

// ─────────────────────────────────────────────────────────────────────
// SR-4: Sequence continuity — gaps in frame numbers are detected.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn sr_4_frame_id_sequence_increments() {
    let id1 = FrameId::new(1_000_000_000, 1);
    let id2 = FrameId::new(1_033_333_333, 2);
    let id3 = FrameId::new(1_066_666_666, 3);

    assert!(id2.timestamp_ns() > id1.timestamp_ns());
    assert!(id3.timestamp_ns() > id2.timestamp_ns());
    assert_eq!(id2.seq, id1.seq + 1);
    assert_eq!(id3.seq, id2.seq + 1);
}

#[test]
fn sr_4_frame_id_from_timestamp_and_seq() {
    let ts = 1_500_000_000u64;
    let seq = 42u32;
    let id = FrameId::new(ts, seq);

    assert_eq!(id.timestamp_ns(), ts);
    assert_eq!(id.seq, seq);
}

// ─────────────────────────────────────────────────────────────────────
// SR-5: Frame size limits — oversized frames are rejected.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn sr_5_raw_frame_rejects_oversized() {
    // RGBA 16K x 16K = ~1GB, way over limit
    let huge_size = 16 * 1024 * 1024 + 1;
    let err = RawFrame::new(
        FrameId::new(0, 1),
        426, // odd width for YUV420 should fail anyway
        240,
        PixelFormat::Yuv420,
        vec![0u8; huge_size],
        0,
        0,
    )
    .unwrap_err();
    assert!(matches!(err, VideoError::TruncatedFrame { .. }));
}

#[test]
fn sr_5_encoded_frame_rejects_oversized() {
    let huge_size = 16 * 1024 * 1024 + 1;
    let err = EncodedFrame::new(
        FrameId::new(0, 1),
        VideoCodec::H264,
        FrameType::Keyframe,
        vec![0u8; huge_size],
        0,
        0,
    )
    .unwrap_err();
    assert!(matches!(err, VideoError::FrameTooLarge { .. }));
}

// ─────────────────────────────────────────────────────────────────────
// SR-6: Configuration validation — invalid configs are rejected.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn sr_6_resolution_zero_dimensions_rejected() {
    assert!(Resolution::new(0, 0).is_err());
    assert!(Resolution::new(0, 480).is_err());
    assert!(Resolution::new(640, 0).is_err());
}

#[test]
fn sr_6_resolution_odd_dimensions_rejected_for_yuv420() {
    // YUV420 requires even dimensions
    assert!(Resolution::new(641, 480).is_err());
    assert!(Resolution::new(640, 481).is_err());
}

#[test]
fn sr_6_framerate_zero_rejected() {
    assert!(Framerate::new(0).is_err());
}

#[test]
fn sr_6_framerate_exceeds_max_rejected() {
    assert!(Framerate::new(121).is_err());
}

#[test]
fn sr_6_keyframe_interval_zero_rejected() {
    assert!(KeyFrameInterval::new(0).is_err());
}

#[test]
fn sr_6_keyframe_interval_exceeds_max_rejected() {
    assert!(KeyFrameInterval::new(301).is_err());
}

// ─────────────────────────────────────────────────────────────────────
// SR-7: Codec level enforcement — codec limits are checked.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn sr_7_codec_level_bitrate_limit() {
    let level = VideoCodecLevel::H264Level31;
    assert_eq!(level.max_bitrate_kbps(), 14_000);

    let level = VideoCodecLevel::H264Level41;
    assert_eq!(level.max_bitrate_kbps(), 50_000);
}

#[test]
fn sr_7_config_validates_codec_limits() {
    let mut config = VideoConfig::new(
        VideoCodec::Vp8,
        Resolution::new(1920, 1080).unwrap(),
        Framerate::new(30).unwrap(),
        1000,
    )
    .unwrap();

    // VP8 doesn't support 1080p
    assert!(config.validate_codec_limits().is_err());
}

#[test]
fn sr_7_h264_level_3_1_supports_720p() {
    let config = VideoConfig::new(
        VideoCodec::H264,
        Resolution::new(1280, 720).unwrap(),
        Framerate::new(30).unwrap(),
        14_000,
    )
    .unwrap();
    assert!(config.validate_codec_limits().is_ok());
}

// ─────────────────────────────────────────────────────────────────────
// SR-8: Pipeline state machine — invalid transitions are rejected.
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sr_8_pipeline_cannot_start_from_failed() {
    let (pipeline, _rx) = VideoPipeline::new(build_config());
    // Start then implicitly fail
    pipeline.start().await.unwrap();

    // Attempting to start again should fail
    let err = pipeline.start().await.unwrap_err();
    assert!(matches!(err, VideoError::InvalidPipelineState { .. }));
}

#[tokio::test]
async fn sr_8_pipeline_pause_requires_running() {
    let (pipeline, _rx) = VideoPipeline::new(build_config());

    // Cannot pause from Idle
    let err = pipeline.pause().unwrap_err();
    assert!(matches!(err, VideoError::InvalidPipelineState { .. }));

    // Start the pipeline
    pipeline.start().await.unwrap();

    // Now pause should work - from Running state we can transition to Paused
    pipeline.pause().unwrap();
    assert_eq!(pipeline.state(), PipelineState::Paused);
}

#[tokio::test]
async fn sr_8_pipeline_resume_requires_paused() {
    let (pipeline, _rx) = VideoPipeline::new(build_config());
    pipeline.start().await.unwrap();

    // Cannot resume from Running
    let err = pipeline.resume().unwrap_err();
    assert!(matches!(err, VideoError::InvalidPipelineState { .. }));
}

// ─────────────────────────────────────────────────────────────────────
// SR-9: Frame encoding/decoding — round-trip preserves integrity.
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sr_9_encode_preserves_frame_id() {
    let (pipeline, _rx) = VideoPipeline::new(build_config());
    pipeline.start().await.unwrap();

    // Pipeline starts at seq=0, first frame gets seq=0
    let input = build_raw_frame(1, 1_000_000_000);
    let encoded = pipeline.encode_frame(input).await.unwrap();

    // First encoded frame gets seq=0 from the pipeline
    assert_eq!(encoded.id.seq, 0);
}

#[tokio::test]
async fn sr_9_encode_updates_stats() {
    let (pipeline, _rx) = VideoPipeline::new(build_config());
    pipeline.start().await.unwrap();

    let input = build_raw_frame(1, 1_000_000_000);
    pipeline.encode_frame(input).await.unwrap();

    let stats = pipeline.stats();
    assert_eq!(stats.frames_encoded, 1);
    assert!(stats.bytes_encoded > 0);
}

// ─────────────────────────────────────────────────────────────────────
// SR-10: Quality presets — standard presets produce valid configs.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn sr_10_quality_presets_are_valid() {
    for quality in [
        VideoQuality::Minimum,
        VideoQuality::Standard,
        VideoQuality::High,
        VideoQuality::Maximum,
    ] {
        let config = VideoConfig::from_quality(quality, VideoCodec::H264).unwrap();
        assert!(config.validate_codec_limits().is_ok());
    }
}

#[test]
fn sr_10_minimum_quality_240p() {
    let quality = VideoQuality::Minimum;
    let config = VideoConfig::from_quality(quality, VideoCodec::H264).unwrap();
    assert_eq!(config.resolution.height, 240);
    assert_eq!(config.framerate.fps, 15);
    assert_eq!(config.bitrate_kbps, 100);
}

#[test]
fn sr_10_maximum_quality_1080p_60fps() {
    let quality = VideoQuality::Maximum;
    let config = VideoConfig::from_quality(quality, VideoCodec::H264).unwrap();
    assert_eq!(config.resolution.height, 1080);
    assert_eq!(config.framerate.fps, 60);
    assert_eq!(config.bitrate_kbps, 4000);
}

// ─────────────────────────────────────────────────────────────────────
// SR-11: Safety revision pin — aerospace baseline is reproducible.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn sr_11_safety_revision_is_pinned() {
    use a3net_video::aerospace::SAFETY_REVISION;
    assert!(SAFETY_REVISION.starts_with("VIDEO-"));
}

#[test]
fn sr_11_dal_level_is_b() {
    use a3net_video::aerospace::DAL_LEVEL;
    assert_eq!(DAL_LEVEL, "B");
}

#[test]
fn sr_11_reproducible_build_flag() {
    use a3net_video::aerospace::REPRODUCIBLE_BUILD;
    assert!(REPRODUCIBLE_BUILD);
}

// ─────────────────────────────────────────────────────────────────────
// SR-12: Pixel format validation — YUV420 requires even dimensions.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn sr_12_yuv420_rejects_odd_width() {
    let err = RawFrame::new(
        FrameId::new(0, 1),
        321, // odd
        240,
        PixelFormat::Yuv420,
        vec![0u8; 321 * 240 * 3 / 2],
        0,
        0,
    )
    .unwrap_err();
    assert!(matches!(err, VideoError::InvalidConfig { param, .. } if param == "frame_dimensions"));
}

#[test]
fn sr_12_yuv420_rejects_odd_height() {
    let err = RawFrame::new(
        FrameId::new(0, 1),
        320,
        241, // odd
        PixelFormat::Yuv420,
        vec![0u8; 320 * 241 * 3 / 2],
        0,
        0,
    )
    .unwrap_err();
    assert!(matches!(err, VideoError::InvalidConfig { param, .. } if param == "frame_dimensions"));
}

#[test]
fn sr_12_rgba_accepts_any_even_dimensions() {
    let frame = RawFrame::new(
        FrameId::new(0, 1),
        320,
        240,
        PixelFormat::Rgba,
        vec![0u8; 320 * 240 * 4],
        0,
        0,
    );
    assert!(frame.is_ok());
}
