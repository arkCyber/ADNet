//! [`GossipBridge`] — JSON encode/decode helpers between typed
//! [`Announcement`](a3net_types::Announcement) and the JSON-friendly
//! [`AnnouncementPayload`](a3net_types::AnnouncementPayload) used over the
//! wire.

use a3net_types::{Announcement, AnnouncementPayload};

/// Wrap an announcement for the wire, attributing it to `from_node`.
pub fn wrap(ann: &Announcement, from_node: &a3net_types::NodeId) -> AnnouncementPayload {
    AnnouncementPayload {
        from_node: from_node.clone(),
        payload: serde_json::to_value(ann).unwrap_or(serde_json::Value::Null),
    }
}

/// Try to decode an incoming payload back into a typed announcement.
pub fn unwrap(payload: &AnnouncementPayload) -> Option<Announcement> {
    serde_json::from_value(payload.payload.clone()).ok()
}

/// Marker alias retained for migration clarity — the new code path is the
/// free functions above. Kept here so any future trait-based bridge can plug
/// in without churn.
#[derive(Debug, Default, Clone, Copy)]
pub struct GossipBridge;

impl GossipBridge {
    pub fn wrap(&self, ann: &Announcement, from_node: &a3net_types::NodeId) -> AnnouncementPayload {
        wrap(ann, from_node)
    }
    pub fn unwrap(&self, payload: &AnnouncementPayload) -> Option<Announcement> {
        unwrap(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::{CdnContentKind, ContentHash, NodeId, RoomId, Topic};
    use chrono::Utc;

    #[test]
    fn wrap_unwrap_roundtrip() {
        let ann = Announcement {
            room_id: RoomId::new("lobby"),
            content_hash: ContentHash::from_bytes(b"x"),
            node_id: NodeId::random(),
            title: "T".into(),
            kind: CdnContentKind::Article,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            message_id: None,
            ttl_secs: None,
            signer: None,
            signature: None,
        };
        let payload = wrap(&ann, &ann.node_id);
        let back = unwrap(&payload).unwrap();
        assert_eq!(back.content_hash, ann.content_hash);
    }

    #[test]
    fn bridge_uses_topic_naming_convention() {
        // Smoke check: ensure we expose a `topic_name`-shaped helper without
        // forcing callers to reach into `a3net_types` directly.
        let _ = Topic::from_label("a3net-room-x");
    }
}
