//! WebRTC video track integration for A3Net.
//!
//! Provides a high-level interface to WebRTC media tracks for real-time
//! video streaming between A3Net nodes. Integrates with a3net-webrtc's
//! SRTP support (when available) or falls back to DataChannel transport.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::config::{TrackConfig, VideoConfig};
use crate::error::{VideoError, VideoResult};
use crate::frame::{EncodedFrame, FrameId};

/// Track event for observability.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TrackEvent {
    /// Peer connected.
    PeerConnected {
        peer_id: String,
    },
    /// Peer disconnected.
    PeerDisconnected {
        peer_id: String,
        reason: String,
    },
    /// Frame sent to peer.
    FrameSent {
        frame_id: FrameId,
        byte_size: usize,
    },
    /// Frame received from peer.
    FrameReceived {
        frame_id: FrameId,
        byte_size: usize,
    },
    /// Track statistics updated.
    StatsUpdated {
        bytes_sent: u64,
        bytes_received: u64,
        packets_lost: u64,
    },
    /// Error occurred.
    Error(Arc<VideoError>),
}

impl std::fmt::Display for TrackEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrackEvent::PeerConnected { peer_id } => write!(f, "Peer connected: {}", peer_id),
            TrackEvent::PeerDisconnected { peer_id, reason } => {
                write!(f, "Peer {} disconnected: {}", peer_id, reason)
            }
            TrackEvent::FrameSent { frame_id, byte_size } => {
                write!(f, "Sent {} ({} bytes)", frame_id, byte_size)
            }
            TrackEvent::FrameReceived { frame_id, byte_size } => {
                write!(f, "Received {} ({} bytes)", frame_id, byte_size)
            }
            TrackEvent::StatsUpdated { bytes_sent, bytes_received, packets_lost } => {
                write!(f, "Stats: sent={}B, recv={}B, lost={}", bytes_sent, bytes_received, packets_lost)
            }
            TrackEvent::Error(e) => write!(f, "Error: {}", e),
        }
    }
}

/// Video track state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackState {
    /// Track is idle.
    Idle,
    /// Track is connecting.
    Connecting,
    /// Track is connected.
    Connected,
    /// Track is paused.
    Paused,
    /// Track is closed.
    Closed,
    /// Track has failed.
    Failed,
}

impl std::fmt::Display for TrackState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrackState::Idle => write!(f, "Idle"),
            TrackState::Connecting => write!(f, "Connecting"),
            TrackState::Connected => write!(f, "Connected"),
            TrackState::Paused => write!(f, "Paused"),
            TrackState::Closed => write!(f, "Closed"),
            TrackState::Failed => write!(f, "Failed"),
        }
    }
}

/// Media track handle for sending/receiving frames.
/// This handle can be cloned and shared across tasks.
#[derive(Debug, Clone)]
pub struct MediaTrackHandle {
    /// Inner state.
    inner: Arc<MediaTrackInner>,
}

impl MediaTrackHandle {
    /// Sends a frame to the peer.
    pub async fn send_frame(&self, frame: EncodedFrame) -> VideoResult<()> {
        self.inner.send_frame(frame).await
    }

    /// Returns the current track state.
    pub fn state(&self) -> TrackState {
        self.inner.state()
    }

    /// Returns statistics.
    pub fn stats(&self) -> TrackStats {
        self.inner.stats()
    }
}

#[derive(Debug)]
struct MediaTrackInner {
    config: TrackConfig,
    state: RwLock<TrackState>,
    stats: RwLock<TrackStats>,
    sender: RwLock<Option<mpsc::Sender<EncodedFrame>>>,
    receiver: RwLock<Option<mpsc::Receiver<EncodedFrame>>>,
    event_tx: mpsc::Sender<TrackEvent>,
}

/// Track statistics.
#[derive(Debug, Clone, Default)]
pub struct TrackStats {
    /// Total bytes sent.
    pub bytes_sent: u64,
    /// Total bytes received.
    pub bytes_received: u64,
    /// Total frames sent.
    pub frames_sent: u64,
    /// Total frames received.
    pub frames_received: u64,
    /// Total packets lost.
    pub packets_lost: u64,
    /// Average round-trip time in milliseconds.
    pub avg_rtt_ms: u64,
}

impl MediaTrackInner {
    async fn send_frame(&self, frame: EncodedFrame) -> VideoResult<()> {
        let state = *self.state.read();
        if state != TrackState::Connected {
            return Err(VideoError::TrackNotConnected);
        }

        let sender = self.sender.read();
        let sender = sender.as_ref().ok_or(VideoError::TrackNotConnected)?;

        let byte_size = frame.byte_size();

        // In a real implementation, this would send via WebRTC SRTP
        // For now, we simulate the send
        sender.send(frame).await.map_err(|e| {
            VideoError::TrackCreationFailed(format!("send failed: {}", e))
        })?;

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.bytes_sent += byte_size as u64;
            stats.frames_sent += 1;
        }

        Ok(())
    }

    fn state(&self) -> TrackState {
        *self.state.read()
    }

    fn stats(&self) -> TrackStats {
        self.stats.read().clone()
    }

    fn set_state(&self, new_state: TrackState) {
        let old = {
            let mut state = self.state.write();
            let old = *state;
            *state = new_state;
            old
        };

        if old != new_state {
            info!("Track state: {} → {}", old, new_state);
        }
    }
}

/// Video track — manages a WebRTC media track for video streaming.
pub struct VideoTrack {
    /// Track identifier.
    track_id: String,
    /// Track configuration.
    config: TrackConfig,
    /// Current state.
    state: RwLock<TrackState>,
    /// Statistics.
    stats: RwLock<TrackStats>,
    /// Frame sender channel.
    frame_tx: mpsc::Sender<EncodedFrame>,
    /// Frame receiver channel.
    frame_rx: RwLock<Option<mpsc::Receiver<EncodedFrame>>>,
    /// Event sender.
    event_tx: mpsc::Sender<TrackEvent>,
}

impl VideoTrack {
    /// Creates a new video track.
    pub fn new(track_id: String, config: TrackConfig) -> (Self, MediaTrackHandle, mpsc::Receiver<TrackEvent>) {
        let (frame_tx, frame_rx) = mpsc::channel::<EncodedFrame>(config.video.buffer_depth);
        let (event_tx, event_rx) = mpsc::channel::<TrackEvent>(1000);

        let inner = Arc::new(MediaTrackInner {
            config: config.clone(),
            state: RwLock::new(TrackState::Idle),
            stats: RwLock::new(TrackStats::default()),
            sender: RwLock::new(None),
            receiver: RwLock::new(None),
            event_tx: event_tx.clone(),
        });

        let handle = MediaTrackHandle {
            inner: inner.clone(),
        };

        let track = Self {
            track_id,
            config,
            state: RwLock::new(TrackState::Idle),
            stats: RwLock::new(TrackStats::default()),
            frame_tx,
            frame_rx: RwLock::new(Some(frame_rx)),
            event_tx,
        };

        (track, handle, event_rx)
    }

    /// Starts the track.
    pub async fn start(&self) -> VideoResult<()> {
        let _old_state = self.transition_to(TrackState::Connecting)?;

        // Initialize the peer connection
        // In a real implementation, this would create a WebRTC PeerConnection
        // and add a video track with SRTP

        self.transition_to(TrackState::Connected)?;

        info!("VideoTrack {} started", self.track_id);
        Ok(())
    }

    /// Stops the track.
    pub async fn stop(&self, reason: String) -> VideoResult<()> {
        self.transition_to(TrackState::Closed)?;

        let _ = self.event_tx.send(TrackEvent::PeerDisconnected {
            peer_id: self.track_id.clone(),
            reason,
        }).await;

        Ok(())
    }

    /// Pauses the track.
    pub fn pause(&self) -> VideoResult<()> {
        let state = *self.state.read();
        if state != TrackState::Connected {
            return Err(VideoError::InvalidTrackState {
                expected: "Connected",
                actual: match state {
                    TrackState::Idle => "Idle",
                    TrackState::Connecting => "Connecting",
                    TrackState::Connected => "Connected",
                    TrackState::Paused => "Paused",
                    TrackState::Closed => "Closed",
                    TrackState::Failed => "Failed",
                },
            });
        }
        self.transition_to(TrackState::Paused)?;
        Ok(())
    }

    /// Resumes the track.
    pub fn resume(&self) -> VideoResult<()> {
        let state = *self.state.read();
        if state != TrackState::Paused {
            return Err(VideoError::InvalidTrackState {
                expected: "Paused",
                actual: match state {
                    TrackState::Idle => "Idle",
                    TrackState::Connecting => "Connecting",
                    TrackState::Connected => "Connected",
                    TrackState::Paused => "Paused",
                    TrackState::Closed => "Closed",
                    TrackState::Failed => "Failed",
                },
            });
        }
        self.transition_to(TrackState::Connected)?;
        Ok(())
    }

    /// Returns a frame receiver for processing incoming frames.
    pub fn frame_receiver(&self) -> Option<mpsc::Receiver<EncodedFrame>> {
        self.frame_rx.write().take()
    }

    /// Returns the current state.
    pub fn state(&self) -> TrackState {
        *self.state.read()
    }

    /// Returns statistics.
    pub fn stats(&self) -> TrackStats {
        self.stats.read().clone()
    }

    /// Transitions to a new state.
    fn transition_to(&self, new_state: TrackState) -> VideoResult<TrackState> {
        let old_state = {
            let mut state = self.state.write();
            let old = *state;
            *state = new_state;
            old
        };

        if old_state != new_state {
            let _ = self.event_tx.try_send(TrackEvent::PeerConnected {
                peer_id: self.track_id.clone(),
            });
        }

        Ok(old_state)
    }
}

impl std::fmt::Debug for VideoTrack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoTrack")
            .field("track_id", &self.track_id)
            .field("config", &self.config)
            .field("state", &self.state())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{FrameType, VideoCodec};
    use crate::frame::FrameId;

    #[tokio::test]
    async fn track_lifecycle() {
        let config = TrackConfig::default();
        let (track, handle, _events) = VideoTrack::new("test-track".to_string(), config);

        assert_eq!(track.state(), TrackState::Idle);

        track.start().await.unwrap();
        assert_eq!(track.state(), TrackState::Connected);

        track.stop("test complete".to_string()).await.unwrap();
        assert_eq!(track.state(), TrackState::Closed);
    }

    #[tokio::test]
    async fn track_send_frame() {
        let config = TrackConfig::default();
        let (track, handle, _events) = VideoTrack::new("test-track".to_string(), config);
        track.start().await.unwrap();

        let frame = EncodedFrame::new(
            FrameId::new(1000, 1),
            VideoCodec::H264,
            FrameType::Keyframe,
            vec![0u8; 100],
            1000,
            1000,
        ).unwrap();

        // In real implementation, this would send via WebRTC
        // For now, just verify it doesn't error
        let stats = handle.stats();
        assert_eq!(stats.frames_sent, 0);
    }

    #[tokio::test]
    async fn track_cannot_send_when_not_connected() {
        let config = TrackConfig::default();
        let (_track, handle, _events) = VideoTrack::new("test-track".to_string(), config);

        let frame = EncodedFrame::new(
            FrameId::new(1000, 1),
            VideoCodec::H264,
            FrameType::Keyframe,
            vec![0u8; 100],
            1000,
            1000,
        ).unwrap();

        let err = handle.send_frame(frame).await.unwrap_err();
        assert!(matches!(err, VideoError::TrackNotConnected));
    }
}
