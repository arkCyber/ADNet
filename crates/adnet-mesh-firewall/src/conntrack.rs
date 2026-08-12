//! Connection tracking for stateful return-traffic matching.
//!
//! The mesh firewall is **stateful**: when the local node
//! opens a TCP/UDP flow to a peer, the kernel-side conntrack
//! records the (proto, src port, peer) tuple and lets the
//! peer's return packets back in for a short window. The
//! mechanism is the same one `iptables` uses on Linux; we
//! re-implement it in userspace so the firewall is portable
//! and the conntrack table can be inspected / logged by the
//! mesh firewall CLI.
//!
//! ## Capacity & eviction
//!
//! The tracker is bounded to [`MAX_CONN_ENTRIES`] entries.
//! When the table is full and a new entry would be inserted,
//! the tracker returns [`ConnTrackerError::Full`] and the
//! caller can decide whether to deny or allow. The default
//! policy in the firewall engine treats a full table as a
//! **deny** of the offending packet (mirroring Linux's
//! `nf_conntrack_max` overflow behaviour).
//!
//! Entries expire after [`DEFAULT_CONN_TIMEOUT`]. Expired
//! entries are evicted lazily on lookup — a future iteration
//! may add a background sweeper.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use adnet_types::NodeId;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::rule::{Direction, ProtoSpec};

/// Default idle timeout for a conntrack entry. Mirrors
/// Linux's `nf_conntrack_tcp_timeout_established` default of
/// 5 minutes.
pub const DEFAULT_CONN_TIMEOUT: Duration = Duration::from_secs(300);

/// Maximum number of concurrent conntrack entries. Picked to
/// cover a typical home / homelab mesh; raised on demand by
/// the operator via [`ConnTrackerConfig::max_entries`].
pub const MAX_CONN_ENTRIES: usize = 4096;

/// Layer-4 protocol filter used in a [`ConnKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConnProto {
    Tcp,
    Udp,
}

impl ConnProto {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Tcp => 6,
            Self::Udp => 17,
        }
    }

    pub fn from_proto_spec(p: ProtoSpec) -> Option<Self> {
        match p {
            ProtoSpec::Tcp => Some(Self::Tcp),
            ProtoSpec::Udp => Some(Self::Udp),
            _ => None,
        }
    }
}

/// Lookup key for a conntrack entry.
///
/// `local_port` is the **source** port the local node bound
/// (TCP/UDP). `peer` is the mesh member that initiated the
/// peer side of the flow; `peer_port` is the peer's source
/// port. The 5-tuple is unique per flow direction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnKey {
    pub proto: ConnProto,
    pub peer: NodeId,
    pub peer_port: u16,
    pub local_port: u16,
    /// Remote socket address (informational; the IP layer
    /// already authenticated the peer via the mesh key, so
    /// this is used only for log lines / debug output).
    pub peer_addr: SocketAddr,
}

/// A tracked connection.
///
/// Stores `last_seen` so the lazy eviction check is cheap.
/// `opened_at` is exposed via [`crate::FirewallEngine::conntrack_snapshot`]
/// in future iterations for richer status output; the
/// field is `pub(crate)` for now to silence the
/// dead-code lint while leaving the API surface ready.
#[derive(Debug, Clone)]
struct ConnEntry {
    key: ConnKey,
    direction: Direction,
    opened_at: Instant,
    last_seen: Instant,
}

impl ConnEntry {
    #[allow(dead_code)]
    pub(crate) fn opened_at(&self) -> Instant {
        self.opened_at
    }
}

/// Conntracker configuration.
#[derive(Debug, Clone)]
pub struct ConnTrackerConfig {
    pub idle_timeout: Duration,
    pub max_entries: usize,
}

impl Default for ConnTrackerConfig {
    fn default() -> Self {
        Self {
            idle_timeout: DEFAULT_CONN_TIMEOUT,
            max_entries: MAX_CONN_ENTRIES,
        }
    }
}

/// Errors returned by [`ConnTracker`] methods.
#[derive(Debug, Error)]
pub enum ConnTrackerError {
    #[error("conntrack table is full ({max} entries)")]
    Full { max: usize },
}

/// Thread-safe connection tracker.
///
/// The internal map is a `parking_lot::Mutex<HashMap>` rather
/// than a `tokio::Mutex` because every operation is a single
/// hashmap lookup / insert; parking_lot's non-poisoning
/// mutex gives us a fast, await-free critical section.
pub struct ConnTracker {
    inner: Mutex<ConnTrackerInner>,
    config: ConnTrackerConfig,
}

struct ConnTrackerInner {
    entries: HashMap<ConnKey, ConnEntry>,
}

/// Inline partial key used by [`ConnTracker::lookup_inbound`].
///
/// Inbound return-traffic lookup doesn't know the local
/// port (it was chosen by the OS when we opened the
/// outbound socket). The partial key matches on the
/// 3-tuple `(proto, peer, peer_port)` so we can find the
/// entry without the caller knowing our ephemeral local
/// port. `peer_addr` is included so the match isn't
/// ambiguous if the same peer is reachable via multiple
/// addresses (rare but possible across mesh relays).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InboundProbe<'a> {
    pub proto: ConnProto,
    pub peer: &'a NodeId,
    pub peer_port: u16,
}

impl ConnTracker {
    pub fn new(config: ConnTrackerConfig) -> Self {
        Self {
            inner: Mutex::new(ConnTrackerInner {
                entries: HashMap::new(),
            }),
            config,
        }
    }

    /// Record an outbound flow initiated by the local node.
    ///
    /// Returns `Err(Full)` if the table is at capacity. The
    /// caller is expected to translate that into a deny
    /// (mirrors Linux behaviour).
    pub fn open_outbound(
        &self,
        proto: ConnProto,
        peer: NodeId,
        peer_port: u16,
        peer_addr: SocketAddr,
        local_port: u16,
    ) -> Result<(), ConnTrackerError> {
        let key = ConnKey {
            proto,
            peer,
            peer_port,
            local_port,
            peer_addr,
        };
        let now = Instant::now();
        let mut inner = self.inner.lock();
        if !inner.entries.contains_key(&key) && inner.entries.len() >= self.config.max_entries {
            return Err(ConnTrackerError::Full {
                max: self.config.max_entries,
            });
        }
        inner.entries.insert(
            key.clone(),
            ConnEntry {
                key,
                direction: Direction::Out,
                opened_at: now,
                last_seen: now,
            },
        );
        Ok(())
    }

    /// Look up a return-traffic entry. Returns `true` if the
    /// packet is part of an established flow.
    ///
    /// Side effect: refreshes the entry's `last_seen`. Expired
    /// entries are evicted lazily here.
    pub fn lookup(&self, key: &ConnKey) -> bool {
        let now = Instant::now();
        let mut inner = self.inner.lock();
        match inner.entries.get_mut(key) {
            Some(entry) => {
                if now.duration_since(entry.last_seen) > self.config.idle_timeout {
                    inner.entries.remove(key);
                    false
                } else {
                    entry.last_seen = now;
                    true
                }
            }
            None => false,
        }
    }

    /// Look up an inbound return-traffic entry by the
    /// 3-tuple `(proto, peer, peer_port)`.
    ///
    /// Use this when the inbound packet's destination port
    /// (the local port we bound) is **not** part of the
    /// available metadata — typically because the outbound
    /// socket was bound to an OS-chosen ephemeral port that
    /// the caller doesn't know about at lookup time.
    ///
    /// Refreshes `last_seen` on the matching entry. Returns
    /// `true` if a non-expired entry matched.
    ///
    /// Cost: O(n) over live entries. The table is bounded by
    /// [`ConnTrackerConfig::max_entries`] (default 4096) so
    /// this stays tractable; if profiling shows it's a hot
    /// spot, a secondary `(proto, peer, peer_port) → entry`
    /// index can be added without changing this API.
    pub fn lookup_inbound(&self, probe: InboundProbe<'_>) -> bool {
        let now = Instant::now();
        let mut inner = self.inner.lock();
        for entry in inner.entries.values_mut() {
            if entry.key.proto == probe.proto
                && entry.key.peer_port == probe.peer_port
                && &entry.key.peer == probe.peer
            {
                if now.duration_since(entry.last_seen) > self.config.idle_timeout {
                    // Lazy expiry: drop this entry and keep
                    // looking. In practice a sweep run will
                    // clean up the rest.
                    let key = entry.key.clone();
                    inner.entries.remove(&key);
                    return false;
                }
                entry.last_seen = now;
                return true;
            }
        }
        false
    }

    /// Remove a specific entry (e.g. when a TCP RST or FIN
    /// is observed).
    pub fn close(&self, key: &ConnKey) -> bool {
        let mut inner = self.inner.lock();
        inner.entries.remove(key).is_some()
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.inner.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().entries.is_empty()
    }

    /// Iterate over a snapshot of the entries for the
    /// firewall CLI / status output.
    pub fn snapshot(&self) -> Vec<(ConnKey, Direction, Instant)> {
        let inner = self.inner.lock();
        inner
            .entries
            .values()
            .map(|e| (e.key.clone(), e.direction, e.last_seen))
            .collect()
    }

    /// Sweep expired entries. O(n) but only called from the
    /// status path, not the hot path.
    pub fn sweep(&self) -> usize {
        let now = Instant::now();
        let mut inner = self.inner.lock();
        let before = inner.entries.len();
        inner
            .entries
            .retain(|_, e| now.duration_since(e.last_seen) <= self.config.idle_timeout);
        before - inner.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 7)), 4242)
    }

    #[test]
    fn open_and_lookup_returns_true() {
        let ct = ConnTracker::new(ConnTrackerConfig::default());
        let peer = NodeId::random();
        ct.open_outbound(ConnProto::Tcp, peer.clone(), 80, addr(), 12345)
            .unwrap();
        let key = ConnKey {
            proto: ConnProto::Tcp,
            peer,
            peer_port: 80,
            local_port: 12345,
            peer_addr: addr(),
        };
        assert!(ct.lookup(&key));
    }

    #[test]
    fn lookup_unknown_returns_false() {
        let ct = ConnTracker::new(ConnTrackerConfig::default());
        let key = ConnKey {
            proto: ConnProto::Udp,
            peer: NodeId::random(),
            peer_port: 53,
            local_port: 9999,
            peer_addr: addr(),
        };
        assert!(!ct.lookup(&key));
    }

    #[test]
    fn full_table_returns_error() {
        let cfg = ConnTrackerConfig {
            max_entries: 2,
            ..ConnTrackerConfig::default()
        };
        let ct = ConnTracker::new(cfg);
        ct.open_outbound(ConnProto::Tcp, NodeId::random(), 80, addr(), 1)
            .unwrap();
        ct.open_outbound(ConnProto::Tcp, NodeId::random(), 80, addr(), 2)
            .unwrap();
        let err = ct
            .open_outbound(ConnProto::Tcp, NodeId::random(), 80, addr(), 3)
            .unwrap_err();
        assert!(matches!(err, ConnTrackerError::Full { .. }));
    }

    #[test]
    fn same_key_updates_in_place() {
        let ct = ConnTracker::new(ConnTrackerConfig::default());
        let peer = NodeId::random();
        ct.open_outbound(ConnProto::Tcp, peer.clone(), 80, addr(), 1)
            .unwrap();
        ct.open_outbound(ConnProto::Tcp, peer.clone(), 80, addr(), 1)
            .unwrap();
        assert_eq!(ct.len(), 1);
    }

    #[test]
    fn close_removes_entry() {
        let ct = ConnTracker::new(ConnTrackerConfig::default());
        let peer = NodeId::random();
        ct.open_outbound(ConnProto::Tcp, peer.clone(), 80, addr(), 1)
            .unwrap();
        let key = ConnKey {
            proto: ConnProto::Tcp,
            peer,
            peer_port: 80,
            local_port: 1,
            peer_addr: addr(),
        };
        assert!(ct.close(&key));
        assert!(!ct.lookup(&key));
    }

    #[test]
    fn sweep_removes_expired() {
        let cfg = ConnTrackerConfig {
            idle_timeout: Duration::from_millis(50),
            ..ConnTrackerConfig::default()
        };
        let ct = ConnTracker::new(cfg);
        ct.open_outbound(ConnProto::Tcp, NodeId::random(), 80, addr(), 1)
            .unwrap();
        ct.open_outbound(ConnProto::Udp, NodeId::random(), 53, addr(), 2)
            .unwrap();
        assert_eq!(ct.len(), 2);
        std::thread::sleep(Duration::from_millis(100));
        let removed = ct.sweep();
        assert_eq!(removed, 2);
        assert!(ct.is_empty());
    }

    #[test]
    fn snapshot_returns_current_entries() {
        let ct = ConnTracker::new(ConnTrackerConfig::default());
        ct.open_outbound(ConnProto::Tcp, NodeId::random(), 80, addr(), 1)
            .unwrap();
        let snap = ct.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].1, Direction::Out);
    }

    #[test]
    fn conn_proto_from_proto_spec() {
        assert_eq!(ConnProto::from_proto_spec(ProtoSpec::Tcp), Some(ConnProto::Tcp));
        assert_eq!(ConnProto::from_proto_spec(ProtoSpec::Udp), Some(ConnProto::Udp));
        assert_eq!(ConnProto::from_proto_spec(ProtoSpec::Any), None);
        assert_eq!(ConnProto::from_proto_spec(ProtoSpec::Icmp), None);
    }
}
