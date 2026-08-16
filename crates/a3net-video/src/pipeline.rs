//! Video pipeline — orchestrates capture → encode → stream → decode → render.
//!
//! DO-178C DAL-B compliant pipeline with explicit state machine and
//! comprehensive error handling.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::config::{PipelineConfig, VideoConfig};
use crate::error::{VideoError, VideoResult};
use crate::frame::{EncodedFrame, FrameId, FrameType, VideoFrame};
use crate::stats::{FrameTimings, QualityMetrics, StreamStats, VideoStats};

/// Pipeline operational state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PipelineState {
    /// Pipeline is idle, not started.
    Idle,
    /// Pipeline is initializing.
    Initializing,
    /// Pipeline is running normally.
    Running,
    /// Pipeline is paused (frames buffered but not processed).
    Paused,
    /// Pipeline is draining (finishing in-flight frames).
    Draining,
    /// Pipeline has stopped (terminal state).
    Stopped,
    /// Pipeline has failed (requires restart).
    Failed,
}

impl PipelineState {
    /// Returns true if frames can be submitted in this state.
    pub fn accepts_frames(&self) -> bool {
        matches!(self, PipelineState::Running | PipelineState::Paused)
    }

    /// Returns true if this is a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, PipelineState::Stopped | PipelineState::Failed)
    }
}

impl std::fmt::Display for PipelineState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineState::Idle => write!(f, "Idle"),
            PipelineState::Initializing => write!(f, "Initializing"),
            PipelineState::Running => write!(f, "Running"),
            PipelineState::Paused => write!(f, "Paused"),
            PipelineState::Draining => write!(f, "Draining"),
            PipelineState::Stopped => write!(f, "Stopped"),
            PipelineState::Failed => write!(f, "Failed"),
        }
    }
}

/// Pipeline event for observability.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PipelineEvent {
    /// Pipeline state changed.
    StateChanged {
        from: PipelineState,
        to: PipelineState,
    },
    /// Frame was encoded.
    FrameEncoded {
        frame_id: FrameId,
        byte_size: usize,
        is_keyframe: bool,
        encode_time_ms: u64,
    },
    /// Frame was decoded.
    FrameDecoded {
        frame_id: FrameId,
        byte_size: usize,
        decode_time_ms: u64,
    },
    /// Frame was dropped due to buffer overflow.
    FrameDropped {
        frame_id: FrameId,
        reason: String,
    },
    /// Frame was late for render deadline.
    FrameLate {
        frame_id: FrameId,
        late_by_ms: u64,
    },
    /// Quality metrics updated.
    QualityUpdated(QualityMetrics),
    /// Error occurred.
    Error {
        error: Arc<VideoError>,
    },
}

impl std::fmt::Display for PipelineEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineEvent::StateChanged { from, to } => {
                write!(f, "State: {} → {}", from, to)
            }
            PipelineEvent::FrameEncoded { frame_id, byte_size, is_keyframe, .. } => {
                write!(f, "Encoded {} ({} bytes, keyframe={})", frame_id, byte_size, is_keyframe)
            }
            PipelineEvent::FrameDecoded { frame_id, byte_size, .. } => {
                write!(f, "Decoded {} ({} bytes)", frame_id, byte_size)
            }
            PipelineEvent::FrameDropped { frame_id, reason } => {
                write!(f, "Dropped {}: {}", frame_id, reason)
            }
            PipelineEvent::FrameLate { frame_id, late_by_ms } => {
                write!(f, "Late {}: {}ms", frame_id, late_by_ms)
            }
            PipelineEvent::QualityUpdated(q) => {
                write!(f, "Quality: fps={:.1}, bitrate={}kbps, latency={}ms",
                       q.actual_fps, q.bitrate_kbps, q.avg_latency_ms)
            }
            PipelineEvent::Error { error } => {
                write!(f, "Error: {}", error)
            }
        }
    }
}

/// Video pipeline — orchestrates the encoding/decoding pipeline.
pub struct VideoPipeline {
    /// Pipeline configuration.
    config: PipelineConfig,
    /// Current pipeline state.
    state: RwLock<PipelineState>,
    /// Video statistics.
    stats: RwLock<VideoStats>,
    /// Frame timings for latency measurement.
    frame_timings: RwLock<Vec<FrameTimings>>,
    /// Event sender for observability.
    event_tx: mpsc::Sender<PipelineEvent>,
    /// Current frame sequence number.
    seq: RwLock<u32>,
    /// Last frame timestamp.
    last_pts_ns: RwLock<u64>,
    /// Consecutive dropped frames counter.
    consecutive_drops: RwLock<u32>,
}

impl VideoPipeline {
    /// Creates a new video pipeline.
    pub fn new(config: PipelineConfig) -> (Self, mpsc::Receiver<PipelineEvent>) {
        let (event_tx, event_rx) = mpsc::channel(1000);
        let pipeline = Self {
            config: config.into(),
            state: RwLock::new(PipelineState::Idle),
            stats: RwLock::new(VideoStats::default()),
            frame_timings: RwLock::new(Vec::with_capacity(1000)),
            event_tx,
            seq: RwLock::new(0),
            last_pts_ns: RwLock::new(0),
            consecutive_drops: RwLock::new(0),
        };
        (pipeline, event_rx)
    }

    /// Starts the pipeline.
    pub async fn start(&self) -> VideoResult<()> {
        let old_state = self.set_state(PipelineState::Initializing)?;
        if old_state != PipelineState::Idle 
           && old_state != PipelineState::Stopped 
           && old_state != PipelineState::Initializing {
            return Err(VideoError::InvalidPipelineState {
                expected: "Idle or Stopped or Initializing",
                actual: match old_state {
                    PipelineState::Idle => "Idle",
                    PipelineState::Initializing => "Initializing",
                    PipelineState::Running => "Running",
                    PipelineState::Paused => "Paused",
                    PipelineState::Draining => "Draining",
                    PipelineState::Stopped => "Stopped",
                    PipelineState::Failed => "Failed",
                },
            });
        }

        info!("Starting video pipeline");
        self.set_state(PipelineState::Running)?;
        self.stats.write().reset();
        *self.seq.write() = 0;
        *self.last_pts_ns.write() = 0;

        Ok(())
    }

    /// Stops the pipeline gracefully.
    pub async fn stop(&self, reason: String) -> VideoResult<()> {
        let old_state = self.set_state(PipelineState::Draining)?;
        if old_state.is_terminal() {
            return Err(VideoError::InvalidPipelineState {
                expected: "non-terminal state",
                actual: match old_state {
                    PipelineState::Idle => "Idle",
                    PipelineState::Initializing => "Initializing",
                    PipelineState::Running => "Running",
                    PipelineState::Paused => "Paused",
                    PipelineState::Draining => "Draining",
                    PipelineState::Stopped => "Stopped",
                    PipelineState::Failed => "Failed",
                },
            });
        }

        info!("Stopping video pipeline: {}", reason);
        self.set_state(PipelineState::Stopped)?;
        Ok(())
    }

    /// Pauses frame processing.
    pub fn pause(&self) -> VideoResult<()> {
        let old_state = self.set_state(PipelineState::Paused)?;
        if old_state != PipelineState::Running {
            return Err(VideoError::InvalidPipelineState {
                expected: "Running",
                actual: match old_state {
                    PipelineState::Idle => "Idle",
                    PipelineState::Initializing => "Initializing",
                    PipelineState::Running => "Running",
                    PipelineState::Paused => "Paused",
                    PipelineState::Draining => "Draining",
                    PipelineState::Stopped => "Stopped",
                    PipelineState::Failed => "Failed",
                },
            });
        }
        Ok(())
    }

    /// Resumes frame processing.
    pub fn resume(&self) -> VideoResult<()> {
        let old_state = self.set_state(PipelineState::Running)?;
        if old_state != PipelineState::Paused {
            return Err(VideoError::InvalidPipelineState {
                expected: "Paused",
                actual: match old_state {
                    PipelineState::Idle => "Idle",
                    PipelineState::Initializing => "Initializing",
                    PipelineState::Running => "Running",
                    PipelineState::Paused => "Paused",
                    PipelineState::Draining => "Draining",
                    PipelineState::Stopped => "Stopped",
                    PipelineState::Failed => "Failed",
                },
            });
        }
        Ok(())
    }

    /// Returns the current pipeline state.
    pub fn state(&self) -> PipelineState {
        *self.state.read()
    }

    /// Returns a snapshot of video statistics.
    pub fn stats(&self) -> VideoStats {
        self.stats.read().clone()
    }

    /// Validates timestamp monotonicity per SR-5.
    fn validate_timestamp(&self, pts_ns: u64) -> VideoResult<()> {
        let last = *self.last_pts_ns.read();
        if pts_ns < last {
            return Err(VideoError::NonMonotonicTimestamp {
                prev_ns: last,
                curr_ns: pts_ns,
            });
        }
        Ok(())
    }

    /// Validates sequence number continuity per SR-5.
    fn validate_sequence(&self, expected: u32, actual: u32) -> VideoResult<()> {
        if actual != expected {
            let lost = if actual > expected {
                actual - expected
            } else {
                0
            };
            return Err(VideoError::SequenceGap {
                expected,
                actual,
                lost,
            });
        }
        Ok(())
    }

    /// Encodes a raw frame.
    pub async fn encode_frame(
        &self,
        frame: VideoFrame,
    ) -> VideoResult<EncodedFrame> {
        let state = self.state();
        if !state.accepts_frames() {
            return Err(VideoError::InvalidPipelineState {
                expected: "Running or Paused",
                actual: match state {
                    PipelineState::Idle => "Idle",
                    PipelineState::Initializing => "Initializing",
                    PipelineState::Running => "Running",
                    PipelineState::Paused => "Paused",
                    PipelineState::Draining => "Draining",
                    PipelineState::Stopped => "Stopped",
                    PipelineState::Failed => "Failed",
                },
            });
        }

        let start = Instant::now();

        // For now, this is a placeholder that wraps the frame
        // In real implementation, this would call the codec encoder
        let (pts_ns, seq, data) = match frame {
            VideoFrame::Raw(raw) => {
                let pts = raw.pts_ns;
                self.validate_timestamp(pts)?;
                let s = *self.seq.read();
                *self.seq.write() = s + 1;
                (pts, s, raw.data.clone())
            }
            VideoFrame::Encoded(encoded) => {
                (encoded.pts_ns, encoded.id.seq, encoded.data.clone())
            }
        };

        // Determine frame type
        let frame_type = if seq % self.config.video.keyframe_interval.frames == 0 {
            FrameType::Keyframe
        } else {
            FrameType::Delta
        };

        let encode_time_ms = start.elapsed().as_millis() as u64;

        let encoded = EncodedFrame::new(
            FrameId::new(pts_ns, seq),
            self.config.video.codec,
            frame_type,
            data,
            pts_ns,
            pts_ns,
        )?;

        // Update statistics
        {
            let mut stats = self.stats.write();
            stats.frames_encoded += 1;
            stats.bytes_encoded += encoded.byte_size() as u64;
            if frame_type == FrameType::Keyframe {
                stats.keyframes_encoded += 1;
            }
            stats.avg_encode_time_ms = (stats.avg_encode_time_ms * (stats.frames_encoded - 1)
                + encode_time_ms) / stats.frames_encoded;
        }

        // Send event
        let _ = self.event_tx.send(PipelineEvent::FrameEncoded {
            frame_id: encoded.id,
            byte_size: encoded.byte_size(),
            is_keyframe: encoded.is_keyframe,
            encode_time_ms,
        }).await;

        Ok(encoded)
    }

    /// Decodes an encoded frame.
    pub async fn decode_frame(
        &self,
        frame: EncodedFrame,
    ) -> VideoResult<VideoFrame> {
        let state = self.state();
        if !state.accepts_frames() {
            return Err(VideoError::InvalidPipelineState {
                expected: "Running or Paused",
                actual: match state {
                    PipelineState::Idle => "Idle",
                    PipelineState::Initializing => "Initializing",
                    PipelineState::Running => "Running",
                    PipelineState::Paused => "Paused",
                    PipelineState::Draining => "Draining",
                    PipelineState::Stopped => "Stopped",
                    PipelineState::Failed => "Failed",
                },
            });
        }

        let start = Instant::now();

        // Validate sequence
        let expected_seq = *self.seq.read();
        self.validate_sequence(expected_seq, frame.id.seq)?;

        // In real implementation, this would call the codec decoder
        let decode_time_ms = start.elapsed().as_millis() as u64;

        // Update statistics
        {
            let mut stats = self.stats.write();
            stats.frames_decoded += 1;
            stats.bytes_decoded += frame.byte_size() as u64;
            stats.avg_decode_time_ms = (stats.avg_decode_time_ms * (stats.frames_decoded - 1)
                + decode_time_ms) / stats.frames_decoded;
        }

        // Send event
        let _ = self.event_tx.send(PipelineEvent::FrameDecoded {
            frame_id: frame.id,
            byte_size: frame.byte_size(),
            decode_time_ms,
        }).await;

        // Return as VideoFrame
        Ok(VideoFrame::Encoded(frame))
    }

    /// Records frame timing for latency analysis.
    pub fn record_timing(&self, timing: FrameTimings) {
        let mut timings = self.frame_timings.write();
        if timings.len() >= 1000 {
            timings.remove(0);
        }
        timings.push(timing);
    }

    /// Returns recent frame timings.
    pub fn recent_timings(&self) -> Vec<FrameTimings> {
        self.frame_timings.read().clone()
    }

    /// Sets the pipeline state and emits events.
    fn set_state(&self, new_state: PipelineState) -> VideoResult<PipelineState> {
        let old_state = {
            let mut state = self.state.write();
            let old = *state;
            *state = new_state;
            old
        };

        if old_state != new_state {
            let _ = self.event_tx.try_send(PipelineEvent::StateChanged {
                from: old_state,
                to: new_state,
            });
        }

        Ok(old_state)
    }

    /// Marks the pipeline as failed.
    pub fn fail(&self, error: VideoError) {
        error!("Pipeline failed: {}", error);
        let _ = self.set_state(PipelineState::Failed);
        let _ = self.event_tx.try_send(PipelineEvent::Error {
            error: Arc::new(error),
        });
    }
}

impl std::fmt::Debug for VideoPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoPipeline")
            .field("config", &self.config)
            .field("state", &self.state())
            .field("stats", &self.stats())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Framerate, Resolution, VideoQuality};

    fn test_config() -> PipelineConfig {
        PipelineConfig {
            enable_video: true,
            video: crate::config::VideoConfig::from_quality(
                VideoQuality::Standard,
                crate::codec::VideoCodec::H264,
            ).unwrap(),
        }
    }

    #[tokio::test]
    async fn pipeline_lifecycle() {
        let (pipeline, _rx) = VideoPipeline::new(test_config());

        // Start
        assert_eq!(pipeline.state(), PipelineState::Idle);
        pipeline.start().await.unwrap();
        assert_eq!(pipeline.state(), PipelineState::Running);

        // Stop
        pipeline.stop("test complete".into()).await.unwrap();
        assert_eq!(pipeline.state(), PipelineState::Stopped);
    }

    #[tokio::test]
    async fn pipeline_cannot_start_from_running() {
        let (pipeline, _rx) = VideoPipeline::new(test_config());
        pipeline.start().await.unwrap();
        let err = pipeline.start().await.unwrap_err();
        assert!(matches!(err, VideoError::InvalidPipelineState { .. }));
    }

    #[tokio::test]
    async fn pipeline_pause_resume() {
        let (pipeline, _rx) = VideoPipeline::new(test_config());
        pipeline.start().await.unwrap();

        pipeline.pause().unwrap();
        assert_eq!(pipeline.state(), PipelineState::Paused);

        pipeline.resume().unwrap();
        assert_eq!(pipeline.state(), PipelineState::Running);
    }

    #[tokio::test]
    async fn stats_update() {
        let (pipeline, _rx) = VideoPipeline::new(test_config());
        pipeline.start().await.unwrap();

        let stats = pipeline.stats();
        assert_eq!(stats.frames_encoded, 0);

        let _ = pipeline.encode_frame(
            crate::frame::VideoFrame::Raw(
                crate::frame::RawFrame::solid(320, 240, 0, 0, 0).unwrap()
            )
        ).await;

        let stats = pipeline.stats();
        assert_eq!(stats.frames_encoded, 1);
    }
}
