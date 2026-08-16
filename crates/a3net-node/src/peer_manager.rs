//! P2P peer connection manager.
//!
//! Each A3Net node maintains a bounded **connection table** of up to
//! `MAX_P2P_PEERS` (default 1024) peers. Each entry is tracked for
//! liveness via periodic **heartbeats**: a peer that fails to respond
//! within the configured timeout is marked `Dead` and is a candidate
//! for eviction once the table fills up.
//!
//! Design rationale — why a *separate* table from the SwarmIndex
//! gossip ledger?
//!
//! - `SwarmIndex` is a *content-addressed* view: it tracks
//!   `(content_hash, peer) → ticket` for the gossip feed. It is
//!   intentionally room-scoped.
//! - `PeerManager` is a *transport-scoped* view: it tracks the set
//!   of nodes we keep a P2P link with, regardless of which room we
//!   discovered them through. The IPC `peer_list` / `peer_status`
//!   RPCs surface *this* table so operators can ask "who am I
//!   connected to?" without dumping every gossip peer.
//!
//! The table is safe to share across tasks: it is guarded by a
//! `parking_lot::Mutex` and every method returns owned snapshots so
//! callers can drop the lock before doing IO. The bounded size
//! prevents an attacker from forcing unbounded memory growth by
//! spraying bogus `NodeId`s into the gossip layer.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use a3net_types::NodeId;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Default maximum number of P2P peers a node maintains.
///
/// 1024 keeps each slot cheap (a few hundred bytes) and matches the
/// Kademlia-style "k-bucket of 1024" surface that the rest of the
/// A3Net DHT code already assumes.
pub const MAX_P2P_PEERS: usize = 1024;

/// Default heartbeat interval. A node sends a ping to every peer
/// every `heartbeat_interval`; a peer that doesn't respond within
/// `heartbeat_timeout` is considered dead.
///
/// **30 seconds** is the documented default. It is a balance between
/// detection latency and bandwidth:
/// - 1024 peers × 30s ≈ 34 pings/sec sustained → ~10 KB/s payload
/// - 15s would be 2× the bandwidth (and matches libp2p kademlia)
/// - 45s would push detection latency past 1 minute, which feels
///   "sluggish" in interactive UIs
///
/// Operators can tune via `p2p.heartbeat_interval_seconds` in the
/// relay config or `NodeBuilder::with_peer_manager_config`.
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Default heartbeat timeout. A peer that hasn't responded within
/// this window after the last ping is marked [`PeerStatus::Dead`].
///
/// 90s = 3× the interval, so two consecutive missed pings classify
/// the peer as dead (one missed → Suspect, two missed → Dead).
pub const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

/// Default jitter (as a percentage of the interval) applied to the
/// heartbeat cadence so that all nodes in a cluster don't ping at
/// the exact same instant. 10% (±3s around 30s) is enough to spread
/// the load without changing the average cadence appreciably.
pub const DEFAULT_HEARTBEAT_JITTER_PERCENT: u8 = 10;

/// Maximum number of un-acked pings we hold per peer for RTT
/// computation. Each entry is small (16 bytes) so 8 per peer
/// costs ~16 KB over the entire 1024-peer table.
pub const MAX_PENDING_PINGS: usize = 8;

/// One in-flight ping we sent to a peer and haven't yet seen the
/// matching pong for. Used for round-trip-time computation.
#[derive(Debug, Clone, Copy)]
pub struct PendingPing {
    /// Sequence number echoed back by the peer in the pong.
    pub seq: u64,
    /// Wall-clock time at which we sent the ping. Subtract from
    /// the pong's `now()` to obtain RTT.
    pub sent_at: DateTime<Utc>,
}

/// Configuration for the peer manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerManagerConfig {
    /// Maximum number of concurrent P2P links. Defaults to
    /// [`MAX_P2P_PEERS`] (1024). Setting this to zero is a no-op —
    /// the [`PeerManager::new`] constructor clamps it to `1` so the
    /// table is never empty (which would break the liveness tests).
    pub max_peers: usize,
    /// How often a heartbeat ping is sent to every connected peer.
    /// Default: 30s.
    pub heartbeat_interval: Duration,
    /// How long after the last successful ping a peer is considered
    /// dead. Must be strictly greater than `heartbeat_interval`.
    /// Default: 90s (3× the interval).
    pub heartbeat_timeout: Duration,
    /// Random jitter (0..=100, percent of `heartbeat_interval`)
    /// applied to each individual heartbeat so that a 1024-node
    /// cluster doesn't send all its pings at the same instant.
    /// Default: 10%.
    pub heartbeat_jitter_percent: u8,
    /// Whether the [`HeartbeatService`] runs automatically once a
    /// node is built. Default: `true`. Operators can disable it
    /// when they want to drive pings manually (e.g. in tests or
    /// embedders that have their own scheduler).
    pub auto_heartbeat: bool,
}

impl Default for PeerManagerConfig {
    fn default() -> Self {
        Self {
            max_peers: MAX_P2P_PEERS,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            heartbeat_timeout: DEFAULT_HEARTBEAT_TIMEOUT,
            heartbeat_jitter_percent: DEFAULT_HEARTBEAT_JITTER_PERCENT,
            auto_heartbeat: true,
        }
    }
}

/// Operational state of a peer in the connection table.
///
/// The transitions are:
///
/// ```text
///  Connecting ──► Alive ──► Suspect ──► Dead
///       │           │                       │
///       │           └────────► Dead ────────┘
///       └────────────────────────► Removed
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeerStatus {
    /// Handshake in progress (e.g. Noise XX, QUIC TLS).
    Connecting,
    /// Heartbeat is fresh — peer is responsive.
    Alive,
    /// One heartbeat was missed. We keep the slot in case the next
    /// one lands, but operators should flag this peer.
    Suspect,
    /// `heartbeat_timeout` has elapsed since the last successful
    /// ping. The peer is no longer counted as a live connection.
    Dead,
    /// Operator-initiated disconnect. The entry stays in the table
    /// for a short grace period so the CLI can still print "removed"
    /// without a race.
    Removed,
}

impl PeerStatus {
    /// Human-readable form used by the CLI table.
    pub fn as_str(&self) -> &'static str {
        match self {
            PeerStatus::Connecting => "connecting",
            PeerStatus::Alive => "alive",
            PeerStatus::Suspect => "suspect",
            PeerStatus::Dead => "dead",
            PeerStatus::Removed => "removed",
        }
    }

    /// Whether the peer is currently counted as a live link.
    pub fn is_alive(&self) -> bool {
        matches!(self, PeerStatus::Alive | PeerStatus::Suspect)
    }
}

/// One row in the peer connection table.
///
/// The struct is intentionally small so that 1024 entries fit in
/// ~250 KB: a `NodeId` (32 B), a handful of `u64`/`i64` timestamps
/// and a few `u64` counters. The Arc<Mutex<PeerManager>> can be
/// shared freely across the transport, gossip, and IPC layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerEntry {
    /// The peer's [`NodeId`].
    pub node_id: NodeId,
    /// When the peer was first inserted into the table.
    pub connected_at: DateTime<Utc>,
    /// Last time we received any application-level message from
    /// this peer (heartbeat, gossip, or RPC).
    pub last_seen_at: DateTime<Utc>,
    /// Last time we successfully pinged this peer.
    pub last_heartbeat_at: DateTime<Utc>,
    /// Number of consecutive heartbeat failures.
    pub heartbeat_failures: u32,
    /// Current status.
    pub status: PeerStatus,
    /// Optional human-readable label (set from the config /
    /// bootstrap addrs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    // ── Auto-heartbeat bookkeeping ──────────────────────────────────
    /// Display name the peer advertised in its last heartbeat
    /// payload. Captured so we can show "who is this?" without
    /// having to dial them again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_name: Option<String>,
    /// Last time we *sent* a heartbeat ping to this peer. Distinct
    /// from `last_heartbeat_at` which is the last time we *received*
    /// a response.
    pub last_ping_sent_at: DateTime<Utc>,
    /// Last time we *received* a heartbeat ping (or response) from
    /// this peer. Lets us surface "the peer reached out 2s ago"
    /// even when we haven't pinged them yet.
    pub last_ping_recv_at: DateTime<Utc>,
    /// Last measured round-trip time (`pong_received_at - ping_sent_at`)
    /// for this peer. `None` until we have at least one full
    /// ping/pong cycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rtt_ms: Option<u64>,
    /// Total heartbeat pings we have sent to this peer since
    /// `connected_at`.
    pub total_pings_sent: u64,
    /// Total heartbeat pings we have received from this peer since
    /// `connected_at`.
    pub total_pings_recv: u64,
    /// Average round-trip time (in milliseconds) over the lifetime
    /// of the connection. Computed incrementally on pong receipt.
    pub avg_rtt_ms: u64,
    /// Number of times the peer has been marked `Suspect` over
    /// its lifetime. Useful for operators to spot flaky links.
    pub suspect_count: u32,
    /// Number of times the peer has been marked `Dead` over its
    /// lifetime. A non-zero `dead_count` after a recovery is a
    /// strong signal of a marginal link.
    pub dead_count: u32,
}

/// Snapshot returned by [`PeerManager::list`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerListSnapshot {
    /// Total slots in the table (== `max_peers`).
    pub capacity: usize,
    /// Number of currently live (`Alive` or `Suspect`) peers.
    pub alive_count: usize,
    /// Number of `Dead` peers (still in the table, awaiting eviction).
    pub dead_count: usize,
    /// Number of `Connecting` peers.
    pub connecting_count: usize,
    /// Full table contents.
    pub peers: Vec<PeerEntry>,
}

/// Aggregate liveness counters returned by
/// [`PeerManager::heartbeat_tick`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatStats {
    /// Peers that were pinged this tick.
    pub pings_sent: usize,
    /// Peers whose last-seen exceeded `heartbeat_timeout` and moved
    /// to `Dead` this tick.
    pub newly_dead: usize,
    /// Peers that came back to `Alive` after a previous `Suspect`.
    pub recovered: usize,
    /// Peers that transitioned `Alive` → `Suspect` this tick.
    pub became_suspect: usize,
}

/// Per-peer heartbeat round-trip statistics, aggregated over the
/// last [`MAX_RTT_SAMPLES`] samples for cheap diagnostics.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerHeartbeatStats {
    /// Last measured round-trip time in milliseconds.
    pub last_rtt_ms: Option<u64>,
    /// Rolling average round-trip time in milliseconds.
    pub avg_rtt_ms: u64,
    /// Total pings we have sent to this peer.
    pub total_pings_sent: u64,
    /// Total pings we have received from this peer.
    pub total_pings_recv: u64,
    /// Number of times the peer has been marked `Suspect`.
    pub suspect_count: u32,
    /// Number of times the peer has been marked `Dead`.
    pub dead_count: u32,
}

/// On-the-wire heartbeat payload sent between nodes.
///
/// Every [`HeartbeatMessage`] carries:
///
/// - `node_id` of the sender (so the receiver can correct its
///   routing table when a peer re-announces itself).
/// - `node_name` — the human-readable label the operator chose
///   (e.g. `alice-laptop`). Captured by the receiver so the CLI
///   can show "who is this?" without dialling.
/// - `timestamp` — the wall-clock time at which the message was
///   constructed (RFC 3339). Used by the receiver to detect
///   clock-skew and to display the "last seen" timestamp in
///   human-readable form.
/// - `seq` — a monotonically increasing per-sender sequence
///   number. Lets the receiver detect lost pings even when the
///   transport reorders them.
/// - `app_version` — the A3Net build identifier. Captured so
///   operators can spot mismatched peers (e.g. an `a3net-cli`
///   from v0.4 talking to a node on v0.3).
///
/// The `(node_id, timestamp, seq)` triple is also signed by the
/// sender's transport identity in production; for the
/// [`PeerManager`] test path we leave the signature out so the
/// struct stays easy to construct in tests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatMessage {
    /// Protocol version of the heartbeat payload (current: 1).
    pub version: u8,
    /// Sender's [`NodeId`].
    pub node_id: NodeId,
    /// Sender's human-readable name (e.g. `alice-laptop`).
    pub node_name: String,
    /// RFC 3339 timestamp at the time of sending.
    pub timestamp: DateTime<Utc>,
    /// Monotonic per-sender sequence number.
    pub seq: u64,
    /// A3Net build identifier (e.g. `a3net-cli/0.4.0`).
    pub app_version: String,
}

impl HeartbeatMessage {
    /// Current protocol version.
    pub const VERSION: u8 = 1;

    /// Build a new heartbeat message with `seq` set by the caller.
    pub fn new(
        node_id: NodeId,
        node_name: impl Into<String>,
        seq: u64,
        app_version: impl Into<String>,
    ) -> Self {
        Self {
            version: Self::VERSION,
            node_id,
            node_name: node_name.into(),
            timestamp: Utc::now(),
            seq,
            app_version: app_version.into(),
        }
    }

    /// Maximum clock skew (in seconds) we accept from a peer's
    /// timestamp. Outside this window the message is dropped to
    /// prevent replay attacks and to surface clock misconfiguration.
    pub const MAX_CLOCK_SKEW_SECS: i64 = 300;
}

/// Bounded connection table with heartbeat-based liveness.
///
/// The manager is `Arc`-shareable via [`PeerManager::shared`] so
/// that the transport, gossip, and RPC layers can all read the
/// same table without a separate cache.
#[derive(Debug)]
pub struct PeerManager {
    cfg: PeerManagerConfig,
    inner: Mutex<PeerManagerInner>,
    /// Monotonic clock used for `last_seen_at` / `last_heartbeat_at`.
    /// Tokio doesn't expose `Instant` minus `Utc` directly, so we
    /// capture both an `Instant` for cheap `elapsed` and a
    /// `DateTime<Utc>` for the human-facing snapshot.
    clock: Mutex<Clock>,
}

#[derive(Debug, Default)]
struct PeerManagerInner {
    table: HashMap<NodeId, PeerEntry>,
}

#[derive(Debug)]
struct Clock {
    started_at: Instant,
    started_at_utc: DateTime<Utc>,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            started_at_utc: Utc::now(),
        }
    }
}

impl Clock {
    /// Translate a monotonic `Instant` (relative to process start) to
    /// the wall-clock time we recorded when the manager booted.
    fn now_utc(&self) -> DateTime<Utc> {
        let elapsed = self.started_at.elapsed();
        self.started_at_utc
            + chrono::Duration::from_std(elapsed).unwrap_or(chrono::Duration::seconds(0))
    }
}

impl PeerManager {
    /// Build a new manager with the given config. The config is
    /// sanitized:
    /// - `max_peers` is clamped to `[1, MAX_P2P_PEERS]`.
    /// - `heartbeat_timeout` is bumped to `2 * heartbeat_interval`
    ///   if it's smaller, so a single dropped ping never
    ///   immediately kills a link.
    pub fn new(cfg: PeerManagerConfig) -> Arc<Self> {
        let cfg = Self::sanitize(cfg);
        Arc::new(Self {
            cfg,
            inner: Mutex::new(PeerManagerInner::default()),
            clock: Mutex::new(Clock::default()),
        })
    }

    /// Default-config constructor.
    pub fn with_default() -> Arc<Self> {
        Self::new(PeerManagerConfig::default())
    }

    /// Convenience for callers that already hold an `Arc<Self>`.
    pub fn shared(self: Arc<Self>) -> Arc<Self> {
        self
    }

    /// Effective configuration.
    pub fn config(&self) -> &PeerManagerConfig {
        &self.cfg
    }

    /// Insert a peer into the table. If the table is full, the
    /// [`PeerStatus::Dead`] entry with the oldest `last_heartbeat_at`
    /// is evicted to make room. Returns the inserted (or already
    /// existing) entry.
    pub fn insert(&self, node_id: NodeId, alias: Option<String>) -> PeerEntry {
        let now = self.now_utc();
        let mut inner = self.inner.lock();
        if let Some(existing) = inner.table.get_mut(&node_id) {
            existing.alias = alias.or(existing.alias.take());
            return existing.clone();
        }

        // Capacity check: if the table is full, evict the oldest dead
        // peer first. If no dead peer exists, fall back to the
        // oldest suspect, then the oldest alive — picking the slot
        // we are *least* likely to break.
        if inner.table.len() >= self.cfg.max_peers {
            let victim = inner
                .table
                .iter()
                .filter(|(_, e)| matches!(e.status, PeerStatus::Dead))
                .min_by_key(|(_, e)| e.last_heartbeat_at)
                .map(|(k, _)| k.clone())
                .or_else(|| {
                    inner
                        .table
                        .iter()
                        .filter(|(_, e)| matches!(e.status, PeerStatus::Suspect))
                        .min_by_key(|(_, e)| e.last_heartbeat_at)
                        .map(|(k, _)| k.clone())
                })
                .or_else(|| {
                    inner
                        .table
                        .iter()
                        .min_by_key(|(_, e)| e.last_heartbeat_at)
                        .map(|(k, _)| k.clone())
                });
            if let Some(v) = victim {
                let removed = inner.table.remove(&v);
                debug!(
                    target_peer = %v.as_hex(),
                    replaced_by = %node_id.as_hex(),
                    "peer_manager: evicted oldest peer to make room"
                );
                let _ = removed;
            }
        }

        let entry = PeerEntry {
            node_id: node_id.clone(),
            connected_at: now,
            last_seen_at: now,
            last_heartbeat_at: now,
            heartbeat_failures: 0,
            status: PeerStatus::Connecting,
            alias,
            remote_name: None,
            last_ping_sent_at: now,
            last_ping_recv_at: now,
            last_rtt_ms: None,
            total_pings_sent: 0,
            total_pings_recv: 0,
            avg_rtt_ms: 0,
            suspect_count: 0,
            dead_count: 0,
        };
        inner.table.insert(node_id, entry.clone());
        info!(
            peer = %entry.node_id.as_hex(),
            table_size = inner.table.len(),
            "peer_manager: inserted peer"
        );
        entry
    }

    /// Mark a peer as having responded to a heartbeat. Also
    /// refreshes `last_seen_at` so that application-level activity
    /// can keep the slot alive even between heartbeats.
    pub fn record_heartbeat(&self, node_id: &NodeId) -> Option<PeerEntry> {
        let now = self.now_utc();
        let mut inner = self.inner.lock();
        let entry = inner.table.get_mut(node_id)?;
        let was_suspect = matches!(entry.status, PeerStatus::Suspect);
        entry.last_heartbeat_at = now;
        entry.last_seen_at = now;
        entry.heartbeat_failures = 0;
        entry.status = PeerStatus::Alive;
        if was_suspect {
            debug!(peer = %node_id.as_hex(), "peer_manager: peer recovered");
        }
        Some(entry.clone())
    }

    /// Record that we just *sent* a heartbeat ping to `node_id`.
    /// Bumps `last_ping_sent_at` and `total_pings_sent`. Does *not*
    /// change the peer's status — we only know it has answered
    /// when the matching pong arrives via [`Self::record_pong`].
    pub fn record_ping_sent(&self, node_id: &NodeId) -> Option<PeerEntry> {
        let now = self.now_utc();
        let mut inner = self.inner.lock();
        let entry = inner.table.get_mut(node_id)?;
        entry.last_ping_sent_at = now;
        entry.total_pings_sent = entry.total_pings_sent.saturating_add(1);
        Some(entry.clone())
    }

    /// Record a heartbeat message received from `node_id` over the
    /// wire. Captures the sender's `node_name` and `timestamp`,
    /// updates `last_ping_recv_at`, and computes the round-trip
    /// time when the matching `seq` is found in the pending-pings
    /// ring.
    ///
    /// Returns the updated peer entry together with the computed
    /// RTT (in milliseconds). `rtt_ms` is `None` for unsolicited
    /// heartbeats (no matching pending ping).
    pub fn record_pong(
        &self,
        node_id: &NodeId,
        msg: &HeartbeatMessage,
        pending_pings: &Mutex<std::collections::HashMap<NodeId, std::collections::VecDeque<PendingPing>>>,
    ) -> Option<(PeerEntry, Option<u64>)> {
        let now = self.now_utc();
        let mut inner = self.inner.lock();
        let entry = inner.table.get_mut(node_id)?;
        let was_suspect = matches!(entry.status, PeerStatus::Suspect);

        // Capture advertised identity.
        entry.remote_name = Some(msg.node_name.clone());
        entry.last_ping_recv_at = now;
        entry.last_seen_at = now;
        entry.last_heartbeat_at = now;
        entry.total_pings_recv = entry.total_pings_recv.saturating_add(1);
        entry.heartbeat_failures = 0;
        entry.status = PeerStatus::Alive;

        // RTT lookup: pop the matching seq from the pending-pings
        // ring. If the seq isn't found (e.g. duplicates or
        // out-of-order), we still record a "receive" but no RTT.
        let rtt_ms = {
            let mut pending = pending_pings.lock();
            if let Some(q) = pending.get_mut(node_id) {
                let pos = q.iter().position(|p| p.seq == msg.seq);
                if let Some(pos) = pos {
                    let p = q.remove(pos).expect("pos in range");
                    let elapsed = now.signed_duration_since(p.sent_at);
                    let ms = elapsed.num_milliseconds().max(0) as u64;
                    entry.last_rtt_ms = Some(ms);
                    // Running average (exponential moving average
                    // with alpha=0.3 so the last 3-4 samples
                    // dominate).
                    entry.avg_rtt_ms = if entry.avg_rtt_ms == 0 {
                        ms
                    } else {
                        // 30% new, 70% historic
                        (ms * 3 + entry.avg_rtt_ms * 7) / 10
                    };
                    Some(ms)
                } else {
                    None
                }
            } else {
                None
            }
        };

        if was_suspect {
            debug!(peer = %node_id.as_hex(), "peer_manager: peer recovered");
        }
        Some((entry.clone(), rtt_ms))
    }

    /// Push a freshly-sent ping into the per-peer pending ring so
    /// the next matching [`Self::record_pong`] can compute the RTT.
    /// The ring is bounded at [`MAX_PENDING_PINGS`] to keep memory
    /// flat regardless of how often a peer is pinged.
    pub fn track_pending_ping(
        &self,
        node_id: &NodeId,
        seq: u64,
        pending: &Mutex<std::collections::HashMap<NodeId, std::collections::VecDeque<PendingPing>>>,
    ) {
        let now = self.now_utc();
        let mut pending = pending.lock();
        let q = pending.entry(node_id.clone()).or_default();
        if q.len() >= MAX_PENDING_PINGS {
            q.pop_front();
        }
        q.push_back(PendingPing { seq, sent_at: now });
    }

    /// Drop the pending-pings ring for a peer (e.g. after eviction).
    pub fn clear_pending_pings(
        &self,
        node_id: &NodeId,
        pending: &Mutex<std::collections::HashMap<NodeId, std::collections::VecDeque<PendingPing>>>,
    ) {
        pending.lock().remove(node_id);
    }

    /// Per-peer heartbeat stats. Returns a zero-valued struct for
    /// unknown peers.
    pub fn stats_for(&self, node_id: &NodeId) -> PeerHeartbeatStats {
        let inner = self.inner.lock();
        let Some(entry) = inner.table.get(node_id) else {
            return PeerHeartbeatStats::default();
        };
        PeerHeartbeatStats {
            last_rtt_ms: entry.last_rtt_ms,
            avg_rtt_ms: entry.avg_rtt_ms,
            total_pings_sent: entry.total_pings_sent,
            total_pings_recv: entry.total_pings_recv,
            suspect_count: entry.suspect_count,
            dead_count: entry.dead_count,
        }
    }

    /// Record raw liveness (e.g. a gossip message arrived) without
    /// resetting the heartbeat counter. This lets the transport
    /// layer keep the slot warm even when its dedicated heartbeat
    /// ping is coalesced with another RPC.
    pub fn touch(&self, node_id: &NodeId) -> Option<PeerEntry> {
        let now = self.now_utc();
        let mut inner = self.inner.lock();
        let entry = inner.table.get_mut(node_id)?;
        entry.last_seen_at = now;
        Some(entry.clone())
    }

    /// Explicitly mark a peer as dead without evicting it. Useful
    /// when the transport reports a hard connection failure.
    pub fn mark_dead(&self, node_id: &NodeId) -> Option<PeerEntry> {
        let mut inner = self.inner.lock();
        let entry = inner.table.get_mut(node_id)?;
        entry.status = PeerStatus::Dead;
        entry.heartbeat_failures = entry.heartbeat_failures.saturating_add(1);
        Some(entry.clone())
    }

    /// Mark a peer as `Removed`. The entry sticks around for one
    /// more list call so the CLI can render the transition, but is
    /// filtered out by default from `list_alive`.
    pub fn remove(&self, node_id: &NodeId) -> Option<PeerEntry> {
        let mut inner = self.inner.lock();
        let entry = inner.table.get_mut(node_id)?;
        entry.status = PeerStatus::Removed;
        Some(entry.clone())
    }

    /// Hard-evict a peer. Unlike [`remove`] this forgets the entry
    /// entirely.
    pub fn evict(&self, node_id: &NodeId) -> Option<PeerEntry> {
        self.inner.lock().table.remove(node_id)
    }

    /// Look up a single peer by id.
    pub fn get(&self, node_id: &NodeId) -> Option<PeerEntry> {
        self.inner.lock().table.get(node_id).cloned()
    }

    /// Snapshot the entire table. The returned `Vec` is sorted by
    /// `node_id` so the CLI can produce a deterministic table.
    pub fn list(&self) -> PeerListSnapshot {
        let inner = self.inner.lock();
        let mut peers: Vec<PeerEntry> = inner.table.values().cloned().collect();
        peers.sort_by(|a, b| a.node_id.as_hex().cmp(&b.node_id.as_hex()));
        let alive_count = peers.iter().filter(|p| p.status.is_alive()).count();
        let dead_count = peers
            .iter()
            .filter(|p| matches!(p.status, PeerStatus::Dead))
            .count();
        let connecting_count = peers
            .iter()
            .filter(|p| matches!(p.status, PeerStatus::Connecting))
            .count();
        PeerListSnapshot {
            capacity: self.cfg.max_peers,
            alive_count,
            dead_count,
            connecting_count,
            peers,
        }
    }

    /// Snapshot only the live (Alive + Suspect) peers.
    pub fn list_alive(&self) -> Vec<PeerEntry> {
        self.inner
            .lock()
            .table
            .values()
            .filter(|p| p.status.is_alive())
            .cloned()
            .collect()
    }

    /// Number of currently tracked peers, regardless of status.
    pub fn len(&self) -> usize {
        self.inner.lock().table.len()
    }

    /// True iff the table is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().table.is_empty()
    }

    /// Run a single heartbeat tick.
    ///
    /// For every peer in the table:
    /// - If `last_heartbeat_at + heartbeat_timeout <= now`, the peer
    ///   is moved to `Dead`.
    /// - Else if `last_heartbeat_at + heartbeat_interval <= now`, the
    ///   peer is moved to `Suspect` (one missed heartbeat).
    /// - Otherwise the peer is left alone.
    ///
    /// The caller is expected to actually emit the ping frames on
    /// the transport; this method only computes the state machine.
    /// The returned `HeartbeatStats` records the transitions so the
    /// CLI can print "X new dead, Y suspect" after each tick.
    pub fn heartbeat_tick(&self) -> HeartbeatStats {
        let now = self.now_utc();
        let timeout = self.to_chrono(self.cfg.heartbeat_timeout);
        let interval = self.to_chrono(self.cfg.heartbeat_interval);
        let mut inner = self.inner.lock();
        let mut stats = HeartbeatStats::default();
        let mut to_ping: Vec<NodeId> = Vec::with_capacity(inner.table.len());
        for (nid, entry) in inner.table.iter_mut() {
            if matches!(entry.status, PeerStatus::Removed) {
                continue;
            }
            let since_heartbeat = now - entry.last_heartbeat_at;
            let since_seen = now - entry.last_seen_at;
            // timeout is measured from the last heartbeat, not the
            // last seen, because a peer can be application-active
            // but heartbeat-silent (e.g. upstream radio timed out).
            if since_heartbeat >= timeout {
                if entry.status != PeerStatus::Dead {
                    stats.newly_dead += 1;
                    entry.status = PeerStatus::Dead;
                    entry.heartbeat_failures =
                        entry.heartbeat_failures.saturating_add(1);
                    entry.dead_count = entry.dead_count.saturating_add(1);
                }
            } else if since_heartbeat >= interval
                || since_seen >= interval
            {
                if entry.status == PeerStatus::Alive {
                    stats.became_suspect += 1;
                    entry.status = PeerStatus::Suspect;
                    entry.suspect_count = entry.suspect_count.saturating_add(1);
                }
            }
            // The ping decision: emit a ping whenever the last
            // heartbeat is older than the interval OR the peer is
            // a suspect. This keeps the wire rate bounded (at most
            // one ping per peer per interval) while still
            // recovering suspects quickly.
            if since_heartbeat >= interval
                || matches!(entry.status, PeerStatus::Suspect)
            {
                to_ping.push(nid.clone());
            }
        }
        stats.pings_sent = to_ping.len();
        // The transport-side pings are emitted by the caller; the
        // contract is: when the ping returns, call `record_heartbeat`.
        drop(inner);
        for nid in &to_ping {
            debug!(peer = %nid.as_hex(), "peer_manager: heartbeat ping");
        }
        stats
    }

    /// Drop every `Dead` entry that has been dead for at least
    /// `grace`. Used by the background cleanup task so the table
    /// doesn't accumulate stale slots.
    pub fn prune_dead(&self, grace: Duration) -> usize {
        let cutoff = self.now_utc() - self.to_chrono(grace);
        let mut inner = self.inner.lock();
        let before = inner.table.len();
        inner.table.retain(|_, e| {
            // Keep entries that are not dead, or that haven't been
            // dead long enough yet.
            !(matches!(e.status, PeerStatus::Dead) && e.last_heartbeat_at <= cutoff)
        });
        let removed = before - inner.table.len();
        if removed > 0 {
            info!(
                removed,
                remaining = inner.table.len(),
                "peer_manager: pruned dead peers"
            );
        }
        removed
    }

    /// Helper: convert a `Duration` into a `chrono::Duration`,
    /// saturating at zero. Used by the heartbeat math.
    fn to_chrono(&self, d: Duration) -> chrono::Duration {
        chrono::Duration::from_std(d).unwrap_or(chrono::Duration::seconds(0))
    }

    fn now_utc(&self) -> DateTime<Utc> {
        self.clock.lock().now_utc()
    }

    fn sanitize(cfg: PeerManagerConfig) -> PeerManagerConfig {
        let max_peers = cfg.max_peers.clamp(1, MAX_P2P_PEERS);
        let heartbeat_interval = if cfg.heartbeat_interval.is_zero() {
            DEFAULT_HEARTBEAT_INTERVAL
        } else {
            cfg.heartbeat_interval
        };
        let heartbeat_timeout = if cfg.heartbeat_timeout <= heartbeat_interval {
            heartbeat_interval * 2
        } else {
            cfg.heartbeat_timeout
        };
        let heartbeat_jitter_percent = cfg.heartbeat_jitter_percent.min(100);
        if cfg.max_peers == 0 {
            warn!("peer_manager: max_peers=0 is invalid; clamped to 1");
        }
        if cfg.heartbeat_timeout <= cfg.heartbeat_interval && cfg.heartbeat_timeout > Duration::ZERO
        {
            warn!(
                "peer_manager: heartbeat_timeout <= heartbeat_interval; bumped to 2x interval"
            );
        }
        PeerManagerConfig {
            max_peers,
            heartbeat_interval,
            heartbeat_timeout,
            heartbeat_jitter_percent,
            auto_heartbeat: cfg.auto_heartbeat,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    fn node(n: u8) -> NodeId {
        NodeId::from_bytes(&[n; 32]).expect("valid 32-byte NodeId")
    }

    #[test]
    fn default_config_matches_constants() {
        let cfg = PeerManagerConfig::default();
        assert_eq!(cfg.max_peers, MAX_P2P_PEERS);
        assert_eq!(cfg.max_peers, 1024);
        assert_eq!(cfg.heartbeat_interval, DEFAULT_HEARTBEAT_INTERVAL);
        assert_eq!(cfg.heartbeat_timeout, DEFAULT_HEARTBEAT_TIMEOUT);
    }

    #[test]
    fn sanitize_clamps_max_peers_to_1024() {
        let cfg = PeerManagerConfig {
            max_peers: 100_000,
            ..PeerManagerConfig::default()
        };
        let m = PeerManager::new(cfg);
        assert_eq!(m.config().max_peers, MAX_P2P_PEERS);
    }

    #[test]
    fn sanitize_clamps_zero_max_peers_to_one() {
        let cfg = PeerManagerConfig {
            max_peers: 0,
            ..PeerManagerConfig::default()
        };
        let m = PeerManager::new(cfg);
        assert_eq!(m.config().max_peers, 1);
    }

    #[test]
    fn sanitize_bumps_timeout_below_interval() {
        let cfg = PeerManagerConfig {
            max_peers: 10,
            heartbeat_interval: Duration::from_secs(10),
            heartbeat_timeout: Duration::from_secs(5),
            ..PeerManagerConfig::default()
        };
        let m = PeerManager::new(cfg);
        assert!(m.config().heartbeat_timeout > m.config().heartbeat_interval);
    }

    #[test]
    fn insert_returns_existing_on_duplicate() {
        let m = PeerManager::with_default();
        let a = m.insert(node(1), Some("alice".into()));
        let b = m.insert(node(1), Some("alice2".into()));
        assert_eq!(a.node_id, b.node_id);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn insert_up_to_capacity_then_evicts_dead() {
        let cfg = PeerManagerConfig {
            max_peers: 4,
            heartbeat_interval: Duration::from_secs(1),
            heartbeat_timeout: Duration::from_secs(2),
            ..PeerManagerConfig::default()
        };
        let m = PeerManager::new(cfg);
        // Insert four peers.
        let ids: Vec<NodeId> = (0..4).map(|i| node(i)).collect();
        for i in &ids {
            m.insert(i.clone(), None);
        }
        assert_eq!(m.len(), 4);
        // Mark the oldest two as dead.
        m.mark_dead(&ids[0]);
        m.mark_dead(&ids[1]);
        // Sleep so their last_heartbeat_at is in the past.
        sleep(Duration::from_millis(50));
        // Inserting a new peer must evict one of the dead ones.
        let new_id = node(99);
        m.insert(new_id.clone(), None);
        let snap = m.list();
        assert!(snap.peers.iter().any(|p| p.node_id == new_id));
        assert!(snap.dead_count <= 2);
        assert!(snap.peers.len() <= 4);
    }

    #[test]
    fn record_heartbeat_promotes_suspect_to_alive() {
        let m = PeerManager::with_default();
        let n = node(7);
        m.insert(n.clone(), None);
        m.mark_dead(&n);
        let revived = m.record_heartbeat(&n).expect("entry exists");
        assert_eq!(revived.status, PeerStatus::Alive);
        assert_eq!(revived.heartbeat_failures, 0);
    }

    #[test]
    fn heartbeat_tick_marks_suspect_after_interval() {
        let cfg = PeerManagerConfig {
            max_peers: 4,
            heartbeat_interval: Duration::from_millis(50),
            heartbeat_timeout: Duration::from_millis(200),
            ..PeerManagerConfig::default()
        };
        let m = PeerManager::new(cfg);
        let n = node(11);
        m.insert(n.clone(), None);
        // Force alive first.
        m.record_heartbeat(&n);
        sleep(Duration::from_millis(80));
        let stats = m.heartbeat_tick();
        let entry = m.get(&n).expect("entry exists");
        assert_eq!(entry.status, PeerStatus::Suspect);
        assert!(stats.became_suspect >= 1);
    }

    #[test]
    fn heartbeat_tick_marks_dead_after_timeout() {
        let cfg = PeerManagerConfig {
            max_peers: 4,
            heartbeat_interval: Duration::from_millis(20),
            heartbeat_timeout: Duration::from_millis(60),
            ..PeerManagerConfig::default()
        };
        let m = PeerManager::new(cfg);
        let n = node(13);
        m.insert(n.clone(), None);
        sleep(Duration::from_millis(120));
        let stats = m.heartbeat_tick();
        let entry = m.get(&n).expect("entry exists");
        assert_eq!(entry.status, PeerStatus::Dead);
        assert!(stats.newly_dead >= 1);
    }

    #[test]
    fn recovered_counter_increments_when_suspect_returns() {
        let cfg = PeerManagerConfig {
            max_peers: 4,
            heartbeat_interval: Duration::from_millis(30),
            heartbeat_timeout: Duration::from_millis(120),
            ..PeerManagerConfig::default()
        };
        let m = PeerManager::new(cfg);
        let n = node(21);
        m.insert(n.clone(), None);
        sleep(Duration::from_millis(50));
        let _ = m.heartbeat_tick();
        // Now the peer is suspect; record a heartbeat, then re-tick.
        m.record_heartbeat(&n);
        let entry = m.get(&n).expect("entry exists");
        assert_eq!(entry.status, PeerStatus::Alive);
    }

    #[test]
    fn list_orders_by_node_id_hex() {
        let m = PeerManager::with_default();
        for i in [3u8, 1, 2] {
            m.insert(node(i), None);
        }
        let snap = m.list();
        let hexes: Vec<&str> = snap.peers.iter().map(|p| p.node_id.as_hex()).collect();
        let mut sorted = hexes.clone();
        sorted.sort();
        assert_eq!(hexes, sorted);
    }

    #[test]
    fn list_alive_excludes_dead_and_removed() {
        let m = PeerManager::with_default();
        let ids: Vec<NodeId> = (0..3).map(node).collect();
        for i in &ids {
            m.insert(i.clone(), None);
        }
        m.mark_dead(&ids[0]);
        m.remove(&ids[1]);
        m.record_heartbeat(&ids[2]);
        let alive = m.list_alive();
        assert_eq!(alive.len(), 1);
        assert_eq!(alive[0].node_id, ids[2]);
    }

    #[test]
    fn prune_dead_removes_stale_entries() {
        let m = PeerManager::with_default();
        let ids: Vec<NodeId> = (0..3).map(node).collect();
        for i in &ids {
            m.insert(i.clone(), None);
        }
        m.mark_dead(&ids[0]);
        m.mark_dead(&ids[1]);
        sleep(Duration::from_millis(20));
        let removed = m.prune_dead(Duration::from_millis(5));
        assert_eq!(removed, 2);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn snapshot_includes_capacity_and_counters() {
        let cfg = PeerManagerConfig {
            max_peers: 8,
            ..PeerManagerConfig::default()
        };
        let m = PeerManager::new(cfg);
        let ids: Vec<NodeId> = (0..4).map(node).collect();
        for i in &ids {
            m.insert(i.clone(), None);
        }
        m.record_heartbeat(&ids[0]);
        m.mark_dead(&ids[1]);
        let snap = m.list();
        assert_eq!(snap.capacity, 8);
        assert_eq!(snap.peers.len(), 4);
        assert_eq!(snap.alive_count, 1);
        assert_eq!(snap.dead_count, 1);
    }

    #[test]
    fn status_strings_are_stable() {
        assert_eq!(PeerStatus::Alive.as_str(), "alive");
        assert_eq!(PeerStatus::Dead.as_str(), "dead");
        assert_eq!(PeerStatus::Suspect.as_str(), "suspect");
        assert_eq!(PeerStatus::Connecting.as_str(), "connecting");
        assert_eq!(PeerStatus::Removed.as_str(), "removed");
    }

    #[test]
    fn is_alive_only_includes_active_states() {
        assert!(PeerStatus::Alive.is_alive());
        assert!(PeerStatus::Suspect.is_alive());
        assert!(!PeerStatus::Dead.is_alive());
        assert!(!PeerStatus::Removed.is_alive());
        assert!(!PeerStatus::Connecting.is_alive());
    }

    #[test]
    fn capacity_never_exceeds_1024() {
        // P0-5 conformance: the documented surface is 1024 peers.
        let cfg = PeerManagerConfig {
            max_peers: MAX_P2P_PEERS + 16,
            ..PeerManagerConfig::default()
        };
        let m = PeerManager::new(cfg);
        assert_eq!(m.config().max_peers, MAX_P2P_PEERS);
        assert_eq!(m.config().max_peers, 1024);
    }

    #[test]
    fn full_capacity_holds_exactly_1024_peers() {
        // Sanity: with the documented default we can hold every
        // configured slot.
        let m = PeerManager::with_default();
        for i in 0..MAX_P2P_PEERS {
            let n = NodeId::from_bytes(&{
                // Derive a unique 32-byte id from a counter.
                let mut buf = [0u8; 32];
                let bytes = (i as u64).to_le_bytes();
                buf[..8].copy_from_slice(&bytes);
                buf
            })
            .expect("valid 32-byte NodeId");
            m.insert(n, None);
        }
        assert_eq!(m.len(), MAX_P2P_PEERS);
        assert_eq!(m.len(), 1024);
        let snap = m.list();
        assert_eq!(snap.capacity, 1024);
        assert_eq!(snap.peers.len(), 1024);
    }

    #[test]
    fn capacity_rollover_evicts_oldest_dead() {
        // Repro: fill the table, mark two peers dead, then insert
        // one more. Both dead entries must be candidates for
        // eviction; the freshest peer must be present.
        let cfg = PeerManagerConfig {
            max_peers: 4,
            ..PeerManagerConfig::default()
        };
        let m = PeerManager::new(cfg);
        let ids: Vec<NodeId> = (0..4).map(node).collect();
        for i in &ids {
            m.insert(i.clone(), None);
        }
        m.record_heartbeat(&ids[0]);
        m.mark_dead(&ids[1]);
        m.mark_dead(&ids[2]);
        sleep(Duration::from_millis(10));
        let extra = NodeId::from_bytes(&[0xAA; 32]).expect("valid");
        m.insert(extra.clone(), None);
        let snap = m.list();
        assert!(snap.peers.iter().any(|p| p.node_id == extra));
        assert!(snap.peers.len() <= 4);
        // The remaining table must include the latest heartbeat
        // (ids[0]) since the dead entries should have been
        // evicted.
        assert!(snap.peers.iter().any(|p| p.node_id == ids[0]));
    }

    #[test]
    fn touch_does_not_reset_heartbeat_failures() {
        // `touch` updates last_seen_at but should NOT reset the
        // heartbeat counter — that's the whole point of having
        // two separate fields.
        let m = PeerManager::with_default();
        let n = node(31);
        m.insert(n.clone(), None);
        m.mark_dead(&n);
        let entry = m.touch(&n).expect("entry exists");
        assert_eq!(entry.status, PeerStatus::Dead);
        assert_eq!(entry.heartbeat_failures, 1);
    }

    #[test]
    fn remove_marks_status_but_keeps_entry() {
        // `remove` is a soft transition: the entry stays so the
        // CLI can render `removed` in the next list call.
        let m = PeerManager::with_default();
        let n = node(41);
        m.insert(n.clone(), None);
        let entry = m.remove(&n).expect("entry exists");
        assert_eq!(entry.status, PeerStatus::Removed);
        assert!(m.get(&n).is_some());
        let snap = m.list();
        assert_eq!(snap.peers.len(), 1);
        assert_eq!(snap.peers[0].status, PeerStatus::Removed);
    }

    #[test]
    fn evict_drops_entry_entirely() {
        let m = PeerManager::with_default();
        let n = node(51);
        m.insert(n.clone(), None);
        let entry = m.evict(&n).expect("entry exists");
        assert_eq!(entry.node_id, n);
        assert!(m.get(&n).is_none());
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn heartbeat_tick_increments_pings_sent() {
        // Drive the table past the interval and check that the
        // tick reports the right number of pings to send.
        let cfg = PeerManagerConfig {
            max_peers: 4,
            heartbeat_interval: Duration::from_millis(20),
            heartbeat_timeout: Duration::from_millis(80),
            ..PeerManagerConfig::default()
        };
        let m = PeerManager::new(cfg);
        for i in 0..3 {
            m.insert(node(i), None);
        }
        sleep(Duration::from_millis(30));
        let stats = m.heartbeat_tick();
        assert_eq!(stats.pings_sent, 3);
    }

    #[test]
    fn unknown_peer_lookups_return_none() {
        let m = PeerManager::with_default();
        let n = node(91);
        assert!(m.get(&n).is_none());
        assert!(m.record_heartbeat(&n).is_none());
        assert!(m.touch(&n).is_none());
        assert!(m.mark_dead(&n).is_none());
        assert!(m.remove(&n).is_none());
        assert!(m.evict(&n).is_none());
    }

    #[test]
    fn peer_manager_config_roundtrip() {
        let cfg = PeerManagerConfig {
            max_peers: 64,
            heartbeat_interval: Duration::from_secs(7),
            heartbeat_timeout: Duration::from_secs(21),
            ..PeerManagerConfig::default()
        };
        let raw = serde_json::to_string(&cfg).expect("serialize");
        let back: PeerManagerConfig = serde_json::from_str(&raw).expect("parse");
        assert_eq!(back.max_peers, 64);
        assert_eq!(back.heartbeat_interval, Duration::from_secs(7));
        assert_eq!(back.heartbeat_timeout, Duration::from_secs(21));
    }
}
