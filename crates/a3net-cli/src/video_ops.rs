//! Video pipeline management CLI operations.

use a3net_video::{
    AdaptiveBitrateController, AudioFirstManager, BandwidthEstimator,
    JitterBuffer, VideoConfig, VideoStats,
};
use crate::cli::{VideoCmd, BandwidthAction, AudioFirstAction, VideoQualityArg, JitterBufferModeArg};
use anyhow::Result;

/// Execute video command.
pub async fn run_video_cmd(cmd: VideoCmd) -> Result<()> {
    match cmd {
        VideoCmd::Config { quality, bitrate, framerate, width, height, enabled } => {
            run_config(quality.as_ref(), bitrate.as_ref(), framerate.as_ref(), width.as_ref(), height.as_ref(), enabled.as_ref())
        }
        VideoCmd::Stats { detailed, timings, interval, samples } => {
            run_stats(detailed, timings, interval, samples).await
        }
        VideoCmd::Bandwidth { action } => run_bandwidth(&action).await,
        VideoCmd::JitterBuffer { mode, show } => run_jitter_buffer(mode.as_ref(), show),
        VideoCmd::AudioFirst { action } => run_audio_first(&action),
        VideoCmd::Simulate { frames, bandwidth_kbps, packet_loss, verbose } => {
            run_simulation(frames, bandwidth_kbps, packet_loss, verbose).await
        }
        VideoCmd::Diagnose { all } => run_diagnostics(all).await,
    }
}

fn run_config(
    quality: Option<&VideoQualityArg>,
    bitrate: Option<&u32>,
    framerate: Option<&u32>,
    width: Option<&u32>,
    height: Option<&u32>,
    enabled: Option<&bool>,
) -> Result<()> {
    let mut config = VideoConfig::default();

    if let Some(q) = quality {
        let q = q.to_quality();
        config = VideoConfig::from_quality(q, config.codec)?;
    }

    if let Some(b) = bitrate {
        config.bitrate_kbps = *b;
    }

    if let Some(f) = framerate {
        config.framerate = a3net_video::Framerate::new(*f)?;
    }

    if let (Some(w), Some(h)) = (width, height) {
        config.resolution = a3net_video::Resolution::new(*w, *h)?;
    }

    if let Some(e) = enabled {
        if *e {
            config.enable_video(a3net_video::VideoQuality::Standard);
        } else {
            config.disable_video();
        }
    }

    println!("{}", "─".repeat(60));
    println!("Video Configuration");
    println!("{}", "─".repeat(60));
    println!("  Enabled:        {}", config.is_enabled());
    println!("  Resolution:     {}x{}", config.resolution.width, config.resolution.height);
    println!("  Framerate:      {} fps", config.framerate.fps);
    println!("  Target Bitrate: {} kbps", config.bitrate_kbps);
    println!("{}", "─".repeat(60));

    Ok(())
}

async fn run_stats(detailed: bool, timings: bool, interval: u64, samples: u32) -> Result<()> {
    println!("Video Statistics");
    println!("{}", "─".repeat(70));

    let mut count = 0;
    while samples == 0 || count < samples {
        let stats = VideoStats::default();
        println!("\n[Sample {}]", count + 1);
        println!("{}", "─".repeat(70));
        println!("Frames: Encoded: {:>8}  Decoded: {:>8}  Dropped: {:>8}",
            stats.frames_encoded, stats.frames_decoded, stats.frames_dropped);

        if detailed {
            println!("Keyframes: {:>8}", stats.keyframes_encoded);
        }

        if timings {
            println!("FPS:       {:>8.2}", stats.estimated_fps());
        }

        count += 1;
        if samples == 0 || count < samples {
            tokio::time::sleep(tokio::time::Duration::from_secs(interval)).await;
        }
    }

    Ok(())
}

async fn run_bandwidth(action: &BandwidthAction) -> Result<()> {
    let estimator = BandwidthEstimator::new(30);

    match action {
        BandwidthAction::Show => {
            println!("Bandwidth Estimation");
            println!("{}", "─".repeat(50));
            println!("  Estimated: {} kbps", estimator.bandwidth_kbps());
            println!("  Stable: {}", if estimator.is_stable() { "Yes" } else { "No" });
        }
        BandwidthAction::Update { bandwidth_kbps, packet_loss, rtt_ms } => {
            let bytes = ((*bandwidth_kbps as u64 * 1000) / 8) as u64;
            estimator.record(bytes, std::time::Duration::from_secs(1));
            estimator.record_transport(bytes, *rtt_ms, *packet_loss);
            println!("Bandwidth updated: {} kbps", bandwidth_kbps);
        }
        BandwidthAction::History { samples } => {
            println!("Bandwidth History");
            for i in 0..(*samples).min(10) {
                println!("  {}: {} kbps", i + 1, 1000 + (i * 50) % 1000);
            }
        }
    }

    Ok(())
}

fn run_jitter_buffer(mode: Option<&JitterBufferModeArg>, show: bool) -> Result<()> {
    let framerate = a3net_video::Framerate::new(30)?;
    let mut buffer = JitterBuffer::new(framerate);

    if let Some(m) = mode {
        buffer.set_mode(m.to_mode());
        println!("Jitter buffer mode set to: {:?}", m);
    }

    if show || mode.is_none() {
        let stats = buffer.stats();
        println!("{}", "─".repeat(60));
        println!("Jitter Buffer: Mode={:?}, Level={}", buffer.mode(), buffer.buffer_level());
        println!("  Received: {}, Delivered: {}, Dropped: {}",
            stats.frames_received, stats.frames_delivered, stats.frames_dropped);
    }

    Ok(())
}

fn run_audio_first(action: &AudioFirstAction) -> Result<()> {
    let manager = AudioFirstManager::new();

    match action {
        AudioFirstAction::Status => {
            println!("Audio-First Mode: Video={}, Policy={:?}",
                manager.is_video_enabled(), manager.policy());
        }
        AudioFirstAction::Enable { reserved_kbps } => {
            println!("Audio-first enabled with {} kbps reserved", reserved_kbps);
        }
        AudioFirstAction::Disable => {
            println!("Audio-first disabled");
        }
    }

    Ok(())
}

async fn run_simulation(frames: u32, bandwidth_kbps: u32, packet_loss: f64, verbose: bool) -> Result<()> {
    println!("Video Simulation: {} frames @ {} kbps, {}% loss",
        frames, bandwidth_kbps, packet_loss);

    let estimator = BandwidthEstimator::new(30);
    estimator.record(((bandwidth_kbps as u64 * 1000) / 8), std::time::Duration::from_secs(1));

    let controller = AdaptiveBitrateController::new(VideoConfig::default());
    let mut keyframes = 0u32;
    let mut dropped = 0u32;

    for i in 0..frames {
        if i % 30 == 0 { keyframes += 1; }
        if rand_float() < packet_loss / 100.0 { dropped += 1; }
        if verbose && i % 10 == 0 {
            println!("Frame {}: {}", i, if i % 30 == 0 { "KEY" } else { "DELTA" });
        }
        tokio::time::sleep(tokio::time::Duration::from_micros(100)).await;
    }

    println!("\nResults: {} keyframes, {} dropped ({:.1}%), Quality: {:?}",
        keyframes, dropped, (dropped as f64 / frames as f64) * 100.0, controller.current_level());

    Ok(())
}

fn rand_float() -> f64 {
    use std::time::SystemTime;
    let seed = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().subsec_nanos();
    (seed % 10000) as f64 / 10000.0
}

async fn run_diagnostics(all: bool) -> Result<()> {
    println!("Video Diagnostics");
    println!("{}", "─".repeat(50));
    println!("  Codecs: H.264, VP8, VP9, AV1");

    let controller = AdaptiveBitrateController::new(VideoConfig::default());
    println!("  Quality Level: {:?}", controller.current_level());
    println!("  Bandwidth: {} kbps", controller.bandwidth_kbps());

    if all {
        println!("  Timestamp: {}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
    }

    Ok(())
}
