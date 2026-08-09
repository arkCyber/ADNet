//! Gossip announcement payloads — what peers broadcast into a room topic.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

use crate::content::{CdnContentKind, ContentHash};
use crate::error::{AdnetError, Result};
use crate::invariants::{MAX_NAME_LEN, validate_name, validate_url};
use crate::node::NodeId;
use crate::room::RoomId;
use crate::ticket::BlobTicket;
use crate::wallet_address::WalletAddress;

/// Maximum size of a single asset that may be announced. 1 TiB matches
/// the blobstore chunk-group ceiling and prevents a malicious peer from
/// gossiping a "trust me bro, it's 10 EiB" announcement.
pub const MAX_ANNOUNCED_SIZE: u64 = 1u64 << 40;

/// Maximum length of an `Announcement::signature` blob on the wire.
///
/// The current largest scheme we expect is 96-byte BLS12-381 G2
/// (`scheme_tag || sig_bytes`). Add a generous constant — 1 KiB —
/// above that so any plausible future scheme (PQ Dilithium ~2 KiB
/// signatures still fit with headroom) is admitted while keeping a
/// gossip-bound payload bounded. Anything larger is rejected by
/// [`Announcement::validate`] before the signature bytes are ever
/// fed to the verifier.
pub const MAX_SIGNATURE_LEN: usize = 1024;

/// Maximum clock skew tolerated between the local wall clock and the
/// `timestamp` field on an announcement. Mirrors
/// [`crate::peer_source::MAX_CLOCK_SKEW_HOURS`] so the same window
/// applies whether the announcement is inspected as a raw gossip
/// frame or after it has been promoted to a `PeerSource`.
pub const MAX_TIMESTAMP_SKEW_HOURS: i64 = 24;

/// Canonical, signer-vouched fields. Hand-rolled rather than using a
/// `serde_json::Map` because we want a stable JSON form (alphabetical
/// keys, no whitespace) that survives `serde_json` upgrades. Adding
/// or removing a vouched field here is a wire-format change and must
/// be done with care. Mirrors `adnet_token::PledgeBody::digest`'s
/// style so the two crates' digest algorithms stay aligned.
#[derive(Debug, Serialize)]
struct SignableAnnouncement<'a> {
    content_hash: &'a ContentHash,
    kind: &'a CdnContentKind,
    mime_type: &'a Option<String>,
    node_id: &'a NodeId,
    room_id: &'a RoomId,
    size_bytes: &'a u64,
    source_url: &'a Option<String>,
    ticket: &'a Option<BlobTicket>,
    timestamp: &'a DateTime<Utc>,
    title: &'a String,
}

/// A peer's announcement that it can serve a particular content hash.
///
/// This is the wire format flowing through both [`adnet-gossip`](crate) and
/// the in-process gossip bus. It is intentionally JSON-friendly so the same
/// payload can travel over UDP gossip, Unix sockets, or HTTP webhook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Announcement {
    pub room_id: RoomId,
    pub content_hash: ContentHash,
    pub node_id: NodeId,
    pub title: String,
    pub kind: CdnContentKind,
    pub size_bytes: u64,
    pub mime_type: Option<String>,
    pub source_url: Option<String>,
    pub ticket: Option<BlobTicket>,
    pub timestamp: DateTime<Utc>,
    /// Optional wallet address that vouches for this announcement.
    ///
    /// When `Some`, gossip peers can verify the announcement was signed
    /// by the holder of this wallet's private key. The signature is
    /// **over a canonical JSON encoding of every other field of the
    /// announcement** (excluding `signer` and `signature` themselves);
    /// see [`Announcement::verify_signature`] for the exact preimage.
    ///
    /// The scheme tag (first byte of `signature`) decides which verifier
    /// to use. `0` = EIP-191 over secp256k1 (65 bytes total, including
    /// the scheme tag); `1` = Ed25519 (65 bytes total). The verifier
    /// lives in `adnet-identity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer: Option<WalletAddress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Vec<u8>>,
}

impl Announcement {
    /// Validate every field of the announcement. Returns `Ok(())` if
    /// the record is safe to ingest into the swarm index, or the first
    /// [`AdnetError::Validation`] failure otherwise.
    ///
    /// Checks:
    /// - `title` is non-empty and bounded by [`MAX_NAME_LEN`]
    /// - `size_bytes` is in `[0, MAX_ANNOUNCED_SIZE]`
    /// - `mime_type` (when present) is bounded by [`MAX_NAME_LEN`]
    /// - `source_url` (when present) is bounded by [`MAX_NAME_LEN`]
    /// - `ticket` (when present) is itself a valid ticket
    /// - `signature` (when present) is bounded by [`MAX_SIGNATURE_LEN`]
    ///   — DoS guard. Larger blobs are rejected before the verifier
    ///   sees them so a gossip peer cannot be tricked into O(n) crypto
    ///   work for an arbitrarily large signature.
    /// - `timestamp` is within ±[`MAX_TIMESTAMP_SKEW_HOURS`] of the
    ///   local wall clock — guards against replay / far-future
    ///   forging. Mirrors [`crate::peer_source::MAX_CLOCK_SKEW_HOURS`].
    /// - `signer` and `signature` are either both `None` or both
    ///   `Some(_)` — a partially-signed announcement is malformed
    ///   and must not slip through.
    pub fn validate(&self) -> Result<()> {
        validate_name("title", &self.title)?;
        if self.title.is_empty() {
            return Err(AdnetError::Validation("title: empty".into()));
        }
        if self.size_bytes > MAX_ANNOUNCED_SIZE {
            return Err(AdnetError::Validation(format!(
                "size_bytes: {} exceeds MAX_ANNOUNCED_SIZE ({} = 1 TiB)",
                self.size_bytes, MAX_ANNOUNCED_SIZE
            )));
        }
        if let Some(m) = &self.mime_type {
            if m.len() > MAX_NAME_LEN {
                return Err(AdnetError::Validation(format!(
                    "mime_type: exceeds {MAX_NAME_LEN} bytes"
                )));
            }
            if m.as_bytes().contains(&0) {
                return Err(AdnetError::Validation("mime_type: contains NUL".into()));
            }
        }
        if let Some(u) = &self.source_url {
            validate_url("source_url", u)?;
        }
        if let Some(t) = &self.ticket {
            crate::ticket::validate_blob_ticket(t)?;
        }
        if let Some(sig) = &self.signature
            && sig.len() > MAX_SIGNATURE_LEN
        {
            return Err(AdnetError::Validation(format!(
                "signature: {} bytes exceeds MAX_SIGNATURE_LEN ({} = 1 KiB)",
                sig.len(),
                MAX_SIGNATURE_LEN
            )));
        }
        if (self.signer.is_some()) != (self.signature.is_some()) {
            return Err(AdnetError::Validation(
                "signer/signature must be both present or both absent".into(),
            ));
        }
        let now = Utc::now();
        let limit = ChronoDuration::hours(MAX_TIMESTAMP_SKEW_HOURS);
        let earliest = now - limit;
        let latest = now + limit;
        if self.timestamp < earliest || self.timestamp > latest {
            return Err(AdnetError::Validation(format!(
                "timestamp: {} outside the ±{}h window around {}",
                self.timestamp, MAX_TIMESTAMP_SKEW_HOURS, now
            )));
        }
        Ok(())
    }

    /// Helper: parse an AI recommendation JSON (snake or camel) into an
    /// announcement bound to a known `room_id` and `node_id`.
    ///
    /// Mirrors the Exodus `from_ai_recommendation` helper for migration parity.
    pub fn from_ai_recommendation(
        room_id: impl Into<RoomId>,
        node_id: &NodeId,
        payload: &serde_json::Value,
    ) -> Option<Self> {
        let content_hash = payload
            .get("contentHash")
            .or_else(|| payload.get("content_hash"))
            .and_then(|v| v.as_str())
            .and_then(|s| ContentHash::from_hex(s).ok())?;
        let title = payload
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Shared content")
            .to_string();
        let kind = payload
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(CdnContentKind::from_str_loose)
            .unwrap_or(CdnContentKind::GenericFile);
        let size_bytes = payload
            .get("sizeBytes")
            .or_else(|| payload.get("size_bytes"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Some(Self {
            room_id: room_id.into(),
            content_hash,
            node_id: node_id.clone(),
            title,
            kind,
            size_bytes,
            mime_type: payload
                .get("mimeType")
                .and_then(|v| v.as_str())
                .map(String::from),
            source_url: payload
                .get("sourceUrl")
                .or_else(|| payload.get("source_url"))
                .and_then(|v| v.as_str())
                .map(String::from),
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
        })
    }

    /// Compute the canonical preimage for signing. The preimage is a
    /// JSON object that contains **only the announcement fields a
    /// signer vouches for** — `signer` and `signature` are excluded.
    /// The struct fields are serialized in alphabetical order so the
    /// byte form is stable across `serde_json` versions and platforms.
    ///
    /// Returns the raw bytes; feed them through whatever hash function
    /// the signature scheme expects before signing.
    pub fn signing_preimage(&self) -> Result<Vec<u8>> {
        let signable = SignableAnnouncement {
            content_hash: &self.content_hash,
            kind: &self.kind,
            mime_type: &self.mime_type,
            node_id: &self.node_id,
            room_id: &self.room_id,
            size_bytes: &self.size_bytes,
            source_url: &self.source_url,
            ticket: &self.ticket,
            timestamp: &self.timestamp,
            title: &self.title,
        };
        Ok(serde_json::to_vec(&signable)?)
    }

    /// Pin a signature onto this announcement. The signature is stored
    /// verbatim; callers are responsible for producing it (typically via
    /// `adnet-identity::Wallet::sign_personal` over `blake3(preimage)`).
    ///
    /// `signature[0]` is the scheme tag (`0` = secp256k1 / EIP-191,
    /// `1` = Ed25519). Length constraints are enforced by the verifier.
    pub fn attach_signature(&mut self, signer: WalletAddress, signature: Vec<u8>) {
        self.signer = Some(signer);
        self.signature = Some(signature);
    }

    /// True iff the announcement carries a signature.
    pub fn is_signed(&self) -> bool {
        self.signer.is_some() && self.signature.is_some()
    }

    /// Strip the signature from a copy of this announcement. Useful
    /// when re-broadcasting a signed announcement through a channel
    /// that doesn't want cryptographic metadata.
    pub fn without_signature(mut self) -> Self {
        self.signer = None;
        self.signature = None;
        self
    }
}

/// Wrapper used by the gossip overlay — the raw payload plus sender metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnouncementPayload {
    pub from_node: NodeId,
    pub payload: serde_json::Value,
}

impl From<&Announcement> for AnnouncementPayload {
    fn from(a: &Announcement) -> Self {
        Self {
            from_node: a.node_id.clone(),
            payload: serde_json::to_value(a).unwrap_or(serde_json::Value::Null),
        }
    }
}

impl TryFrom<&AnnouncementPayload> for Announcement {
    type Error = crate::error::AdnetError;

    fn try_from(p: &AnnouncementPayload) -> Result<Self, Self::Error> {
        serde_json::from_value(p.payload.clone())
            .map_err(|e| crate::error::AdnetError::InvalidTicket(format!("announcement json: {e}")))
    }
}

/// Forward [`Announcement::validate`] to the [`Validate`] trait so the
/// IPC and gossip layers can use a uniform gate.
impl crate::Validate for Announcement {
    fn validate(&self) -> Result<()> {
        self.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeAddr;

    #[test]
    fn announcement_json_roundtrip() {
        let a = Announcement {
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
            signer: None,
            signature: None,
        };
        let v = serde_json::to_value(&a).unwrap();
        let back: Announcement = serde_json::from_value(v).unwrap();
        assert_eq!(back.content_hash, a.content_hash);
        assert_eq!(back.room_id, a.room_id);
    }

    #[test]
    fn from_ai_recommendation_accepts_both_cases() {
        let nid = NodeId::random();
        let p = serde_json::json!({
            "content_hash": "5daf0f9324cf6cd9fbcb09e8a3eaaaa3e7c84aeef1bba7e8b4fec7b4d28d2c33",
            "title": "Llama 8B GGUF",
            "kind": "llm",
            "size_bytes": 5_000_000_000_u64
        });
        let a = Announcement::from_ai_recommendation("lobby", &nid, &p).unwrap();
        assert_eq!(a.kind, CdnContentKind::AiModel);
        assert_eq!(a.size_bytes, 5_000_000_000);
    }

    fn good_announcement() -> Announcement {
        Announcement {
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
            signer: None,
            signature: None,
        }
    }

    #[test]
    fn validate_accepts_good_announcement() {
        assert!(good_announcement().validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_title() {
        let mut a = good_announcement();
        a.title = "".into();
        let err = a.validate().unwrap_err();
        assert!(err.to_string().contains("title"), "got {err}");
    }

    #[test]
    fn validate_rejects_oversize_size() {
        let mut a = good_announcement();
        a.size_bytes = MAX_ANNOUNCED_SIZE + 1;
        let err = a.validate().unwrap_err();
        assert!(err.to_string().contains("size_bytes"), "got {err}");
    }

    #[test]
    fn validate_rejects_oversize_mime() {
        let mut a = good_announcement();
        a.mime_type = Some("x".repeat(MAX_NAME_LEN + 1));
        assert!(a.validate().is_err());
    }

    #[test]
    fn validate_rejects_mime_with_nul() {
        let mut a = good_announcement();
        a.mime_type = Some("ok\0bad".into());
        assert!(a.validate().is_err());
    }

    #[test]
    fn validate_rejects_oversize_url() {
        let mut a = good_announcement();
        a.source_url = Some("x".repeat(MAX_NAME_LEN + 1));
        assert!(a.validate().is_err());
    }

    #[test]
    fn validate_rejects_malformed_ticket() {
        let mut a = good_announcement();
        // A `BlobTicket` with an unparseable endpoint is malformed at
        // the wire level. The round-trip parse must reject it.
        let bad_ticket = crate::ticket::BlobTicket::whole(
            &NodeId::random(),
            // Construct a NodeAddr whose `node_id` is valid but skip
            // the direct / relay by using the new() constructor — the
            // ticket will still encode a valid addr part. The easiest
            // way to make it fail round-trip is to corrupt the
            // content_hash bytes, which fails at the parser.
            &NodeAddr::new(NodeId::random()),
            &ContentHash::from_bytes(b"x"),
        );
        // Mutate `range` to an invalid (inverted) Single range. The
        // round-trip parse will reject this with `InvalidTicket`,
        // proving that validate_blob_ticket surfaces the parse error.
        let mut bad_ticket = bad_ticket;
        bad_ticket.range = crate::range::RangeSpec::Single(crate::range::ByteRange {
            start: 100,
            end: 50,
        });
        a.ticket = Some(bad_ticket);
        let err = a.validate().unwrap_err();
        assert!(
            err.to_string().contains("blob_ticket") || err.to_string().contains("range"),
            "got {err}"
        );
    }

    #[test]
    fn validate_accepts_edge_size_max() {
        let mut a = good_announcement();
        a.size_bytes = MAX_ANNOUNCED_SIZE;
        assert!(a.validate().is_ok());
    }

    #[test]
    fn unsigned_announcement_is_not_signed() {
        let a = good_announcement();
        assert!(!a.is_signed());
    }

    #[test]
    fn signing_preimage_is_deterministic() {
        let a = good_announcement();
        let p1 = a.signing_preimage().unwrap();
        let p2 = a.signing_preimage().unwrap();
        assert_eq!(p1, p2);
        // Mutating any field mutates the preimage.
        let mut a2 = a.clone();
        a2.title = "Different".into();
        let p3 = a2.signing_preimage().unwrap();
        assert_ne!(p1, p3);
    }

    #[test]
    fn attach_and_strip_signature() {
        let mut a = good_announcement();
        let signer = WalletAddress::from_bytes([0x42u8; 20]);
        a.attach_signature(signer, vec![0x00; 65]);
        assert!(a.is_signed());
        assert_eq!(a.signer, Some(signer));
        assert_eq!(a.signature.as_ref().unwrap().len(), 65);

        let stripped = a.clone().without_signature();
        assert!(!stripped.is_signed());
        // Stripped version still has the same content_hash.
        assert_eq!(stripped.content_hash, a.content_hash);
    }

    #[test]
    fn signed_announcement_serde_round_trip() {
        let mut a = good_announcement();
        let signer = WalletAddress::from_bytes([0x42u8; 20]);
        a.attach_signature(signer, vec![0x00, 0x01, 0x02, 0x03]);
        let json = serde_json::to_string(&a).unwrap();
        let back: Announcement = serde_json::from_str(&json).unwrap();
        assert_eq!(back.signer, Some(signer));
        assert_eq!(back.signature, Some(vec![0x00, 0x01, 0x02, 0x03]));
    }

    #[test]
    fn missing_signature_fields_default_to_none() {
        // Old gossip peers don't know about the new optional fields;
        // serde should give us None. We use the wire-format camelCase.
        let json = serde_json::json!({
            "roomId": "lobby",
            "contentHash": "5daf0f9324cf6cd9fbcb09e8a3eaaaa3e7c84aeef1bba7e8b4fec7b4d28d2c33",
            "nodeId": "0000000000000000000000000000000000000000000000000000000000000000",
            "title": "T",
            "kind": "article",
            "sizeBytes": 1,
            "mimeType": null,
            "sourceUrl": null,
            "ticket": null,
            "timestamp": "2024-01-01T00:00:00Z",
        });
        let a: Announcement = serde_json::from_value(json).unwrap();
        assert!(!a.is_signed());
    }

    #[test]
    fn validate_rejects_oversize_signature() {
        // DoS guard — a 2 KiB signature blob must be rejected before
        // any verifier touches it.
        let mut a = good_announcement();
        a.attach_signature(
            WalletAddress::from_bytes([0x42u8; 20]),
            vec![0u8; MAX_SIGNATURE_LEN + 1],
        );
        let err = a.validate().unwrap_err();
        assert!(
            err.to_string().contains("signature:") && err.to_string().contains("MAX_SIGNATURE_LEN"),
            "got {err}"
        );
    }

    #[test]
    fn validate_accepts_signature_at_limit() {
        let mut a = good_announcement();
        a.attach_signature(
            WalletAddress::from_bytes([0x42u8; 20]),
            vec![0u8; MAX_SIGNATURE_LEN],
        );
        assert!(a.validate().is_ok());
    }

    #[test]
    fn validate_rejects_partial_signature() {
        let mut a = good_announcement();
        a.signer = Some(WalletAddress::from_bytes([0x01u8; 20]));
        // signature stays None — partially signed.
        let err = a.validate().unwrap_err();
        assert!(err.to_string().contains("signer/signature"), "got {err}");

        let mut a = good_announcement();
        a.signature = Some(vec![0u8; 65]);
        let err = a.validate().unwrap_err();
        assert!(err.to_string().contains("signer/signature"), "got {err}");
    }

    #[test]
    fn validate_rejects_far_future_timestamp() {
        let mut a = good_announcement();
        a.timestamp = Utc::now() + ChronoDuration::hours(MAX_TIMESTAMP_SKEW_HOURS + 1);
        let err = a.validate().unwrap_err();
        assert!(err.to_string().contains("timestamp:"), "got {err}");
    }

    #[test]
    fn validate_rejects_far_past_timestamp() {
        let mut a = good_announcement();
        a.timestamp = Utc::now() - ChronoDuration::hours(MAX_TIMESTAMP_SKEW_HOURS + 1);
        let err = a.validate().unwrap_err();
        assert!(err.to_string().contains("timestamp:"), "got {err}");
    }

    #[test]
    fn validate_accepts_timestamp_at_skew_edge() {
        let mut a = good_announcement();
        a.timestamp = Utc::now() + ChronoDuration::hours(MAX_TIMESTAMP_SKEW_HOURS);
        assert!(a.validate().is_ok());
    }
}
