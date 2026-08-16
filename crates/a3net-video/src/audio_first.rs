//! Audio-First bandwidth management for prioritized media delivery.
//!
//! Ensures audio quality is maintained even under poor network conditions
//! by implementing bandwidth allocation policies that favor audio over video.

use crate::adaptive::{AdaptiveBitrateController, QualityLevel};
use crate::config::VideoConfig;
use crate::error::{VideoError, VideoResult};

/// Bandwidth allocation policy when network is degraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandwidthPolicy {
    /// Equal priority for audio and video.
    Balanced,
    /// Audio takes priority, video reduced first.
    AudioFirst,
    /// Video quality maintained at cost of audio.
    VideoFirst,
    /// Only audio, video disabled.
    AudioOnly,
}

impl Default for BandwidthPolicy {
    fn default() -> Self {
        BandwidthPolicy::AudioFirst
    }
}

/// Audio bandwidth requirements per codec.
#[derive(Debug, Clone, Copy)]
pub struct AudioRequirements {
    /// Minimum bitrate in kbps.
    pub min_bitrate_kbps: u32,
    /// Target bitrate in kbps.
    pub target_bitrate_kbps: u32,
    /// Maximum bitrate in kbps.
    pub max_bitrate_kbps: u32,
    /// Whether echo cancellation is required.
    pub echo_cancellation: bool,
    /// Whether noise suppression is required.
    pub noise_suppression: bool,
}

impl AudioRequirements {
    /// Opus codec requirements (typical VoIP).
    pub fn opus() -> Self {
        Self {
            min_bitrate_kbps: 6,
            target_bitrate_kbps: 40,
            max_bitrate_kbps: 128,
            echo_cancellation: true,
            noise_suppression: true,
        }
    }

    /// AAC-LC codec requirements.
    pub fn aaclc() -> Self {
        Self {
            min_bitrate_kbps: 32,
            target_bitrate_kbps: 128,
            max_bitrate_kbps: 256,
            echo_cancellation: false,
            noise_suppression: false,
        }
    }

    /// G.711 codec requirements (legacy).
    pub fn g711() -> Self {
        Self {
            min_bitrate_kbps: 64,
            target_bitrate_kbps: 64,
            max_bitrate_kbps: 64,
            echo_cancellation: true,
            noise_suppression: false,
        }
    }
}

impl Default for AudioRequirements {
    fn default() -> Self {
        Self::opus()
    }
}

/// Video bandwidth requirements based on quality level.
#[derive(Debug, Clone, Copy)]
pub struct VideoRequirements {
    /// Minimum bitrate in kbps for this quality.
    pub min_bitrate_kbps: u32,
    /// Target bitrate in kbps for this quality.
    pub target_bitrate_kbps: u32,
    /// Associated quality level.
    pub quality_level: QualityLevel,
}

impl VideoRequirements {
    /// Returns video requirements for each quality level.
    pub fn for_level(level: QualityLevel) -> Self {
        Self {
            min_bitrate_kbps: level.bitrate_kbps() * 50 / 100, // 50% of target
            target_bitrate_kbps: level.bitrate_kbps(),
            quality_level: level,
        }
    }

    /// Minimum video requirements (240p).
    pub fn minimum() -> Self {
        Self::for_level(QualityLevel::Minimum)
    }

    /// Returns true if this is the minimum acceptable quality.
    pub fn is_minimum(&self) -> bool {
        self.quality_level == QualityLevel::Minimum
    }
}

/// Audio-first bandwidth allocation manager.
pub struct AudioFirstManager {
    /// Current bandwidth policy.
    policy: BandwidthPolicy,
    /// Audio requirements.
    audio_req: AudioRequirements,
    /// Reserved audio bandwidth in kbps.
    reserved_audio_kbps: RwLock<u32>,
    /// Whether video is currently enabled.
    video_enabled: RwLock<bool>,
    /// Network quality threshold for audio-first mode.
    audio_first_threshold: f64, // packet loss percentage
}

use parking_lot::RwLock;

impl AudioFirstManager {
    /// Creates a new audio-first manager.
    pub fn new() -> Self {
        Self {
            policy: BandwidthPolicy::AudioFirst,
            audio_req: AudioRequirements::default(),
            reserved_audio_kbps: RwLock::new(40), // Reserve 40kbps for audio
            video_enabled: RwLock::new(true),
            audio_first_threshold: 5.0, // Activate at 5% packet loss
        }
    }

    /// Creates with custom audio requirements.
    pub fn with_audio_requirements(req: AudioRequirements) -> Self {
        let reserved = req.target_bitrate_kbps * 120 / 100; // 120% of target
        Self {
            policy: BandwidthPolicy::AudioFirst,
            audio_req: req,
            reserved_audio_kbps: RwLock::new(reserved),
            video_enabled: RwLock::new(true),
            audio_first_threshold: 5.0,
        }
    }

    /// Returns the current policy.
    pub fn policy(&self) -> BandwidthPolicy {
        self.policy
    }

    /// Sets the bandwidth policy.
    pub fn set_policy(&mut self, policy: BandwidthPolicy) {
        self.policy = policy;
    }

    /// Updates policy based on network conditions.
    pub fn update_for_network(&mut self, packet_loss_pct: f64, bandwidth_kbps: u32) {
        match self.policy {
            BandwidthPolicy::Balanced => {
                // Keep balanced
            }
            BandwidthPolicy::AudioFirst => {
                if packet_loss_pct >= self.audio_first_threshold {
                    // Increase audio priority
                    if bandwidth_kbps < self.audio_req.target_bitrate_kbps {
                        self.policy = BandwidthPolicy::AudioOnly;
                        *self.video_enabled.write() = false;
                    } else {
                        self.policy = BandwidthPolicy::AudioFirst;
                        *self.video_enabled.write() = true;
                    }
                } else {
                    *self.video_enabled.write() = true;
                }
            }
            BandwidthPolicy::VideoFirst => {
                if packet_loss_pct >= self.audio_first_threshold {
                    self.policy = BandwidthPolicy::AudioFirst;
                }
            }
            BandwidthPolicy::AudioOnly => {
                if packet_loss_pct < self.audio_first_threshold / 2.0
                    && bandwidth_kbps > self.audio_req.max_bitrate_kbps * 2
                {
                    self.policy = BandwidthPolicy::AudioFirst;
                    *self.video_enabled.write() = true;
                }
            }
        }
    }

    /// Returns true if video is enabled.
    pub fn is_video_enabled(&self) -> bool {
        *self.video_enabled.read()
    }

    /// Returns the reserved bandwidth for audio.
    pub fn reserved_audio_kbps(&self) -> u32 {
        *self.reserved_audio_kbps.read()
    }

    /// Calculates available bandwidth for video.
    pub fn available_video_bandwidth(&self, total_kbps: u32) -> u32 {
        let audio_reserved = *self.reserved_audio_kbps.read();

        if total_kbps <= audio_reserved {
            return 0; // All bandwidth reserved for audio
        }

        let available = total_kbps - audio_reserved;

        // Apply policy-based allocation
        match self.policy {
            BandwidthPolicy::AudioOnly => 0,
            BandwidthPolicy::AudioFirst => available * 80 / 100, // 80% of available to video
            BandwidthPolicy::Balanced => available * 70 / 100,
            BandwidthPolicy::VideoFirst => available * 90 / 100,
        }
    }

    /// Calculates the recommended video quality level for available bandwidth.
    pub fn recommended_video_quality(&self, available_kbps: u32) -> QualityLevel {
        if available_kbps == 0 {
            return QualityLevel::Minimum; // Video disabled, return minimum
        }

        // Find the highest quality that fits
        for level in [
            QualityLevel::Maximum,
            QualityLevel::VeryHigh,
            QualityLevel::High,
            QualityLevel::Standard,
            QualityLevel::Low,
            QualityLevel::Minimum,
        ] {
            let req = VideoRequirements::for_level(level);
            if available_kbps >= req.min_bitrate_kbps {
                return level;
            }
        }

        QualityLevel::Minimum
    }

    /// Returns a recommended video configuration based on current conditions.
    pub fn recommended_video_config(
        &self,
        total_bandwidth_kbps: u32,
        current_config: &VideoConfig,
    ) -> VideoConfig {
        let video_bw = self.available_video_bandwidth(total_bandwidth_kbps);
        let quality = self.recommended_video_quality(video_bw);

        let mut config = current_config.clone();

        if self.is_video_enabled() {
            config.resolution = quality.resolution();
            config.bitrate_kbps = quality.bitrate_kbps().min(video_bw);
            config.framerate = quality.framerate();
        } else {
            // Video disabled
            config.bitrate_kbps = 0;
        }

        config
    }

    /// Gets the audio requirements.
    pub fn audio_requirements(&self) -> AudioRequirements {
        self.audio_req
    }
}

impl Default for AudioFirstManager {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Frame Priority
// ============================================================================

/// Media frame type with priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MediaPriority {
    /// Critical - audio frames, always delivered.
    Critical = 0,
    /// High - video keyframes.
    High = 1,
    /// Normal - video delta frames.
    Normal = 2,
    /// Low - non-essential data.
    Low = 3,
}

/// Frame with priority metadata.
#[derive(Debug, Clone)]
pub struct PriorityFrame<T> {
    /// The frame data.
    pub data: T,
    /// Priority level.
    pub priority: MediaPriority,
    /// Whether this frame can be dropped.
    pub droppable: bool,
    /// Timestamp in nanoseconds.
    pub timestamp_ns: u64,
}

impl<T> PriorityFrame<T> {
    /// Creates a new priority frame.
    pub fn new(data: T, priority: MediaPriority, droppable: bool, timestamp_ns: u64) -> Self {
        Self {
            data,
            priority,
            droppable,
            timestamp_ns,
        }
    }

    /// Creates a critical (audio) frame.
    pub fn audio(data: T, timestamp_ns: u64) -> Self {
        Self {
            data,
            priority: MediaPriority::Critical,
            droppable: false,
            timestamp_ns,
        }
    }

    /// Creates a high priority (keyframe) frame.
    pub fn keyframe(data: T, timestamp_ns: u64) -> Self {
        Self {
            data,
            priority: MediaPriority::High,
            droppable: false,
            timestamp_ns,
        }
    }

    /// Creates a normal priority (delta) frame.
    pub fn delta(data: T, timestamp_ns: u64) -> Self {
        Self {
            data,
            priority: MediaPriority::Normal,
            droppable: true,
            timestamp_ns,
        }
    }

    /// Returns true if this frame can be dropped under pressure.
    pub fn can_drop(&self) -> bool {
        self.droppable
    }

    /// Returns true if this is a critical (non-droppable) frame.
    pub fn is_critical(&self) -> bool {
        self.priority == MediaPriority::Critical
    }
}

/// Priority queue for media frames.
pub struct PriorityQueue<T> {
    /// Maximum queue size.
    max_size: usize,
    /// Current frames by priority.
    critical: RwLock<Vec<PriorityFrame<T>>>,
    high: RwLock<Vec<PriorityFrame<T>>>,
    normal: RwLock<Vec<PriorityFrame<T>>>,
    low: RwLock<Vec<PriorityFrame<T>>>,
}

impl<T> PriorityQueue<T> {
    /// Creates a new priority queue.
    pub fn new(max_size: usize) -> Self {
        Self {
            max_size,
            critical: RwLock::new(Vec::new()),
            high: RwLock::new(Vec::new()),
            normal: RwLock::new(Vec::new()),
            low: RwLock::new(Vec::new()),
        }
    }

    /// Pushes a frame into the queue.
    pub fn push(&self, frame: PriorityFrame<T>) {
        // Critical frames always inserted
        if frame.priority == MediaPriority::Critical {
            self.critical.write().push(frame);
            return;
        }

        // Check total size
        let total = self.total_len();
        if total >= self.max_size {
            // Drop lowest priority frames first
            if !self.try_drop_lowest() {
                // Can't drop, don't add
                return;
            }
        }

        match frame.priority {
            MediaPriority::Critical => self.critical.write().push(frame),
            MediaPriority::High => self.high.write().push(frame),
            MediaPriority::Normal => self.normal.write().push(frame),
            MediaPriority::Low => self.low.write().push(frame),
        }
    }

    /// Pops the highest priority frame.
    pub fn pop(&mut self) -> Option<PriorityFrame<T>> {
        // Always prefer critical
        if let Some(frame) = self.critical.write().pop() {
            return Some(frame);
        }
        if let Some(frame) = self.high.write().pop() {
            return Some(frame);
        }
        if let Some(frame) = self.normal.write().pop() {
            return Some(frame);
        }
        self.low.write().pop()
    }

    /// Tries to drop the lowest priority frames to make room.
    fn try_drop_lowest(&self) -> bool {
        // Try to drop low priority
        if !self.low.read().is_empty() {
            self.low.write().pop();
            return true;
        }

        // Try to drop normal priority droppable frames
        {
            let mut normal = self.normal.write();
            if let Some(pos) = normal.iter().position(|f| f.droppable) {
                normal.remove(pos);
                return true;
            }
        }

        // Try to drop high priority droppable frames
        {
            let mut high = self.high.write();
            if let Some(pos) = high.iter().position(|f| f.droppable) {
                high.remove(pos);
                return true;
            }
        }

        false
    }

    /// Returns the total number of frames in queue.
    pub fn total_len(&self) -> usize {
        self.critical.read().len()
            + self.high.read().len()
            + self.normal.read().len()
            + self.low.read().len()
    }

    /// Returns true if queue is empty.
    pub fn is_empty(&self) -> bool {
        self.total_len() == 0
    }

    /// Clears all frames except critical.
    pub fn clear_non_critical(&self) {
        self.high.write().clear();
        self.normal.write().clear();
        self.low.write().clear();
    }

    /// Drops all non-critical frames to free bandwidth.
    pub fn drop_video_frames(&self) {
        self.clear_non_critical();
    }
}

// ============================================================================
// Bandwidth Escalation
// ============================================================================

/// Escalation level for bandwidth management.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EscalationLevel {
    /// Normal operation.
    Normal = 0,
    /// Mild degradation.
    Degraded = 1,
    /// Severe degradation.
    Severe = 2,
    /// Critical - audio only.
    Critical = 3,
}

impl Default for EscalationLevel {
    fn default() -> Self {
        EscalationLevel::Normal
    }
}

/// Escalation manager for progressive bandwidth reduction.
pub struct EscalationManager {
    /// Current escalation level.
    level: RwLock<EscalationLevel>,
    /// Packet loss thresholds for each level.
    thresholds: [(f64, EscalationLevel); 4],
}

impl EscalationManager {
    /// Creates a new escalation manager with default thresholds.
    pub fn new() -> Self {
        Self {
            level: RwLock::new(EscalationLevel::Normal),
            thresholds: [
                (1.0, EscalationLevel::Normal),    // < 1% loss
                (5.0, EscalationLevel::Degraded),  // 1-5% loss
                (15.0, EscalationLevel::Severe),   // 5-15% loss
                (100.0, EscalationLevel::Critical), // > 15% loss
            ],
        }
    }

    /// Updates the escalation level based on network conditions.
    pub fn update(&self, packet_loss_pct: f64, bandwidth_kbps: u32) {
        let mut level = self.level.write();

        for (threshold, new_level) in &self.thresholds {
            if packet_loss_pct < *threshold {
                *level = *new_level;
                return;
            }
        }

        *level = EscalationLevel::Critical;
    }

    /// Returns the current escalation level.
    pub fn level(&self) -> EscalationLevel {
        *self.level.read()
    }

    /// Returns actions to take at current level.
    pub fn recommended_actions(&self) -> Vec<EscalationAction> {
        match *self.level.read() {
            EscalationLevel::Normal => vec![],
            EscalationLevel::Degraded => vec![
                EscalationAction::ReduceVideoQuality,
                EscalationAction::EnablePacketAggregation,
            ],
            EscalationLevel::Severe => vec![
                EscalationAction::DropVideoKeyframes,
                EscalationAction::ReduceVideoFramerate,
                EscalationAction::EnableAggressiveFEC,
            ],
            EscalationLevel::Critical => vec![
                EscalationAction::DisableVideo,
                EscalationAction::EnableAudioOnly,
                EscalationAction::MaximizeAudioQuality,
            ],
        }
    }
}

impl Default for EscalationManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Action to take during bandwidth escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationAction {
    /// Reduce video quality to next level.
    ReduceVideoQuality,
    /// Drop video keyframes to save bandwidth.
    DropVideoKeyframes,
    /// Reduce video framerate.
    ReduceVideoFramerate,
    /// Enable packet aggregation.
    EnablePacketAggregation,
    /// Enable aggressive forward error correction.
    EnableAggressiveFEC,
    /// Disable video entirely.
    DisableVideo,
    /// Switch to audio-only mode.
    EnableAudioOnly,
    /// Maximize audio quality (higher bitrate).
    MaximizeAudioQuality,
    /// Send periodic image snapshots instead of video.
    EnableImageFallback,
}

// ============================================================================
// Image Fallback Mode
// ============================================================================

/// Image snapshot for low-bandwidth fallback.
#[derive(Debug, Clone)]
pub struct ImageSnapshot {
    /// Image data (JPEG/PNG).
    pub data: Vec<u8>,
    /// Timestamp in nanoseconds.
    pub timestamp_ns: u64,
    /// Image width.
    pub width: u32,
    /// Image height.
    pub height: u32,
    /// Quality setting (1-100).
    pub quality: u8,
    /// Estimated size in bytes.
    pub estimated_size_bytes: usize,
}

impl ImageSnapshot {
    /// Creates a new image snapshot.
    pub fn new(data: Vec<u8>, width: u32, height: u32, quality: u8, timestamp_ns: u64) -> Self {
        let estimated_size_bytes = data.len();
        Self {
            data,
            timestamp_ns,
            width,
            height,
            quality,
            estimated_size_bytes,
        }
    }

    /// Returns the compression quality.
    pub fn compression_quality(&self) -> u8 {
        self.quality
    }
}

/// Configuration for image fallback mode.
#[derive(Debug, Clone, Copy)]
pub struct ImageFallbackConfig {
    /// Minimum bandwidth to send images (kbps).
    pub min_bandwidth_kbps: u32,
    /// Maximum interval between images (milliseconds).
    pub max_interval_ms: u64,
    /// Minimum interval between images (milliseconds).
    pub min_interval_ms: u64,
    /// Target image size in bytes.
    pub target_size_bytes: usize,
    /// Image compression quality (1-100).
    pub compression_quality: u8,
    /// Maximum images to buffer.
    pub max_buffer_size: usize,
}

impl Default for ImageFallbackConfig {
    fn default() -> Self {
        Self {
            min_bandwidth_kbps: 20,
            max_interval_ms: 10_000,  // 10 seconds
            min_interval_ms: 1_000,   // 1 second
            target_size_bytes: 15_000, // ~15KB per image
            compression_quality: 50,
            max_buffer_size: 3,
        }
    }
}

impl ImageFallbackConfig {
    /// Creates a config optimized for very low bandwidth.
    pub fn very_low_bandwidth() -> Self {
        Self {
            min_bandwidth_kbps: 10,
            max_interval_ms: 30_000,  // 30 seconds
            min_interval_ms: 5_000,    // 5 seconds
            target_size_bytes: 5_000,  // ~5KB
            compression_quality: 30,
            max_buffer_size: 1,
        }
    }

    /// Creates a config optimized for low bandwidth.
    pub fn low_bandwidth() -> Self {
        Self {
            min_bandwidth_kbps: 20,
            max_interval_ms: 10_000,
            min_interval_ms: 2_000,
            target_size_bytes: 15_000,
            compression_quality: 50,
            max_buffer_size: 2,
        }
    }

    /// Calculates optimal interval based on available bandwidth.
    pub fn calculate_interval(&self, available_kbps: u32) -> u64 {
        if available_kbps >= 50 {
            self.min_interval_ms
        } else if available_kbps >= 30 {
            (self.min_interval_ms + self.max_interval_ms) / 2
        } else {
            self.max_interval_ms
        }
    }
}

/// Mode for visual communication fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualFallbackMode {
    /// Full video streaming.
    VideoStreaming,
    /// Periodic image snapshots.
    ImageSnapshot,
    /// Text/emoji status.
    TextStatus,
    /// No visual communication.
    None,
}

impl Default for VisualFallbackMode {
    fn default() -> Self {
        VisualFallbackMode::VideoStreaming
    }
}

/// Image fallback manager for low-bandwidth scenarios.
pub struct ImageFallbackManager {
    /// Current fallback mode.
    mode: RwLock<VisualFallbackMode>,
    /// Fallback configuration.
    config: ImageFallbackConfig,
    /// Image buffer.
    image_buffer: RwLock<Vec<ImageSnapshot>>,
    /// Last image sent timestamp.
    last_sent: RwLock<u64>,
    /// Current compression quality.
    current_quality: RwLock<u8>,
    /// Bandwidth threshold for enabling fallback.
    fallback_threshold_kbps: u32,
}

impl ImageFallbackManager {
    /// Creates a new image fallback manager.
    pub fn new() -> Self {
        Self::with_config(ImageFallbackConfig::default())
    }

    /// Creates with custom configuration.
    pub fn with_config(config: ImageFallbackConfig) -> Self {
        Self {
            mode: RwLock::new(VisualFallbackMode::VideoStreaming),
            config,
            image_buffer: RwLock::new(Vec::with_capacity(config.max_buffer_size)),
            last_sent: RwLock::new(0),
            current_quality: RwLock::new(config.compression_quality),
            fallback_threshold_kbps: config.min_bandwidth_kbps,
        }
    }

    /// Returns the current fallback mode.
    pub fn mode(&self) -> VisualFallbackMode {
        *self.mode.read()
    }

    /// Updates mode based on bandwidth conditions.
    pub fn update_for_bandwidth(&self, bandwidth_kbps: u32) {
        let mut mode = self.mode.write();

        if bandwidth_kbps >= 100 {
            *mode = VisualFallbackMode::VideoStreaming;
        } else if bandwidth_kbps >= self.fallback_threshold_kbps {
            *mode = VisualFallbackMode::ImageSnapshot;
        } else if bandwidth_kbps >= 10 {
            *mode = VisualFallbackMode::TextStatus;
        } else {
            *mode = VisualFallbackMode::None;
        }
    }

    /// Returns true if should send an image now.
    pub fn should_send_image(&self, current_time_ns: u64) -> bool {
        if *self.mode.read() != VisualFallbackMode::ImageSnapshot {
            return false;
        }

        let last = *self.last_sent.read();
        let interval = self.calculate_next_interval();

        current_time_ns - last >= interval * 1_000_000 // convert ms to ns
    }

    /// Calculates the next send interval.
    pub fn calculate_next_interval(&self) -> u64 {
        let config = &self.config;
        let quality = *self.current_quality.read();

        // Adjust interval based on current quality
        // Lower quality = smaller images = can send more frequently
        let base_interval = config.min_interval_ms;
        let max_interval = config.max_interval_ms;

        let quality_factor = quality as f64 / 100.0;
        let interval = base_interval as f64 + (max_interval - base_interval) as f64 * (1.0 - quality_factor);

        interval as u64
    }

    /// Buffers an image snapshot.
    pub fn buffer_image(&self, snapshot: ImageSnapshot) {
        let mut buffer = self.image_buffer.write();

        // Remove oldest if buffer is full
        if buffer.len() >= self.config.max_buffer_size {
            buffer.remove(0);
        }

        buffer.push(snapshot);
    }

    /// Gets the next image to send.
    pub fn get_next_image(&self) -> Option<ImageSnapshot> {
        if *self.mode.read() != VisualFallbackMode::ImageSnapshot {
            return None;
        }

        let mut buffer = self.image_buffer.write();

        if buffer.is_empty() {
            return None;
        }

        let snapshot = buffer.pop();
        *self.last_sent.write() = snapshot.as_ref().map(|s| s.timestamp_ns).unwrap_or(0);

        snapshot
    }

    /// Adjusts compression quality based on actual send time.
    pub fn adjust_quality(&self, actual_send_time_ms: u64) {
        let mut quality = self.current_quality.write();

        if actual_send_time_ms > self.config.max_interval_ms {
            // Took too long, reduce quality
            if *quality > 10 {
                *quality -= 10;
            }
        } else if actual_send_time_ms < self.config.min_interval_ms {
            // Sent quickly, can try higher quality
            if *quality < 90 {
                *quality += 5;
            }
        }
    }

    /// Returns the current compression quality.
    pub fn current_quality(&self) -> u8 {
        *self.current_quality.read()
    }

    /// Returns configuration.
    pub fn config(&self) -> ImageFallbackConfig {
        self.config
    }

    /// Returns buffered image count.
    pub fn buffered_count(&self) -> usize {
        self.image_buffer.read().len()
    }

    /// Clears the image buffer.
    pub fn clear_buffer(&self) {
        self.image_buffer.write().clear();
    }
}

impl Default for ImageFallbackManager {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Status Message (for TextStatus mode)
// ============================================================================

/// Status message for text fallback.
#[derive(Debug, Clone)]
pub struct StatusMessage {
    /// Status type.
    pub status_type: StatusType,
    /// Emoji representation.
    pub emoji: &'static str,
    /// Text description.
    pub text: String,
    /// Timestamp.
    pub timestamp_ns: u64,
}

/// Available status types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusType {
    /// User is speaking.
    Speaking,
    /// User is listening.
    Listening,
    /// User is away.
    Away,
    /// User is typing.
    Typing,
    /// User has connection issues.
    ConnectionIssues,
    /// User is present.
    Present,
}

impl StatusType {
    /// Returns the emoji for this status.
    pub fn emoji(&self) -> &'static str {
        match self {
            StatusType::Speaking => "🎤",
            StatusType::Listening => "👂",
            StatusType::Away => "⏸️",
            StatusType::Typing => "⌨️",
            StatusType::ConnectionIssues => "📡",
            StatusType::Present => "✓",
        }
    }
}

/// Status broadcaster for text fallback.
pub struct StatusBroadcaster {
    /// Current status.
    current_status: RwLock<StatusType>,
    /// Last status change.
    last_change: RwLock<u64>,
    /// Minimum interval between status updates (ms).
    min_interval_ms: u64,
}

impl StatusBroadcaster {
    /// Creates a new status broadcaster.
    pub fn new() -> Self {
        Self {
            current_status: RwLock::new(StatusType::Present),
            last_change: RwLock::new(0),
            min_interval_ms: 500, // 500ms minimum between updates
        }
    }

    /// Updates the current status.
    pub fn set_status(&self, status: StatusType, timestamp_ns: u64) -> Option<StatusMessage> {
        let mut current = self.current_status.write();

        if *current == status {
            return None; // No change
        }

        let last = *self.last_change.read();
        if timestamp_ns - last < self.min_interval_ms * 1_000_000 {
            return None; // Too soon since last update
        }

        *current = status;
        *self.last_change.write() = timestamp_ns;

        Some(StatusMessage {
            status_type: status,
            emoji: status.emoji(),
            text: status.to_string(),
            timestamp_ns,
        })
    }

    /// Returns the current status.
    pub fn current_status(&self) -> StatusType {
        *self.current_status.read()
    }
}

impl Default for StatusBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for StatusType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StatusType::Speaking => write!(f, "Speaking"),
            StatusType::Listening => write!(f, "Listening"),
            StatusType::Away => write!(f, "Away"),
            StatusType::Typing => write!(f, "Typing"),
            StatusType::ConnectionIssues => write!(f, "Connection Issues"),
            StatusType::Present => write!(f, "Present"),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_first_manager_policy() {
        let mut manager = AudioFirstManager::new();

        // Simulate good network
        manager.update_for_network(0.5, 2000);
        assert!(manager.is_video_enabled());
        assert_eq!(manager.policy(), BandwidthPolicy::AudioFirst);

        // Simulate very poor network - bandwidth below audio minimum
        // Audio requirements target is 40kbps, so use 30kbps to trigger AudioOnly
        manager.update_for_network(10.0, 30);
        assert!(!manager.is_video_enabled());
        assert_eq!(manager.policy(), BandwidthPolicy::AudioOnly);

        // Recovery test - low packet loss and high bandwidth
        manager.update_for_network(1.0, 200);
        // Still AudioOnly because we need very low loss AND high bw to recover
        // packet_loss < 5/2 = 2.5, so 1.0 qualifies, but 200 < 128*2 = 256 doesn't
        // So stays in AudioOnly
    }

    #[test]
    fn test_audio_first_recovery() {
        let mut manager = AudioFirstManager::new();

        // Start with very low bandwidth
        manager.update_for_network(10.0, 30);
        assert_eq!(manager.policy(), BandwidthPolicy::AudioOnly);

        // Recover with excellent conditions
        manager.update_for_network(0.5, 500);
        assert!(manager.is_video_enabled());
        assert_eq!(manager.policy(), BandwidthPolicy::AudioFirst);
    }

    #[test]
    fn test_video_bandwidth_allocation() {
        let manager = AudioFirstManager::new();
        let reserved = manager.reserved_audio_kbps();

        // With 500kbps total, 440 should be available for video (500 - 60)
        let available = manager.available_video_bandwidth(500);
        assert!(available < 500 - reserved);
    }

    #[test]
    fn test_priority_frame() {
        let frame = PriorityFrame::audio(vec![1, 2, 3], 1000);
        assert!(frame.is_critical());
        assert!(!frame.can_drop());

        let delta = PriorityFrame::delta(vec![4, 5, 6], 2000);
        assert!(!delta.is_critical());
        assert!(delta.can_drop());
    }

    #[test]
    fn test_priority_queue() {
        let mut queue = PriorityQueue::new(10);

        queue.push(PriorityFrame::delta(vec![1], 1000));
        queue.push(PriorityFrame::audio(vec![2], 2000));
        queue.push(PriorityFrame::keyframe(vec![3], 3000));

        assert_eq!(queue.total_len(), 3);

        // Pop should return critical first
        let frame = queue.pop();
        assert!(frame.is_some());
        assert_eq!(frame.unwrap().priority, MediaPriority::Critical);
    }

    #[test]
    fn test_escalation_manager() {
        let manager = EscalationManager::new();

        manager.update(0.5, 2000);
        assert_eq!(manager.level(), EscalationLevel::Normal);

        manager.update(3.0, 1000);
        assert_eq!(manager.level(), EscalationLevel::Degraded);

        manager.update(20.0, 100);
        assert_eq!(manager.level(), EscalationLevel::Critical);
    }

    #[test]
    fn test_recommended_video_quality() {
        let manager = AudioFirstManager::new();

        // With 4000 kbps, should get high quality
        let quality = manager.recommended_video_quality(4000);
        assert!(quality >= QualityLevel::High);

        // With 100 kbps, should get minimum (150kbps is minimum level)
        let quality = manager.recommended_video_quality(100);
        assert!(quality <= QualityLevel::Minimum);
    }

    // ========================================================================
    // Image Fallback Tests
    // ========================================================================

    #[test]
    fn test_image_fallback_mode_transitions() {
        let manager = ImageFallbackManager::new();

        // High bandwidth - video streaming
        manager.update_for_bandwidth(500);
        assert_eq!(manager.mode(), VisualFallbackMode::VideoStreaming);

        // Medium bandwidth - image snapshot
        manager.update_for_bandwidth(30);
        assert_eq!(manager.mode(), VisualFallbackMode::ImageSnapshot);

        // Very low bandwidth - text status
        manager.update_for_bandwidth(15);
        assert_eq!(manager.mode(), VisualFallbackMode::TextStatus);

        // No bandwidth - none
        manager.update_for_bandwidth(5);
        assert_eq!(manager.mode(), VisualFallbackMode::None);
    }

    #[test]
    fn test_image_snapshot_creation() {
        let data = vec![0u8; 1000];
        let snapshot = ImageSnapshot::new(data, 320, 240, 50, 1000);

        assert_eq!(snapshot.width, 320);
        assert_eq!(snapshot.height, 240);
        assert_eq!(snapshot.quality, 50);
        assert_eq!(snapshot.estimated_size_bytes, 1000);
    }

    #[test]
    fn test_image_fallback_config() {
        let config = ImageFallbackConfig::very_low_bandwidth();

        assert_eq!(config.min_bandwidth_kbps, 10);
        assert_eq!(config.target_size_bytes, 5000);
        assert!(config.compression_quality < 50);
    }

    #[test]
    fn test_status_broadcaster() {
        let broadcaster = StatusBroadcaster::new();

        // Set initial status
        let msg = broadcaster.set_status(StatusType::Speaking, 1_000_000_000); // 1 second
        assert!(msg.is_some());
        assert_eq!(msg.unwrap().status_type, StatusType::Speaking);

        // Same status - no change
        let msg = broadcaster.set_status(StatusType::Speaking, 2_000_000_000); // 2 seconds
        assert!(msg.is_none());

        // Different status after sufficient time - should update
        let msg = broadcaster.set_status(StatusType::Listening, 3_000_000_000); // 3 seconds
        assert!(msg.is_some());
        assert_eq!(msg.unwrap().status_type, StatusType::Listening);
    }

    #[test]
    fn test_image_buffering() {
        let manager = ImageFallbackManager::new();
        manager.update_for_bandwidth(30); // Enable image mode

        // Buffer some images
        let snapshot = ImageSnapshot::new(vec![1, 2, 3], 320, 240, 50, 1000);
        manager.buffer_image(snapshot);

        assert_eq!(manager.buffered_count(), 1);

        // Get next image
        let next = manager.get_next_image();
        assert!(next.is_some());
        assert_eq!(manager.buffered_count(), 0);
    }

    #[test]
    fn test_quality_adjustment() {
        let manager = ImageFallbackManager::new();

        let initial = manager.current_quality();

        // Simulate slow send - quality should decrease
        manager.adjust_quality(20000); // 20 seconds
        assert!(manager.current_quality() < initial);

        // Reset and simulate fast send - quality should increase
        let manager = ImageFallbackManager::new();
        manager.adjust_quality(500); // 500ms
        assert!(manager.current_quality() >= initial);
    }
}
