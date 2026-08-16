//! P2P Video Call Example - Aerospace-Grade
//!
//! This example demonstrates P2P video functionality using A3Net video pipeline.
//!
//! Features:
//! - Adaptive bitrate based on network conditions
//! - Audio-first bandwidth management  
//! - Jitter buffer for smooth playback
//! - Statistics and diagnostics
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-video --example p2p_video_call
//! ```

use a3net_video::{
    AdaptiveBitrateController, AudioFirstManager, BandwidthEstimator,
    JitterBuffer, QualityLevel,
    VideoConfig, VideoStats,
    Framerate,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ============================================================================
// CLI Arguments
// ============================================================================

#[derive(Debug, Clone)]
struct CliArgs {
    role: Role,
    port: u16,
    target_addr: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum Role {
    Initiator,
    Responder,
}

impl std::str::FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "initiator" => Ok(Role::Initiator),
            "responder" => Ok(Role::Responder),
            _ => Err(format!("Unknown role: {}", s)),
        }
    }
}

// ============================================================================
// P2P Video Call Application
// ============================================================================

struct P2PVideoApp {
    config: VideoConfig,
    stats: VideoStats,
    bandwidth_estimator: BandwidthEstimator,
    adaptive_controller: AdaptiveBitrateController,
    audio_first_manager: AudioFirstManager,
    jitter_buffer: JitterBuffer,
}

impl P2PVideoApp {
    fn new() -> Self {
        let config = VideoConfig::default();
        let framerate = Framerate::new(30).unwrap();
        
        Self {
            config: config.clone(),
            stats: VideoStats::default(),
            bandwidth_estimator: BandwidthEstimator::new(30),
            adaptive_controller: AdaptiveBitrateController::new(config),
            audio_first_manager: AudioFirstManager::new(),
            jitter_buffer: JitterBuffer::new(framerate),
        }
    }

    fn record_frame(&mut self, byte_size: u64) {
        // Update bandwidth estimation
        self.bandwidth_estimator.record(byte_size, Duration::from_millis(33));
        
        // Update stats manually
        self.stats.bytes_encoded += byte_size;
        self.stats.frames_encoded += 1;
    }

    fn record_dropped(&mut self) {
        self.stats.frames_dropped += 1;
    }

    fn adjust_quality(&mut self) -> QualityLevel {
        let bw = self.bandwidth_estimator.bandwidth_kbps();
        self.adaptive_controller.update_network(bw, 50, 0.0, 10.0);
        
        self.adaptive_controller.current_level()
    }

    fn print_status(&self) {
        println!("\n{}", "=".repeat(60));
        println!("P2P Video Call Status");
        println!("{}", "=".repeat(60));
        
        // Bandwidth
        let bw = self.bandwidth_estimator.bandwidth_kbps();
        println!("Bandwidth: {} kbps", bw);
        println!("Stable: {}", self.bandwidth_estimator.is_stable());
        
        // Stats
        println!("\nStatistics:");
        println!("  Frames Encoded:  {}", self.stats.frames_encoded);
        println!("  Frames Decoded:  {}", self.stats.frames_decoded);
        println!("  Frames Dropped: {}", self.stats.frames_dropped);
        println!("  Bytes Encoded:   {}", self.stats.bytes_encoded);
        println!("  Estimated FPS:  {:.2}", self.stats.estimated_fps());
        
        // Audio-first
        println!("\nAudio-First Mode:");
        println!("  Video Enabled: {}", self.audio_first_manager.is_video_enabled());
        println!("  Policy:       {:?}", self.audio_first_manager.policy());
        
        // Adaptive bitrate
        println!("\nAdaptive Bitrate:");
        println!("  Quality Level: {:?}", self.adaptive_controller.current_level());
        
        // Jitter buffer
        println!("\nJitter Buffer:");
        println!("  Mode:        {:?}", self.jitter_buffer.mode());
        println!("  Buffer Level: {}", self.jitter_buffer.buffer_level());
        
        println!("{}", "=".repeat(60));
    }
}

// ============================================================================
// Network Simulation
// ============================================================================

struct NetworkSimulator {
    latency_ms: u32,
    packet_loss_pct: f64,
}

impl NetworkSimulator {
    fn new() -> Self {
        Self {
            latency_ms: 50,
            packet_loss_pct: 0.0,
        }
    }
    
    fn simulate(&self, data: &[u8]) -> bool {
        // Use a simple pseudo-random check for packet loss
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        data.hash(&mut hasher);
        let hash = hasher.finish();
        
        // 1% chance of packet loss based on hash
        (hash % 100) as f64 >= self.packet_loss_pct
    }
    
    fn set_quality(&mut self, _bandwidth_kbps: u32, packet_loss: f64, latency: u32) {
        self.latency_ms = latency;
        self.packet_loss_pct = packet_loss;
    }
}

// ============================================================================
// Main Function
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("A3Net P2P Video Call Example - Aerospace Grade");
    println!("{}", "=".repeat(60));
    
    // Parse CLI args
    let args = parse_args();
    println!("Role: {:?}", args.role);
    println!("Port: {}", args.port);
    if let Some(ref addr) = args.target_addr {
        println!("Target: {}", addr);
    }
    
    // Create application
    let mut app = P2PVideoApp::new();
    
    println!("\nInitializing video call simulation...");
    println!("Video Config:");
    println!("  Enabled:    {}", app.config.is_enabled());
    println!("  Resolution: {}x{}", app.config.resolution.width, app.config.resolution.height);
    println!("  Framerate: {} fps", app.config.framerate.fps);
    println!("  Codec:     {:?}", app.config.codec);
    
    // Simulate video call
    println!("\nSimulating video call (10 seconds @ 30fps)...");
    
    // Simulate network
    let mut network = NetworkSimulator::new();
    let mut frame_count = 0u32;
    let mut total_bytes = 0u64;
    let start_time = Instant::now();
    
    for i in 0..300 { // 10 seconds at 30fps
        // Simulate frame size (keyframes larger than delta frames)
        let is_keyframe = i % 30 == 0;
        let frame_size = if is_keyframe { 50000u64 } else { 2000u64 };
        
        // Record frame
        app.record_frame(frame_size);
        total_bytes += frame_size;
        frame_count += 1;
        
        // Simulate network transmission
        let data = vec![0u8; frame_size as usize];
        if !network.simulate(&data) {
            app.record_dropped();
        }
        
        // Adjust quality every 30 frames
        if i % 30 == 0 {
            let _quality = app.adjust_quality();
            
            // Simulate changing network conditions
            if i > 150 {
                network.set_quality(500, 5.0, 100); // Degrade network
            }
            
            app.print_status();
        }
        
        // Simulate frame interval
        tokio::time::sleep(Duration::from_millis(33)).await;
    }
    
    let elapsed = start_time.elapsed();
    let actual_fps = frame_count as f64 / elapsed.as_secs_f64();
    let avg_bitrate = (total_bytes * 8) as f64 / elapsed.as_secs_f64() / 1000.0;
    
    println!("\n{}", "=".repeat(60));
    println!("Video Call Summary");
    println!("{}", "=".repeat(60));
    println!("  Role:         {:?}", args.role);
    println!("  Frames:       {}", frame_count);
    println!("  Duration:     {:?}", elapsed);
    println!("  Average FPS: {:.2}", actual_fps);
    println!("  Total Bytes: {} ({:.2} MB)", total_bytes, total_bytes as f64 / 1_000_000.0);
    println!("  Avg Bitrate: {:.2} kbps", avg_bitrate);
    println!("  Final Quality: {:?}", app.adaptive_controller.current_level());
    println!("{}", "=".repeat(60));
    
    println!("\nExample completed successfully!");
    Ok(())
}

fn parse_args() -> CliArgs {
    let mut role = Role::Initiator;
    let mut port = 9000u16;
    let mut target_addr = None;
    
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    
    while i < args.len() {
        match args[i].as_str() {
            "--role" => {
                if i + 1 < args.len() {
                    role = args[i + 1].parse().unwrap_or(Role::Initiator);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--port" => {
                if i + 1 < args.len() {
                    port = args[i + 1].parse().unwrap_or(9000);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--target" => {
                if i + 1 < args.len() {
                    target_addr = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    
    CliArgs {
        role,
        port,
        target_addr,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_args_parsing() {
        let role: Role = "initiator".parse().unwrap();
        assert!(matches!(role, Role::Initiator));
        
        let role: Role = "responder".parse().unwrap();
        assert!(matches!(role, Role::Responder));
        
        assert!("invalid".parse::<Role>().is_err());
    }

    #[test]
    fn test_network_simulator() {
        let network = NetworkSimulator::new();
        
        // Should always succeed with 0% packet loss
        let data = vec![1u8, 2, 3, 4, 5];
        assert!(network.simulate(&data));
    }

    #[test]
    fn test_p2p_app_creation() {
        let app = P2PVideoApp::new();
        assert!(app.config.is_enabled());
    }
    
    #[test]
    fn test_bandwidth_estimation() {
        let mut estimator = BandwidthEstimator::new(30);
        estimator.record(1000, Duration::from_secs(1));
        
        let bw = estimator.bandwidth_kbps();
        assert!(bw > 0);
    }
    
    #[test]
    fn test_adaptive_controller() {
        let config = VideoConfig::default();
        let mut controller = AdaptiveBitrateController::new(config);
        
        controller.update_network(2000, 50, 0.0, 10.0);
        
        let level = controller.current_level();
        assert!(matches!(level, QualityLevel::Standard | QualityLevel::High));
    }
    
    #[test]
    fn test_stats_estimation() {
        let stats = VideoStats::default();
        
        // Initially zero
        assert_eq!(stats.frames_encoded, 0);
        assert_eq!(stats.frames_decoded, 0);
        assert_eq!(stats.estimated_fps(), 0.0);
    }
}
