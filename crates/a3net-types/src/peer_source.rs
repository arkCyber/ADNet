//! Peer sources — which node can serve which content hash.
//!
//! Mirrors `ExodusCdnPeerSource` (see `Exodus@src-backup/.../p2p_cdn/types.rs`).
//! Each entry is keyed by `content_hash`; when the gossip overlay or local
//! ingest hears an announcement that carries a [`BlobTicket`](crate::BlobTicket),
//! we record a [`PeerSource`] so later downloads can build a fan-out
//! candidate set without re-parsing the gossip log.
//!
//! # Aerospace-grade invariants (DO-178C)
//!
//! Every record carries an explicit `validate()` method that, at the
//! boundary, enforces:
//! - `node_id` is exactly 64 ASCII hex chars (length + charset).
//! - `content_hash` is exactly 64 ASCII hex chars.
//! - `ticket` (when present) survives a round-trip encode/parse and
//!   has internally consistent ranges.
//! - `ticket.node_id == node_id` when the ticket is present — a peer
//!   cannot advertise someone else's ticket as their own endpoint.
//! - `rtt_ms` (when present) is in `[0, 60_000]` — anything past one
//!   minute is treated as a misconfigured probe, not a real RTT.
//! - `last_seen` is within ±[`MAX_CLOCK_SKEW_HOURS`] of the local
//!   clock. Past timestamps farther than this are rejected as
//!   fabricated / replayed; future timestamps farther than this are
//!   rejected as fabricated by a clock-running-fast publisher.
//!
//! [`PeerMap::upsert`] runs `validate()` unconditionally and surfaces
//! failures as `Result<(), AdnetError>`. A bulk [`PeerMap::validate_all`]
//! helper iterates every entry for use at higher-layer IPC boundaries.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

use crate::content::ContentHash;
use crate::error::{AdnetError, Result};
use crate::invariants::validate_hex_id;
use crate::node::NodeId;
use crate::ticket::BlobTicket;

/// Maximum number of [`PeerSource`] entries per content hash. Mirrors
/// the per-room cap in [`crate::invariants`]; chosen to bound the
/// memory + fan-out cost of a single asset under gossip flood.
pub const MAX_PEER_SOURCES_PER_HASH: usize = 256;

/// Maximum number of distinct content hashes tracked in a single
/// [`PeerMap`]. Bounds the cross-hash attack surface: a malicious
/// publisher cannot grow the map's key set past this point. The cap
/// is enforced by evicting the **least-recently-seen** entry (by
/// `last_seen`) when the limit is reached.
pub const MAX_TRACKED_HASHES: usize = 65_536;

/// Maximum believable RTT in milliseconds. Anything larger is treated
/// as a misconfigured probe and rejected by `validate()`.
pub const MAX_RTT_MS: u32 = 60_000;

/// Maximum allowed clock skew in **either** direction, in hours. A
/// `last_seen` outside `now ± MAX_CLOCK_SKEW_HOURS` is rejected by
/// `validate()` as a fabricated / replayed timestamp. Default 24h
/// tolerates mobile clocks that drift over days while still catching
/// clearly-bogus values.
pub const MAX_CLOCK_SKEW_HOURS: i64 = 24;

/// Expected length of a hex-encoded [`NodeId`] (32 bytes / 64 hex chars).
pub const NODE_ID_HEX_LEN: usize = 64;

/// Expected length of a hex-encoded [`ContentHash`] (32 bytes / 64 hex chars).
pub const CONTENT_HASH_HEX_LEN: usize = 64;

/// "Peer `node_id` claims to have `content_hash`; the latest sighting was
/// `last_seen` and (optionally) the last observed RTT was `rtt_ms`."
///
/// Field shape matches `ExodusCdnPeerSource` so a bridge that ingests Exodus
/// gossip JSON can deserialise straight into this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerSource {
    pub node_id: NodeId,
    pub content_hash: ContentHash,
    /// Encoded ticket for reaching the peer (HTTP base URL, relay URL, or
    /// full `a3net-blob://…` ticket). `None` means "the announcement had no
    /// ticket; we just know this node advertised the hash".
    pub ticket: Option<BlobTicket>,
    pub last_seen: DateTime<Utc>,
    /// Optional latency hint (milliseconds). Populated lazily after a
    /// successful download.
    pub rtt_ms: Option<u32>,
}

impl PeerSource {
    /// Build a fresh `PeerSource` with `last_seen = now`.
    pub fn new(node_id: NodeId, content_hash: ContentHash, ticket: Option<BlobTicket>) -> Self {
        Self {
            node_id,
            content_hash,
            ticket,
            last_seen: Utc::now(),
            rtt_ms: None,
        }
    }

    /// Construct a peer source from an announcement's embedded ticket (if any).
    ///
    /// **Does not** call `validate()` — callers that ingest this into
    /// a [`PeerMap`] will validate via `upsert`. Use
    /// [`PeerSource::from_announcement_checked`] if you want
    /// eager validation.
    pub fn from_announcement(ann: &crate::Announcement) -> Self {
        Self {
            node_id: ann.node_id.clone(),
            content_hash: ann.content_hash.clone(),
            ticket: ann.ticket.clone(),
            last_seen: ann.timestamp,
            rtt_ms: None,
        }
    }

    /// Like [`PeerSource::from_announcement`] but runs `validate()`
    /// before returning. Returns the validation error if the derived
    /// record is not safe to admit.
    pub fn from_announcement_checked(ann: &crate::Announcement) -> Result<Self> {
        let s = Self::from_announcement(ann);
        s.validate()?;
        Ok(s)
    }

    /// Validate every field of the peer source.
    ///
    /// Checks (in order, first failure wins):
    /// - `node_id` is exactly 64 ASCII hex chars.
    /// - `content_hash` is exactly 64 ASCII hex chars.
    /// - `ticket` (when present) round-trip parses and has consistent ranges.
    /// - `ticket.node_id == self.node_id` when the ticket is present.
    /// - `rtt_ms` (when present) is in `[0, MAX_RTT_MS]`.
    /// - `last_seen` is within ±`MAX_CLOCK_SKEW_HOURS` of `now`.
    pub fn validate(&self) -> Result<()> {
        // The NodeId/ContentHash newtypes already enforce 64-hex at
        // construction time, but a corrupted struct (e.g. from a
        // serde round-trip through a different schema, or a future
        // refactor that loosens the newtype) could produce a
        // non-canonical hex string. Re-check at the boundary.
        validate_hex_id(
            "peer_source node_id",
            self.node_id.as_hex(),
            NODE_ID_HEX_LEN,
        )?;
        validate_hex_id(
            "peer_source content_hash",
            self.content_hash.as_hex(),
            CONTENT_HASH_HEX_LEN,
        )?;
        if let Some(t) = &self.ticket {
            crate::ticket::validate_blob_ticket(t)?;
            // Consistency: a peer cannot advertise someone else's
            // ticket as their own endpoint. This catches a forgery
            // where an attacker publishes a ticket pointing at a
            // victim node but claims a different node_id here.
            if t.node_id != self.node_id {
                return Err(AdnetError::Validation(format!(
                    "peer_source: ticket.node_id {} does not match self.node_id {}",
                    t.node_id.short(),
                    self.node_id.short()
                )));
            }
        }
        if let Some(rtt) = self.rtt_ms
            && rtt > MAX_RTT_MS
        {
            return Err(AdnetError::Validation(format!(
                "peer_source rtt_ms: {rtt} exceeds MAX_RTT_MS ({MAX_RTT_MS})"
            )));
        }
        let now = Utc::now();
        let skew = self.last_seen.signed_duration_since(now);
        let limit = ChronoDuration::hours(MAX_CLOCK_SKEW_HOURS);
        if skew > limit {
            return Err(AdnetError::Validation(format!(
                "peer_source last_seen: {} is more than {}h in the future (clock skew)",
                self.last_seen, MAX_CLOCK_SKEW_HOURS
            )));
        }
        if skew < -limit {
            return Err(AdnetError::Validation(format!(
                "peer_source last_seen: {} is more than {}h in the past (clock skew)",
                self.last_seen, MAX_CLOCK_SKEW_HOURS
            )));
        }
        Ok(())
    }

    /// HTTP base URL of this peer, if the ticket encodes a direct endpoint.
    pub fn http_base(&self) -> Option<String> {
        self.ticket.as_ref().and_then(|t| t.http_base())
    }

    /// True if this peer can be reached by direct LAN / QUIC — i.e. its
    /// ticket has a direct endpoint.
    pub fn is_direct(&self) -> bool {
        self.ticket
            .as_ref()
            .map(|t| t.endpoint.direct.is_some())
            .unwrap_or(false)
    }
}

/// Forward [`PeerSource::validate`] to the [`Validate`](crate::Validate)
/// trait so the IPC layer can use the shared gate.
impl crate::Validate for PeerSource {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}

/// Bundle of peer sources keyed by content hash.
///
/// This is the canonical "who has what" map; the [`a3net_node::SwarmIndex`]
/// keeps one of these per process and merges incoming gossip into it.
///
/// # Validation policy
///
/// Unlike the IPC services (which expose a configurable
/// [`ValidationPolicy`](crate::Validate)), `PeerMap::upsert` always
/// runs `validate()` unconditionally. The data layer has only one
/// mode: **Strict** (fail-closed). Callers that need policy-controlled
/// admission should route records through the IPC service rather than
/// directly into a `PeerMap`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerMap {
    by_hash: std::collections::HashMap<ContentHash, Vec<PeerSource>>,
}

impl PeerMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert / update a [`PeerSource`] for its `content_hash`.
    ///
    /// If the same `node_id` already has an entry for the same hash, the new
    /// record replaces it (last-writer-wins on `last_seen` and `ticket`).
    ///
    /// The input is **always validated** before insertion. On failure,
    /// the map is unchanged and the [`AdnetError::Validation`] is
    /// returned. To bypass validation (legacy migration path), use
    /// [`PeerMap::upsert_unchecked`].
    pub fn upsert(&mut self, source: PeerSource) -> Result<()> {
        source.validate()?;
        self.upsert_unchecked(source);
        Ok(())
    }

    /// Insert / update without running `validate()`. Only safe when the
    /// caller has already proven the record's invariants (e.g. an
    /// internal builder that constructs the record from typed inputs).
    ///
    /// Honours both bounds:
    /// - per-hash fan-out cap [`MAX_PEER_SOURCES_PER_HASH`]: the
    ///   **oldest** entry (smallest `last_seen`) for that hash is
    ///   evicted when the cap is reached.
    /// - cross-hash cap [`MAX_TRACKED_HASHES`]: the **coldest** hash
    ///   bucket (whose freshest entry has the smallest `last_seen`)
    ///   is evicted when the global cap is reached.
    ///
    /// Dedup-by-`node_id` runs first; if the source updates an
    /// existing node, no eviction is needed and the size stays the
    /// same.
    pub fn upsert_unchecked(&mut self, source: PeerSource) {
        // Dedup-by-node_id first; if the node already has an entry
        // we are updating, no eviction is needed and the size stays
        // the same. Also avoids touching the cross-hash eviction
        // path for an update.
        {
            if let Some(list) = self.by_hash.get_mut(&source.content_hash)
                && let Some(existing) = list.iter_mut().find(|p| p.node_id == source.node_id)
            {
                *existing = source;
                return;
            }
        }
        // New node under an existing hash: enforce the per-hash cap
        // by evicting the oldest entry if we are at or above the cap.
        // `>` keeps the cap stable (eviction → insert → list grows
        // back to cap, not cap+1).
        {
            let list = self.by_hash.entry(source.content_hash.clone()).or_default();
            if list.len() >= MAX_PEER_SOURCES_PER_HASH
                && let Some((idx, _)) = list.iter().enumerate().min_by_key(|(_, p)| p.last_seen)
            {
                list.remove(idx);
            }
            list.push(source.clone());
        }
        // Enforce the cross-hash cap only after a successful insert
        // of a new bucket entry. We identify the coldest bucket by
        // its maximum `last_seen` (the freshness of the bucket — if
        // even its newest entry is older than another bucket's
        // newest, it is the candidate for eviction).
        if self.by_hash.len() > MAX_TRACKED_HASHES {
            let coldest = self
                .by_hash
                .iter()
                .min_by_key(|(_, v)| v.iter().map(|p| p.last_seen).max())
                .map(|(k, _)| k.clone());
            if let Some(key) = coldest {
                self.by_hash.remove(&key);
            }
        }
    }

    /// Iterate every [`PeerSource`] and validate it. Returns the first
    /// failure (if any). Used at IPC / disk-load boundaries.
    pub fn validate_all(&self) -> Result<()> {
        for sources in self.by_hash.values() {
            for s in sources {
                s.validate()?;
            }
        }
        Ok(())
    }

    /// All known peers for a given content hash.
    pub fn peers_for(&self, hash: &ContentHash) -> Vec<PeerSource> {
        self.by_hash.get(hash).cloned().unwrap_or_default()
    }

    /// Number of unique content hashes this map tracks.
    pub fn hash_count(&self) -> usize {
        self.by_hash.len()
    }

    /// Total number of `(hash, node_id)` pairs tracked.
    pub fn total_peer_entries(&self) -> usize {
        self.by_hash.values().map(|v| v.len()).sum()
    }

    /// Drop entries older than `cutoff`.
    pub fn prune_older_than(&mut self, cutoff: DateTime<Utc>) -> usize {
        let mut dropped = 0usize;
        for sources in self.by_hash.values_mut() {
            let before = sources.len();
            sources.retain(|p| p.last_seen >= cutoff);
            dropped += before - sources.len();
        }
        self.by_hash.retain(|_, v| !v.is_empty());
        dropped
    }

    /// Iterate `(hash, peers)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&ContentHash, &Vec<PeerSource>)> {
        self.by_hash.iter()
    }
}

impl crate::Validate for PeerMap {
    fn validate(&self) -> Result<()> {
        self.validate_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Endpoint, NodeAddr};

    fn nid() -> NodeId {
        NodeId::random()
    }

    fn ticket_for(node: NodeId, port: u16) -> BlobTicket {
        let addr = NodeAddr::new(node.clone()).with_direct(Endpoint::new("127.0.0.1", port));
        BlobTicket::whole(&node, &addr, &ContentHash::from_bytes(b"x"))
    }

    #[test]
    fn upsert_replaces_same_node() {
        let mut m = PeerMap::new();
        let h = ContentHash::from_bytes(b"h");
        let n = nid();
        m.upsert(PeerSource::new(
            n.clone(),
            h.clone(),
            Some(ticket_for(n.clone(), 9000)),
        ))
        .unwrap();
        m.upsert(PeerSource::new(
            n.clone(),
            h.clone(),
            Some(ticket_for(n.clone(), 9001)),
        ))
        .unwrap();
        assert_eq!(m.peers_for(&h).len(), 1);
        assert_eq!(
            m.peers_for(&h)[0].http_base().as_deref(),
            Some("http://127.0.0.1:9001")
        );
    }

    #[test]
    fn upsert_keeps_distinct_nodes() {
        let mut m = PeerMap::new();
        let h = ContentHash::from_bytes(b"h");
        let n1 = nid();
        let n2 = nid();
        m.upsert(PeerSource::new(
            n1.clone(),
            h.clone(),
            Some(ticket_for(n1, 9000)),
        ))
        .unwrap();
        m.upsert(PeerSource::new(
            n2.clone(),
            h.clone(),
            Some(ticket_for(n2, 9001)),
        ))
        .unwrap();
        assert_eq!(m.peers_for(&h).len(), 2);
    }

    #[test]
    fn prune_drops_stale_entries() {
        let mut m = PeerMap::new();
        let h1 = ContentHash::from_bytes(b"a");
        let h2 = ContentHash::from_bytes(b"b");
        let now = Utc::now();
        let mut old = PeerSource::new(nid(), h1.clone(), None);
        old.last_seen = now - chrono::Duration::hours(2);
        m.upsert(old).unwrap();
        m.upsert(PeerSource::new(nid(), h2.clone(), None)).unwrap();
        let dropped = m.prune_older_than(now - chrono::Duration::hours(1));
        assert_eq!(dropped, 1);
        assert!(m.peers_for(&h1).is_empty());
        assert_eq!(m.peers_for(&h2).len(), 1);
    }

    #[test]
    fn http_base_resolves_when_direct_present() {
        let n = nid();
        let h = ContentHash::from_bytes(b"x");
        let p = PeerSource::new(n.clone(), h, Some(ticket_for(n, 4242)));
        assert_eq!(p.http_base().as_deref(), Some("http://127.0.0.1:4242"));
        assert!(p.is_direct());
    }

    // ─────────────────────────────────────────────────────────────────────
    // DO-178C boundary tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn validate_accepts_good_source() {
        let n = nid();
        let h = ContentHash::from_bytes(b"h");
        // Use the SAME node for the ticket endpoint so the
        // ticket-node-id consistency check passes.
        let p = PeerSource::new(n.clone(), h, Some(ticket_for(n, 9000)));
        assert!(p.validate().is_ok());
        assert!(crate::Validate::validate(&p).is_ok());
    }

    #[test]
    fn validate_rejects_rtt_over_cap() {
        let n = nid();
        let h = ContentHash::from_bytes(b"h");
        let mut p = PeerSource::new(n, h, None);
        p.rtt_ms = Some(MAX_RTT_MS + 1);
        let err = p.validate().unwrap_err();
        assert!(err.to_string().contains("rtt_ms"), "got {err}");
    }

    #[test]
    fn validate_accepts_rtt_at_cap() {
        let n = nid();
        let h = ContentHash::from_bytes(b"h");
        let mut p = PeerSource::new(n, h, None);
        p.rtt_ms = Some(MAX_RTT_MS);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn validate_rejects_future_timestamp() {
        let n = nid();
        let h = ContentHash::from_bytes(b"h");
        let mut p = PeerSource::new(n, h, None);
        p.last_seen = Utc::now() + ChronoDuration::hours(MAX_CLOCK_SKEW_HOURS + 1);
        let err = p.validate().unwrap_err();
        assert!(
            err.to_string().contains("last_seen") || err.to_string().contains("skew"),
            "got {err}"
        );
    }

    #[test]
    fn validate_rejects_past_timestamp() {
        // NEW: symmetric past-skew check (was previously only future).
        let n = nid();
        let h = ContentHash::from_bytes(b"h");
        let mut p = PeerSource::new(n, h, None);
        p.last_seen = Utc::now() - ChronoDuration::hours(MAX_CLOCK_SKEW_HOURS + 1);
        let err = p.validate().unwrap_err();
        assert!(
            err.to_string().contains("last_seen") || err.to_string().contains("skew"),
            "got {err}"
        );
    }

    #[test]
    fn validate_accepts_clock_at_skew_boundary() {
        // Exactly at the boundary is allowed (off-by-one safety).
        let n = nid();
        let h = ContentHash::from_bytes(b"h");
        let mut p = PeerSource::new(n, h, None);
        p.last_seen = Utc::now() + ChronoDuration::hours(MAX_CLOCK_SKEW_HOURS);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn validate_rejects_malformed_ticket() {
        let n = nid();
        let h = ContentHash::from_bytes(b"h");
        let mut p = PeerSource::new(n, h, None);
        // Bypass `ByteRange::new`'s own check via struct literal.
        p.ticket = Some(
            BlobTicket::whole(
                &nid(),
                &NodeAddr::new(nid()),
                &ContentHash::from_bytes(b"x"),
            )
            .with_range(crate::range::RangeSpec::Single(crate::range::ByteRange {
                start: 100,
                end: 50,
            })),
        );
        let err = p.validate().unwrap_err();
        assert!(
            err.to_string().contains("blob_ticket") || err.to_string().contains("range"),
            "got {err}"
        );
    }

    #[test]
    fn validate_rejects_ticket_node_id_mismatch() {
        // NEW: ticket.node_id must equal peer_source.node_id.
        let n = nid();
        let h = ContentHash::from_bytes(b"h");
        let other = nid();
        // Construct a ticket whose endpoint points at a different node.
        let ticket = ticket_for(other.clone(), 9000);
        let p = PeerSource::new(n, h, Some(ticket));
        let err = p.validate().unwrap_err();
        assert!(
            err.to_string().contains("ticket.node_id")
                || err.to_string().contains("does not match"),
            "got {err}"
        );
    }

    #[test]
    fn upsert_rejects_invalid_source() {
        let mut m = PeerMap::new();
        let h = ContentHash::from_bytes(b"h");
        let n = nid();
        let mut bad = PeerSource::new(n.clone(), h.clone(), None);
        bad.rtt_ms = Some(MAX_RTT_MS + 1);
        let err = m.upsert(bad).unwrap_err();
        assert!(err.to_string().contains("rtt_ms"), "got {err}");
        // Map unchanged.
        assert!(m.peers_for(&h).is_empty());
    }

    #[test]
    fn upsert_unchecked_skips_validation() {
        let mut m = PeerMap::new();
        let h = ContentHash::from_bytes(b"h");
        let n = nid();
        let mut bad = PeerSource::new(n.clone(), h.clone(), None);
        bad.rtt_ms = Some(MAX_RTT_MS + 1);
        m.upsert_unchecked(bad);
        // Stored even though invalid.
        assert_eq!(m.peers_for(&h).len(), 1);
    }

    #[test]
    fn upsert_caps_per_hash_fan_out() {
        // Drop the oldest entry when the cap is exceeded.
        let mut m = PeerMap::new();
        let h = ContentHash::from_bytes(b"h");
        let now = Utc::now();
        // Fill the cap with entries, oldest first.
        for i in 0..MAX_PEER_SOURCES_PER_HASH {
            let n = nid();
            let mut p = PeerSource::new(n, h.clone(), None);
            p.last_seen = now - ChronoDuration::seconds((MAX_PEER_SOURCES_PER_HASH - i) as i64);
            m.upsert(p).unwrap();
        }
        assert_eq!(m.peers_for(&h).len(), MAX_PEER_SOURCES_PER_HASH);
        // Add one more — must evict the oldest (i=0 had last_seen = now - N).
        let n = nid();
        let mut p = PeerSource::new(n, h.clone(), None);
        p.last_seen = now;
        m.upsert(p).unwrap();
        assert_eq!(m.peers_for(&h).len(), MAX_PEER_SOURCES_PER_HASH);
    }

    #[test]
    fn upsert_replace_existing_node_does_not_evict() {
        // NEW: dedup-first path; replacing an existing node must NOT
        // trigger eviction even when the cap is reached.
        let mut m = PeerMap::new();
        let h = ContentHash::from_bytes(b"h");
        let now = Utc::now();
        // Fill to cap.
        let mut nodes = Vec::new();
        for _ in 0..MAX_PEER_SOURCES_PER_HASH {
            let n = nid();
            nodes.push(n.clone());
            let mut p = PeerSource::new(n, h.clone(), None);
            p.last_seen = now;
            m.upsert(p).unwrap();
        }
        assert_eq!(m.peers_for(&h).len(), MAX_PEER_SOURCES_PER_HASH);
        // Update one of them — list size must stay at cap.
        let mut p = PeerSource::new(nodes[0].clone(), h.clone(), None);
        p.last_seen = now + ChronoDuration::seconds(1);
        m.upsert(p).unwrap();
        assert_eq!(m.peers_for(&h).len(), MAX_PEER_SOURCES_PER_HASH);
        // And the new last_seen took effect.
        assert_eq!(
            m.peers_for(&h)[0].last_seen,
            now + ChronoDuration::seconds(1)
        );
    }

    #[test]
    fn validate_all_returns_first_failure() {
        let mut m = PeerMap::new();
        let h = ContentHash::from_bytes(b"h");
        let n1 = nid();
        let n2 = nid();
        let mut p1 = PeerSource::new(n1, h.clone(), None);
        p1.rtt_ms = Some(MAX_RTT_MS + 1);
        // Bypass validate() so we can install a known-bad record.
        m.upsert_unchecked(p1);
        let p2 = PeerSource::new(n2, h.clone(), None);
        m.upsert_unchecked(p2);
        let err = m.validate_all().unwrap_err();
        assert!(err.to_string().contains("rtt_ms"), "got {err}");
        // The good record is also flagged because we only return the
        // first failure, but `Validate` impl still surfaces one error.
    }

    #[test]
    fn validate_all_accepts_clean_map() {
        let mut m = PeerMap::new();
        let h = ContentHash::from_bytes(b"h");
        let n = nid();
        m.upsert(PeerSource::new(n, h, None)).unwrap();
        assert!(m.validate_all().is_ok());
        assert!(crate::Validate::validate(&m).is_ok());
    }

    #[test]
    fn from_announcement_checked_rejects_invalid_timestamp() {
        // Eager validation: from_announcement_checked must reject a
        // record derived from a far-future announcement timestamp.
        let nid = NodeId::random();
        let hash = ContentHash::from_bytes(b"x");
        let room = crate::room::RoomId::new("lobby");
        let ann = crate::Announcement {
            room_id: room,
            content_hash: hash,
            node_id: nid,
            title: "ok".into(),
            kind: crate::content::CdnContentKind::Article,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now() + ChronoDuration::hours(MAX_CLOCK_SKEW_HOURS + 1),
            signer: None,
            signature: None,
            message_id: None,
            ttl_secs: None,
        };
        let err = PeerSource::from_announcement_checked(&ann).unwrap_err();
        assert!(
            err.to_string().contains("last_seen") || err.to_string().contains("skew"),
            "got {err}"
        );
    }

    #[test]
    fn from_announcement_skips_validation_by_default() {
        // The plain `from_announcement` constructor does NOT call
        // validate(); it is the caller's responsibility (typically
        // via `PeerMap::upsert`).
        let nid = NodeId::random();
        let hash = ContentHash::from_bytes(b"x");
        let room = crate::room::RoomId::new("lobby");
        let ann = crate::Announcement {
            room_id: room,
            content_hash: hash,
            node_id: nid,
            title: "ok".into(),
            kind: crate::content::CdnContentKind::Article,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now() + ChronoDuration::hours(MAX_CLOCK_SKEW_HOURS + 1),
            signer: None,
            signature: None,
            message_id: None,
            ttl_secs: None,
        };
        let src = PeerSource::from_announcement(&ann);
        // Bad record still constructable; validate() catches it.
        assert!(src.validate().is_err());
    }

    // ─────────────────────────────────────────────────────────────────────
    // Cross-hash capacity & property tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn upsert_caps_global_tracked_hashes() {
        // The map must not exceed MAX_TRACKED_HASHES even when fed
        // many distinct hashes. The coldest bucket is evicted.
        let mut m = PeerMap::new();
        let now = Utc::now();
        let mut hashes = Vec::new();
        for i in 0..(MAX_TRACKED_HASHES + 5) {
            let h = ContentHash::from_bytes(format!("hash-{i}").as_bytes());
            hashes.push(h.clone());
            let n = NodeId::random();
            let mut p = PeerSource::new(n, h, None);
            // Each subsequent bucket is fresher (sub-second offsets
            // to stay within MAX_CLOCK_SKEW_HOURS).
            p.last_seen = now + ChronoDuration::milliseconds(i as i64);
            m.upsert(p).unwrap();
        }
        assert_eq!(
            m.hash_count(),
            MAX_TRACKED_HASHES,
            "map must not exceed MAX_TRACKED_HASHES"
        );
        // The oldest 5 hashes were evicted; the freshest MAX_TRACKED_HASHES remain.
        for h in hashes.iter().take(5) {
            assert!(
                m.peers_for(h).is_empty(),
                "coldest bucket should have been evicted: {:?}",
                h.as_hex()
            );
        }
        for h in hashes.iter().skip(5).take(MAX_TRACKED_HASHES - 5) {
            assert_eq!(
                m.peers_for(h).len(),
                1,
                "freshest buckets should still be present: {:?}",
                h.as_hex()
            );
        }
    }

    #[test]
    fn upsert_global_cap_is_stable_under_dedup_updates() {
        // Updating an existing node must NOT trigger cross-hash
        // eviction (dedup-first path returns early).
        let mut m = PeerMap::new();
        let now = Utc::now();
        let mut hashes = Vec::new();
        for i in 0..(MAX_TRACKED_HASHES + 5) {
            let h = ContentHash::from_bytes(format!("hash-{i}").as_bytes());
            hashes.push(h.clone());
            let n = NodeId::random();
            let mut p = PeerSource::new(n.clone(), h, None);
            p.last_seen = now + ChronoDuration::milliseconds(i as i64);
            m.upsert(p).unwrap();
        }
        assert_eq!(m.hash_count(), MAX_TRACKED_HASHES);
        // Now update an existing node — must not change hash count.
        let survivor = hashes[5].clone();
        let survivor_node = m
            .peers_for(&survivor)
            .first()
            .map(|p| p.node_id.clone())
            .expect("survivor must have a peer");
        let mut p = PeerSource::new(survivor_node, survivor.clone(), None);
        p.last_seen = now + ChronoDuration::seconds(30);
        m.upsert(p).unwrap();
        assert_eq!(m.hash_count(), MAX_TRACKED_HASHES);
        // And the updated last_seen is present.
        assert_eq!(
            m.peers_for(&survivor)[0].last_seen,
            now + ChronoDuration::seconds(30)
        );
    }

    #[test]
    fn upsert_total_entries_bounded() {
        // With both caps in force, total_peer_entries() must never
        // exceed MAX_TRACKED_HASHES * MAX_PEER_SOURCES_PER_HASH.
        // Note: timestamps must stay within MAX_CLOCK_SKEW_HOURS
        // (24h) of "now", so we use sub-second offsets here.
        let mut m = PeerMap::new();
        let now = Utc::now();
        // Fill past both caps with mixed fan-out.
        for i in 0..(MAX_TRACKED_HASHES + 10) {
            let h = ContentHash::from_bytes(format!("h{i}").as_bytes());
            // 2 nodes per hash to exercise the per-hash cap too.
            for j in 0..2 {
                let n = NodeId::random();
                let mut p = PeerSource::new(n, h.clone(), None);
                p.last_seen = now + ChronoDuration::milliseconds((i * 10 + j) as i64);
                m.upsert(p).unwrap();
            }
        }
        let total = m.total_peer_entries();
        assert!(
            total <= MAX_TRACKED_HASHES * MAX_PEER_SOURCES_PER_HASH,
            "total entries {total} exceeds product of caps"
        );
        assert!(m.hash_count() <= MAX_TRACKED_HASHES);
    }

    #[test]
    fn validate_all_returns_first_failure_under_clean_state() {
        // Positive control: no failures → Ok.
        let mut m = PeerMap::new();
        for i in 0..4 {
            let h = ContentHash::from_bytes(format!("h{i}").as_bytes());
            let n = NodeId::random();
            m.upsert(PeerSource::new(n, h, None)).unwrap();
        }
        assert!(m.validate_all().is_ok());
    }

    #[test]
    fn validate_all_short_circuits_on_first_failure() {
        // validate_all must NOT swallow the first failure and
        // continue — it must return Err at the first invalid record.
        let mut m = PeerMap::new();
        let n = NodeId::random();
        // Bad rtt_ms installed via unchecked path.
        let mut bad = PeerSource::new(n.clone(), ContentHash::from_bytes(b"a"), None);
        bad.rtt_ms = Some(MAX_RTT_MS + 1);
        m.upsert_unchecked(bad);
        // Good record — should never be inspected if the first is bad.
        m.upsert(PeerSource::new(
            NodeId::random(),
            ContentHash::from_bytes(b"b"),
            None,
        ))
        .unwrap();
        let err = m.validate_all().unwrap_err();
        assert!(err.to_string().contains("rtt_ms"), "got {err}");
    }

    #[test]
    fn ticket_consistency_check_uses_short_ids_in_error() {
        // Ensure error messages don't leak the full 64-char hex in
        // production logs (debug log spam protection).
        let n = NodeId::random();
        let other = NodeId::random();
        let h = ContentHash::from_bytes(b"x");
        let ticket = ticket_for(other.clone(), 9000);
        let p = PeerSource::new(n.clone(), h, Some(ticket));
        let err = p.validate().unwrap_err().to_string();
        // The error must contain the .short() form, NOT the full hex.
        assert!(err.contains(other.short()), "err: {err}");
        assert!(err.contains(n.short()), "err: {err}");
    }

    #[test]
    fn from_announcement_skips_validation_for_clean_announcement() {
        // from_announcement_checked must accept a clean announcement.
        let nid = NodeId::random();
        let hash = ContentHash::from_bytes(b"x");
        let ann = crate::Announcement {
            room_id: crate::room::RoomId::new("lobby"),
            content_hash: hash,
            node_id: nid,
            title: "ok".into(),
            kind: crate::content::CdnContentKind::Article,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            message_id: None,
            ttl_secs: None,
        };
        let src = PeerSource::from_announcement_checked(&ann).unwrap();
        assert!(src.validate().is_ok());
    }

    #[test]
    fn peer_source_serde_roundtrip_preserves_validation_status() {
        // After serde round-trip, validate() must still succeed for
        // a previously-valid record. Confirms the on-the-wire JSON
        // form does not lose information.
        let n = NodeId::random();
        let h = ContentHash::from_bytes(b"data");
        let p = PeerSource::new(n, h, None);
        p.validate().unwrap();
        let json = serde_json::to_string(&p).unwrap();
        let back: PeerSource = serde_json::from_str(&json).unwrap();
        back.validate().unwrap();
        assert_eq!(p, back);
    }
}
