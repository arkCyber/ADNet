//! Room identifiers and asset records.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::content::{CdnContentKind, ContentHash};
use crate::node::NodeId;

/// Logical room / lobby / topic identifier (free-form UTF-8 string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomId(String);

impl RoomId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RoomId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for RoomId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for RoomId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Metadata describing a single content-addressed asset shared inside a room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomAsset {
    pub content_hash: ContentHash,
    pub title: String,
    pub kind: CdnContentKind,
    pub size_bytes: u64,
    pub mime_type: Option<String>,
    pub source_url: Option<String>,
    pub room_id: RoomId,
    pub announcer_node_id: NodeId,
    pub announced_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_id_is_transparent() {
        let json = serde_json::to_string(&RoomId::new("lobby")).unwrap();
        assert_eq!(json, "\"lobby\"");
    }

    #[test]
    fn room_asset_serializes_camel() {
        let asset = RoomAsset {
            content_hash: ContentHash::from_bytes(b"x"),
            title: "demo".into(),
            kind: CdnContentKind::Article,
            size_bytes: 42,
            mime_type: Some("text/plain".into()),
            source_url: None,
            room_id: RoomId::new("lobby"),
            announcer_node_id: NodeId::random(),
            announced_at: Utc::now(),
        };
        let v: serde_json::Value = serde_json::to_value(&asset).unwrap();
        assert!(v.get("contentHash").is_some());
        assert!(v.get("sizeBytes").is_some());
        assert!(v.get("announcerNodeId").is_some());
    }
}
