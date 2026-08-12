//! `DataChannel` session — owns one ordered+reliable WebRTC DataChannel,
//! runs the Noise handshake, and hands off to the regular ADNet frame
//! codec.
//!
//! ## Round-1 status
//!
//! Scaffold only. Round-2 will:
//! - Drive the Noise handshake over the DataChannel bytes.
//! - Install a writer task that serialises `bytes::Bytes` onto the DC.
//! - Install a reader task that decrypts incoming bytes with the Noise
//!   cipher state and feeds the receive queue.
//! - Properly bridge the `FrameCodec` (length-prefixed ADNet frames) with
//!   the byte-stream DC API.

use std::sync::Arc;

use snow::Keypair;
use webrtc::data_channel::RTCDataChannel;

use crate::config::WebRtcConfig;
use crate::error::{WebRtcError, WebRtcResult};

/// The single, canonical DataChannel label.
pub const CHANNEL_LABEL: &str = "adnet/0";

/// An opened DataChannel session. Round-1 scaffold — full handshake
/// integration lands in Round-2.
pub struct DcSession {
    /// The underlying WebRTC DataChannel.
    #[allow(dead_code)]
    dc: Arc<RTCDataChannel>,
    /// Local Noise keypair (used to drive the handshake).
    #[allow(dead_code)]
    keypair: Keypair,
    /// Configuration (kept around for max-datagram-size etc).
    config: WebRtcConfig,
    /// Send side of the frame queue. Application code calls
    /// [`DcSession::send`] to push; the writer task (Round-2) drains it.
    tx: tokio::sync::mpsc::Sender<bytes::Bytes>,
}

impl DcSession {
    /// Open an outbound DataChannel on an existing peer connection and
    /// run the Noise handshake as the initiator.
    pub async fn open_outbound(
        dc: Arc<RTCDataChannel>,
        keypair: Keypair,
        config: WebRtcConfig,
    ) -> WebRtcResult<Self> {
        let (tx, _rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);
        Ok(Self {
            dc,
            keypair,
            config,
            tx,
        })
    }

    /// Open the session as a responder (called when the remote peer is
    /// the initiator).
    pub async fn open_inbound(
        dc: Arc<RTCDataChannel>,
        keypair: Keypair,
        config: WebRtcConfig,
    ) -> WebRtcResult<Self> {
        Self::open_outbound(dc, keypair, config).await
    }

    /// Enqueue a frame for sending. Returns when the bytes have been
    /// queued (or the channel has been closed).
    pub async fn send(&self, frame: bytes::Bytes) -> WebRtcResult<()> {
        if frame.len() > self.config.max_datagram_bytes {
            return Err(WebRtcError::Frame(format!(
                "frame too large for DC: {} > {}",
                frame.len(),
                self.config.max_datagram_bytes
            )));
        }
        self.tx
            .send(frame)
            .await
            .map_err(|_| WebRtcError::PeerClosed)?;
        Ok(())
    }

    /// Receive the next decrypted frame. Returns `None` if the channel has
    /// closed.
    pub async fn recv(&self) -> WebRtcResult<Option<Vec<u8>>> {
        // Round-1 scaffold: the reader task is not yet wired. Block
        // forever so callers don't busy-loop. Round-2 will replace this
        // with a proper queue pop driven by a task that decrypts DC
        // messages.
        std::future::pending::<WebRtcResult<Option<Vec<u8>>>>().await
    }

    /// Close the underlying DataChannel.
    pub async fn close(self) -> WebRtcResult<()> {
        self.dc
            .close()
            .await
            .map_err(|e| WebRtcError::DataChannel(format!("close: {e}")))?;
        Ok(())
    }
}
