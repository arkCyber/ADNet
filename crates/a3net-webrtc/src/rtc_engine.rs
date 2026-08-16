//! `webrtc::RTCPeerConnection` wrapper.
//!
//! Round-1 scaffold: structure is in place; full SDP/ICE plumbing lands in
//! Round-2 against a pinned `webrtc-rs` version. The point of this module
//! is to **isolate the upstream API in one file** so future major-version
//! bumps only touch this file.
//!
//! What works today:
//! - `Engine::build(cfg)` constructs a `RTCPeerConnection` with the
//!   configured ICE servers.
//! - ICE candidates are gathered into an internal buffer that the caller
//!   can drain with [`Engine::drain_local_candidates`].
//! - Connection state is exposed via a `tokio::sync::watch` channel.
//!
//! What Round-2 will add:
//! - SDP offer/answer creation and application against a remote SDP.
//! - Remote ICE candidate application.
//! - ICE-establishment timeout.
//! - Full DataChannel integration via [`super::dc_session`].

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::watch;
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::RTCIceCandidate;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

use crate::config::WebRtcConfig;
use crate::error::{WebRtcError, WebRtcResult};

/// A pre-built WebRTC peer connection, ready to be used as either the
/// offerer or the answerer side of a session.
pub struct Engine {
    /// The underlying `RTCPeerConnection`. We hold an `Arc` only to share
    /// with the DataChannel session; `RTCPeerConnection` itself is
    /// internally reference-counted so we just keep one strong reference.
    pc: Arc<RTCPeerConnection>,
    state_rx: watch::Receiver<RTCPeerConnectionState>,
    local_candidates: Arc<Mutex<Vec<RTCIceCandidate>>>,
    config: WebRtcConfig,
}

impl Engine {
    /// Build a new `Engine` from a [`WebRtcConfig`].
    pub async fn build(config: &WebRtcConfig) -> WebRtcResult<Self> {
        let mut ice_servers: Vec<RTCIceServer> = config
            .stun
            .iter()
            .map(|url| RTCIceServer {
                urls: vec![url.clone()],
                ..Default::default()
            })
            .collect();

        for turn in &config.turn {
            ice_servers.push(RTCIceServer {
                urls: vec![turn.url.clone()],
                username: turn.username.clone().unwrap_or_default(),
                credential: turn.credential.clone().unwrap_or_default(),
                ..Default::default()
            });
        }

        let rtc_config = RTCConfiguration {
            ice_servers,
            ..Default::default()
        };

        let api = APIBuilder::new().build();
        let pc = api
            .new_peer_connection(rtc_config)
            .await
            .map_err(|e| WebRtcError::Backend(format!("new_peer_connection: {e}")))?;

        let (state_tx, state_rx) = watch::channel(RTCPeerConnectionState::New);
        let local_candidates = Arc::new(Mutex::new(Vec::new()));
        let lc_clone = local_candidates.clone();
        pc.on_ice_candidate(Box::new(move |maybe_candidate| {
            let lc_clone = lc_clone.clone();
            Box::pin(async move {
                if let Some(c) = maybe_candidate {
                    lc_clone.lock().push(c);
                }
            })
        }));
        pc.on_peer_connection_state_change(Box::new(move |state| {
            let _ = state_tx.send(state);
            Box::pin(async {})
        }));

        Ok(Self {
            pc: Arc::new(pc),
            state_rx,
            local_candidates,
            config: config.clone(),
        })
    }

    /// Underlying peer connection handle.
    pub fn peer_connection(&self) -> Arc<RTCPeerConnection> {
        self.pc.clone()
    }

    /// Current ICE/peer connection state.
    pub fn current_state(&self) -> RTCPeerConnectionState {
        *self.state_rx.borrow()
    }

    /// Wait until the connection reaches `Connected`, or return an error
    /// after `timeout`.
    pub async fn wait_connected(&self, timeout: std::time::Duration) -> WebRtcResult<()> {
        let mut rx = self.state_rx.clone();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match *rx.borrow() {
                RTCPeerConnectionState::Connected => return Ok(()),
                RTCPeerConnectionState::Failed
                | RTCPeerConnectionState::Closed
                | RTCPeerConnectionState::Disconnected => {
                    return Err(WebRtcError::Backend(format!(
                        "peer connection state: {:?}",
                        *rx.borrow()
                    )));
                }
                _ => {}
            }
            tokio::select! {
                _ = rx.changed() => continue,
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(WebRtcError::IceEstablishTimeout(timeout));
                }
            }
        }
    }

    /// Generate the SDP offer and return it as a base64 string.
    pub async fn create_offer(&self) -> WebRtcResult<String> {
        let offer = self
            .pc
            .create_offer(None)
            .await
            .map_err(|e| WebRtcError::Sdp(format!("create_offer: {e}")))?;
        self.pc
            .set_local_description(offer)
            .await
            .map_err(|e| WebRtcError::Sdp(format!("set_local_description: {e}")))?;
        let local = self
            .pc
            .local_description()
            .await
            .ok_or_else(|| WebRtcError::Sdp("no local description after set".into()))?;
        Ok(encode_sdp(&local))
    }

    /// Accept an SDP offer, generate the answer, and return the answer.
    pub async fn accept_offer(&self, offer_b64: &str) -> WebRtcResult<String> {
        let offer = decode_sdp(offer_b64)?;
        self.pc
            .set_remote_description(offer)
            .await
            .map_err(|e| WebRtcError::Sdp(format!("set_remote_description: {e}")))?;
        let answer = self
            .pc
            .create_answer(None)
            .await
            .map_err(|e| WebRtcError::Sdp(format!("create_answer: {e}")))?;
        self.pc
            .set_local_description(answer)
            .await
            .map_err(|e| WebRtcError::Sdp(format!("set_local_description: {e}")))?;
        let local = self
            .pc
            .local_description()
            .await
            .ok_or_else(|| WebRtcError::Sdp("no local description after set".into()))?;
        Ok(encode_sdp(&local))
    }

    /// Apply an answer to a previously-created offer.
    pub async fn apply_answer(&self, answer_b64: &str) -> WebRtcResult<()> {
        let answer = decode_sdp(answer_b64)?;
        self.pc
            .set_remote_description(answer)
            .await
            .map_err(|e| WebRtcError::Sdp(format!("set_remote_description: {e}")))?;
        Ok(())
    }

    /// Add a remote ICE candidate (trickle ICE).
    pub async fn add_remote_candidate(&self, cand_json: &str) -> WebRtcResult<()> {
        let cand_init: webrtc::ice_transport::ice_candidate::RTCIceCandidateInit =
            serde_json::from_str(cand_json)
                .map_err(|e| WebRtcError::Sdp(format!("decode candidate: {e}")))?;
        self.pc
            .add_ice_candidate(cand_init)
            .await
            .map_err(|e| WebRtcError::Sdp(format!("add_ice_candidate: {e}")))?;
        Ok(())
    }

    /// Drain the locally-gathered ICE candidates. Returns JSON-encoded
    /// `RTCIceCandidateInit`s ready to send over the signaling channel.
    ///
    /// We use JSON (not base64) for ICE candidates because the candidate
    /// string itself is small and human-inspectable. This is the same
    /// convention that libp2p and iroh use for their SDP exchange.
    pub fn drain_local_candidates(&self) -> Vec<String> {
        let mut guard = self.local_candidates.lock();
        let out: Vec<String> = guard
            .drain(..)
            .filter_map(|c| match c.to_json() {
                Ok(init) => serde_json::to_string(&init).ok(),
                Err(_) => None,
            })
            .collect();
        out
    }

    /// Configured ICE timeout.
    pub fn establish_timeout(&self) -> std::time::Duration {
        self.config.establish_timeout()
    }
}

fn encode_sdp(sdp: &RTCSessionDescription) -> String {
    let s = sdp.sdp.clone();
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}

fn decode_sdp(b64: &str) -> WebRtcResult<RTCSessionDescription> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| WebRtcError::Sdp(format!("base64 decode: {e}")))?;
    let sdp_str = String::from_utf8(bytes)
        .map_err(|e| WebRtcError::Sdp(format!("utf-8 decode: {e}")))?;
    RTCSessionDescription::offer(sdp_str).map_err(|e| WebRtcError::Sdp(e.to_string()))
}
