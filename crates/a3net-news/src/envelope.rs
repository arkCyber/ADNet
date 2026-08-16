//! Wire envelope carrying a [`BulletinItem`] over gossip.
//!
//! The envelope is a tiny wrapper around the business object so the
//! underlying `GossipTransport` (which already speaks
//! `AnnouncementPayload { from_node, payload: serde_json::Value }`)
//! can route bulletins without further plumbing. The `payload`
//! field carries the JSON-encoded `BulletinItem`, exactly like the
//! announcement path does.
//!
//! ## Versioning
//!
//! `version` starts at `1`. Any breaking change to the envelope
//! shape MUST bump it and the receiver MUST reject envelopes with
//! a higher version than it knows about — silently accepting a
//! newer wire format is how post-quantum crypto rollouts silently
//! break a fleet.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

use a3net_types::{
    AdnetError, BulletinItem, BulletinKind, NodeId, Result as AdnetResult, WalletAddress,
    validate_id,
};

use crate::error::{NewsError, NewsResult};

/// Topic-name prefix for bulletin broadcasts. The full topic id is
/// derived from `<prefix>-<room_id>` (matching the `a3net-room-…`
/// convention used by `a3net-gossip`), so external observers can
/// recognise the family.
pub const BULLETIN_TOPIC_PREFIX: &str = "a3net-news";

/// Current wire version. Bump on any breaking envelope change.
pub const BULLETIN_ENVELOPE_VERSION: u8 = 1;

/// Wire envelope — `(version, from_node, item, signer, signature)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulletinEnvelope {
    pub version: u8,
    /// Originating node id. May not match the bulletin author's
    /// `NodeId` — gossip bridges, mirrors and replicas forward
    /// envelopes as-is. Receivers must verify
    /// `envelope.from_node == item.author_id` for `Strict` mode.
    pub from_node: NodeId,
    /// The business object.
    pub item: BulletinItem,
    /// Optional wallet signer (mirrors `Announcement::signer`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer: Option<WalletAddress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Vec<u8>>,
}

impl BulletinEnvelope {
    /// Wrap a `BulletinItem` into an envelope ready for publishing.
    /// The envelope is NOT validated here — `validate()` is left to
    /// the caller so the service layer can perform signature checks
    /// before the gossip broadcast.
    pub fn wrap(item: BulletinItem, from_node: NodeId) -> Self {
        Self {
            version: BULLETIN_ENVELOPE_VERSION,
            from_node,
            item,
            signer: None,
            signature: None,
        }
    }

    /// Attach a wallet signature (signature scheme tag is
    /// `signature[0]`; verifier is `a3net-identity::Wallet`).
    /// Mirrors the signature onto the embedded item so the
    /// canonical preimage includes the wallet claim.
    pub fn attach_signature(&mut self, signer: WalletAddress, signature: Vec<u8>) {
        self.item.attach_signature(signer, signature.clone());
        self.signer = Some(signer);
        self.signature = Some(signature);
    }

    /// `true` iff the envelope carries a wallet signature.
    pub fn is_signed(&self) -> bool {
        self.signer.is_some() && self.signature.is_some()
    }

    /// Validate every field of the envelope. Runs
    /// [`BulletinItem::validate`] and enforces the version cap.
    pub fn validate(&self) -> NewsResult<()> {
        if self.version == 0 || self.version > BULLETIN_ENVELOPE_VERSION {
            return Err(NewsError::Validation(format!(
                "envelope version: {} not in 1..={}",
                self.version, BULLETIN_ENVELOPE_VERSION
            )));
        }
        self.item.validate().map_err(|e| match e {
            AdnetError::Validation(m) => NewsError::Validation(m),
            other => NewsError::Internal(other.to_string()),
        })?;
        Ok(())
    }

    /// Compute the JSON-encoded payload (`item` only) the way the
    /// gossip bridge expects.
    pub fn payload_value(&self) -> NewsResult<Value> {
        serde_json::to_value(&self.item).map_err(NewsError::from)
    }
}

/// Helper: convert an envelope to the gossip bridge's wire
/// payload. The transport expects `(from_node, payload: Value)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulletinEnvelopePayload {
    pub from_node: NodeId,
    pub payload: Value,
}

impl BulletinEnvelopePayload {
    pub fn from_envelope(env: &BulletinEnvelope) -> NewsResult<Self> {
        Ok(Self {
            from_node: env.from_node.clone(),
            payload: env.payload_value()?,
        })
    }

    pub fn into_envelope(self, default_version: u8) -> NewsResult<BulletinEnvelope> {
        let item: BulletinItem =
            serde_json::from_value(self.payload).map_err(NewsError::from)?;
        Ok(BulletinEnvelope {
            version: default_version,
            from_node: self.from_node,
            item,
            signer: None,
            signature: None,
        })
    }
}

/// Topic id derivation. Matches `GossipBus::topic_for` so external
/// observers see the same BLAKE3 hex.
pub fn topic_id(room: &a3net_types::RoomId) -> a3net_types::Topic {
    use a3net_types::Topic;
    Topic::from_label(&format!("{BULLETIN_TOPIC_PREFIX}-{}", room.as_str()))
}

/// Typed event stream emitted to subscribers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulletinEvent {
    /// A brand-new bulletin was inserted into the store. The store
    /// has already deduplicated it by id (so receiving the same id
    /// twice does not produce two events).
    Insert(BulletinItem),
    /// A correction was applied — the store keeps the original
    /// bulletin (id = `supersedes`) and surfaces the corrected
    /// payload here. UI clients can use this to refresh rendered
    /// cards.
    Correction {
        superseded_id: a3net_types::BulletinId,
        corrected: BulletinItem,
    },
    /// A retraction superseded a bulletin. The original record is
    /// still in the store but is considered "withdrawn" downstream.
    Retraction {
        superseded_id: a3net_types::BulletinId,
        retraction: BulletinItem,
    },
    /// Catch-up replay finished after restart. Useful for tests /
    /// clients that want to know when the local store has finished
    /// re-emitting historical bulletins.
    ReplayComplete {
        room: a3net_types::RoomId,
        replayed: usize,
    },
}

impl BulletinEvent {
    pub fn item(&self) -> &BulletinItem {
        match self {
            Self::Insert(item) | Self::Correction { corrected: item, .. }
            | Self::Retraction { retraction: item, .. } => item,
            Self::ReplayComplete { .. } => {
                static EMPTY: std::sync::OnceLock<BulletinItem> = std::sync::OnceLock::new();
                EMPTY.get_or_init(|| {
                    BulletinItem::new(
                        BulletinKind::Announcement,
                        a3net_types::BulletinCategory::General,
                        a3net_types::BulletinSeverity::Info,
                        a3net_types::RoomId::new("__internal__"),
                        NodeId::random(),
                        "",
                        "internal",
                        "internal event — no body",
                        b"internal",
                        None,
                    )
                    .expect("internal bulletin template validates")
                })
            }
        }
    }
}

/// Helper used by tests to obtain a typed receiver wrapper for a
/// sender. Mirrors `AnnouncementRx` in `a3net-gossip`.
pub fn empty_sender() -> broadcast::Sender<BulletinEvent> {
    broadcast::channel::<BulletinEvent>(16).0
}

/// Sanity check used by `validate_id` callers. Kept here so the
/// envelope layer doesn't grow dependent on `a3net-types` for the
/// auxiliary helpers.
pub(crate) fn ensure_id(field: &str, value: &str) -> AdnetResult<()> {
    validate_id(field, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::{BulletinCategory, BulletinSeverity, RoomId};
    use chrono::Utc;

    fn node() -> NodeId {
        NodeId::random()
    }

    fn good_item() -> BulletinItem {
        BulletinItem::new(
            BulletinKind::Announcement,
            BulletinCategory::General,
            BulletinSeverity::Info,
            RoomId::new("tech"),
            node(),
            "Title",
            "Summary",
            "Body",
            b"nonce",
            None,
        )
        .unwrap()
    }

    #[test]
    fn wrap_roundtrip_via_payload() {
        let item = good_item();
        let env = BulletinEnvelope::wrap(item.clone(), node());
        let payload = BulletinEnvelopePayload::from_envelope(&env).unwrap();
        assert_eq!(payload.from_node, env.from_node);
        let back = payload.into_envelope(BULLETIN_ENVELOPE_VERSION).unwrap();
        assert_eq!(back.item, item);
        assert_eq!(back.version, BULLETIN_ENVELOPE_VERSION);
    }

    #[test]
    fn validate_accepts_well_formed_envelope() {
        let env = BulletinEnvelope::wrap(good_item(), node());
        assert!(env.validate().is_ok());
    }

    #[test]
    fn validate_rejects_zero_version() {
        let mut env = BulletinEnvelope::wrap(good_item(), node());
        env.version = 0;
        let err = env.validate().unwrap_err();
        assert!(
            err.to_string().contains("envelope version"),
            "got {err}"
        );
    }

    #[test]
    fn validate_rejects_future_version() {
        let mut env = BulletinEnvelope::wrap(good_item(), node());
        env.version = BULLETIN_ENVELOPE_VERSION + 1;
        assert!(env.validate().is_err());
    }

    #[test]
    fn topic_id_uses_news_prefix() {
        let t = topic_id(&RoomId::new("tech"));
        assert!(t.as_hex().len() == 64);
        // Determinism.
        assert_eq!(t, topic_id(&RoomId::new("tech")));
        // Different room => different topic.
        assert_ne!(t, topic_id(&RoomId::new("general")));
    }

    #[test]
    fn attach_signature_marks_signed() {
        let mut env = BulletinEnvelope::wrap(good_item(), node());
        assert!(!env.is_signed());
        env.attach_signature(
            WalletAddress::from_bytes([0x01u8; 20]),
            vec![0u8; 65],
        );
        assert!(env.is_signed());
    }
}