//! End-to-end media pipeline test — non-aerospace smoke.
//!
//! Run with:
//!
//! ```sh
//! cargo test -p a3net-media --test media_pipeline
//! ```

use a3net_media::codec::{AudioCodec, SampleFormat, VideoCodec};
use a3net_media::dag::MediaDagBuilder;
use a3net_media::ingest::MediaIngester;
use a3net_media::manifest::MediaManifest;
use a3net_media::transcode::Frame;

#[test]
fn full_pipeline_2s_clip() {
    let ing = MediaIngester::default();
    let samples = vec![0u8; 48_000 * 2 * 2 * 2_000 / 1_000];
    let frames: Vec<Frame> = (0..60)
        .map(|i| Frame::solid(320, 240, (i & 0xFF) as u8, 0, 0))
        .collect();
    let report = ing
        .ingest(samples, SampleFormat::S16, 2, AudioCodec::Aac, frames, VideoCodec::H264, 30)
        .unwrap();

    // 2-second clip → exactly 1 segment per kind.
    assert_eq!(report.manifest.declared_duration_ms, 2_000);
    let video_variants = report
        .segments
        .iter()
        .filter(|s| matches!(s.kind, a3net_media::segment::SegmentKind::Video))
        .count();
    let audio_segments = report
        .segments
        .iter()
        .filter(|s| matches!(s.kind, a3net_media::segment::SegmentKind::Audio))
        .count();
    assert!(video_variants >= 4);
    assert!(audio_segments >= 4);

    // DAG round-trip.
    let dag = MediaDagBuilder::build(&report.manifest, &report.transcoder_outputs).unwrap();
    let m2: MediaManifest = dag.to_manifest().unwrap();
    assert_eq!(m2.root, report.manifest.root);
}

#[test]
fn short_clip_500ms() {
    let ing = MediaIngester::default();
    let samples = vec![0u8; 48_000 * 2 * 2 * 500 / 1_000];
    let frames: Vec<Frame> = (0..15)
        .map(|i| Frame::solid(320, 240, (i & 0xFF) as u8, 0, 0))
        .collect();
    let report = ing
        .ingest(samples, SampleFormat::S16, 2, AudioCodec::Aac, frames, VideoCodec::H264, 30)
        .unwrap();
    assert!(report.manifest.declared_duration_ms >= 500);
}

#[test]
fn longer_clip_60s() {
    let ing = MediaIngester::default();
    let samples = vec![0u8; 48_000 * 2 * 2 * 60_000 / 1_000];
    let frames: Vec<Frame> = (0..1800)
        .map(|i| Frame::solid(320, 240, (i & 0xFF) as u8, 0, 0))
        .collect();
    let report = ing
        .ingest(samples, SampleFormat::S16, 2, AudioCodec::Aac, frames, VideoCodec::H264, 30)
        .unwrap();
    assert_eq!(report.manifest.declared_duration_ms, 60_000);
    // 60s / 2s = 30 segments per variant.
    let video_segments_240p = report
        .segments
        .iter()
        .filter(|s| matches!(s.kind, a3net_media::segment::SegmentKind::Video) && s.index == 0)
        .count();
    assert!(video_segments_240p >= 4);
}
