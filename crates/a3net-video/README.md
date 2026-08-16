# a3net-video

> **Aerospace-Grade Real-Time Video Streaming for A3Net**

`a3net-video` provides production-ready, fault-tolerant video pipeline components for the A3Net decentralized network. Designed to meet DO-178C DAL-B safety standards for mission-critical applications.

## Features

### Core Video Pipeline

- **Multi-Codec Support**: H.264, VP8, VP9, AV1 with automatic negotiation
- **Cross-Platform Capture**: V4L2 (Linux), AVFoundation (macOS), MediaFoundation (Windows), MediaDevices (WASM)
- **Configurable Quality Presets**: Ultra-low latency, Balanced, High quality
- **Adaptive Bitrate Control**: Real-time network condition adaptation

### Fault Tolerance (DO-178C DAL-B Compliant)

- **Automatic Error Recovery**: Exponential backoff with configurable retry strategies
- **Circuit Breaker Pattern**: Prevents cascade failures with automatic recovery
- **Health Monitoring**: Real-time component health tracking
- **Rate Limiting**: Token bucket algorithm for resource protection
- **Jitter Buffer**: Frame reordering with configurable latency/quality tradeoff

### Audio-First Architecture

- **Priority-Based Bandwidth Allocation**: Audio always gets bandwidth priority
- **Visual Fallback Modes**: Image snapshots, slides, or static frames when bandwidth is limited
- **Escalation Management**: Automatic quality adjustment based on conditions

### Comprehensive Error Handling

- **Structured Error Types**: 40+ error variants with severity levels
- **Error Classification**: Transient, fatal, recoverable, network errors
- **Error Codes**: Unique codes for logging and debugging
- **Recovery Context**: Automatic error wrapping with attempt tracking

## Installation

```toml
[dependencies]
a3net-video = { path = "crates/a3net-video" }
```

### Feature Flags

```toml
[dependencies.a3net-video]
features = [
    "capture",       # Video capture support
    "h264",         # H.264 codec
    "vpx",          # VP8/VP9 codecs
    "image-processing",  # Image utilities
    "metrics",       # Statistics collection
    "aerospace",     # DO-178C DAL-B compliance suite
]
```

## Quick Start

```rust
use a3net_video::{VideoPipeline, VideoConfig, Framerate, Resolution};

fn main() -> anyhow::Result<()> {
    // Create configuration
    let config = VideoConfig {
        enabled: true,
        resolution: Resolution::HD,
        framerate: Framerate::new(30)?,
        codec: VideoCodec::H264,
        ..Default::default()
    };

    // Create pipeline
    let pipeline = VideoPipeline::new(config.clone())?;

    // Start pipeline
    pipeline.start()?;

    // Send frames
    pipeline.send_frame(raw_frame)?;

    Ok(())
}
```

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Video Pipeline                               │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│   Camera/Screen                                                      │
│      │                                                               │
│      ▼  capture                                                      │
│   RawFrame { width, height, timestamp, pixel_data }                 │
│      │                                                               │
│      ▼  encode                                                       │
│   EncodedFrame { codec, keyframe, data, timestamp, seq }             │
│      │                                                               │
│      ▼  stream (via WebRTC SRTP)                                     │
│   MediaTrack ──── WebRTC PeerConnection ──── Peer                   │
│      │                                                               │
│      ▼  decode                                                       │
│   EncodedFrame                                                       │
│      │                                                               │
│      ▼  render                                                       │
│   RawFrame                                                           │
│      │                                                               │
│      ▼  display                                                      │
│   [Render Target]                                                    │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

### Safety Invariants (DO-178C DAL-B)

- Every public function returns `Result<_, VideoError>` - no unwraps in production
- Frame buffers carry explicit length prefixes
- All timestamps are monotonic `u64` nanoseconds
- Sequence numbers are `u32` wrapping integers with gap detection
- Keyframes are mandatory at configurable intervals

## Error Handling

```rust
use a3net_video::{VideoError, VideoResult, ErrorSeverity};

fn process_frame(result: VideoResult<EncodedFrame>) {
    match result {
        Ok(frame) => { /* process */ }
        Err(e) => {
            match e.severity() {
                ErrorSeverity::Critical => {
                    // Pipeline restart required
                    eprintln!("Critical error: {}", e);
                }
                ErrorSeverity::Warning => {
                    // Log and continue
                    eprintln!("Warning: {}", e);
                }
                ErrorSeverity::Info => {
                    // Debug info only
                    tracing::debug!("Info: {}", e);
                }
                ErrorSeverity::Error => {
                    // Handle but continue
                    eprintln!("Error: {}", e);
                }
            }
        }
    }
}
```

### Error Categories

| Category | Errors | Retry? |
|----------|--------|--------|
| Transient | BufferOverflow, FrameLate, NetworkCongestion | Yes |
| Fatal | CodecInit, PipelineStopped, OutOfMemory | No |
| Recoverable | EncodeFailed, DecodeFailed, HardwareAccelFailed | With Fallback |
| Network | TrackNotConnected, PeerConnectionFailed | Yes |

## Resilience Patterns

### Circuit Breaker

```rust
use a3net_video::{CircuitBreaker, CircuitState};

let cb = CircuitBreaker::new();

// Check before operation
if cb.allows_request() {
    match operation() {
        Ok(result) => cb.record_success(),
        Err(_) => cb.record_failure(),
    }
}

// Query state
match cb.state() {
    CircuitState::Closed => println!("Healthy"),
    CircuitState::Open => println!("Circuit open - failing fast"),
    CircuitState::HalfOpen => println!("Testing recovery"),
}
```

### Automatic Recovery

```rust
use a3net_video::{ErrorRecovery, RecoveryConfig, RecoveryState};

let config = RecoveryConfig {
    max_attempts: 3,
    base_delay_ms: 100,
    max_delay_ms: 5000,
    ..Default::default()
};

let recovery = ErrorRecovery::with_config(config);

loop {
    recovery.record_attempt();
    match try_operation() {
        Ok(result) => {
            recovery.record_success();
            break result;
        }
        Err(e) => {
            recovery.record_failure();
            if recovery.state() == RecoveryState::Failed {
                return Err(e); // Give up
            }
            // Exponential backoff before retry
            std::thread::sleep(recovery.next_retry_delay());
        }
    }
}
```

### Health Monitoring

```rust
use a3net_video::{HealthMonitor, HealthStatus};

let monitor = HealthMonitor::new();

// Record operations
monitor.record_success(10.5);  // latency in ms
monitor.record_failure();

// Check health
let metrics = monitor.metrics();
println!("Success rate: {:.2}%", metrics.success_rate() * 100.0);
println!("Health: {:?}", monitor.status());
```

## Audio-First Bandwidth Management

```rust
use a3net_video::{AudioFirstManager, BandwidthPolicy};

let manager = AudioFirstManager::new();

// Update network conditions
manager.update(packet_loss_pct, bandwidth_kbps);

// Check video status
if manager.is_video_enabled() {
    let quality = manager.recommended_video_quality();
    println!("Recommended quality: {:?}", quality);
}

// Access audio requirements
let requirements = manager.audio_requirements();
println!("Audio bitrate: {} kbps", requirements.min_bitrate_kbps);
```

## Adaptive Bitrate Control

```rust
use a3net_video::{AdaptiveBitrateController, QualityLevel};

let controller = AdaptiveBitrateController::new(config);

// Update network metrics
controller.update_network(
    bandwidth_kbps,
    rtt_ms,
    packet_loss_pct,
    jitter_ms
);

// Get current quality level
match controller.current_level() {
    QualityLevel::UltraLow => { /* 320x240 @ 15fps */ }
    QualityLevel::Low => { /* 480x360 @ 24fps */ }
    QualityLevel::Standard => { /* 854x480 @ 30fps */ }
    QualityLevel::High => { /* 1280x720 @ 30fps */ }
    QualityLevel::UltraHigh => { /* 1920x1080 @ 60fps */ }
}
```

## Jitter Buffer

```rust
use a3net_video::{JitterBuffer, JitterBufferMode};

let buffer = JitterBuffer::new(framerate);

// Insert frames
buffer.insert(encoded_frame)?;

// Get next frame (with reordering)
if let Some(frame) = buffer.next() {
    // Process in-order frame
}

// Configure mode
buffer.set_mode(JitterBufferMode::Adaptive);

// Get statistics
let stats = buffer.stats();
println!("Frames received: {}", stats.frames_received);
println!("Frames delivered: {}", stats.frames_delivered);
println!("Frames dropped: {}", stats.frames_dropped);
```

## Testing

```bash
# Run all tests
cargo test -p a3net-video

# Run with coverage
cargo test -p a3net-video -- --nocapture

# Run specific test
cargo test -p a3net-video test_circuit_breaker

# Run example
cargo run -p a3net-video --example video_app -- --duration 5
```

### Example Output

```
╔══════════════════════════════════════════════════════════════╗
║         A3Net P2P Video Application - Aerospace Grade       ║
╚══════════════════════════════════════════════════════════════╝

Video Configuration:
  Enabled:    true
  Resolution: 854x480
  Framerate: 30 fps
  Codec:     H264

  Encoded:     144 frames
  Decoded:     144 frames
  Dropped:     0 frames (0.0%)

Assessment:
  ✓ Excellent session quality
```

## Platform Support

| Platform | Capture Backend | Notes |
|----------|-----------------|-------|
| Linux | V4L2 | Requires v4l2loopback for testing |
| macOS | AVFoundation | Camera and screen capture |
| Windows | MediaFoundation | DirectShow fallback |
| WebAssembly | MediaDevices | Browser-based capture |
| Other | Software Generator | Built-in test source |

## Safety Standards (DO-178C DAL-B)

This crate implements the following safety requirements:

| Requirement | Description |
|-------------|-------------|
| SR-1 | Frame buffer overflow prevention |
| SR-2 | Manifest hash verification |
| SR-3 | Timestamp monotonicity enforcement |
| SR-4 | Keyframe insertion guarantees |
| SR-5 | Sequence gap detection and reporting |
| SR-6 | Truncated frame detection |
| SR-7 | Codec initialization verification |
| SR-8 | Pipeline state machine correctness |
| SR-9 | Frame size limits enforcement |
| SR-10 | Timeout handling |
| SR-11 | Integrity check verification |
| SR-12 | Error recovery validation |

## License

MIT OR Apache-2.0
