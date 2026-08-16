//! HTTP/JSON wire protocol between the Rust harness and any iroh sidecar.
//!
//! The protocol is intentionally tiny — the iroh wire protocol itself
//! is the *real* protocol under test. The HTTP/JSON layer just
//! exchanges enough metadata (node id, ticket, topic) for both sides
//! to dial or subscribe.
//!
//! ## Endpoint set
//!
//! The sidecar listens on a single HTTP endpoint (path arbitrary;
//! the harness is told the base URL). All requests are POSTs with
//! JSON bodies; all responses are JSON.
//!
//! * `POST {base}/v1/version`           — handshake / version probe
//! * `POST {base}/v1/node_addr`         — return the sidecar's
//!                                         `NodeAddr` (endpoint id +
//!                                         relay URL + direct addrs)
//! * `POST {base}/v1/blob/put`          — sidecar ingests `bytes`,
//!                                         returns ticket + hash
//! * `POST {base}/v1/blob/get`          — sidecar fetches by ticket,
//!                                         returns bytes
//! * `POST {base}/v1/gossip/join`       — sidecar subscribes to a
//!                                         topic, returns sub_id
//! * `POST {base}/v1/gossip/leave`      — sidecar unsubscribes
//! * `POST {base}/v1/gossip/publish`    — sidecar publishes a
//!                                         payload on a topic
//! * `POST {base}/v1/gossip/next_event` — long-poll one gossip
//!                                         event off the bus
//!                                         (timeout_ms in body)
//! * `POST {base}/v1/dht/lookup`        — (nightly only) pkarr /
//!                                         mainline DHT lookup
//! * `POST {base}/v1/ipns/resolve`      — (nightly only)
//!
//! The reverse direction (Rust harness side) is exposed by
//! [`crate::sidecar::server`] under the same paths; the sidecar can
//! dial back to receive gossip events the A3Net side published, so
//! the sidecar never has to open a TCP listener of its own.
//!
//! ## Versioning
//!
//! The first request the harness makes is `version`. If the sidecar
//! reports anything other than the current
//! [`crate::WIRE_PROTOCOL_VERSION`], the harness aborts with a
//! `WIRE_VERSION_MISMATCH` error. This is the protocol's only
//! non-optional negotiation — anything else (new fields, new
//! endpoints) is additive and the sidecar can ignore unknown
//! fields.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Handshake / version probe request body (empty struct kept for
/// forward-compat — a future `v2` might carry capabilities here).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VersionProbe {}

/// Handshake response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionReply {
    /// The sidecar's wire-protocol version. Must equal
    /// [`crate::WIRE_PROTOCOL_VERSION`] or the harness aborts.
    pub version: u32,
    /// Free-form identity string for diagnostics — e.g.
    /// `"iroh-go/0.14.2 linux/amd64"`. Never parsed by the harness.
    pub sidecar: String,
    /// Sidecar-reported capability flags. The harness treats any
    /// unknown bit as a soft warning (the field is informational).
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// A 32-byte iroh `NodeId` carried as a 64-char hex string over the
/// wire. (Decoded back to bytes by the harness via `hex::decode`.)
pub type NodeIdHex = String;

/// A blob ticket. The sidecar is expected to accept the canonical
/// iroh blob ticket format ("blob/<hash>@<node_id>...").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobTicketWire {
    pub ticket: String,
}

/// A blob hash, carried as 64-char hex (BLAKE3 / iroh `Hash`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobHashWire {
    pub hash: String,
}

/// `put_bytes` request. `data` is the raw blob bytes, base64-encoded
/// for safe JSON transport.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobPutRequest {
    /// Base64-encoded blob bytes. The sidecar base64-decodes and
    /// ingests via its own `BlobsProtocol`.
    pub data_b64: String,
    /// Optional tag (e.g. "interop-blob-alpha") for diagnostics.
    /// Sidecars should not interpret it.
    #[serde(default)]
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobPutReply {
    /// Canonical iroh `Hash` as hex.
    pub hash: String,
    /// Canonical iroh `BlobTicket` string. A3Net side decodes with
    /// `BlobTicket::parse` (see `a3net-types`).
    pub ticket: String,
    /// Sidecar-reported byte count (post-decode). The harness uses
    /// this only for sanity-check; the source of truth is the
    /// bytes the harness sent.
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobGetRequest {
    pub ticket: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobGetReply {
    /// Base64-encoded bytes. Harness base64-decodes and BLAKE3-checks
    /// against the expected hash.
    pub data_b64: String,
    pub size: u64,
}

/// `NodeAddr` exchange. The sidecar's `node_addr` reply carries the
/// 32-byte endpoint id in hex plus any direct addresses / relay URL
/// the sidecar wants to advertise. The harness uses it to dial via
/// `iroh::Endpoint::connect`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeAddrWire {
    /// 32-byte iroh `EndpointId` as 64-char hex.
    pub node_id: NodeIdHex,
    /// `host:port` direct QUIC addresses, if any. The harness dials
    /// the first one that works.
    #[serde(default)]
    pub direct_addrs: Vec<String>,
    /// DERP relay URL (e.g. `https://relay.example.com`). Empty
    /// string means "no relay configured"; the harness will dial
    /// direct only.
    #[serde(default)]
    pub relay_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeAddrRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeAddrReply {
    pub addr: NodeAddrWire,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipTopicWire {
    /// Topic name. Both sides use the raw bytes as the iroh
    /// `TopicId` (BLAKE3-hashed inside iroh-gossip).
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipJoinRequest {
    pub topic: String,
    /// Optional sidecar-side subscription id. If `None`, the sidecar
    /// generates one (e.g. UUID) and returns it. The harness
    /// remembers the id so it can later call `next_event` or
    /// `leave`.
    #[serde(default)]
    pub sub_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipJoinReply {
    pub sub_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipLeaveRequest {
    pub sub_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipPublishRequest {
    pub topic: String,
    /// Base64-encoded payload. Both sides do a JSON round-trip via
    /// `serde_json::to_vec` so binary safety is preserved.
    pub payload_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipPublishReply {
    /// Sidecar-assigned message id (e.g. ULID / UUID). The harness
    /// uses it only for cross-correlation in logs.
    pub msg_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipNextEventRequest {
    pub sub_id: String,
    /// How long the sidecar is allowed to long-poll before
    /// returning `GossipNextEventReply { event: None }`. A
    /// short timeout keeps the test loop responsive; a long
    /// timeout cuts round-trip overhead.
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipEventWire {
    pub topic: String,
    /// Base64-encoded payload bytes (same encoding as publish).
    pub payload_b64: String,
    /// Hex-encoded `NodeId` of the original publisher. iroh-go /
    /// iroh-net both populate this from the gossip envelope.
    pub from_node_id: NodeIdHex,
    /// Unix epoch milliseconds. Sidecar's local clock.
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipNextEventReply {
    /// `None` if the long-poll timed out. The harness treats this
    /// as "no event in this window" and continues.
    pub event: Option<GossipEventWire>,
}

/// Discriminated union of all requests the harness can send to a
/// sidecar. The HTTP layer encodes it as JSON; the on-the-wire
/// shape is a tagged enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum SidecarRequest {
    #[serde(rename = "version")]
    Version(VersionProbe),
    #[serde(rename = "node_addr")]
    NodeAddr(NodeAddrRequest),
    #[serde(rename = "blob_put")]
    BlobPut(BlobPutRequest),
    #[serde(rename = "blob_get")]
    BlobGet(BlobGetRequest),
    #[serde(rename = "gossip_join")]
    GossipJoin(GossipJoinRequest),
    #[serde(rename = "gossip_leave")]
    GossipLeave(GossipLeaveRequest),
    #[serde(rename = "gossip_publish")]
    GossipPublish(GossipPublishRequest),
    #[serde(rename = "gossip_next_event")]
    GossipNextEvent(GossipNextEventRequest),
}

/// Discriminated union of all responses. The `ok: bool` flag is
/// additive so legacy sidecars that only return
/// `SidecarResponse::Version { version, sidecar }` can still talk
/// to a newer harness (the harness checks `ok` only on the
/// error-returning variants).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum SidecarResponse {
    #[serde(rename = "version")]
    Version {
        ok: bool,
        #[serde(flatten)]
        reply: VersionReply,
        #[serde(default)]
        err: Option<String>,
    },
    #[serde(rename = "node_addr")]
    NodeAddr {
        ok: bool,
        #[serde(default)]
        reply: Option<NodeAddrReply>,
        #[serde(default)]
        err: Option<String>,
    },
    #[serde(rename = "blob_put")]
    BlobPut {
        ok: bool,
        #[serde(default)]
        reply: Option<BlobPutReply>,
        #[serde(default)]
        err: Option<String>,
    },
    #[serde(rename = "blob_get")]
    BlobGet {
        ok: bool,
        #[serde(default)]
        reply: Option<BlobGetReply>,
        #[serde(default)]
        err: Option<String>,
    },
    #[serde(rename = "gossip_join")]
    GossipJoin {
        ok: bool,
        #[serde(default)]
        reply: Option<GossipJoinReply>,
        #[serde(default)]
        err: Option<String>,
    },
    #[serde(rename = "gossip_leave")]
    GossipLeave {
        ok: bool,
        #[serde(default)]
        err: Option<String>,
    },
    #[serde(rename = "gossip_publish")]
    GossipPublish {
        ok: bool,
        #[serde(default)]
        reply: Option<GossipPublishReply>,
        #[serde(default)]
        err: Option<String>,
    },
    #[serde(rename = "gossip_next_event")]
    GossipNextEvent {
        ok: bool,
        #[serde(default)]
        reply: Option<GossipNextEventReply>,
        #[serde(default)]
        err: Option<String>,
    },
}

/// Convenience: extract `reply` from a `SidecarResponse` or turn
/// the error string into a `SidecarError`. Used by every typed
/// accessor on [`crate::sidecar::client`].
///
/// Wire shape: every `SidecarResponse` variant uses
/// `#[serde(flatten)]` on the reply, so on the wire the reply
/// fields sit at the *same* level as `op`, `ok`, and `err` — not
/// under a nested `reply` key. The harness therefore re-parses
/// the whole flat object as the expected reply type (which
/// ignores `op` / `ok` / `err` because the reply type doesn't
/// have those fields). This keeps the on-the-wire JSON
/// compact and matches how `iroh` itself shapes its public
/// response objects.
#[track_caller]
pub fn unwrap_response<T>(op: &str, r: SidecarResponse) -> Result<T, SidecarError>
where
    T: for<'de> Deserialize<'de>,
{
    let raw = serde_json::to_value(&r).map_err(|e| SidecarError::Serialization {
        op: op.to_string(),
        message: e.to_string(),
    })?;
    if !raw.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let err = raw
            .get("err")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        return Err(SidecarError::Remote {
            op: op.to_string(),
            message: err,
        });
    }
    // Decode the whole flat object as the expected reply type.
    // `T` doesn't have `op` / `ok` / `err` fields, so serde just
    // ignores them. The reply type's own fields (e.g. `version`,
    // `sidecar` for `VersionReply`) are read directly.
    serde_json::from_value(raw).map_err(|e| SidecarError::Serialization {
        op: op.to_string(),
        message: e.to_string(),
    })
}

#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("wire-protocol version mismatch: sidecar reports {sidecar_version}, harness requires {harness_version}")]
    WireVersionMismatch { sidecar_version: u32, harness_version: u32 },
    #[error("sidecar returned error for op `{op}`: {message}")]
    Remote { op: String, message: String },
    #[error("serialization error for op `{op}`: {message}")]
    Serialization { op: String, message: String },
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("invalid hex: {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sidecar process: {0}")]
    Process(String),
    #[error("timeout after {ms} ms waiting for {what}")]
    Timeout { what: &'static str, ms: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_probe_round_trip() {
        let p = VersionProbe::default();
        let j = serde_json::to_string(&p).unwrap();
        let back: VersionProbe = serde_json::from_str(&j).unwrap();
        let _ = back;
    }

    #[test]
    fn version_reply_deserializes_unknown_capabilities() {
        // Sidecar may add new capabilities without warning; the
        // harness must ignore unknown values.
        let j = r#"{
            "version": 1,
            "sidecar": "iroh-go/test",
            "capabilities": ["blob", "gossip", "future-thing"]
        }"#;
        let v: VersionReply = serde_json::from_str(j).unwrap();
        assert_eq!(v.version, 1);
        assert_eq!(v.sidecar, "iroh-go/test");
        assert_eq!(v.capabilities.len(), 3);
    }

    #[test]
    fn request_tagged_enum_serializes() {
        // Critical: the wire layer relies on the `op` tag being
        // present and the inner payload being flattened / inlined
        // depending on the variant. If this changes, the sidecar
        // dispatch table breaks.
        let r = SidecarRequest::GossipPublish(GossipPublishRequest {
            topic: "test-room".into(),
            payload_b64: "aGVsbG8=".into(),
        });
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains(r#""op":"gossip_publish""#));
        assert!(j.contains(r#""topic":"test-room""#));
    }

    #[test]
    fn unwrap_response_extracts_inner() {
        // Build a `SidecarResponse` via the typed constructor so
        // the test exercises the same code path the typed
        // accessors use, then re-decode the inner reply.
        let r = SidecarResponse::Version {
            ok: true,
            reply: VersionReply {
                version: 1,
                sidecar: "test".into(),
                capabilities: vec![],
            },
            err: None,
        };
        let v: VersionReply = unwrap_response("version", r).unwrap();
        assert_eq!(v.version, 1);
        assert_eq!(v.sidecar, "test");
    }

    #[test]
    fn unwrap_response_surfaces_remote_error() {
        let r = SidecarResponse::BlobPut {
            ok: false,
            reply: None,
            err: Some("store is full".into()),
        };
        let e = unwrap_response::<BlobPutReply>("blob_put", r).unwrap_err();
        match e {
            SidecarError::Remote { op, message } => {
                assert_eq!(op, "blob_put");
                assert!(message.contains("full"));
            }
            other => panic!("expected Remote, got {other:?}"),
        }
    }
}
