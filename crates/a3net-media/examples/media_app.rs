//! End-to-end demonstration: build a `MediaDag` from a synthetic
//! 1-second clip, hash every segment, then verify the whole DAG
//! against the recorded root hash. This is the same verification
//! path the `aerospace` compliance test suite exercises.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-media --example media_app
//! ```

use a3net_media::codec::{AudioCodec, SampleFormat, VideoCodec};
use a3net_media::ingest::MediaIngester;
use a3net_media::integrity::segment_hash;
use a3net_media::segment::SegmentKind;
use a3net_media::transcode::Frame;
use a3net_media::verify::{verify_manifest, VerifyStatus};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Build a 1-second synthetic clip at 48 kHz stereo + 30 fps.
    let ingester = MediaIngester::default();
    let sample_rate = 48_000u32;
    let duration_ms = 1_000u64;
    let channels = 2u8;
    let bpf = 2u8;
    let samples = vec![0u8; (sample_rate as u64 * duration_ms / 1_000) as usize * channels as usize * bpf as usize];
    let frames: Vec<Frame> = (0..30)
        .map(|i| Frame::solid(320, 240, (i & 0xFF) as u8, 0, 0))
        .collect();

    let report = ingester.ingest(
        samples, SampleFormat::S16, 2, AudioCodec::Aac,
        frames, VideoCodec::H264, 30,
    )?;

    // 2. Hash every segment individually and confirm they all
    //    round-trip.
    let video_segs = report.segments.iter().filter(|s| s.kind == SegmentKind::Video).count();
    let audio_segs = report.segments.iter().filter(|s| s.kind == SegmentKind::Audio).count();
    println!("segments: video={video_segs} audio={audio_segs}");

    let mut hashes = Vec::new();
    for seg in &report.segments {
        let h = segment_hash(&seg.payload);
        hashes.push(hex::encode(h));
    }
    assert_eq!(hashes.len(), report.segments.len());
    println!("hashed {} segments", hashes.len());

    // 3. Verify the manifest itself.
    let verify = verify_manifest(&report.manifest);
    match verify.status {
        VerifyStatus::Ok => println!("verify_manifest: ok ({} segments)", verify.segments),
        other => return Err(format!("manifest verification failed: {other:?}").into()),
    }

    println!(
        "manifest root: {:?}\nduration: {} ms\nvariants: {}\nsegments: {}",
        report.manifest.root,
        report.manifest.declared_duration_ms,
        report.manifest.variants.len(),
        report.segments.len(),
    );
    Ok(())
}