//! Minimal adnet-media example.
//!
//! Builds a 4-second silent video from solid RGB frames, ingests
//! it through the full pipeline, verifies the manifest, and prints
//! a small summary.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p adnet-media --example media_basic
//! ```

use adnet_media::transcode::Frame;
use adnet_media::{AudioCodec, MediaIngester, SampleFormat, VideoCodec};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ingester = MediaIngester::default();

    // 4 seconds of silence at 48kHz / 16-bit / stereo
    let samples = vec![0u8; 48_000 * 2 * 2 * 4];

    // 120 frames at 30 fps => 4 seconds, 320x240
    let frames: Vec<Frame> = (0..120)
        .map(|i| Frame::solid(320, 240, (i & 0xFF) as u8, 0, 0))
        .collect();

    let report = ingester.ingest(
        samples,
        SampleFormat::S16,
        2,
        AudioCodec::Aac,
        frames,
        VideoCodec::H264,
        30,
    )?;

    println!("== Ingest summary ==");
    println!("manifest.version    : {}", report.manifest.manifest_version);
    println!("declared_duration_ms: {}", report.manifest.declared_duration_ms);
    println!("declared_byte_size  : {}", report.manifest.declared_byte_size);
    println!("variants            : {}", report.manifest.variants.len());
    for v in &report.manifest.variants {
        println!(
            "  - {:>6} {}x{} @ {}kbps ({} segments)",
            v.label, v.width, v.height, v.bitrate_kbps, v.segments.len()
        );
    }
    println!("audio.segments      : {}", report.manifest.audio.segments.len());
    println!("audio.avg_rms_q16   : {}", report.manifest.audio.avg_rms_q16);
    println!("audio.silence_ratio : {}", report.manifest.audio.silence_ratio_q16);

    report.manifest.verify()?;
    println!("\nmanifest.verify()   : OK");
    println!("root digest         : {}", report.manifest.root.as_hex());

    use adnet_media::verify::verify_dag;
    let verify = verify_dag(
        &report.manifest,
        report.manifest.declared_duration_ms,
        0,
        50,
    );
    println!("verify_dag(...)     : {:?}", verify.status);

    Ok(())
}
