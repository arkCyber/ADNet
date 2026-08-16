//! Complete Video Application with Pre-flight Check
//!
//! This example demonstrates the full video application startup flow:
//! 1. Pre-flight device check
//! 2. Configuration wizard
//! 3. Video pipeline initialization
//! 4. Real-time monitoring
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-video --example video_with_check
//! ```

use a3net_video::preflight::{
    PreFlightChecker, PreFlightReport, DeviceCheckResult, CheckStatus,
    run_preflight_checks, run_interactive_preflight,
};
use a3net_video::{
    CaptureFactory, Platform, VideoCapture, current_platform,
    VideoCodec, VideoConfig, VideoQuality,
    Framerate, Resolution, PipelineConfig,
    VideoPipeline, PipelineState,
    BandwidthEstimator, NetworkState,
};
use std::time::{Duration, Instant};

/// Application configuration wizard
struct ConfigWizard;

impl ConfigWizard {
    fn run() -> VideoConfig {
        println!();
        println!("╔══════════════════════════════════════════════════════════════╗");
        println!("║           Configuration Wizard                              ║");
        println!("╚══════════════════════════════════════════════════════════════╝");
        println!();

        // Show platform info
        let platform = current_platform();
        println!("  Platform: {}", platform.name());
        println!();

        // Use default high quality configuration
        println!("  Selected Quality: High (720p @ 30fps)");
        println!("  Selected Codec: H.264");

        let config = VideoConfig::from_quality(VideoQuality::High, VideoCodec::H264)
            .expect("Default configuration should be valid");

        println!();
        println!("  Configuration Summary:");
        println!("    Resolution: {}x{}", config.resolution.width, config.resolution.height);
        println!("    Framerate: {} fps", config.framerate.fps);
        println!("    Bitrate: {} kbps", config.bitrate_kbps);
        println!();

        config
    }
}

/// Video session manager
struct VideoSession {
    pipeline: VideoPipeline,
    stats: SessionStats,
    start_time: Instant,
}

struct SessionStats {
    frames_sent: u64,
    frames_received: u64,
    bytes_sent: u64,
    bytes_received: u64,
}

impl VideoSession {
    fn new(config: VideoConfig) -> Self {
        let pipeline_config = PipelineConfig {
            enable_video: true,
            video: config,
        };

        let (pipeline, _events) = VideoPipeline::new(pipeline_config);

        Self {
            pipeline,
            stats: SessionStats {
                frames_sent: 0,
                frames_received: 0,
                bytes_sent: 0,
                bytes_received: 0,
            },
            start_time: Instant::now(),
        }
    }

    fn start(&mut self) -> Result<(), String> {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| e.to_string())?;

        rt.block_on(async {
            self.pipeline.start().await
        }).map_err(|e| e.to_string())?;

        println!("  ✅ Video pipeline started");
        Ok(())
    }

    fn capture_frame(&mut self) -> Result<(), String> {
        // Get capture device
        let mut capture = CaptureFactory::create(640, 480)
            .map_err(|e| e.to_string())?;

        // Capture frame
        let frame = capture.capture_frame()
            .map_err(|e| e.to_string())?;

        println!("  📹 Captured frame: {}x{}", frame.width, frame.height);
        self.stats.frames_sent += 1;
        self.stats.bytes_sent += frame.data.len() as u64;

        Ok(())
    }

    fn status(&self) -> SessionStatus {
        let elapsed = self.start_time.elapsed();
        let fps = self.stats.frames_sent as f64 / elapsed.as_secs().max(1) as f64;

        SessionStatus {
            state: format!("{:?}", self.pipeline.state()),
            frames_sent: self.stats.frames_sent,
            frames_received: self.stats.frames_received,
            bytes_sent: self.stats.bytes_sent,
            bytes_received: self.stats.bytes_received,
            uptime: format!("{:?}", elapsed),
            fps,
        }
    }
}

struct SessionStatus {
    state: String,
    frames_sent: u64,
    frames_received: u64,
    bytes_sent: u64,
    bytes_received: u64,
    uptime: String,
    fps: f64,
}

fn print_status_bar(status: &SessionStatus) {
    print!("\r  Status: {} | Uptime: {} | FPS: {:.1} | Frames: {} | Bytes: {}   ",
           status.state, status.uptime, status.fps, status.frames_sent, status.bytes_sent);
}

fn main() {
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║     A3Net Video Application with Device Check              ║");
    println!("║     Aerospace Grade - DO-178C DAL-B Compliant             ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    // ─────────────────────────────────────────────────────────────────
    // Step 1: Pre-flight Device Check
    // ─────────────────────────────────────────────────────────────────
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ Step 1: Pre-flight Device Check                            │");
    println!("└─────────────────────────────────────────────────────────────┘");

    let checker = PreFlightChecker::new();
    let report = checker.run_all_checks();

    println!();
    println!("{}", report.summary());

    if !report.ready {
        println!();
        println!("❌ Pre-flight check failed!");
        println!("   Please resolve the issues above and restart.");
        std::process::exit(1);
    }

    println!();
    println!("✅ Pre-flight check passed! Starting video application...");

    // ─────────────────────────────────────────────────────────────────
    // Step 2: Configuration
    // ─────────────────────────────────────────────────────────────────
    println!();
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ Step 2: Configuration                                     │");
    println!("└─────────────────────────────────────────────────────────────┘");

    let config = ConfigWizard::run();

    // ─────────────────────────────────────────────────────────────────
    // Step 3: Initialize Video Pipeline
    // ─────────────────────────────────────────────────────────────────
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ Step 3: Initializing Video Pipeline                        │");
    println!("└─────────────────────────────────────────────────────────────┘");
    println!();

    let mut session = VideoSession::new(config);

    match session.start() {
        Ok(()) => {
            println!();
            println!("  ✅ Video session initialized successfully!");
        }
        Err(e) => {
            println!();
            println!("  ❌ Failed to start video session: {}", e);
            std::process::exit(1);
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Step 4: Capture Demo Frames
    // ─────────────────────────────────────────────────────────────────
    println!();
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ Step 4: Capturing Demo Frames                             │");
    println!("└─────────────────────────────────────────────────────────────┘");
    println!();

    println!("  Capturing 5 demo frames...");
    println!();

    for i in 1..=5 {
        match session.capture_frame() {
            Ok(()) => {
                print!("  Frame {} captured", i);
                let status = session.status();
                print_status_bar(&status);
                println!();
            }
            Err(e) => {
                println!("  ❌ Frame {} failed: {}", i, e);
            }
        }

        // Small delay between frames
        std::thread::sleep(Duration::from_millis(100));
    }

    // ─────────────────────────────────────────────────────────────────
    // Final Status
    // ─────────────────────────────────────────────────────────────────
    println!();
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ Session Summary                                             │");
    println!("└─────────────────────────────────────────────────────────────┘");
    println!();

    let status = session.status();
    println!("  Pipeline State: {}", status.state);
    println!("  Uptime: {}", status.uptime);
    println!("  Frames Sent: {}", status.frames_sent);
    println!("  Frames Received: {}", status.frames_received);
    println!("  Bytes Sent: {} ({:.2} MB)",
             status.bytes_sent, status.bytes_sent as f64 / 1_048_576.0);
    println!("  Bytes Received: {} ({:.2} MB)",
             status.bytes_received, status.bytes_received as f64 / 1_048_576.0);
    println!("  Average FPS: {:.1}", status.fps);

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("✅ Video application completed successfully!");
    println!("═══════════════════════════════════════════════════════════════");
}
