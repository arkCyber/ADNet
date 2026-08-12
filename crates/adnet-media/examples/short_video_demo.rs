//! Short-video demo — generate a synthetic 4-second clip and
//! inspect the resulting manifest + DAG.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example short_video_demo -p adnet-media
//! ```

use adnet_media::codec::{AudioCodec, SampleFormat, VideoCodec};
use adnet_media::ingest::MediaIngester;
use adnet_media::segment::SegmentKind;
use adnet_media::transcode::Frame;

fn main() {
    let ingester = MediaIngester::default();
    let sample_rate = 48_000u32;
    let duration_ms = 4_000u64;
    let channels = 2u8;
    let bpf = 2u8;
    let samples = vec![0u8; (sample_rate as u64 * duration_ms / 1_000) as usize * channels as usize * bpf as usize];
    let frames: Vec<Frame> = (0..120)
        .map(|i| Frame::solid(320, 240, (i & 0xFF) as u8, 0, 0))
        .collect();

    let report = ingester
        .ingest(samples, SampleFormat::S16, 2, AudioCodec::Aac, frames, VideoCodec::H264, 30)
        .expect("ingest");

    println!("Manifest root: {}", report.manifest.root.as_hex());
    println!("Duration: {} ms", report.manifest.declared_duration_ms);
    println!("Variants: {}", report.manifest.variants.len());
    for v in &report.manifest.variants {
        println!(
            "  - {} {}x{} @ {} kbps ({} segments)",
            v.label,
            v.width,
            v.height,
            v.bitrate_kbps,
            v.segments.len(),
        );
    }
    let v_segs = report.segments.iter().filter(|s| s.kind == SegmentKind::Video).count();
    let a_segs = report.segments.iter().filter(|s| s.kind == SegmentKind::Audio).count();
    println!("Video segments: {}", v_segs);
    println!("Audio segments: {}", a_segs);
    println!("Audio avg RMS: {:.4}", report.audio_energy.avg_rms);
    println!("Audio silence ratio: {:.4}", report.audio_energy.silence_ratio);
}
