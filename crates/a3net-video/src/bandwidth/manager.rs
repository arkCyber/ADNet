//! Automatic bandwidth management for video streaming.
//!
//! This module provides intelligent bandwidth management that:
//! - Continuously monitors network conditions
//! - Automatically adjusts video quality based on available bandwidth
//! - Provides smooth quality transitions
//! - Prevents rapid quality oscillation
//! - Maintains video smoothness during bandwidth changes

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::adaptive::{
    AdaptiveBitrateController, BandwidthEstimator, NetworkQuality, QualityLevel,
};
use crate::config::{Framerate, Resolution, VideoConfig, VideoQuality};
use crate::error::VideoError;
use crate::resilience::HealthMonitor;

/// Network state based on latency and packet loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkState {
    /// Network is in excellent condition.
    Excellent,
    /// Network is in good condition.
    Good,
    /// Network is experiencing minor congestion.
    Congested,
    /// Network is heavily congested.
    Degraded,
    /// Network is severely degraded.
    Critical,
}

impl NetworkState {
    /// Returns the recommended bitrate multiplier for this state.
    pub fn bitrate_multiplier(&self) -> f64 {
        match self {
            NetworkState::Excellent => 1.0,
            NetworkState::Good => 0.9,
            NetworkState::Congested => 0.75,
            NetworkState::Degraded => 0.5,
            NetworkState::Critical => 0.3,
        }
    }

    /// Returns whether to prioritize stability over quality.
    pub fn prioritize_stability(&self) -> bool {
        matches!(
            self,
            NetworkState::Congested | NetworkState::Degraded | NetworkState::Critical
        )
    }
}

impl Default for NetworkState {
    fn default() -> Self {
        NetworkState::Good
    }
}

/// Statistics for bandwidth management.
#[derive(Debug, Clone, Default)]
pub struct BandwidthStats {
    /// Estimated bandwidth in kbps.
    pub bandwidth_kbps: u32,
    /// Effective bandwidth after loss.
    pub effective_bandwidth_kbps: u32,
    /// Network state.
    pub network_state: NetworkState,
    /// Current quality level.
    pub quality_level: QualityLevel,
    /// Recommended quality level.
    pub recommended_level: QualityLevel,
    /// Whether adjustment is needed.
    pub needs_adjustment: bool,
    /// Time since last adjustment.
    pub time_since_adjustment: Duration,
    /// Number of adjustments in the last minute.
    pub adjustments_per_minute: f64,
    /// Estimated FPS based on current settings.
    pub estimated_fps: f64,
}

/// Configuration for automatic bandwidth management.
#[derive(Debug, Clone)]
pub struct BandwidthManagerConfig {
    /// Minimum time between quality adjustments.
    pub min_adjustment_interval: Duration,
    /// Maximum adjustments per minute (to prevent oscillation).
    pub max_adjustments_per_minute: f64,
    /// Bandwidth estimation window size.
    pub estimation_window_frames: usize,
    /// Enable aggressive downgrading on congestion.
    pub aggressive_downgrade: bool,
    /// Conservative upgrade threshold (bandwidth must be this much higher).
    pub upgrade_threshold_multiplier: f64,
    /// Conservative downgrade threshold.
    pub downgrade_threshold_multiplier: f64,
}

impl Default for BandwidthManagerConfig {
    fn default() -> Self {
        Self {
            min_adjustment_interval: Duration::from_secs(2),
            max_adjustments_per_minute: 3.0,
            estimation_window_frames: 30,
            aggressive_downgrade: true,
            upgrade_threshold_multiplier: 1.15, // 15% headroom for upgrades
            downgrade_threshold_multiplier: 0.85, // 15% margin for downgrades
        }
    }
}

/// Automatic bandwidth manager for video streaming.
///
/// This manager continuously monitors network conditions and automatically
/// adjusts video quality to maintain smooth streaming while maximizing
/// quality within available bandwidth.
pub struct BandwidthManager {
    /// Configuration.
    config: BandwidthManagerConfig,
    /// Adaptive bitrate controller.
    controller: Arc<AdaptiveBitrateController>,
    /// Health monitor for tracking performance.
    health_monitor: Arc<HealthMonitor>,
    /// Network state history.
    state_history: RwLock<VecDeque<(Instant, NetworkState)>>,
    /// Recent quality adjustments.
    recent_adjustments: RwLock<VecDeque<Instant>>,
    /// Last quality level.
    last_level: RwLock<QualityLevel>,
    /// Startup time.
    startup_time: Instant,
    /// Target bitrate in kbps.
    target_bitrate_kbps: RwLock<u32>,
    /// Current frame rate.
    current_fps: RwLock<u32>,
}

impl BandwidthManager {
    /// Creates a new bandwidth manager.
    pub fn new(config: VideoConfig) -> Self {
        let manager_config = BandwidthManagerConfig::default();

        Self {
            config: manager_config,
            controller: Arc::new(AdaptiveBitrateController::new(config.clone())),
            health_monitor: Arc::new(HealthMonitor::new()),
            state_history: RwLock::new(VecDeque::with_capacity(60)),
            recent_adjustments: RwLock::new(VecDeque::new()),
            last_level: RwLock::new(QualityLevel::Standard),
            startup_time: Instant::now(),
            target_bitrate_kbps: RwLock::new(config.bitrate_kbps),
            current_fps: RwLock::new(30),
        }
    }

    /// Creates a bandwidth manager with custom configuration.
    pub fn with_config(config: VideoConfig, manager_config: BandwidthManagerConfig) -> Self {
        Self {
            config: manager_config,
            controller: Arc::new(AdaptiveBitrateController::new(config.clone())),
            health_monitor: Arc::new(HealthMonitor::new()),
            state_history: RwLock::new(VecDeque::with_capacity(60)),
            recent_adjustments: RwLock::new(VecDeque::new()),
            last_level: RwLock::new(QualityLevel::Standard),
            startup_time: Instant::now(),
            target_bitrate_kbps: RwLock::new(config.bitrate_kbps),
            current_fps: RwLock::new(30),
        }
    }

    /// Records a frame send event.
    pub fn on_frame_sent(&self, frame_size_bytes: u64, duration_ms: u64) {
        let duration = Duration::from_millis(duration_ms);
        self.controller.record_throughput(frame_size_bytes, duration);
        self.health_monitor.record_success(duration_ms as f64);
    }

    /// Records a frame receive event.
    pub fn on_frame_received(&self, frame_size_bytes: u64) {
        // Update throughput tracking
        let _ = frame_size_bytes;
    }

    /// Records a frame drop event.
    pub fn on_frame_dropped(&self) {
        self.health_monitor.record_failure();
    }

    /// Records an encode timeout.
    pub fn on_encode_timeout(&self) {
        self.health_monitor.record_timeout();
    }

    /// Records a decode timeout.
    pub fn on_decode_timeout(&self) {
        self.health_monitor.record_timeout();
    }

    /// Updates network metrics from the transport layer.
    ///
    /// This should be called periodically with the latest RTT and packet loss
    /// measurements from the network transport.
    pub fn update_network_metrics(
        &self,
        rtt_ms: u32,
        packet_loss_pct: f64,
        jitter_ms: f64,
    ) {
        let bandwidth_kbps = self.controller.bandwidth_kbps();

        // Update the adaptive controller
        self.controller.update_network(bandwidth_kbps, rtt_ms, packet_loss_pct, jitter_ms);

        // Record network state
        let state = self.assess_network_state(rtt_ms, packet_loss_pct, jitter_ms);
        self.state_history.write().push_back((Instant::now(), state));

        // Keep history bounded
        let mut history = self.state_history.write();
        while history.len() > 60 {
            history.pop_front();
        }
    }

    /// Assesses the current network state based on metrics.
    fn assess_network_state(
        &self,
        rtt_ms: u32,
        packet_loss_pct: f64,
        jitter_ms: f64,
    ) -> NetworkState {
        // Critical conditions
        if packet_loss_pct >= 20.0 || rtt_ms >= 500 || jitter_ms >= 200.0 {
            return NetworkState::Critical;
        }

        // Degraded conditions
        if packet_loss_pct >= 10.0 || rtt_ms >= 300 || jitter_ms >= 100.0 {
            return NetworkState::Degraded;
        }

        // Congested conditions
        if packet_loss_pct >= 3.0 || rtt_ms >= 150 || jitter_ms >= 50.0 {
            return NetworkState::Congested;
        }

        // Good conditions
        if packet_loss_pct >= 0.5 || rtt_ms >= 50 || jitter_ms >= 20.0 {
            return NetworkState::Good;
        }

        NetworkState::Excellent
    }

    /// Checks if we should perform a quality adjustment.
    ///
    /// Returns `true` if a quality change is recommended based on current
    /// network conditions.
    pub fn should_adjust(&self) -> bool {
        // Don't adjust too frequently
        if !self.has_enough_time_since_adjustment() {
            return false;
        }

        // Check if we're adjusting too frequently
        if self.too_many_recent_adjustments() {
            return false;
        }

        // Check if the controller thinks we should adjust
        self.controller.should_adjust()
    }

    /// Checks if enough time has passed since the last adjustment.
    fn has_enough_time_since_adjustment(&self) -> bool {
        let recent = self.recent_adjustments.read();

        if let Some(last) = recent.back() {
            last.elapsed() >= self.config.min_adjustment_interval
        } else {
            true // No previous adjustment
        }
    }

    /// Checks if there have been too many recent adjustments.
    fn too_many_recent_adjustments(&self) -> bool {
        let recent = self.recent_adjustments.read();
        let now = Instant::now();

        // Count adjustments in the last minute
        let recent_count = recent
            .iter()
            .filter(|&&t| now.duration_since(t) < Duration::from_secs(60))
            .count();

        recent_count as f64 >= self.config.max_adjustments_per_minute
    }

    /// Performs a quality adjustment if needed.
    ///
    /// Returns the old and new quality levels if an adjustment was made.
    pub fn adjust(&self) -> Option<(QualityLevel, QualityLevel)> {
        if !self.should_adjust() {
            return None;
        }

        let current = *self.last_level.read();
        let target = self.controller.calculate_optimal_level();

        if current == target {
            return None;
        }

        // Record the adjustment
        self.recent_adjustments.write().push_back(Instant::now());
        *self.last_level.write() = target;

        // Update target bitrate
        *self.target_bitrate_kbps.write() = target.bitrate_kbps();
        *self.current_fps.write() = target.framerate().fps;

        // Record in controller
        self.controller.adjust();

        Some((current, target))
    }

    /// Gets the recommended video configuration for current conditions.
    ///
    /// This returns a configuration that can be applied to the video encoder
    /// to achieve the optimal balance of quality and bandwidth usage.
    pub fn recommended_config(&self) -> VideoConfig {
        let mut config = self.controller.recommended_config();

        // Apply network state adjustments
        let state = self.current_network_state();
        let multiplier = state.bitrate_multiplier();

        // Scale bitrate based on network state
        let adjusted_bitrate = ((config.bitrate_kbps as f64) * multiplier) as u32;
        config.bitrate_kbps = adjusted_bitrate.max(50); // Minimum 50 kbps

        // Adjust framerate for stability in degraded conditions
        if state.prioritize_stability() {
            let current_fps = *self.current_fps.read();
            if current_fps > 15 {
                *self.current_fps.write() = current_fps.saturating_sub(5);
            }
            if let Ok(fps) = Framerate::new(*self.current_fps.read()) {
                config.framerate = fps;
            }
        }

        config
    }

    /// Gets the recommended quality level for current conditions.
    pub fn recommended_quality(&self) -> QualityLevel {
        let target = self.controller.calculate_optimal_level();
        let state = self.current_network_state();

        // In critical conditions, always downgrade
        if state == NetworkState::Critical {
            return QualityLevel::Minimum;
        }

        // In degraded conditions, be conservative
        if state.prioritize_stability() && !self.config.aggressive_downgrade {
            let current = *self.last_level.read();
            if target > current {
                return current; // Don't upgrade in degraded conditions
            }
        }

        target
    }

    /// Gets the current network state.
    pub fn current_network_state(&self) -> NetworkState {
        let history = self.state_history.read();
        history.back().map(|(_, s)| *s).unwrap_or_default()
    }

    /// Gets bandwidth statistics.
    pub fn stats(&self) -> BandwidthStats {
        let bandwidth_kbps = self.controller.bandwidth_kbps();
        let network = self.controller.network_quality();
        let recommended = self.recommended_quality();
        let current = *self.last_level.read();

        // Calculate adjustments per minute
        let recent = self.recent_adjustments.read();
        let now = Instant::now();
        let adjustments_last_minute = recent
            .iter()
            .filter(|&&t| now.duration_since(t) < Duration::from_secs(60))
            .count();

        // Calculate time since last adjustment
        let time_since_adjustment = recent
            .back()
            .map(|t| t.elapsed())
            .unwrap_or(self.startup_time.elapsed());

        BandwidthStats {
            bandwidth_kbps,
            effective_bandwidth_kbps: network.effective_bandwidth(),
            network_state: self.current_network_state(),
            quality_level: current,
            recommended_level: recommended,
            needs_adjustment: self.should_adjust(),
            time_since_adjustment,
            adjustments_per_minute: adjustments_last_minute as f64,
            estimated_fps: *self.current_fps.read() as f64,
        }
    }

    /// Gets the target bitrate in kbps.
    pub fn target_bitrate(&self) -> u32 {
        *self.target_bitrate_kbps.read()
    }

    /// Gets the current FPS setting.
    pub fn current_fps(&self) -> u32 {
        *self.current_fps.read()
    }

    /// Forces a specific quality level (bypasses automatic adjustment).
    ///
    /// This is useful when the application knows better than the automatic
    /// manager (e.g., user manually selecting quality).
    pub fn force_quality_level(&self, level: QualityLevel) {
        *self.last_level.write() = level;
        *self.target_bitrate_kbps.write() = level.bitrate_kbps();
        *self.current_fps.write() = level.framerate().fps;
        self.recent_adjustments.write().push_back(Instant::now());
    }

    /// Resets the bandwidth manager to initial state.
    pub fn reset(&self) {
        self.controller.adjust(); // Triggers a reset of adjustment state
        self.state_history.write().clear();
        self.recent_adjustments.write().clear();
        *self.last_level.write() = QualityLevel::Standard;
        *self.target_bitrate_kbps.write() = 750;
        *self.current_fps.write() = 30;
    }

    /// Gets the underlying adaptive controller.
    pub fn controller(&self) -> &Arc<AdaptiveBitrateController> {
        &self.controller
    }

    /// Gets the health monitor.
    pub fn health_monitor(&self) -> &Arc<HealthMonitor> {
        &self.health_monitor
    }
}

/// Automatic bandwidth manager runner for continuous monitoring.
///
/// This helper runs the bandwidth manager in the background,
/// automatically adjusting quality based on network conditions.
pub struct BandwidthManagerRunner {
    manager: Arc<BandwidthManager>,
    running: RwLock<bool>,
}

impl BandwidthManagerRunner {
    /// Creates a new runner.
    pub fn new(manager: Arc<BandwidthManager>) -> Self {
        Self {
            manager,
            running: RwLock::new(false),
        }
    }

    /// Starts the background monitoring task.
    ///
    /// This should be called once and allowed to run in the background.
    /// The task will periodically check network conditions and adjust
    /// video quality as needed.
    pub fn start(&self) {
        *self.running.write() = true;
        // Note: In a real implementation, this would spawn a background task
        // For now, we provide a method to be called from the main loop
    }

    /// Stops the background monitoring task.
    pub fn stop(&self) {
        *self.running.write() = false;
    }

    /// Returns whether the runner is active.
    pub fn is_running(&self) -> bool {
        *self.running.read()
    }

    /// Runs a single iteration of the monitoring loop.
    ///
    /// Call this periodically (e.g., every second) to check and adjust
    /// video quality.
    pub fn tick(&self) -> Option<(QualityLevel, QualityLevel)> {
        if !*self.running.read() {
            return None;
        }

        self.manager.adjust()
    }

    /// Gets the current stats.
    pub fn stats(&self) -> BandwidthStats {
        self.manager.stats()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_state_from_metrics() {
        let config = VideoConfig::default();
        let manager = BandwidthManager::new(config);

        // Test excellent conditions
        manager.update_network_metrics(30, 0.1, 5.0);
        assert_eq!(manager.current_network_state(), NetworkState::Excellent);

        // Test good conditions
        manager.update_network_metrics(60, 0.5, 15.0);
        assert_eq!(manager.current_network_state(), NetworkState::Good);

        // Test congested conditions
        manager.update_network_metrics(200, 5.0, 60.0);
        assert_eq!(manager.current_network_state(), NetworkState::Congested);

        // Test degraded conditions
        manager.update_network_metrics(400, 15.0, 150.0);
        assert_eq!(manager.current_network_state(), NetworkState::Degraded);

        // Test critical conditions
        manager.update_network_metrics(600, 30.0, 250.0);
        assert_eq!(manager.current_network_state(), NetworkState::Critical);
    }

    #[test]
    fn test_network_state_multiplier() {
        assert_eq!(NetworkState::Excellent.bitrate_multiplier(), 1.0);
        assert_eq!(NetworkState::Good.bitrate_multiplier(), 0.9);
        assert_eq!(NetworkState::Congested.bitrate_multiplier(), 0.75);
        assert_eq!(NetworkState::Degraded.bitrate_multiplier(), 0.5);
        assert_eq!(NetworkState::Critical.bitrate_multiplier(), 0.3);
    }

    #[test]
    fn test_stability_prioritization() {
        assert!(!NetworkState::Excellent.prioritize_stability());
        assert!(!NetworkState::Good.prioritize_stability());
        assert!(NetworkState::Congested.prioritize_stability());
        assert!(NetworkState::Degraded.prioritize_stability());
        assert!(NetworkState::Critical.prioritize_stability());
    }

    #[test]
    fn test_bandwidth_stats() {
        let config = VideoConfig::default();
        let manager = BandwidthManager::new(config);

        let stats = manager.stats();
        assert_eq!(stats.bandwidth_kbps, 0); // Initial state
        assert_eq!(stats.quality_level, QualityLevel::Standard);
    }

    #[test]
    fn test_force_quality_level() {
        let config = VideoConfig::default();
        let manager = BandwidthManager::new(config);

        manager.force_quality_level(QualityLevel::High);
        assert_eq!(manager.stats().quality_level, QualityLevel::High);
        assert_eq!(manager.target_bitrate(), 1500);
    }

    #[test]
    fn test_recommended_config() {
        let config = VideoConfig::default();
        let manager = BandwidthManager::new(config);

        // In excellent conditions, should get full quality
        manager.update_network_metrics(30, 0.1, 5.0);
        let recommended = manager.recommended_config();
        assert!(recommended.bitrate_kbps >= 100);
    }

    #[test]
    fn test_frame_event_recording() {
        let config = VideoConfig::default();
        let manager = BandwidthManager::new(config);

        // Record frame events
        manager.on_frame_sent(1000, 33);
        manager.on_frame_dropped();

        // Check health (on_frame_received doesn't count as operation)
        let health = manager.health_monitor().metrics();
        assert_eq!(health.operations_attempted, 2);
    }

    #[test]
    fn test_runner_lifecycle() {
        let config = VideoConfig::default();
        let manager = Arc::new(BandwidthManager::new(config));
        let runner = BandwidthManagerRunner::new(manager.clone());

        assert!(!runner.is_running());

        runner.start();
        assert!(runner.is_running());

        runner.stop();
        assert!(!runner.is_running());
    }

    #[test]
    fn test_reset() {
        let config = VideoConfig::default();
        let manager = BandwidthManager::new(config);

        manager.force_quality_level(QualityLevel::Maximum);
        manager.reset();

        let stats = manager.stats();
        assert_eq!(stats.quality_level, QualityLevel::Standard);
        assert_eq!(manager.target_bitrate(), 750);
    }

    #[test]
    fn test_adjustment_throttling() {
        let config = VideoConfig::default();
        let manager = BandwidthManager::new(config);

        // First adjustment should work
        manager.update_network_metrics(30, 0.1, 5.0);
        let result = manager.adjust();
        // May or may not adjust depending on initial state

        // Immediate second adjustment should be throttled
        assert!(!manager.should_adjust());
    }
}
