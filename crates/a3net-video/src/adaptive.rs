//! Adaptive bitrate control for dynamic video quality adjustment.
//!
//! Monitors network conditions and adjusts video quality to maintain smooth playback
//! while maximizing quality within available bandwidth.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::config::{Framerate, Resolution, VideoConfig, VideoQuality};
use crate::error::{VideoError, VideoResult};

/// Bandwidth estimation window size.
const BANDWIDTH_WINDOW_FRAMES: usize = 30;

/// Minimum bitrate in kbps.
const MIN_BITRATE_KBPS: u32 = 100;

/// Maximum bitrate in kbps.
const MAX_BITRATE_KBPS: u32 = 10_000;

/// Target buffer level in frames.
const TARGET_BUFFER_LEVEL: usize = 15;

/// Bandwidth estimation method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandwidthMethod {
    /// Use transport RTT and packet loss.
    TransportBased,
    /// Use receive rate.
    ReceiveRate,
    /// Use both with weights.
    Hybrid,
}

impl Default for BandwidthMethod {
    fn default() -> Self {
        BandwidthMethod::Hybrid
    }
}

/// Network quality indicators.
#[derive(Debug, Clone, Default)]
pub struct NetworkQuality {
    /// Estimated bandwidth in kbps.
    pub bandwidth_kbps: u32,
    /// Round-trip time in milliseconds.
    pub rtt_ms: u32,
    /// Packet loss percentage (0-100).
    pub packet_loss_pct: f64,
    /// Jitter in milliseconds.
    pub jitter_ms: f64,
    /// Is bandwidth estimated as stable.
    pub is_stable: bool,
}

impl NetworkQuality {
    /// Returns the effective bandwidth after accounting for packet loss.
    pub fn effective_bandwidth(&self) -> u32 {
        if self.packet_loss_pct >= 100.0 {
            return 0;
        }
        let loss_factor = 1.0 - self.packet_loss_pct / 100.0;
        ((self.bandwidth_kbps as f64) * loss_factor) as u32
    }
}

/// Bandwidth estimator using a moving window.
pub struct BandwidthEstimator {
    /// Recent throughput samples (bytes per second).
    samples: RwLock<VecDeque<u64>>,
    /// Window size.
    window_size: usize,
    /// Estimated bandwidth in bps.
    estimated_bps: RwLock<u64>,
    /// Last update timestamp.
    last_update: RwLock<Instant>,
    /// Estimation method.
    method: BandwidthMethod,
    /// Smoothing factor (0.0-1.0).
    smoothing: f64,
}

impl BandwidthEstimator {
    /// Creates a new bandwidth estimator.
    pub fn new(window_size: usize) -> Self {
        Self {
            samples: RwLock::new(VecDeque::with_capacity(window_size)),
            window_size,
            estimated_bps: RwLock::new(0),
            last_update: RwLock::new(Instant::now()),
            method: BandwidthMethod::default(),
            smoothing: 0.2,
        }
    }

    /// Records a throughput sample.
    pub fn record(&self, bytes: u64, duration: Duration) {
        if duration.is_zero() {
            return;
        }

        let now = Instant::now();
        let throughput = (bytes as f64 / duration.as_secs_f64()) as u64;

        let mut samples = self.samples.write();

        // Add new sample
        samples.push_back(throughput);

        // Remove old samples
        while samples.len() > self.window_size {
            samples.pop_front();
        }

        // Calculate estimated bandwidth using median (more robust than average)
        let mut sorted: Vec<_> = samples.iter().collect();
        sorted.sort_unstable();

        let median = if sorted.is_empty() {
            0
        } else if sorted.len() % 2 == 0 {
            (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2
        } else {
            *sorted[sorted.len() / 2]
        };

        // Apply smoothing
        let mut estimated = self.estimated_bps.write();
        if *estimated == 0 {
            *estimated = median;
        } else {
            *estimated = ((*estimated as f64) * (1.0 - self.smoothing) + (median as f64) * self.smoothing) as u64;
        }

        *self.last_update.write() = now;
    }

    /// Records throughput using RTT and packet loss.
    pub fn record_transport(&self, bytes: u64, rtt_ms: u32, packet_loss_pct: f64) {
        // Simple model: throughput = bytes / rtt * (1 - loss)
        if rtt_ms == 0 {
            return;
        }

        let throughput = (bytes as f64 / (rtt_ms as f64 / 1000.0) * (1.0 - packet_loss_pct / 100.0)) as u64;

        let mut estimated = self.estimated_bps.write();
        if *estimated == 0 {
            *estimated = throughput;
        } else {
            *estimated = ((*estimated as f64) * (1.0 - self.smoothing) + (throughput as f64) * self.smoothing) as u64;
        }
    }

    /// Returns the estimated bandwidth in kbps.
    pub fn bandwidth_kbps(&self) -> u32 {
        (*self.estimated_bps.read() / 1000) as u32
    }

    /// Returns the estimated bandwidth in bps.
    pub fn bandwidth_bps(&self) -> u64 {
        *self.estimated_bps.read()
    }

    /// Returns true if the bandwidth estimate is stable.
    pub fn is_stable(&self) -> bool {
        let samples = self.samples.read();
        if samples.len() < self.window_size / 2 {
            return false;
        }

        // Calculate coefficient of variation
        let mean: f64 = samples.iter().map(|&x| x as f64).sum::<f64>() / samples.len() as f64;
        let variance: f64 = samples.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / samples.len() as f64;
        let stddev = variance.sqrt();
        let cv = if mean > 0.0 { stddev / mean } else { 0.0 };

        cv < 0.3 // CV < 30% indicates stability
    }

    /// Resets the estimator.
    pub fn reset(&self) {
        self.samples.write().clear();
        *self.estimated_bps.write() = 0;
        *self.last_update.write() = Instant::now();
    }
}

/// Quality level for adaptive streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QualityLevel {
    /// Minimum quality (240p, low bitrate).
    Minimum = 0,
    /// Low quality (360p).
    Low = 1,
    /// Standard quality (480p).
    Standard = 2,
    /// High quality (720p).
    High = 3,
    /// Very high quality (1080p).
    VeryHigh = 4,
    /// Maximum quality (4K if supported).
    Maximum = 5,
}

impl QualityLevel {
    /// Returns the recommended resolution for this level.
    pub fn resolution(&self) -> Resolution {
        match self {
            QualityLevel::Minimum => Resolution::new(320, 240).unwrap(),
            QualityLevel::Low => Resolution::new(640, 360).unwrap(),
            QualityLevel::Standard => Resolution::new(854, 480).unwrap(),
            QualityLevel::High => Resolution::new(1280, 720).unwrap(),
            QualityLevel::VeryHigh => Resolution::new(1920, 1080).unwrap(),
            QualityLevel::Maximum => Resolution::new(3840, 2160).unwrap(),
        }
    }

    /// Returns the recommended bitrate in kbps.
    pub fn bitrate_kbps(&self) -> u32 {
        match self {
            QualityLevel::Minimum => 150,
            QualityLevel::Low => 300,
            QualityLevel::Standard => 750,
            QualityLevel::High => 1500,
            QualityLevel::VeryHigh => 3500,
            QualityLevel::Maximum => 8000,
        }
    }

    /// Returns the recommended framerate.
    pub fn framerate(&self) -> Framerate {
        match self {
            QualityLevel::Minimum => Framerate::new(15).unwrap(),
            QualityLevel::Low => Framerate::new(24).unwrap(),
            QualityLevel::Standard => Framerate::new(30).unwrap(),
            QualityLevel::High => Framerate::new(30).unwrap(),
            QualityLevel::VeryHigh => Framerate::new(60).unwrap(),
            QualityLevel::Maximum => Framerate::new(60).unwrap(),
        }
    }

    /// Returns the next higher quality level.
    pub fn higher(&self) -> Option<QualityLevel> {
        match self {
            QualityLevel::Minimum => Some(QualityLevel::Low),
            QualityLevel::Low => Some(QualityLevel::Standard),
            QualityLevel::Standard => Some(QualityLevel::High),
            QualityLevel::High => Some(QualityLevel::VeryHigh),
            QualityLevel::VeryHigh => Some(QualityLevel::Maximum),
            QualityLevel::Maximum => None,
        }
    }

    /// Returns the next lower quality level.
    pub fn lower(&self) -> Option<QualityLevel> {
        match self {
            QualityLevel::Minimum => None,
            QualityLevel::Low => Some(QualityLevel::Minimum),
            QualityLevel::Standard => Some(QualityLevel::Low),
            QualityLevel::High => Some(QualityLevel::Standard),
            QualityLevel::VeryHigh => Some(QualityLevel::High),
            QualityLevel::Maximum => Some(QualityLevel::VeryHigh),
        }
    }
}

impl Default for QualityLevel {
    fn default() -> Self {
        QualityLevel::Standard
    }
}

/// Adaptive bitrate controller.
pub struct AdaptiveBitrateController {
    /// Current quality level.
    current_level: RwLock<QualityLevel>,
    /// Target quality level (may differ during transitions).
    target_level: RwLock<QualityLevel>,
    /// Bandwidth estimator.
    estimator: Arc<BandwidthEstimator>,
    /// Current configuration.
    config: RwLock<VideoConfig>,
    /// Network quality.
    network_quality: RwLock<NetworkQuality>,
    /// Adjustment history.
    history: RwLock<VecDeque<Adjustment>>,
    /// Time since last adjustment.
    last_adjustment: RwLock<Instant>,
    /// Minimum time between adjustments.
    min_adjustment_interval: Duration,
}

struct Adjustment {
    at: Instant,
    from: QualityLevel,
    to: QualityLevel,
    reason: String,
}

impl AdaptiveBitrateController {
    /// Creates a new adaptive bitrate controller.
    pub fn new(initial_config: VideoConfig) -> Self {
        let level = Self::config_to_level(&initial_config);

        Self {
            current_level: RwLock::new(level),
            target_level: RwLock::new(level),
            estimator: Arc::new(BandwidthEstimator::new(BANDWIDTH_WINDOW_FRAMES)),
            config: RwLock::new(initial_config),
            network_quality: RwLock::new(NetworkQuality::default()),
            history: RwLock::new(VecDeque::with_capacity(100)),
            last_adjustment: RwLock::new(Instant::now()),
            min_adjustment_interval: Duration::from_secs(2),
        }
    }

    /// Updates network quality metrics.
    pub fn update_network(&self, bandwidth_kbps: u32, rtt_ms: u32, packet_loss_pct: f64, jitter_ms: f64) {
        let mut quality = self.network_quality.write();
        quality.bandwidth_kbps = bandwidth_kbps;
        quality.rtt_ms = rtt_ms;
        quality.packet_loss_pct = packet_loss_pct;
        quality.jitter_ms = jitter_ms;
        quality.is_stable = self.estimator.is_stable();
    }

    /// Records throughput for bandwidth estimation.
    pub fn record_throughput(&self, bytes: u64, duration: Duration) {
        self.estimator.record(bytes, duration);
    }

    /// Decides whether to adjust quality based on current conditions.
    pub fn should_adjust(&self) -> bool {
        // Don't adjust too frequently
        let elapsed = self.last_adjustment.read().elapsed();
        if elapsed < self.min_adjustment_interval {
            return false;
        }

        let quality = self.network_quality.read();
        let current = *self.current_level.read();

        // Check if we need to downgrade due to poor conditions
        if quality.packet_loss_pct > 15.0 || quality.jitter_ms > 100.0 {
            return true;
        }

        // Check if we should upgrade
        let effective_bw = quality.effective_bandwidth();
        if let Some(higher) = current.higher() {
            if effective_bw > higher.bitrate_kbps() * 110 / 100 && quality.is_stable {
                return true;
            }
        }

        // Check if we should downgrade
        if effective_bw < current.bitrate_kbps() * 70 / 100 {
            return true;
        }

        false
    }

    /// Calculates the optimal quality level based on current conditions.
    pub fn calculate_optimal_level(&self) -> QualityLevel {
        let quality = self.network_quality.read();
        let effective_bw = quality.effective_bandwidth();

        // Find the highest quality that fits within bandwidth
        for level in [
            QualityLevel::Maximum,
            QualityLevel::VeryHigh,
            QualityLevel::High,
            QualityLevel::Standard,
            QualityLevel::Low,
            QualityLevel::Minimum,
        ] {
            let required = level.bitrate_kbps();
            // Add 20% margin for safety
            if effective_bw >= required * 120 / 100 {
                return level;
            }
        }

        QualityLevel::Minimum
    }

    /// Attempts to adjust to the optimal quality level.
    pub fn adjust(&self) -> Option<(QualityLevel, QualityLevel)> {
        if !self.should_adjust() {
            return None;
        }

        let optimal = self.calculate_optimal_level();
        let current = *self.current_level.read();

        if optimal == current {
            return None;
        }

        let mut target = self.target_level.write();
        *target = optimal;

        let mut current_mut = self.current_level.write();

        // Transition to optimal level
        let from = *current_mut;
        *current_mut = optimal;
        *self.last_adjustment.write() = Instant::now();

        // Record adjustment
        let net_quality = self.network_quality.read().clone();
        let reason = if optimal > from {
            "bandwidth increase".to_string()
        } else {
            format!(
                "low bandwidth ({} kbps) or poor conditions (loss={:.1}%, jitter={:.0}ms)",
                net_quality.effective_bandwidth(),
                net_quality.packet_loss_pct,
                net_quality.jitter_ms
            )
        };

        self.history.write().push_back(Adjustment {
            at: Instant::now(),
            from,
            to: optimal,
            reason,
        });

        // Keep only recent history
        let mut history = self.history.write();
        while history.len() > 100 {
            history.pop_front();
        }

        if optimal != from {
            Some((from, optimal))
        } else {
            None
        }
    }

    /// Returns the current quality level.
    pub fn current_level(&self) -> QualityLevel {
        *self.current_level.read()
    }

    /// Returns the current video configuration.
    pub fn config(&self) -> VideoConfig {
        self.config.read().clone()
    }

    /// Returns the recommended video configuration for the current level.
    pub fn recommended_config(&self) -> VideoConfig {
        let level = *self.current_level.read();
        let mut config = self.config.read().clone();

        config.resolution = level.resolution();
        config.bitrate_kbps = level.bitrate_kbps();
        config.framerate = level.framerate();

        config
    }

    /// Returns network quality metrics.
    pub fn network_quality(&self) -> NetworkQuality {
        self.network_quality.read().clone()
    }

    /// Returns the bandwidth estimate.
    pub fn bandwidth_kbps(&self) -> u32 {
        self.estimator.bandwidth_kbps()
    }

    /// Returns adjustment history.
    pub fn adjustment_history(&self) -> Vec<(Instant, QualityLevel, QualityLevel, String)> {
        self.history
            .read()
            .iter()
            .map(|a| (a.at, a.from, a.to, a.reason.clone()))
            .collect()
    }

    fn config_to_level(config: &VideoConfig) -> QualityLevel {
        let pixels = config.resolution.pixels();
        if pixels <= 320 * 240 {
            QualityLevel::Minimum
        } else if pixels <= 640 * 360 {
            QualityLevel::Low
        } else if pixels <= 854 * 480 {
            QualityLevel::Standard
        } else if pixels <= 1280 * 720 {
            QualityLevel::High
        } else if pixels <= 1920 * 1080 {
            QualityLevel::VeryHigh
        } else {
            QualityLevel::Maximum
        }
    }
}

/// Trait for components that can receive quality changes.
pub trait QualityChangeListener: Send + Sync {
    /// Called when quality level changes.
    fn on_quality_change(&self, from: QualityLevel, to: QualityLevel);
}

/// No-op listener for testing.
pub struct NoOpListener;

impl QualityChangeListener for NoOpListener {
    fn on_quality_change(&self, _from: QualityLevel, _to: QualityLevel) {}
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quality_level_ordering() {
        assert!(QualityLevel::Maximum > QualityLevel::VeryHigh);
        assert!(QualityLevel::VeryHigh > QualityLevel::High);
        assert!(QualityLevel::High > QualityLevel::Standard);
        assert!(QualityLevel::Standard > QualityLevel::Low);
        assert!(QualityLevel::Low > QualityLevel::Minimum);
    }

    #[test]
    fn test_quality_level_transitions() {
        assert_eq!(QualityLevel::Minimum.higher(), Some(QualityLevel::Low));
        assert_eq!(QualityLevel::Low.higher(), Some(QualityLevel::Standard));
        assert_eq!(QualityLevel::Maximum.higher(), None);

        assert_eq!(QualityLevel::Maximum.lower(), Some(QualityLevel::VeryHigh));
        assert_eq!(QualityLevel::Minimum.lower(), None);
    }

    #[test]
    fn test_bandwidth_estimator() {
        let estimator = BandwidthEstimator::new(10);

        // Record some samples
        estimator.record(100_000, Duration::from_secs(1)); // 100 KB/s
        estimator.record(200_000, Duration::from_secs(1)); // 200 KB/s
        estimator.record(150_000, Duration::from_secs(1)); // 150 KB/s

        let bps = estimator.bandwidth_bps();
        assert!(bps > 0);
        assert!(estimator.bandwidth_kbps() > 0);
    }

    #[test]
    fn test_adaptive_controller_initialization() {
        let config = VideoConfig::from_quality(VideoQuality::Standard, crate::codec::VideoCodec::H264).unwrap();
        let controller = AdaptiveBitrateController::new(config);

        assert_eq!(controller.current_level(), QualityLevel::Standard);
    }

    #[test]
    fn test_network_quality_effective_bandwidth() {
        let mut quality = NetworkQuality::default();
        quality.bandwidth_kbps = 1000;
        quality.packet_loss_pct = 10.0;

        assert_eq!(quality.effective_bandwidth(), 900);
    }
}
