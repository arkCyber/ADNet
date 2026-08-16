//! `agent_v1` — P2P AI-agent wire protocol over a [`Transport`] connection.
//!
//! This module defines the JSON-encoded message envelope used when one A3Net node
//! asks another node's registered AI agent a question.  The protocol sits on top
//! of the existing [`Transport`] framing and mirrors the pattern established by
//! [`crate::blob_proto`].
//!
//! # Wire shape
//!
//! ```text
//! requester (Node A) ──────────────────────────────────► peer (Node B)
//!   { "cmd": "AgentV1Chat",
//!     "requestId": "<uuid>",
//!     "from": "<node_id_hex>",
//!     "body": { /* ChatRequest */ }
//!   }
//!
//! peer (Node B) ───────────────────────────────────────► requester (Node A)
//!   { "cmd": "AgentV1ChatResponse",
//!     "requestId": "<uuid>",
//!     "from": "<node_id_hex>",
//!     "body": { /* ChatResponse */ },
//!     "error": null
//!   }
//!
//! Either side may also send:
//!   { "cmd": "Close" }
//! ```
//!
//! The `body` field carries the full [`a3net_agent::ChatRequest`] / [`a3net_agent::ChatResponse`]
//! which are now serializable via their `Serialize` / `Deserialize` derives.

use serde::{Deserialize, Serialize};

/// Unique identifier for correlating request/response pairs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestId(pub String);

impl RequestId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

/// Inbound envelope: Node A → Node B asking Node B's agent a question.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentV1Chat {
    /// Correlator returned verbatim in the response.
    pub request_id: String,
    /// NodeId of the requesting node (authenticated by the transport layer).
    pub from: String,
    /// The chat request to forward to Node B's registered model.
    pub body: a3net_agent::ChatRequest,
}

/// Outbound envelope: Node B → Node A returning the agent's answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentV1ChatResponse {
    /// Matches the `request_id` of the originating `AgentV1Chat`.
    pub request_id: String,
    /// NodeId of the responding node (authenticated by the transport layer).
    pub from: String,
    /// The chat response from Node B's registered model.
    #[serde(default)]
    pub body: Option<a3net_agent::ChatResponse>,
    /// Human-readable error if the request could not be fulfilled.
    /// When `None` the call succeeded; when `Some(...)` the caller
    /// should surface the message and treat `body` as absent.
    #[serde(default)]
    pub error: Option<String>,
}

impl AgentV1ChatResponse {
    /// Construct a successful (non-error) response.
    pub fn ok(request_id: String, from: String, body: a3net_agent::ChatResponse) -> Self {
        Self {
            request_id,
            from,
            body: Some(body),
            error: None,
        }
    }

    /// Construct an error response.
    pub fn error(request_id: String, from: String, message: impl Into<String>) -> Self {
        Self {
            request_id,
            from,
            body: None,
            error: Some(message.into()),
        }
    }
}

/// Full round-trip pair for type-level tests.
#[cfg(test)]
mod roundtrip {
    use super::*;

    #[test]
    fn request_id_new_is_unique() {
        let a = RequestId::new();
        let b = RequestId::new();
        assert_ne!(a.0, b.0);
    }

    #[test]
    fn request_serde_roundtrip() {
        let req = AgentV1Chat {
            request_id: "req-1".into(),
            from: "abcd1234".into(),
            body: a3net_agent::ChatRequest::new(vec![
                a3net_agent::ChatMessage::user("hello"),
            ]),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: AgentV1Chat = serde_json::from_str(&json).unwrap();
        assert_eq!(back.request_id, "req-1");
        assert_eq!(back.body.messages.len(), 1);
    }

    #[test]
    fn response_ok_serde_roundtrip() {
        let resp = AgentV1ChatResponse::ok(
            "req-1".into(),
            "peer-xyz".into(),
            a3net_agent::ChatResponse::text("hi there"),
        );
        let json = serde_json::to_string(&resp).unwrap();
        let back: AgentV1ChatResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.request_id, "req-1");
        assert_eq!(back.body.as_ref().unwrap().content, "hi there");
        assert!(back.error.is_none());
    }

    #[test]
    fn response_error_serde_roundtrip() {
        let resp = AgentV1ChatResponse::error("req-2".into(), "peer-xyz".into(), "no model");
        let json = serde_json::to_string(&resp).unwrap();
        let back: AgentV1ChatResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.request_id, "req-2");
        assert!(back.body.is_none());
        assert_eq!(back.error.as_deref(), Some("no model"));
    }

    #[test]
    fn request_id_default_is_unique() {
        let a = RequestId::default();
        let b = RequestId::default();
        assert_ne!(a.0, b.0);
    }
}
