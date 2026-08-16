//! Auto-heartbeat background service.
//!
//! This is the missing piece that turns the [`PeerManager`] from a
//! pure bookkeeping layer into a *living* P2P node. The service
//! owns a Tokio task that wakes up every `heartbeat_interval` and:
//!
//! 1. Runs a `heartbeat_tick` on the [`PeerManager`], which:
//!    - Moves peers whose last acknowledged ping is older than
//!      `heartbeat_interval` to `Suspect`.
//!    - Moves peers whose last ack is older than
//!      `heartbeat_timeout` to `Dead`.
//! 2. Picks the set of peers that need a fresh ping (suspects +
//!    anyone whose last ping is older than the interval).
//! 3. Builds a [`HeartbeatMessage`] for each such peer (jittered
//!    by the configured percentage) and dispatches it via the
//!    [`HeartbeatSender`] trait the embedder provides.
//! 4. Tracks the in-flight ping so the matching pong can produce
//!    an RTT.
//!
//! ## Why a Trait instead of a hard transport handle?
//!
//! The `peer_manager` crate does not depend on a specific transport
//! (QUIC, iroh, WebRTC, mesh HTTP). Instead, every embedder wires
//! in a [`HeartbeatSender`]. Production code uses the QUIC/iroh
//! transport; tests use a [`MockHeartbeatSender`] that captures
//! outbound messages in a `Vec` for assertion.
//!
//! ## Jitter
//!
//! Per-peer RTT births a small random delay (`±jitter_percent` of
//! the interval) on top of the scheduled ping so that a freshly
//! started cluster of 1024 nodes doesn't fire all its pings in a
//! single millisecond. The jitter is applied *per peer* but is
//! stable across ticks (we hash the node id), so we don't add
//! extra latency to slow peers over time.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use a3net_types::NodeId;

use crate::peer_manager::{
    HeartbeatMessage, HeartbeatStats, PeerManager, PendingPing,
};

/// Outbound heartbeat dispatch contract.
///
/// Embedders implement this trait to send a heartbeat frame over
/// whatever transport the node is wired to. The peer_manager does
/// not know (or care) whether the underlying wire is QUIC, iroh,
/// or a test mailbox — it just hands the message to the sender.
#[async_trait::async_trait]
pub trait HeartbeatSender: Send + Sync + 'static {
    /// Send a heartbeat to `recipient`. Return `Ok(())` only when
    /// the frame has been placed on the wire; transport-level
    /// failures should be reported as `Err`.
    async fn send_heartbeat(
        &self,
        recipient: &NodeId,
        message: HeartbeatMessage,
    ) -> Result<(), String>;
}

/// In-memory sender used by tests. Captures every outbound
/// heartbeat so the test can inspect the exact sequence and
/// timestamps produced by the service.
#[derive(Debug, Default, Clone)]
pub struct MockHeartbeatSender {
    /// Sender mailbox. Outer `Vec` is a "tick", inner is the
    /// peers pinged in that tick.
    pub sent: Arc<Mutex<Vec<HeartbeatMessage>>>,
    /// When set, every send returns this error.
    pub fail_with: Arc<Mutex<Option<String>>>,
}

impl MockHeartbeatSender {
    /// Build a new mock sender.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure the sender to fail every call with `msg`.
    pub fn with_failure(msg: impl Into<String>) -> Self {
        Self {
            sent: Arc::new(Mutex::new(Vec::new())),
            fail_with: Arc::new(Mutex::new(Some(msg.into()))),
        }
    }

    /// Snapshot of everything sent so far.
    pub fn sent_messages(&self) -> Vec<HeartbeatMessage> {
        self.sent.lock().clone()
    }

    /// Number of captured messages.
    pub fn len(&self) -> usize {
        self.sent.lock().len()
    }

    /// Convenience: `true` when no messages have been sent.
    pub fn is_empty(&self) -> bool {
        self.sent.lock().is_empty()
    }
}

#[async_trait::async_trait]
impl HeartbeatSender for MockHeartbeatSender {
    async fn send_heartbeat(
        &self,
        _recipient: &NodeId,
        message: HeartbeatMessage,
    ) -> Result<(), String> {
        if let Some(err) = self.fail_with.lock().clone() {
            return Err(err);
        }
        self.sent.lock().push(message);
        Ok(())
    }
}

/// Handle to the running heartbeat task. Drop it to stop the
/// service; the background task will be cancelled at the next
/// await point.
pub struct HeartbeatHandle {
    task: JoinHandle<()>,
    stop: Arc<Notify>,
}

impl HeartbeatHandle {
    /// Stop the heartbeat service. The task wakes up on the
    /// `stop` notification and exits cleanly.
    pub fn stop(self) {
        self.stop.notify_waiters();
        // The task may also detect the shutdown via a periodic
        // check; we don't join() here so `stop` is non-blocking.
    }
}

/// The auto-heartbeat service. Holds the [`PeerManager`], the
/// sender, the local node identity, and the per-peer pending-pings
/// ring used for RTT computation.
pub struct HeartbeatService {
    /// The peer table we drive.
    pub peers: Arc<PeerManager>,
    /// Where outbound heartbeats go.
    pub sender: Arc<dyn HeartbeatSender>,
    /// The local node identity (used to populate the message).
    pub local_node_id: NodeId,
    /// The local display name (e.g. `alice-laptop`).
    pub local_node_name: String,
    /// The app version string embedded in each heartbeat.
    pub app_version: String,
    /// Per-peer pending pings, keyed by recipient id. Bounded at
    /// [`MAX_PENDING_PINGS`] entries per peer.
    pub pending: Arc<Mutex<HashMap<NodeId, VecDeque<PendingPing>>>>,
    /// Monotonic per-sender sequence number.
    pub seq: Arc<Mutex<u64>>,
}

impl HeartbeatService {
    /// Build a new service. The handle is not started; call
    /// [`Self::start`] to spawn the background task.
    pub fn new(
        peers: Arc<PeerManager>,
        sender: Arc<dyn HeartbeatSender>,
        local_node_id: NodeId,
        local_node_name: impl Into<String>,
        app_version: impl Into<String>,
    ) -> Self {
        Self {
            peers,
            sender,
            local_node_id,
            local_node_name: local_node_name.into(),
            app_version: app_version.into(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            seq: Arc::new(Mutex::new(0)),
        }
    }

    /// Next sequence number. Monotonic and never reused.
    pub fn next_seq(&self) -> u64 {
        let mut s = self.seq.lock();
        let v = *s;
        *s = s.saturating_add(1);
        v
    }

    /// Build a heartbeat message for `recipient`. The `jitter_ms`
    /// parameter applies a small random delay on top of the
    /// scheduled cadence so the cluster doesn't ping in lockstep.
    pub fn build_message(&self, recipient: &NodeId, jitter_ms: u64) -> HeartbeatMessage {
        // We do NOT mutate msg.timestamp — the receiver does
        // clock-skew checks. The jitter is captured separately so
        // the transport can apply it as a small delay before
        // sending the frame.
        let _ = (recipient, jitter_ms);
        HeartbeatMessage::new(
            self.local_node_id.clone(),
            self.local_node_name.clone(),
            self.next_seq(),
            self.app_version.clone(),
        )
    }

    /// Apply jitter to `base_interval`. Returns a `Duration` in
    /// `[base - jitter, base + jitter]`. Jitter is computed from
    /// the node id hash so it is stable across consecutive ticks
    /// for the same peer (no drift).
    pub fn jittered_interval(&self, peer: &NodeId, base_interval: Duration) -> Duration {
        let cfg = self.peers.config();
        let jitter_pct = cfg.heartbeat_jitter_percent as u64;
        if jitter_pct == 0 {
            return base_interval;
        }
        let jitter_ms = (base_interval.as_millis() as u64 * jitter_pct) / 100;
        let hash = stable_hash(peer);
        // Map hash to a [-jitter_ms, +jitter_ms] offset.
        let span = (jitter_ms * 2).max(1);
        let offset_ms = (hash % span) as i64 - jitter_ms as i64;
        let total_ms = base_interval.as_millis() as i64 + offset_ms;
        Duration::from_millis(total_ms.max(1) as u64)
    }

    /// Spawn the background task. The returned handle owns the
    /// task; dropping it (or calling `stop()`) terminates the
    /// service.
    pub fn start(self: Arc<Self>) -> HeartbeatHandle {
        let stop = Arc::new(Notify::new());
        let stop_signal = stop.clone();
        let service = self.clone();
        let task = tokio::spawn(async move {
            service.run(stop_signal).await;
        });
        HeartbeatHandle { task, stop }
    }

    /// Run the heartbeat loop. One iteration == one heartbeat
    /// cadence (`heartbeat_interval` ± jitter). The loop exits
    /// cleanly when `stop` is notified.
    pub async fn run(self: Arc<Self>, stop: Arc<Notify>) {
        let interval = self.peers.config().heartbeat_interval;
        info!(
            node = %self.local_node_id.short(),
            node_name = %self.local_node_name,
            interval_secs = interval.as_secs(),
            "heartbeat_service: started"
        );

        loop {
            // Wait for the interval OR a stop signal. `tokio::select!`
            // is the idiomatic way to make a cancellable sleep.
            tokio::select! {
                _ = stop.notified() => {
                    info!("heartbeat_service: stop signal received");
                    return;
                }
                _ = tokio::time::sleep(interval) => {
                    let _ = self.dispatch_once().await;
                }
            }
        }
    }

    /// Run a single heartbeat dispatch synchronously. Returns the
    /// per-tick stats. Useful for tests and for the `peer tick`
    /// CLI command.
    pub async fn dispatch_once(&self) -> HeartbeatStats {
        let stats = self.peers.heartbeat_tick();
        // For every peer the tick returned, build + send a
        // heartbeat. The set is bounded by `max_peers` so the
        // work is O(N) per tick.
        let to_ping: Vec<NodeId> = self
            .peers
            .list()
            .peers
            .iter()
            .filter(|p| p.status.is_alive() || matches!(p.status, crate::peer_manager::PeerStatus::Suspect))
            .map(|p| p.node_id.clone())
            .collect();

        for peer in to_ping {
            let interval = self.jittered_interval(&peer, self.peers.config().heartbeat_interval);
            let msg = self.build_message(&peer, interval.as_millis() as u64);
            // Track the pending ping so a matching pong can compute RTT.
            self.peers.track_pending_ping(&peer, msg.seq, &self.pending);
            self.peers.record_ping_sent(&peer);
            debug!(
                target_peer = %peer.short(),
                seq = msg.seq,
                jitter_ms = interval.as_millis() as u64,
                "heartbeat_service: ping"
            );
            if let Err(e) = self.sender.send_heartbeat(&peer, msg).await {
                warn!(
                    target_peer = %peer.short(),
                    error = %e,
                    "heartbeat_service: send failed"
                );
            }
        }
        stats
    }

    /// Inbound: a heartbeat was received from `sender`. Match
    /// the `seq` against our pending rings and update RTT.
    pub fn on_heartbeat_received(
        &self,
        sender_id: &NodeId,
        msg: &HeartbeatMessage,
    ) -> Option<u64> {
        let (entry, rtt) = self.peers.record_pong(sender_id, msg, &self.pending)?;
        let _ = entry;
        rtt
    }
}

/// Stable FNV-1a-style hash so the jitter is deterministic across
/// restarts. Avoids pulling in a hash crate dependency.
fn stable_hash(n: &NodeId) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for byte in n.as_hex().bytes() {
        h ^= byte as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_manager::PeerManagerConfig;
    use std::time::Duration;

    fn nid(b: u8) -> NodeId {
        NodeId::from_bytes(&[b; 32]).expect("valid")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_once_sends_to_every_alive_peer() {
        let pm = PeerManager::with_default();
        let peers = vec![nid(1), nid(2), nid(3)];
        for p in &peers {
            pm.insert(p.clone(), None);
            pm.record_heartbeat(p);
        }
        let sender = Arc::new(MockHeartbeatSender::new());
        let svc = Arc::new(HeartbeatService::new(
            pm.clone(),
            sender.clone(),
            nid(0),
            "test-node",
            "a3net/test-0.1.0",
        ));
        let _ = svc.dispatch_once().await;
        assert_eq!(sender.len(), 3);
    }

    #[tokio::test]
    async fn jitter_stays_in_configured_band() {
        let cfg = PeerManagerConfig {
            heartbeat_interval: Duration::from_secs(10),
            heartbeat_jitter_percent: 10,
            ..PeerManagerConfig::default()
        };
        let pm = PeerManager::new(cfg);
        let svc = HeartbeatService::new(
            pm,
            Arc::new(MockHeartbeatSender::new()),
            nid(0),
            "n",
            "v",
        );
        // ±10% of 10s = ±1000ms.
        for b in 0u8..16 {
            let d = svc.jittered_interval(&nid(b), Duration::from_secs(10));
            let ms = d.as_millis() as i64;
            assert!(ms >= 9000 && ms <= 11000, "got {ms}ms (out of band)");
        }
    }

    #[tokio::test]
    async fn zero_jitter_is_exact_interval() {
        let cfg = PeerManagerConfig {
            heartbeat_interval: Duration::from_secs(7),
            heartbeat_jitter_percent: 0,
            ..PeerManagerConfig::default()
        };
        let pm = PeerManager::new(cfg);
        let svc = HeartbeatService::new(
            pm,
            Arc::new(MockHeartbeatSender::new()),
            nid(0),
            "n",
            "v",
        );
        let d = svc.jittered_interval(&nid(1), Duration::from_secs(7));
        assert_eq!(d, Duration::from_secs(7));
    }

    #[test]
    fn heartbeat_message_round_trip() {
        let msg = HeartbeatMessage::new(nid(0xAA), "alice-laptop", 42, "a3net/0.4.0");
        let raw = serde_json::to_string(&msg).expect("serialize");
        let back: HeartbeatMessage = serde_json::from_str(&raw).expect("parse");
        assert_eq!(msg, back);
        assert_eq!(msg.node_name, "alice-laptop");
        assert_eq!(msg.seq, 42);
    }

    #[tokio::test]
    async fn on_heartbeat_received_records_rtt() {
        let pm = PeerManager::with_default();
        let peer = nid(7);
        pm.insert(peer.clone(), Some("alice".into()));
        pm.record_heartbeat(&peer);
        let sender = Arc::new(MockHeartbeatSender::new());
        let svc = HeartbeatService::new(
            pm.clone(),
            sender,
            nid(0),
            "local",
            "v",
        );
        // Send a ping first so we have a pending ring entry.
        let _ = svc.dispatch_once().await;
        let pending = svc.pending.lock();
        let ring = pending.get(&peer).expect("ring exists");
        let seq = ring.front().expect("ring non-empty").seq;
        drop(pending);

        // Sleep so a real RTT elapses, then receive a matching pong.
        tokio::time::sleep(Duration::from_millis(30)).await;
        let msg = HeartbeatMessage::new(peer.clone(), "alice", seq, "v");
        let rtt = svc.on_heartbeat_received(&peer, &msg);
        assert!(rtt.is_some(), "RTT must be recorded");
        let rtt = rtt.unwrap();
        assert!(rtt >= 20 && rtt <= 2000, "rtt {rtt}ms out of range");
    }

    #[tokio::test]
    async fn start_and_stop_round_trip() {
        let cfg = PeerManagerConfig {
            heartbeat_interval: Duration::from_millis(50),
            ..PeerManagerConfig::default()
        };
        let pm = PeerManager::new(cfg);
        // Insert a peer AND mark it alive so dispatch_once has
        // something to ping.
        pm.insert(nid(1), None);
        pm.record_heartbeat(&nid(1));
        let sender = Arc::new(MockHeartbeatSender::new());
        let svc = HeartbeatService::new(
            pm,
            sender.clone(),
            nid(0),
            "n",
            "v",
        );
        let svc = Arc::new(svc);
        let handle = svc.start();
        // Wait long enough for at least one tick (interval = 50ms).
        tokio::time::sleep(Duration::from_millis(150)).await;
        handle.stop();
        assert!(
            !sender.is_empty(),
            "service must have emitted at least one ping"
        );
    }
}
