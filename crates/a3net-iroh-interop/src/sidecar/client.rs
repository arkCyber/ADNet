#![doc(test(attr(allow(unused_variables, dead_code))))]

//! HTTP/JSON client that the Rust harness uses to drive a sidecar.
//!
//! Construction:
//!
//! ```no_run
//! use a3net_iroh_interop::sidecar::SidecarClient;
//! let _c = SidecarClient::connect("http://127.0.0.1:7443").unwrap();
//! ```
//!
//! Every call is a JSON POST against the sidecar's base URL. The
//! client owns a `reqwest::Client` internally and reuses
//! connections.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::wire::{
    BlobGetReply, BlobGetRequest, BlobPutReply, BlobPutRequest, GossipJoinReply, GossipJoinRequest,
    GossipLeaveRequest, GossipNextEventReply, GossipNextEventRequest, GossipPublishReply,
    GossipPublishRequest, NodeAddrReply, NodeAddrRequest, SidecarError, SidecarRequest,
    SidecarResponse, VersionProbe, VersionReply, unwrap_response,
};

/// HTTP/JSON client to a single sidecar. Cheap to clone (shares
/// the inner `reqwest::Client`).
#[derive(Debug, Clone)]
pub struct SidecarClient {
    base: String,
    http: Client,
}

impl SidecarClient {
    /// Connect to a sidecar at `base_url` (e.g. `http://127.0.0.1:7443`).
    /// Does NOT perform a `version` handshake — call [`Self::version`]
    /// explicitly so callers can decide what to do on mismatch.
    pub fn connect(base_url: impl Into<String>) -> Result<Self, SidecarError> {
        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            base: base_url.into(),
            http,
        })
    }

    /// Connect + handshake. The first request is `version`; the
    /// sidecar must report the same [`crate::WIRE_PROTOCOL_VERSION`]
    /// as this crate or [`SidecarError::WireVersionMismatch`] is
    /// returned.
    pub async fn handshake(&self) -> Result<VersionReply, SidecarError> {
        let r = self.version().await?;
        if r.version != crate::WIRE_PROTOCOL_VERSION {
            return Err(SidecarError::WireVersionMismatch {
                sidecar_version: r.version,
                harness_version: crate::WIRE_PROTOCOL_VERSION,
            });
        }
        Ok(r)
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base.trim_end_matches('/'), path.trim_start_matches('/'))
    }

    async fn post<T: DeserializeOwned>(
        &self,
        op: &str,
        body: SidecarRequest,
    ) -> Result<T, SidecarError> {
        let url = self.endpoint("/v1/dispatch");
        let resp = self.http.post(&url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(SidecarError::Process(format!(
                "sidecar returned HTTP {status}: {text}"
            )));
        }
        let parsed: SidecarResponse = serde_json::from_str(&text).map_err(|e| {
            SidecarError::Serialization {
                op: op.to_string(),
                message: format!("bad sidecar response: {e}; body={text}"),
            }
        })?;
        unwrap_response(op, parsed)
    }

    /// Version probe. Returns the sidecar's reported version +
    /// identity string + capability list. The harness calls this
    /// before anything else.
    pub async fn version(&self) -> Result<VersionReply, SidecarError> {
        self.post(
            "version",
            SidecarRequest::Version(VersionProbe::default()),
        )
        .await
    }

    /// Ask the sidecar for its `NodeAddr` (endpoint id + relay URL
    /// + direct addrs). The harness dials the returned id.
    pub async fn node_addr(&self) -> Result<NodeAddrReply, SidecarError> {
        self.post(
            "node_addr",
            SidecarRequest::NodeAddr(NodeAddrRequest::default()),
        )
        .await
    }

    /// Ask the sidecar to ingest `bytes` and return a `BlobTicket`.
    /// Used in the "A3Net fetches what iroh-go put" direction.
    pub async fn blob_put(&self, bytes: &[u8], tag: Option<&str>) -> Result<BlobPutReply, SidecarError> {
        let req = BlobPutRequest {
            data_b64: B64.encode(bytes),
            tag: tag.map(str::to_string),
        };
        self.post("blob_put", SidecarRequest::BlobPut(req)).await
    }

    /// Ask the sidecar to fetch by ticket. The harness then
    /// BLAKE3-checks the bytes against the expected hash.
    pub async fn blob_get(&self, ticket: &str) -> Result<BlobGetReply, SidecarError> {
        let req = BlobGetRequest {
            ticket: ticket.to_string(),
        };
        self.post("blob_get", SidecarRequest::BlobGet(req)).await
    }

    /// Ask the sidecar to subscribe to `topic`. The returned
    /// `sub_id` is opaque to the harness; it must be passed to
    /// `gossip_next_event` and `gossip_leave`.
    pub async fn gossip_join(
        &self,
        topic: &str,
        sub_id: Option<&str>,
    ) -> Result<GossipJoinReply, SidecarError> {
        let req = GossipJoinRequest {
            topic: topic.to_string(),
            sub_id: sub_id.map(str::to_string),
        };
        self.post("gossip_join", SidecarRequest::GossipJoin(req))
            .await
    }

    /// Ask the sidecar to leave a subscription. Idempotent — a
    /// second call with an unknown sub_id returns `ok=false` with
    /// an "unknown sub_id" error.
    pub async fn gossip_leave(&self, sub_id: &str) -> Result<(), SidecarError> {
        let req = GossipLeaveRequest {
            sub_id: sub_id.to_string(),
        };
        // The reply body is empty on success; we still want to
        // assert `ok=true`. Discard the typed reply.
        let _ok: serde_json::Value = self
            .post("gossip_leave", SidecarRequest::GossipLeave(req))
            .await?;
        Ok(())
    }

    /// Ask the sidecar to publish `payload` on `topic`. The
    /// returned `msg_id` is informational; the harness never
    /// queries it back.
    pub async fn gossip_publish(
        &self,
        topic: &str,
        payload: &[u8],
    ) -> Result<GossipPublishReply, SidecarError> {
        let req = GossipPublishRequest {
            topic: topic.to_string(),
            payload_b64: B64.encode(payload),
        };
        self.post("gossip_publish", SidecarRequest::GossipPublish(req))
            .await
    }

    /// Long-poll the sidecar for the next gossip event on `sub_id`.
    /// Returns `Ok(None)` if the long-poll timed out without an
    /// event. The harness typically loops with a short timeout
    /// (e.g. 200 ms) and a larger total wall-clock budget.
    pub async fn gossip_next_event(
        &self,
        sub_id: &str,
        timeout_ms: u64,
    ) -> Result<Option<crate::wire::GossipEventWire>, SidecarError> {
        let req = GossipNextEventRequest {
            sub_id: sub_id.to_string(),
            timeout_ms,
        };
        let reply: GossipNextEventReply = self
            .post("gossip_next_event", SidecarRequest::GossipNextEvent(req))
            .await?;
        Ok(reply.event)
    }

    /// Convenience helper: build a `NodeAddrWire`-shaped JSON value
    /// for callers that want to hand a sidecar's `node_addr` to
    /// something other than the harness' own dialer. Not used by
    /// the harness' own driver — kept for sidecar-style callers
    /// that want to share JSON snippets with the harness.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Bypass the typed accessors and POST an arbitrary
    /// `SidecarRequest`. Used by the comprehensive subset
    /// (DHT / IPNS / docs / relay) once those ops are added to
    /// [`SidecarRequest`].
    pub async fn raw(&self, req: SidecarRequest) -> Result<SidecarResponse, SidecarError> {
        let url = self.endpoint("/v1/dispatch");
        let resp = self.http.post(&url).json(&req).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(SidecarError::Process(format!(
                "sidecar returned HTTP {status}: {text}"
            )));
        }
        serde_json::from_str(&text).map_err(|e| SidecarError::Serialization {
            op: "<raw>".into(),
            message: e.to_string(),
        })
    }

    /// Build a synthetic `SidecarRequest::NodeAddr` JSON body
    /// without making a request. For tests that want to verify the
    /// wire shape independent of the HTTP transport.
    pub fn example_node_addr_request() -> serde_json::Value {
        json!(SidecarRequest::NodeAddr(NodeAddrRequest::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client can be cloned (it owns an `Arc` internally via
    /// `reqwest::Client`) and the `base_url` is preserved.
    #[test]
    fn client_is_cloneable_and_preserves_base() {
        let c = SidecarClient::connect("http://example.com:7443").unwrap();
        let c2 = c.clone();
        assert_eq!(c.base_url(), "http://example.com:7443");
        assert_eq!(c2.base_url(), "http://example.com:7443");
    }

    /// `example_node_addr_request` produces a tagged enum body —
    /// the wire layer must NOT lose the `op` field.
    #[test]
    fn example_request_preserves_op_tag() {
        let v = SidecarClient::example_node_addr_request();
        assert_eq!(v["op"], "node_addr");
    }

    /// The client must reject sidecar URLs without a scheme by
    /// failing the connect (reqwest surfaces this). A unit test
    /// that doesn't need a server: confirm the URL we generate
    /// has the expected shape.
    #[test]
    fn endpoint_joins_paths_without_double_slash() {
        let c = SidecarClient::connect("http://127.0.0.1:7443/").unwrap();
        assert_eq!(c.endpoint("/v1/dispatch"), "http://127.0.0.1:7443/v1/dispatch");
        let c = SidecarClient::connect("http://127.0.0.1:7443").unwrap();
        assert_eq!(c.endpoint("v1/dispatch"), "http://127.0.0.1:7443/v1/dispatch");
    }
}
