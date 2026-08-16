//! Signaling channel for WebRTC SDP + ICE candidate exchange.
//!
//! In production this would push SDP blobs to pkarr, the public-key
//! addressable record system, keyed on the A3Net NodeId. For tests we use
//! an in-process `mpsc` channel.
//!
//! ## Wire format
//!
//! The pkarr-style payload is a JSON document with the following shape:
//!
//! ```json
//! {
//!   "kind": "webrtc-offer",
//!   "node_id": "<hex>",
//!   "sdp": "<base64 sdp>",
//!   "ttl_seconds": 60
//! }
//! ```
//!
//! ICE candidates ride a separate but identical-shape document with
//! `kind = "webrtc-ice"` and a `candidate` field instead of `sdp`.

use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use a3net_types::NodeId;

use crate::error::{WebRtcError, WebRtcResult};

/// Discriminator for the signaling payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignalingKind {
    /// SDP offer. Sent by the dialer.
    WebRtcOffer,
    /// SDP answer. Sent by the listener.
    WebRtcAnswer,
    /// ICE candidate trickle.
    WebRtcIce,
}

/// Payload exchanged via the signaling channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalingPayload {
    pub kind: SignalingKind,
    pub node_id: NodeId,
    /// SDP body (base64-encoded).
    pub sdp: Option<String>,
    /// ICE candidate (JSON-encoded `RTCIceCandidateInit`).
    pub candidate: Option<String>,
    /// Time-to-live in seconds. After this, the receiver can discard.
    pub ttl_seconds: u64,
}

impl SignalingPayload {
    pub fn offer(node_id: NodeId, sdp_b64: String) -> Self {
        Self {
            kind: SignalingKind::WebRtcOffer,
            node_id,
            sdp: Some(sdp_b64),
            candidate: None,
            ttl_seconds: 60,
        }
    }

    pub fn answer(node_id: NodeId, sdp_b64: String) -> Self {
        Self {
            kind: SignalingKind::WebRtcAnswer,
            node_id,
            sdp: Some(sdp_b64),
            candidate: None,
            ttl_seconds: 60,
        }
    }

    pub fn ice(node_id: NodeId, candidate_json: String) -> Self {
        Self {
            kind: SignalingKind::WebRtcIce,
            node_id,
            sdp: None,
            candidate: Some(candidate_json),
            ttl_seconds: 60,
        }
    }
}

/// Abstract signaling channel. Implementations are responsible for
/// persisting the payload where the remote peer can find it.
#[async_trait::async_trait]
pub trait SignalingChannel: Send + Sync + 'static {
    /// Publish a payload. Replaces any previous payload for the same
    /// `(node_id, kind)` pair.
    async fn publish(&self, payload: SignalingPayload) -> WebRtcResult<()>;

    /// Read the most recent payload for `(node_id, kind)`, if any.
    async fn read(
        &self,
        node_id: &NodeId,
        kind: SignalingKind,
    ) -> WebRtcResult<Option<SignalingPayload>>;

    /// Subscribe to changes for `(node_id, kind)`. Returns a receiver that
    /// yields new payloads as they appear.
    async fn subscribe(
        &self,
        node_id: NodeId,
        kind: SignalingKind,
    ) -> WebRtcResult<mpsc::Receiver<SignalingPayload>>;
}

/// In-memory signaling channel. Useful for tests and for running two A3Net
/// nodes in the same process. It is `Clone` and thread-safe.
#[derive(Clone, Default)]
pub struct InMemorySignaling {
    inner: Arc<Mutex<InMemorySignalingInner>>,
}

#[derive(Default)]
struct InMemorySignalingInner {
    next_id: u64,
    subscribers: Vec<(u64, NodeId, SignalingKind, mpsc::Sender<SignalingPayload>)>,
}

impl InMemorySignaling {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_subscriber_id(&self) -> u64 {
        let mut inner = self.inner.lock();
        inner.next_id += 1;
        inner.next_id
    }
}

#[async_trait::async_trait]
impl SignalingChannel for InMemorySignaling {
    async fn publish(&self, payload: SignalingPayload) -> WebRtcResult<()> {
        let mut inner = self.inner.lock();
        inner.subscribers.retain(|(id, node, kind, tx)| {
            if *node == payload.node_id && *kind == payload.kind {
                // Best-effort; receiver may be gone.
                let _ = tx.try_send(payload.clone());
                // Remove exhausted receivers.
                !tx.is_closed()
            } else {
                // Don't remove, keep other subscribers.
                let _ = id;
                true
            }
        });
        Ok(())
    }

    async fn read(
        &self,
        _node_id: &NodeId,
        _kind: SignalingKind,
    ) -> WebRtcResult<Option<SignalingPayload>> {
        // The in-memory channel is push-based only; `read` is a no-op.
        Ok(None)
    }

    async fn subscribe(
        &self,
        node_id: NodeId,
        kind: SignalingKind,
    ) -> WebRtcResult<mpsc::Receiver<SignalingPayload>> {
        let (tx, rx) = mpsc::channel(16);
        let id = self.next_subscriber_id();
        self.inner.lock().subscribers.push((id, node_id, kind, tx));
        Ok(rx)
    }
}

/// pkarr-based signaling. Activated when the `signaling` feature is on AND
/// the caller has configured pkarr relay URLs. Not implemented in
/// Round-1; the trait exists so callers can plug their own backend.
pub struct PkarrSignaling {
    // Placeholder; will hold pkarr client + relay URLs in Round-2.
    _placeholder: (),
}

impl PkarrSignaling {
    pub fn new() -> WebRtcResult<Self> {
        Err(WebRtcError::Signaling(
            "PkarrSignaling is not implemented yet (Round-2)".into(),
        ))
    }
}
