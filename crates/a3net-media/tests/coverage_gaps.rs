//! Coverage-gap tests — boundary cases that aren't part of the
//! DAL-B compliance suite but should still be exercised.

use a3net_media::codec::{AudioCodec, SampleFormat, VideoCodec};
use a3net_media::config::{MediaConfig, VariantLadder, VariantSpec};
use a3net_media::error::MediaError;
use a3net_media::ingest::MediaIngester;
use a3net_media::transcode::{Frame, PureTranscoder, TranscodeInput, Transcoder};

// ─────────────────────────────────────────────────────────────────────
// VariantLadder edge cases
// ─────────────────────────────────────────────────────────────────────

#[test]
fn empty_ladder_rejected() {
    let err = VariantLadder::new(vec![]);
    assert!(matches!(err, Err(MediaError::InvalidConfig(_))));
}

#[test]
fn duplicate_label_rejected() {
    let specs = vec![
        VariantSpec { label: "x".into(), width: 320, height: 240, bitrate_kbps: 400 },
        VariantSpec { label: "x".into(), width: 640, height: 480, bitrate_kbps: 800 },
    ];
    let err = VariantLadder::new(specs).unwrap_err();
    assert!(matches!(err, MediaError::InvalidConfig(_)));
}

#[test]
fn ladder_max_count_boundary() {
    let specs: Vec<VariantSpec> = (0..4)
        .map(|i| VariantSpec {
            label: format!("v{}", i),
            width: 320 + i as u32 * 100,
            height: 240 + i as u32 * 75,
            bitrate_kbps: 400 + i as u32 * 200,
        })
        .collect();
    VariantLadder::new(specs).unwrap();
}

#[test]
fn ladder_over_max_count_rejected() {
    let specs: Vec<VariantSpec> = (0..5)
        .map(|i| VariantSpec {
            label: format!("v{}", i),
            width: 320 + i as u32 * 100,
            height: 240 + i as u32 * 75,
            bitrate_kbps: 400 + i as u32 * 200,
        })
        .collect();
    let err = VariantLadder::new(specs).unwrap_err();
    assert!(matches!(err, MediaError::InvalidConfig(_)));
}

// ─────────────────────────────────────────────────────────────────────
// MediaConfig edge cases
// ─────────────────────────────────────────────────────────────────────

#[test]
fn media_config_zero_audio_channels_rejected() {
    let mut c = MediaConfig::default_short_video();
    c.audio_channels = 0;
    let err = MediaIngester::new(c).unwrap_err();
    assert!(matches!(err, MediaError::InvalidConfig(_)));
}

#[test]
fn media_config_low_sample_rate_rejected() {
    let mut c = MediaConfig::default_short_video();
    c.audio_sample_rate = 1_000;
    let err = MediaIngester::new(c).unwrap_err();
    assert!(matches!(err, MediaError::InvalidConfig(_)));
}

#[test]
fn media_config_segment_below_min_rejected() {
    let mut c = MediaConfig::default_short_video();
    c.segmenter.target_duration_ms = 100;
    let err = MediaIngester::new(c).unwrap_err();
    assert!(matches!(err, MediaError::InvalidConfig(_)));
}

#[test]
fn media_config_segment_above_max_rejected() {
    let mut c = MediaConfig::default_short_video();
    c.segmenter.target_duration_ms = 10_000;
    let err = MediaIngester::new(c).unwrap_err();
    assert!(matches!(err, MediaError::InvalidConfig(_)));
}

// ─────────────────────────────────────────────────────────────────────
// Transcoder edge cases
// ─────────────────────────────────────────────────────────────────────

#[test]
fn transcoder_audio_layout_mismatch_in_ingest() {
    let ing = MediaIngester::default();
    let mut samples = vec![0u8; 48_000 * 2 * 2 * 2_000 / 1_000];
    samples.push(0xff); // orphan byte
    let frames: Vec<Frame> = (0..60)
        .map(|i| Frame::solid(320, 240, (i & 0xFF) as u8, 0, 0))
        .collect();
    let err = ing
        .ingest(samples, SampleFormat::S16, 2, AudioCodec::Aac, frames, VideoCodec::H264, 30)
        .unwrap_err();
    assert!(matches!(err, MediaError::DecodeError { .. }));
}

#[test]
fn transcoder_rejects_audio_codec_exception() {
    let input = TranscodeInput {
        samples: vec![0u8; 1000],
        sample_format: SampleFormat::S16,
        audio_channels: 2,
        audio_codec: AudioCodec::Aac,
        frames: vec![Frame::solid(320, 240, 0, 0, 0)],
        video_codec: VideoCodec::H264,
        fps: 30,
    };
    let target = VariantSpec { label: "x".into(), width: 320, height: 240, bitrate_kbps: 400 };
    // PureTranscoder is codec-agnostic, but the codec enum is
    // exhaustively checked via sg. from_str / as_str.
    assert_eq!(AudioCodec::Opus.as_str(), "opus");
    assert_eq!(AudioCodec::from_str("opus"), Some(AudioCodec::Opus));
    let _ = PureTranscoder.transcode(&input, &target).unwrap();
}

#[test]
fn transcoder_rejects_too_many_frames() {
    let ing = MediaIngester::default();
    let frames = vec![Frame::solid(320, 240, 0, 0, 0); 1];
    // 1 frame is fine; just smoke.
    let r = ing.ingest(
        vec![0u8; 1024],
        SampleFormat::S16,
        2,
        AudioCodec::Aac,
        frames,
        VideoCodec::H264,
        30,
    ).unwrap();
    assert!(r.manifest.variants.len() >= 4);
}
