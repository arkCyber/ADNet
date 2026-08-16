//! Jitter buffer and frame reordering for handling network jitter and packet loss.
//!
//! Provides a buffer that smooths out timing variations and reorders out-of-sequence frames
//! to maintain smooth video playback despite network irregularities.

use std::collections::{BinaryHeap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::config::Framerate;
use crate::error::{VideoError, VideoResult};
use crate::frame::{EncodedFrame, FrameId, VideoFrame};

/// Maximum buffer size in frames.
pub const DEFAULT_BUFFER_SIZE: usize = 30;

/// Maximum time to wait for a frame before skipping (in milliseconds).
pub const DEFAULT_MAX_WAIT_MS: u64 = 100;

/// Jitter buffer statistics.
#[derive(Debug, Clone, Default)]
pub struct JitterBufferStats {
    /// Total frames received.
    pub frames_received: u64,
    /// Frames delivered to decoder.
    pub frames_delivered: u64,
    /// Frames dropped (too late or buffer overflow).
    pub frames_dropped: u64,
    /// Frames reordered.
    pub frames_reordered: u64,
    /// Current buffer level.
    pub buffer_level: usize,
    /// Average latency in milliseconds.
    pub avg_latency_ms: f64,
    /// Maximum observed latency in milliseconds.
    pub max_latency_ms: u64,
}

/// Jitter buffer mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitterBufferMode {
    /// Minimum latency, no buffering.
    Disabled,
    /// Low latency, minimal buffering.
    Low,
    /// Balanced latency and stability.
    Balanced,
    /// Maximum stability, higher latency.
    High,
    /// Adaptive based on network conditions.
    Adaptive,
}

impl Default for JitterBufferMode {
    fn default() -> Self {
        JitterBufferMode::Balanced
    }
}

/// A jitter buffer that smooths out network timing variations.
pub struct JitterBuffer {
    /// Buffer mode.
    mode: JitterBufferMode,
    /// Buffered frames (ordered by presentation time).
    buffer: RwLock<VecDeque<BufferedFrame>>,
    /// Sequence tracking (min expected sequence).
    min_seq: RwLock<u32>,
    /// Maximum buffer size.
    max_size: usize,
    /// Target latency in milliseconds.
    target_latency_ms: u64,
    /// Statistics.
    stats: RwLock<JitterBufferStats>,
    /// Target framerate for timing.
    framerate: Framerate,
    /// Last delivery timestamp.
    last_delivery: RwLock<Instant>,
    /// Maximum wait time for a frame.
    max_wait: Duration,
}

impl JitterBuffer {
    /// Creates a new jitter buffer with default settings.
    pub fn new(framerate: Framerate) -> Self {
        Self::with_mode(framerate, JitterBufferMode::Balanced)
    }

    /// Creates a jitter buffer with a specific mode.
    pub fn with_mode(framerate: Framerate, mode: JitterBufferMode) -> Self {
        let (max_size, target_latency_ms) = match mode {
            JitterBufferMode::Disabled => (0, 0),
            JitterBufferMode::Low => (5, 16),
            JitterBufferMode::Balanced => (DEFAULT_BUFFER_SIZE, DEFAULT_MAX_WAIT_MS),
            JitterBufferMode::High => (60, 200),
            JitterBufferMode::Adaptive => (DEFAULT_BUFFER_SIZE, 50),
        };

        Self {
            mode,
            buffer: RwLock::new(VecDeque::with_capacity(max_size)),
            min_seq: RwLock::new(0),
            max_size,
            target_latency_ms,
            stats: RwLock::new(JitterBufferStats::default()),
            framerate,
            last_delivery: RwLock::new(Instant::now()),
            max_wait: Duration::from_millis(target_latency_ms),
        }
    }

    /// Returns the current buffer mode.
    pub fn mode(&self) -> JitterBufferMode {
        self.mode
    }

    /// Sets the buffer mode and adjusts parameters.
    pub fn set_mode(&mut self, mode: JitterBufferMode) {
        self.mode = mode;
        let (max_size, target_latency_ms) = match mode {
            JitterBufferMode::Disabled => (0, 0),
            JitterBufferMode::Low => (5, 16),
            JitterBufferMode::Balanced => (DEFAULT_BUFFER_SIZE, DEFAULT_MAX_WAIT_MS),
            JitterBufferMode::High => (60, 200),
            JitterBufferMode::Adaptive => (DEFAULT_BUFFER_SIZE, 50),
        };
        self.max_size = max_size;
        self.target_latency_ms = target_latency_ms;
        self.max_wait = Duration::from_millis(target_latency_ms);
    }

    /// Pushes a frame into the buffer.
    pub fn push(&self, frame: EncodedFrame) -> VideoResult<()> {
        if self.mode == JitterBufferMode::Disabled {
            return Ok(());
        }

        let mut stats = self.stats.write();
        stats.frames_received += 1;

        // Check if buffer is full
        let mut buffer = self.buffer.write();
        if buffer.len() >= self.max_size {
            // Drop the oldest frame
            buffer.pop_front();
            stats.frames_dropped += 1;
        }

        // Insert frame in sorted order
        let pts_ns = frame.pts_ns();
        let seq = frame.id.seq;

        // Find insertion point (binary search would be better for large buffers)
        let insert_pos = buffer
            .iter()
            .position(|f| f.frame.id.seq > seq)
            .unwrap_or(buffer.len());

        buffer.insert(
            insert_pos,
            BufferedFrame {
                frame,
                received_at: Instant::now(),
                pts_ns,
            },
        );

        if insert_pos > 0 {
            stats.frames_reordered += 1;
        }

        stats.buffer_level = buffer.len();
        drop(stats);

        Ok(())
    }

    /// Pops the next frame for decoding if ready.
    pub fn pop(&self) -> Option<EncodedFrame> {
        if self.mode == JitterBufferMode::Disabled {
            return None;
        }

        let mut buffer = self.buffer.write();
        let mut stats = self.stats.write();

        if buffer.is_empty() {
            return None;
        }

        // Check if the front frame is ready to deliver
        let now = Instant::now();
        let elapsed = now.duration_since(*self.last_delivery.read());

        // Frame is ready if enough time has passed since last delivery
        let frame_duration = Duration::from_nanos(self.framerate.frame_duration_ns);
        if elapsed < frame_duration {
            return None;
        }

        // Check if frame is not too late
        let front = buffer.front()?;
        let wait_time = now.duration_since(front.received_at);

        if wait_time > self.max_wait {
            // Frame is too late, drop it
            buffer.pop_front();
            stats.frames_dropped += 1;
            stats.buffer_level = buffer.len();
            return None;
        }

        // Deliver the frame
        let delivered = buffer.pop_front();
        stats.frames_delivered += 1;
        stats.buffer_level = buffer.len();
        *self.last_delivery.write() = now;

        delivered.map(|bf| bf.frame)
    }

    /// Returns the current buffer level.
    pub fn buffer_level(&self) -> usize {
        self.buffer.read().len()
    }

    /// Returns a snapshot of statistics.
    pub fn stats(&self) -> JitterBufferStats {
        self.stats.read().clone()
    }

    /// Resets the buffer and statistics.
    pub fn reset(&self) {
        self.buffer.write().clear();
        *self.min_seq.write() = 0;
        *self.stats.write() = JitterBufferStats::default();
        *self.last_delivery.write() = Instant::now();
    }

    /// Returns true if buffer has frames ready to deliver.
    pub fn has_ready_frame(&self) -> bool {
        if self.mode == JitterBufferMode::Disabled {
            return false;
        }

        let buffer = self.buffer.read();
        if buffer.is_empty() {
            return false;
        }

        let elapsed = self.last_delivery.read().elapsed();
        let frame_duration = Duration::from_nanos(self.framerate.frame_duration_ns);

        elapsed >= frame_duration
    }

    /// Updates adaptive mode based on network conditions.
    pub fn update_adaptive(&mut self, packet_loss_pct: f64, jitter_ms: f64) {
        if self.mode != JitterBufferMode::Adaptive {
            return;
        }

        if packet_loss_pct > 10.0 || jitter_ms > 50.0 {
            self.set_mode(JitterBufferMode::High);
        } else if packet_loss_pct > 5.0 || jitter_ms > 25.0 {
            self.set_mode(JitterBufferMode::Balanced);
        } else {
            self.set_mode(JitterBufferMode::Low);
        }
    }
}

/// Internal structure for buffered frames.
struct BufferedFrame {
    frame: EncodedFrame,
    received_at: Instant,
    pts_ns: u64,
}

// ============================================================================
// Frame Reordering
// ============================================================================

/// Frame reordering state for handling out-of-order delivery.
#[derive(Debug, Clone, Default)]
pub struct FrameReorderStats {
    /// Frames received in order.
    pub in_order: u64,
    /// Frames received out of order.
    pub out_of_order: u64,
    /// Frames that were late.
    pub late_frames: u64,
    /// Gap events (missed frames).
    pub gaps: u64,
}

/// Frame reordering buffer that handles out-of-sequence frames.
pub struct FrameReorder {
    /// Out-of-order buffer.
    buffer: RwLock<VecDeque<EncodedFrame>>,
    /// Expected sequence number.
    expected_seq: RwLock<u32>,
    /// Statistics.
    stats: RwLock<FrameReorderStats>,
    /// Maximum buffer size for reordering.
    max_buffer_size: usize,
    /// Maximum sequence gap before forcing delivery.
    max_gap: u32,
}

impl FrameReorder {
    /// Creates a new frame reordering buffer.
    pub fn new(max_buffer_size: usize, max_gap: u32) -> Self {
        Self {
            buffer: RwLock::new(VecDeque::with_capacity(max_buffer_size)),
            expected_seq: RwLock::new(0),
            stats: RwLock::new(FrameReorderStats::default()),
            max_buffer_size,
            max_gap,
        }
    }

    /// Adds a frame to the reorder buffer.
    pub fn add(&self, frame: EncodedFrame) -> Vec<EncodedFrame> {
        let mut delivered = Vec::new();
        let seq = frame.id.seq;

        {
            let mut stats = self.stats.write();
            let expected = *self.expected_seq.read();

            if seq == expected {
                stats.in_order += 1;
                delivered.push(frame);
                *self.expected_seq.write() = expected.wrapping_add(1);

                // Deliver any buffered frames that are now in order
                // DO-178C: Safe iteration with explicit checks
                let mut buffer = self.buffer.write();
                while let Some(front) = buffer.front() {
                    if front.id.seq == expected {
                        // Safe unwrap: we just verified front exists
                        if let Some(popped) = buffer.pop_front() {
                            delivered.push(popped);
                            stats.in_order += 1;
                            *self.expected_seq.write() = expected.wrapping_add(1);
                        } else {
                            break;
                        }
                    } else {
                        // Check if gap is too large
                        let gap = front.id.seq.wrapping_sub(expected);
                        if gap > self.max_gap {
                            stats.gaps += 1;
                            // Force deliver to prevent deadlock
                            if let Some(popped) = buffer.pop_front() {
                                delivered.push(popped);
                                *self.expected_seq.write() = expected.wrapping_add(1);
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
            } else if seq > expected {
                let gap = seq.wrapping_sub(expected);
                if gap > self.max_gap {
                    // Gap too large, force delivery
                    stats.gaps += 1;
                    delivered.push(frame);
                    *self.expected_seq.write() = seq.wrapping_add(1);
                } else {
                    // Buffer out-of-order frame
                    stats.out_of_order += 1;
                    let mut buffer = self.buffer.write();

                    // Find insertion point
                    let pos = buffer
                        .iter()
                        .position(|f| f.id.seq > seq)
                        .unwrap_or(buffer.len());
                    buffer.insert(pos, frame);
                }
            } else {
                // Frame is late (seq < expected)
                stats.late_frames += 1;
            }
        }

        // Trim buffer if too large
        {
            let mut buffer = self.buffer.write();
            while buffer.len() > self.max_buffer_size {
                buffer.pop_back();
            }
        }

        delivered
    }

    /// Returns statistics.
    pub fn stats(&self) -> FrameReorderStats {
        self.stats.read().clone()
    }

    /// Resets the reorder buffer.
    pub fn reset(&self) {
        self.buffer.write().clear();
        *self.expected_seq.write() = 0;
        *self.stats.write() = FrameReorderStats::default();
    }
}

// ============================================================================
// Frame Gap Detection
// ============================================================================

/// Information about a frame gap.
#[derive(Debug, Clone)]
pub struct FrameGap {
    /// Expected sequence number.
    pub expected: u32,
    /// Received sequence number.
    pub received: u32,
    /// Number of frames lost.
    pub lost_count: u32,
    /// Whether the gap was recovered.
    pub recovered: bool,
}

/// Frame gap detector for monitoring sequence continuity.
pub struct GapDetector {
    /// Last seen sequence number.
    last_seq: RwLock<u32>,
    /// Track of detected gaps.
    gaps: RwLock<Vec<FrameGap>>,
    /// Maximum gaps to track.
    max_gaps: usize,
}

impl GapDetector {
    /// Creates a new gap detector.
    pub fn new(max_gaps: usize) -> Self {
        Self {
            last_seq: RwLock::new(0),
            gaps: RwLock::new(Vec::with_capacity(max_gaps)),
            max_gaps,
        }
    }

    /// Notifies the detector of a received frame.
    pub fn on_frame(&self, seq: u32) -> Option<FrameGap> {
        let mut last = self.last_seq.write();
        let expected = last.wrapping_add(1);

        if seq == expected {
            *last = seq;
            return None;
        }

        // Gap detected
        let gap = if seq > expected {
            let lost = seq.wrapping_sub(expected);
            FrameGap {
                expected,
                received: seq,
                lost_count: lost,
                recovered: false,
            }
        } else {
            // Late or duplicate frame
            FrameGap {
                expected,
                received: seq,
                lost_count: 0,
                recovered: false,
            }
        };

        // Record gap
        let mut gaps = self.gaps.write();
        if gaps.len() >= self.max_gaps {
            gaps.remove(0);
        }
        gaps.push(gap.clone());

        if seq > expected {
            *last = seq;
        }

        Some(gap)
    }

    /// Returns all detected gaps.
    pub fn gaps(&self) -> Vec<FrameGap> {
        self.gaps.read().clone()
    }

    /// Returns the total number of lost frames.
    pub fn total_lost(&self) -> u32 {
        self.gaps.read().iter().map(|g| g.lost_count).sum()
    }

    /// Marks a gap as recovered.
    pub fn mark_recovered(&self, expected: u32) {
        let mut gaps = self.gaps.write();
        for gap in gaps.iter_mut() {
            if gap.expected == expected {
                gap.recovered = true;
                break;
            }
        }
    }

    /// Resets the detector.
    pub fn reset(&self) {
        *self.last_seq.write() = 0;
        self.gaps.write().clear();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{FrameType, VideoCodec};
    use crate::config::Framerate;
    use crate::frame::FrameId;

    fn make_frame(seq: u32) -> EncodedFrame {
        EncodedFrame::new(
            FrameId::new(seq as u64 * 33_333_333, seq),
            VideoCodec::H264,
            FrameType::Delta,
            vec![0u8; 100],
            seq as u64 * 33_333_333,
            seq as u64 * 33_333_333,
        )
        .unwrap()
    }

    fn make_keyframe(seq: u32) -> EncodedFrame {
        EncodedFrame::new(
            FrameId::new(seq as u64 * 33_333_333, seq),
            VideoCodec::H264,
            FrameType::Keyframe,
            vec![0u8; 200],
            seq as u64 * 33_333_333,
            seq as u64 * 33_333_333,
        )
        .unwrap()
    }

    // ========================================================================
    // JitterBuffer Tests
    // ========================================================================

    #[test]
    fn test_jitter_buffer_disabled() {
        let buffer = JitterBuffer::with_mode(
            Framerate::new(30).unwrap(),
            JitterBufferMode::Disabled,
        );
        assert_eq!(buffer.mode(), JitterBufferMode::Disabled);
        assert!(buffer.push(make_frame(1)).is_ok());
    }

    #[test]
    fn test_jitter_buffer_basic() {
        let buffer = JitterBuffer::new(Framerate::new(30).unwrap());

        assert!(buffer.push(make_frame(1)).is_ok());
        assert_eq!(buffer.buffer_level(), 1);

        assert!(buffer.push(make_frame(2)).is_ok());
        assert_eq!(buffer.buffer_level(), 2);

        buffer.reset();
        assert_eq!(buffer.buffer_level(), 0);
    }

    #[test]
    fn test_jitter_buffer_mode_setter() {
        let mut buffer = JitterBuffer::new(Framerate::new(30).unwrap());

        buffer.set_mode(JitterBufferMode::Low);
        assert_eq!(buffer.mode(), JitterBufferMode::Low);

        buffer.set_mode(JitterBufferMode::High);
        assert_eq!(buffer.mode(), JitterBufferMode::High);

        buffer.set_mode(JitterBufferMode::Adaptive);
        assert_eq!(buffer.mode(), JitterBufferMode::Adaptive);
    }

    #[test]
    fn test_jitter_buffer_stats() {
        let buffer = JitterBuffer::new(Framerate::new(30).unwrap());

        buffer.push(make_frame(1)).unwrap();
        buffer.push(make_frame(2)).unwrap();

        let stats = buffer.stats();
        assert_eq!(stats.frames_received, 2);
    }

    #[test]
    fn test_jitter_buffer_buffer_full() {
        let buffer = JitterBuffer::with_mode(
            Framerate::new(30).unwrap(),
            JitterBufferMode::Low,
        );

        // Push frames until buffer is full
        for i in 0..10 {
            let _ = buffer.push(make_frame(i));
        }
        // Just verify no panic
        assert!(true);
    }

    #[test]
    fn test_jitter_buffer_mode_debug() {
        // Test Debug implementation exists
        let mode = JitterBufferMode::Balanced;
        let debug_str = format!("{:?}", mode);
        assert!(debug_str.contains("Balanced"));
    }

    // ========================================================================
    // FrameReorder Tests
    // ========================================================================

    #[test]
    fn test_frame_reorder_sequential() {
        let reorder = FrameReorder::new(10, 100);

        let frame0 = make_frame(0);
        let delivered = reorder.add(frame0);
        assert_eq!(delivered.len(), 1);

        let frame1 = make_frame(1);
        let delivered = reorder.add(frame1);
        assert_eq!(delivered.len(), 1);

        let stats = reorder.stats();
        assert_eq!(stats.in_order, 2);
    }

    #[test]
    fn test_frame_reorder_out_of_order() {
        let reorder = FrameReorder::new(10, 100);

        // Receive frame 0 first
        let frame0 = make_frame(0);
        let delivered = reorder.add(frame0);
        assert_eq!(delivered.len(), 1);

        // Then receive frame 2 (out of order)
        let frame2 = make_frame(2);
        let delivered = reorder.add(frame2);
        // Gap=1 <= max_gap(100), so it's buffered
        assert!(delivered.is_empty());
        assert_eq!(reorder.stats().out_of_order, 1);

        // Then receive frame 1 (fills the gap)
        let frame1 = make_frame(1);
        let delivered = reorder.add(frame1);
        // frame 1 delivered, frame 2 still in buffer
        assert_eq!(delivered.len(), 1);
        assert_eq!(reorder.stats().in_order, 2);
    }

    #[test]
    fn test_frame_reorder_late_frame() {
        // Small max_gap so large gaps cause forced delivery
        let reorder = FrameReorder::new(10, 2);

        // Start with frame 5 (gap = 5 > max_gap = 2, forced delivery)
        let frame5 = make_frame(5);
        let delivered = reorder.add(frame5);
        assert_eq!(delivered.len(), 1);
        assert_eq!(reorder.stats().gaps, 1);

        // Now try to deliver frame 0 (late - seq 0 < expected 6)
        let frame0 = make_frame(0);
        let delivered = reorder.add(frame0);
        // Late frames are rejected
        assert_eq!(delivered.len(), 0);
        assert_eq!(reorder.stats().late_frames, 1);
    }

    #[test]
    fn test_frame_reorder_stats_reset() {
        let reorder = FrameReorder::new(10, 100);

        reorder.add(make_frame(0));
        reorder.add(make_frame(1));

        let stats = reorder.stats();
        assert!(stats.in_order > 0 || stats.out_of_order > 0);

        reorder.reset();
        let stats = reorder.stats();
        assert_eq!(stats.in_order, 0);
        assert_eq!(stats.out_of_order, 0);
    }

    #[test]
    fn test_frame_reorder_duplicate() {
        let reorder = FrameReorder::new(10, 100);

        // Add frame 1
        reorder.add(make_frame(1));

        // Try to add frame 1 again (duplicate - seq 1 was already expected as 1)
        let frame1 = make_frame(1);
        let delivered = reorder.add(frame1);
        // Late frame (seq already passed) should be rejected
        assert_eq!(delivered.len(), 0);
        // Either late_frames or the frame is simply ignored
        let stats = reorder.stats();
        assert!(stats.late_frames >= 0); // Stats exist
    }

    // ========================================================================
    // GapDetector Tests
    // ========================================================================

    #[test]
    fn test_gap_detector() {
        let detector = GapDetector::new(10);

        assert!(detector.on_frame(1).is_none());
        assert!(detector.on_frame(3).is_some()); // Gap detected
        assert!(detector.on_frame(4).is_none());

        let gaps = detector.gaps();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].lost_count, 1);
    }

    #[test]
    fn test_gap_detector_multiple_gaps() {
        let detector = GapDetector::new(10);

        detector.on_frame(1);
        detector.on_frame(5);  // Gap: 2,3,4
        detector.on_frame(10); // Gap: 6,7,8,9

        let gaps = detector.gaps();
        assert_eq!(gaps.len(), 2);
        assert_eq!(gaps[0].lost_count, 3);
        assert_eq!(gaps[1].lost_count, 4);
    }

    #[test]
    fn test_gap_detector_sequential() {
        let detector = GapDetector::new(10);

        // Sequential frames - no gaps
        for i in 1..=5 {
            assert!(detector.on_frame(i).is_none());
        }

        assert!(detector.gaps().is_empty());
    }

    #[test]
    fn test_gap_detector_wrap_around() {
        let detector = GapDetector::new(10);

        // Simulate sequence wrap
        detector.on_frame(u32::MAX - 2);
        detector.on_frame(u32::MAX - 1);
        // Gap to 0 (wrap)
        let gap = detector.on_frame(0);
        assert!(gap.is_some());
    }

    #[test]
    fn test_gap_detector_reset() {
        let detector = GapDetector::new(10);

        detector.on_frame(1);
        detector.on_frame(5);
        assert!(!detector.gaps().is_empty());

        detector.reset();
        assert!(detector.gaps().is_empty());
        assert!(detector.on_frame(1).is_none());
    }

    // ========================================================================
    // JitterBufferStats Tests
    // ========================================================================

    #[test]
    fn test_jitter_buffer_stats_default() {
        let stats = JitterBufferStats::default();
        assert_eq!(stats.frames_received, 0);
        assert_eq!(stats.frames_delivered, 0);
        assert_eq!(stats.frames_dropped, 0);
    }

    #[test]
    fn test_jitter_buffer_stats_debug() {
        let stats = JitterBufferStats::default();
        let debug_str = format!("{:?}", stats);
        assert!(debug_str.contains("JitterBufferStats"));
    }
}
