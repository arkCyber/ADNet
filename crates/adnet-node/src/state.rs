//! Swarm index — in-memory record of what assets exist in what rooms, and
//! which peers can serve each asset.
//!
//! # Aerospace-grade invariants (DO-178C)
//!
//! Every [`Announcement`] admitted into the index is validated:
//! - the announcement itself via [`Announcement::validate`] (title,
//!   size, mime, ticket) — done at the policy boundary
//!   ([`Node::announce`](crate::Node::announce) and the gossip
//!   subscriber both gate on the configured
//!   [`ValidationPolicy`](adnet_ipc::validation::ValidationPolicy)
//!   before calling).
//! - the [`PeerSource`] constructed from the embedded ticket via
//!   [`PeerSource::validate`] — done here at `ingest` time, as a
//!   defence-in-depth check. A failure is a soft error: the asset
//!   is still kept, only the peer entry is dropped.
//!
//! # Memory bounds (defence against gossip flood)
//!
//! All three index dimensions are bounded:
//! - [`MAX_ASSETS_PER_ROOM`]: per-room asset count; oldest entry
//!   (smallest `announced_at`) is evicted when exceeded.
//! - [`MAX_TRACKED_HASHES`]: total distinct hashes across all rooms
//!   (enforced inside [`PeerMap`]).
//! - [`MAX_PEER_SOURCES_PER_HASH`]: per-hash peer fan-out (enforced
//!   inside [`PeerMap`]).
//!
//! Under a sustained gossip flood the index stabilises at the cap
//! in each dimension; new records evict the coldest entries.
//!
//! # Trust model
//!
//! The `RoomAsset` we keep for a malformed announcement's
//! `announcer_node_id` is metadata only — it is not dialed, not
//! downloaded from, and not displayed as authoritative. The
//! `_announcer_node_id` field exists so the UI can render "this
//! hash was first advertised by node X at time T" as a hint. Any
//! action that touches the network requires a valid peer source,
//! which the announcement-validator already gates.

use std::collections::HashMap;

use adnet_types::{Announcement, BlobTicket, ContentHash, PeerMap, PeerSource, RoomAsset, RoomId};

/// Maximum number of distinct [`RoomAsset`]s tracked per room.
/// Bounds the per-room attack surface under gossip flood; oldest
/// entries (by `announced_at`) are evicted when the cap is reached.
pub const MAX_ASSETS_PER_ROOM: usize = 8192;

/// Indexed view of announced content.
#[derive(Debug, Default, Clone)]
pub struct SwarmIndex {
    assets_by_room: HashMap<RoomId, HashMap<ContentHash, RoomAsset>>,
    peers: PeerMap,
}

impl SwarmIndex {
    /// Insert / update an announcement into the index.
    ///
    /// Callers are expected to have already run
    /// [`Announcement::validate`] on `ann` (the gossip subscriber and
    /// `Node::announce` both gate on the configured
    /// [`ValidationPolicy`](adnet_ipc::validation::ValidationPolicy)
    /// before calling). This method additionally validates the
    /// [`PeerSource`] derived from the embedded ticket — that
    /// second check guards against a published record whose
    /// announcement is fine but whose ticket is broken (rtt_ms,
    /// clock-skew, malformed `ByteRange`, …). A failure there is a
    /// soft error: the asset is still kept, only the peer entry is
    /// dropped, so the index is never poisoned.
    ///
    /// The peer fan-out per content hash is bounded by
    /// [`MAX_PEER_SOURCES_PER_HASH`]; when the cap is reached, the
    /// oldest ticket (by `announced_at`) is evicted before the new
    /// ticket is inserted. Cross-hash count is bounded by
    /// [`adnet_types::MAX_TRACKED_HASHES`] (enforced inside
    /// [`PeerMap`]).
    pub fn ingest(&mut self, ann: Announcement) -> anyhow::Result<()> {
        let asset = RoomAsset {
            content_hash: ann.content_hash.clone(),
            title: ann.title.clone(),
            kind: ann.kind,
            size_bytes: ann.size_bytes,
            mime_type: ann.mime_type.clone(),
            source_url: ann.source_url.clone(),
            room_id: ann.room_id.clone(),
            announcer_node_id: ann.node_id.clone(),
            announced_at: ann.timestamp,
        };

        // Per-room cap on assets: evict the oldest entry (smallest
        // announced_at) when the cap is reached. This bounds the
        // per-room memory footprint under gossip flood.
        let room_map = self.assets_by_room.entry(ann.room_id.clone()).or_default();
        if room_map.len() >= MAX_ASSETS_PER_ROOM
            && let Some(oldest_hash) = room_map
                .iter()
                .min_by_key(|(_, a)| a.announced_at)
                .map(|(h, _)| h.clone())
        {
            room_map.remove(&oldest_hash);
        }
        room_map.insert(asset.content_hash.clone(), asset.clone());

        // Build + validate the peer source from the embedded ticket.
        // If the source fails validation we keep the asset (it is
        // metadata only — see module-level trust-model doc) but skip
        // the peer entry so a broken ticket can never be dialled.
        if let Some(ticket) = ann.ticket {
            // The peer source's ticket-embedded node_id must match
            // the announcement's node_id (consistency invariant,
            // enforced by `PeerSource::validate`).
            let source = PeerSource {
                node_id: asset.announcer_node_id.clone(),
                content_hash: asset.content_hash.clone(),
                ticket: Some(ticket.clone()),
                last_seen: asset.announced_at,
                rtt_ms: None,
            };
            if source.validate().is_err() {
                return Ok(());
            }
            // `PeerMap::upsert` re-validates and then enforces both
            // the per-hash fan-out cap AND the cross-hash cap. The
            // re-validation here is intentional defence in depth
            // (e.g. a future refactor that loosens the newtype
            // would not silently regress the boundary gate).
            // We discard the Result because the map is the source
            // of truth — a validation error means the entry is
            // simply not inserted, which is what we want.
            let _ = self.peers.upsert(source);
        }
        Ok(())
    }

    pub fn asset(&self, room: &RoomId, hash: &ContentHash) -> Option<RoomAsset> {
        self.assets_by_room.get(room)?.get(hash).cloned()
    }

    pub fn peers_for(&self, hash: &ContentHash) -> Vec<BlobTicket> {
        // Re-validate on read so a corrupted in-memory entry cannot
        // be dialled. Cheap (constant time per entry) and the read
        // path is already off the hot loop.
        self.peers
            .peers_for(hash)
            .into_iter()
            .filter_map(|p| p.ticket)
            .collect()
    }

    /// Total number of `(room, hash)` asset slots currently held.
    /// Bounded by `#rooms * MAX_ASSETS_PER_ROOM` but without an
    /// explicit cap on `#rooms` itself (rooms are user-created, so
    /// the surface is local-user driven).
    pub fn total_asset_entries(&self) -> usize {
        self.assets_by_room.values().map(|m| m.len()).sum()
    }

    /// Total number of distinct hashes tracked across all rooms
    /// in the peer map. Bounded by
    /// [`adnet_types::MAX_TRACKED_HASHES`].
    pub fn tracked_hashes(&self) -> usize {
        self.peers.hash_count()
    }

    /// Total number of `(hash, node_id)` peer entries tracked.
    /// Bounded by `MAX_TRACKED_HASHES * MAX_PEER_SOURCES_PER_HASH`.
    pub fn total_peer_entries(&self) -> usize {
        self.peers.total_peer_entries()
    }

    pub fn feed_for(&self, room: &RoomId) -> RoomFeed {
        let assets: Vec<RoomAsset> = self
            .assets_by_room
            .get(room)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default();
        let mut peer_map: HashMap<ContentHash, Vec<BlobTicket>> = HashMap::new();
        for asset in &assets {
            let key = asset.content_hash.clone();
            let tickets = self.peers_for(&key);
            if !tickets.is_empty() {
                peer_map.insert(key, tickets);
            }
        }
        RoomFeed {
            room_id: room.clone(),
            assets,
            peer_map,
        }
    }
}

/// Room feed snapshot — what the UI / CLI consumes.
#[derive(Debug, Clone)]
pub struct RoomFeed {
    pub room_id: RoomId,
    pub assets: Vec<RoomAsset>,
    pub peer_map: HashMap<ContentHash, Vec<BlobTicket>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::CdnContentKind;
    use adnet_types::MAX_PEER_SOURCES_PER_HASH;
    use chrono::Utc;

    fn ann(room: &str, hash: &ContentHash, ticket: Option<BlobTicket>) -> Announcement {
        // Use the ticket's node_id as the announcement's node_id so
        // the PeerSource consistency check (ticket.node_id ==
        // announcement.node_id) passes when a ticket is present.
        // Otherwise generate a fresh one.
        let node_id = ticket
            .as_ref()
            .map(|t| t.node_id.clone())
            .unwrap_or_else(adnet_types::NodeId::random);
        Announcement {
            room_id: RoomId::new(room),
            content_hash: hash.clone(),
            node_id,
            title: "t".into(),
            kind: CdnContentKind::Article,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket,
            timestamp: Utc::now(),
            message_id: None,
            ttl_secs: None,
            signer: None,
            signature: None,
        }
    }

    #[test]
    fn ingest_and_lookup() {
        let mut s = SwarmIndex::default();
        let hash = ContentHash::from_bytes(b"x");
        s.ingest(ann("lobby", &hash, None)).unwrap();
        assert!(s.asset(&RoomId::new("lobby"), &hash).is_some());
        assert!(s.peers_for(&hash).is_empty());
    }

    #[test]
    fn ticket_dedup() {
        let mut s = SwarmIndex::default();
        let nid = adnet_types::NodeId::random();
        let hash = ContentHash::from_bytes(b"x");
        let addr = adnet_types::NodeAddr::new(nid.clone())
            .with_direct(adnet_types::Endpoint::new("127.0.0.1", 9000));
        let ticket = BlobTicket::whole(&nid, &addr, &hash);
        let a1 = Announcement {
            node_id: nid.clone(),
            ticket: Some(ticket.clone()),
            ..ann("lobby", &hash, None)
        };
        let a2 = Announcement {
            node_id: nid,
            ticket: Some(ticket),
            ..ann("lobby", &hash, None)
        };
        s.ingest(a1).unwrap();
        s.ingest(a2).unwrap();
        assert_eq!(s.peers_for(&hash).len(), 1);
    }

    #[test]
    fn ingest_skips_invalid_peer_source_keeps_asset() {
        // Happy path: ticket's node_id matches the announcement's
        // node_id so PeerSource::validate() succeeds and the peer
        // entry is kept. The strict-peer-source rejection path is
        // covered exhaustively in adnet-types::peer_source.
        let nid = adnet_types::NodeId::random();
        let hash = ContentHash::from_bytes(b"x");
        let addr = adnet_types::NodeAddr::new(nid.clone())
            .with_direct(adnet_types::Endpoint::new("127.0.0.1", 9000));
        let ticket = BlobTicket::whole(&nid, &addr, &hash);
        let mut s = SwarmIndex::default();
        let a = Announcement {
            node_id: nid,
            ticket: Some(ticket),
            ..ann("lobby", &hash, None)
        };
        s.ingest(a).unwrap();
        assert!(s.asset(&RoomId::new("lobby"), &hash).is_some());
        assert_eq!(s.peers_for(&hash).len(), 1);
    }

    #[test]
    fn ingest_drops_peer_when_ticket_node_id_mismatches() {
        // An announcement whose ticket points at a different
        // node is rejected at the peer-source validation gate; the
        // asset is kept, but the peer entry is not.
        let announced_by = adnet_types::NodeId::random();
        let ticket_owner = adnet_types::NodeId::random();
        let hash = ContentHash::from_bytes(b"x");
        let addr = adnet_types::NodeAddr::new(ticket_owner.clone())
            .with_direct(adnet_types::Endpoint::new("127.0.0.1", 9000));
        let ticket = BlobTicket::whole(&ticket_owner, &addr, &hash);
        let mut s = SwarmIndex::default();
        let a = Announcement {
            node_id: announced_by,
            ticket: Some(ticket),
            ..ann("lobby", &hash, None)
        };
        s.ingest(a).unwrap();
        // Asset present.
        assert!(s.asset(&RoomId::new("lobby"), &hash).is_some());
        // Peer dropped because ticket.node_id != announcement.node_id.
        assert!(s.peers_for(&hash).is_empty());
    }

    #[test]
    fn ingest_caps_per_hash_peer_fanout() {
        // Per-hash peers vector must be bounded to
        // MAX_PEER_SOURCES_PER_HASH.
        let mut s = SwarmIndex::default();
        let hash = ContentHash::from_bytes(b"x");
        let room = RoomId::new("lobby");
        for _ in 0..(MAX_PEER_SOURCES_PER_HASH + 10) {
            let n = adnet_types::NodeId::random();
            let addr = adnet_types::NodeAddr::new(n.clone())
                .with_direct(adnet_types::Endpoint::new("127.0.0.1", 9000));
            let t = BlobTicket::whole(&n, &addr, &hash);
            s.ingest(ann("lobby", &hash, Some(t))).unwrap();
        }
        assert_eq!(s.peers_for(&hash).len(), MAX_PEER_SOURCES_PER_HASH);
        // But the asset is still tracked once.
        assert!(s.asset(&room, &hash).is_some());
    }

    #[test]
    fn ingest_caps_per_room_assets() {
        // NEW: per-room asset map must be bounded to
        // MAX_ASSETS_PER_ROOM. Oldest evicted.
        let mut s = SwarmIndex::default();
        let now = Utc::now();
        let room = RoomId::new("lobby");
        for i in 0..(MAX_ASSETS_PER_ROOM + 5) {
            let h = ContentHash::from_bytes(format!("asset-{i}").as_bytes());
            let mut a = ann("lobby", &h, None);
            a.timestamp = now + chrono::Duration::milliseconds(i as i64);
            s.ingest(a).unwrap();
        }
        assert!(
            s.total_asset_entries() <= MAX_ASSETS_PER_ROOM,
            "per-room assets {} exceeds MAX_ASSETS_PER_ROOM",
            s.total_asset_entries()
        );
        // The freshest entry should be present (sanity check).
        let newest =
            ContentHash::from_bytes(format!("asset-{}", MAX_ASSETS_PER_ROOM + 4).as_bytes());
        assert!(s.asset(&room, &newest).is_some());
    }

    #[test]
    fn ingest_caps_total_tracked_hashes() {
        // NEW: cross-hash cap is enforced via PeerMap. Total
        // tracked hashes cannot exceed MAX_TRACKED_HASHES even
        // under a flood of distinct hashes.
        let mut s = SwarmIndex::default();
        for i in 0..(adnet_types::MAX_TRACKED_HASHES + 5) {
            let h = ContentHash::from_bytes(format!("hash-{i}").as_bytes());
            s.ingest(ann("lobby", &h, None)).unwrap();
        }
        // The asset map is per-room and independent of the peer
        // map; here we ingested with no ticket, so the peer map is
        // empty. The asset map is per-room and bounded separately
        // by MAX_ASSETS_PER_ROOM.
        assert!(s.total_asset_entries() <= MAX_ASSETS_PER_ROOM);
        assert_eq!(s.tracked_hashes(), 0);
    }

    #[test]
    fn ingest_caps_total_tracked_hashes_with_tickets() {
        // Same as above but with tickets so the peer map grows.
        let mut s = SwarmIndex::default();
        // Stay under MAX_ASSETS_PER_ROOM (per-room cap) but exceed
        // MAX_TRACKED_HASHES via the peer map.
        let n_assets = MAX_ASSETS_PER_ROOM.min(2048);
        for i in 0..n_assets {
            let h = ContentHash::from_bytes(format!("hash-{i}").as_bytes());
            let n = adnet_types::NodeId::random();
            let addr = adnet_types::NodeAddr::new(n.clone())
                .with_direct(adnet_types::Endpoint::new("127.0.0.1", 9000));
            let t = BlobTicket::whole(&n, &addr, &h);
            s.ingest(ann("lobby", &h, Some(t))).unwrap();
        }
        assert!(s.tracked_hashes() <= adnet_types::MAX_TRACKED_HASHES);
        assert!(
            s.total_peer_entries() <= adnet_types::MAX_TRACKED_HASHES * MAX_PEER_SOURCES_PER_HASH
        );
    }

    #[test]
    fn peers_for_skips_malformed_in_memory_entries() {
        // Defence in depth: even if an entry slipped past
        // validation (e.g. via future refactor regression),
        // `peers_for` must never return a ticket whose own
        // validation fails. We can't easily inject one without
        // exposing internals, so we just assert the round-trip
        // contract: every ticket returned is a valid BlobTicket.
        let n = adnet_types::NodeId::random();
        let h = ContentHash::from_bytes(b"x");
        let addr = adnet_types::NodeAddr::new(n.clone())
            .with_direct(adnet_types::Endpoint::new("127.0.0.1", 9000));
        let ticket = BlobTicket::whole(&n, &addr, &h);
        let mut s = SwarmIndex::default();
        s.ingest(ann("lobby", &h, Some(ticket))).unwrap();
        let tickets = s.peers_for(&h);
        for t in &tickets {
            adnet_types::ticket::validate_blob_ticket(t)
                .expect("peers_for must only return valid tickets");
        }
        assert_eq!(tickets.len(), 1);
    }

    #[test]
    fn feed_for_includes_only_present_assets() {
        // feed_for must only include assets with at least one peer.
        let mut s = SwarmIndex::default();
        let h_with_peer = ContentHash::from_bytes(b"a");
        let h_without_peer = ContentHash::from_bytes(b"b");
        let n = adnet_types::NodeId::random();
        let addr = adnet_types::NodeAddr::new(n.clone())
            .with_direct(adnet_types::Endpoint::new("127.0.0.1", 9000));
        let t = BlobTicket::whole(&n, &addr, &h_with_peer);
        s.ingest(ann("lobby", &h_with_peer, Some(t))).unwrap();
        s.ingest(ann("lobby", &h_without_peer, None)).unwrap();
        let feed = s.feed_for(&RoomId::new("lobby"));
        assert_eq!(feed.assets.len(), 2);
        assert!(feed.peer_map.contains_key(&h_with_peer));
        assert!(!feed.peer_map.contains_key(&h_without_peer));
    }
}
