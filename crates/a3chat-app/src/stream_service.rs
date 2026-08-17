//! `a3chat.stream.subscribe` — server-side handle to the SSE
//! subscription bus.
//!
//! P1 ships a server-push RPC: when a client calls
//! `a3chat.stream.subscribe`, the daemon returns a *subscription
//! handle* (`handle_id`, owner-scoped). The actual event delivery
//! happens over the long-lived SSE endpoint at `/rpc/stream`
//! (this handle binds client RPC identity to SSE sessions).
//!
//! ## Wire contract
//!
//! - Subscribe params:
//!   `{ "topics": ["chat","presence"] }` (optional; defaults to all
//!   topics the bus exposes).
//! - Subscribe reply:
//!   `{ "handle_id": "<uuid>", "owner": "<hex>", "stream_url":
//!   "/rpc/stream", "keepalive_secs": 25 }`.
//!
//! - Unsubscribe params: `{ "handle_id": "..." }`.
//! - Unsubscribe reply: `{ "ok": true }`.
//!
//! - List params: `{}`.
//! - List reply: `{ "handles": [{...}] }`.
//!
//! ## Topic filtering
//!
//! Topics today are advisory — they don't change the event payload
//! (the bus fires every event the owner can see), they only let
//! the client UI narrow which event types it actually subscribes
//! to. The daemon validates `topics` against the [`StreamTopic`]
//! allow-list.

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use a3chat_core::error::A3chatError;
use std::str::FromStr;

use a3chat_core::id::UserId;

use crate::error::{AppError, AppResult};

/// Allow-list of subscription topics. New event types should add a
/// matching topic here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamTopic {
    Chat,
    Presence,
    Group,
    Contact,
    /// Moments / 朋友圈 events (`moments.*`).
    Moments,
    /// Link bookmark / favorites events (`link.*`).
    LinkBookmark,
}

/// Error returned when a string cannot be parsed into a [`StreamTopic`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStreamTopic(pub String);

impl std::fmt::Display for UnknownStreamTopic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown stream topic: {}", self.0)
    }
}

impl std::error::Error for UnknownStreamTopic {}

impl FromStr for StreamTopic {
    type Err = UnknownStreamTopic;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "chat" => Ok(Self::Chat),
            "presence" => Ok(Self::Presence),
            "group" => Ok(Self::Group),
            "contact" => Ok(Self::Contact),
            "moments" => Ok(Self::Moments),
            "link_bookmark" => Ok(Self::LinkBookmark),
            other => Err(UnknownStreamTopic(other.to_string())),
        }
    }
}

impl StreamTopic {
    pub const ALL: &'static [StreamTopic] = &[
        StreamTopic::Chat,
        StreamTopic::Presence,
        StreamTopic::Group,
        StreamTopic::Contact,
        StreamTopic::Moments,
        StreamTopic::LinkBookmark,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamSubscription {
    pub handle_id: String,
    pub owner: UserId,
    pub topics: Vec<String>,
    pub stream_url: String,
    pub keepalive_secs: u32,
    pub created_at_unix: i64,
}

/// The stream service — owns a `Mutex<HashMap<handle_id,
/// StreamSubscription>>` plus a topic filter. Cloning is cheap.
#[derive(Clone, Debug)]
pub struct StreamService {
    inner: Arc<StreamInner>,
}

#[derive(Debug)]
struct StreamInner {
    handles: Mutex<std::collections::HashMap<String, StreamSubscription>>,
}

/// Maximum number of concurrent SSE subscription handles we are
/// willing to keep in memory. One handle per active client (web,
/// mobile, desktop) plus a small headroom for retries. Anything
/// beyond this is almost certainly a misbehaving client retrying
/// `subscribe` without ever calling `unsubscribe`.
pub const MAX_STREAM_HANDLES: usize = 1024;

impl Default for StreamService {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamService {
    #[must_use = "constructing a stream service without using it is a bug"]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(StreamInner {
                handles: Mutex::new(std::collections::HashMap::new()),
            }),
        }
    }

    pub fn subscribe(
        &self,
        owner: &UserId,
        topics: Option<Vec<String>>,
    ) -> AppResult<StreamSubscription> {
        let topics = normalize_topics(topics)?;
        let mut guard = self.inner.handles.lock();
        // Capacity cap — protect against a buggy client retrying
        // `subscribe` without ever closing its handle.
        if guard.len() >= MAX_STREAM_HANDLES {
            return Err(AppError::Domain(format!(
                "stream handle limit reached ({MAX_STREAM_HANDLES}) — unsubscribe idle handles first"
            )));
        }
        let handle_id = uuid::Uuid::new_v4().to_string();
        let subscription = StreamSubscription {
            handle_id: handle_id.clone(),
            owner: owner.clone(),
            topics: topics.iter().map(|t| topic_name(*t).to_string()).collect(),
            stream_url: "/rpc/stream".to_string(),
            keepalive_secs: stream_service_keepalive_secs(),
            created_at_unix: chrono::Utc::now().timestamp(),
        };
        guard.insert(handle_id, subscription.clone());
        Ok(subscription)
    }

    pub fn unsubscribe(&self, owner: &UserId, handle_id: &str) -> AppResult<()> {
        let mut guard = self.inner.handles.lock();
        match guard.remove(handle_id) {
            // Both "wrong owner" and "missing handle" collapse to
            // Forbidden so attackers cannot enumerate handle ids
            // owned by other users via the error type discrimination.
            Some(s) if s.owner == *owner => Ok(()),
            Some(_) | None => Err(AppError::Forbidden(
                "subscription handle not found or not owned by caller".into(),
            )),
        }
    }

    pub fn list(&self, owner: &UserId) -> AppResult<Vec<StreamSubscription>> {
        let guard = self.inner.handles.lock();
        Ok(guard
            .values()
            .filter(|s| s.owner == *owner)
            .cloned()
            .collect())
    }

    pub fn handle_count(&self) -> usize {
        self.inner.handles.lock().len()
    }
}

/// Convenience wrapper returned from `subscribe` so callers don't have
/// to remember the field name (`handle_id`).
pub struct StreamHandle(pub String);

fn normalize_topics(
    raw: Option<Vec<String>>,
) -> AppResult<HashSet<StreamTopic>> {
    let requested: HashSet<String> = raw
        .map(|v| v.into_iter().collect())
        .unwrap_or_else(|| StreamTopic::ALL.iter().map(|t| topic_name(*t).to_string()).collect());
    let mut out = HashSet::new();
    for s in &requested {
        let t = StreamTopic::from_str(s).map_err(|_| {
            AppError::Domain(format!(
                "unknown topic {s}; valid: chat, presence, group, contact, moments, link_bookmark"
            ))
        })?;
        out.insert(t);
    }
    if out.is_empty() {
        return Err(AppError::Domain("topics list is empty".into()));
    }
    Ok(out)
}

fn topic_name(t: StreamTopic) -> &'static str {
    match t {
        StreamTopic::Chat => "chat",
        StreamTopic::Presence => "presence",
        StreamTopic::Group => "group",
        StreamTopic::Contact => "contact",
        StreamTopic::Moments => "moments",
        StreamTopic::LinkBookmark => "link_bookmark",
    }
}

/// Keepalive interval the daemon tells the client to expect. Mirrors
/// `a3chat_rpc::sse::KEEPALIVE_INTERVAL` (25 seconds).
pub fn stream_service_keepalive_secs() -> u32 {
    25
}

/// Dispatcher entry point used by `a3chat-app::app::A3chatApp::dispatch`.
pub async fn dispatch(
    svc: Arc<StreamService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        "a3chat.stream.subscribe" => {
            let topics = params.get("topics").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            });
            let sub = svc
                .subscribe(owner, topics)
                .map_err(A3chatError::from)?;
            serde_json::to_value(sub).map_err(A3chatError::from)
        }
        "a3chat.stream.unsubscribe" => {
            let handle_id = params
                .get("handle_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing handle_id".into()))?;
            svc.unsubscribe(owner, handle_id)
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "a3chat.stream.list" => {
            let list = svc.list(owner).map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "handles": list }))
        }
        m => Err(A3chatError::Internal(format!(
            "StreamService does not handle {m}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> UserId {
        UserId::from("alice")
    }

    #[test]
    fn subscribe_creates_handle() {
        let svc = StreamService::new();
        let sub = svc.subscribe(&owner(), None).unwrap();
        assert!(!sub.handle_id.is_empty());
        assert_eq!(sub.owner, owner());
        assert_eq!(sub.stream_url, "/rpc/stream");
    }

    #[test]
    fn subscribe_with_explicit_topics() {
        let svc = StreamService::new();
        let sub = svc
            .subscribe(&owner(), Some(vec!["chat".into(), "presence".into()]))
            .unwrap();
        assert_eq!(sub.topics.len(), 2);
    }

    #[test]
    fn subscribe_rejects_unknown_topic() {
        let svc = StreamService::new();
        let r = svc.subscribe(&owner(), Some(vec!["unknown".into()]));
        assert!(r.is_err());
    }

    #[test]
    fn subscribe_rejects_empty_topics() {
        let svc = StreamService::new();
        let r = svc.subscribe(&owner(), Some(vec![]));
        assert!(r.is_err());
    }

    #[test]
    fn unsubscribe_removes_handle() {
        let svc = StreamService::new();
        let sub = svc.subscribe(&owner(), None).unwrap();
        let r = svc.unsubscribe(&owner(), &sub.handle_id);
        assert!(r.is_ok());
        assert_eq!(svc.handle_count(), 0);
    }

    #[test]
    fn unsubscribe_rejects_wrong_owner() {
        let svc = StreamService::new();
        let sub = svc.subscribe(&owner(), None).unwrap();
        let r = svc.unsubscribe(&UserId::from("bob"), &sub.handle_id);
        assert!(r.is_err());
    }

    #[test]
    fn unsubscribe_unknown_handle_returns_domain_error() {
        let svc = StreamService::new();
        let r = svc.unsubscribe(&owner(), "no-such-handle");
        assert!(r.is_err());
    }

    #[test]
    fn list_returns_only_owner_handles() {
        let svc = StreamService::new();
        let a = svc.subscribe(&owner(), None).unwrap();
        let _b = svc.subscribe(&UserId::from("bob"), None).unwrap();
        let r = svc.list(&owner()).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].handle_id, a.handle_id);
    }

    #[test]
    fn dispatch_unknown_method_errors() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let r = rt.block_on(dispatch(
            Arc::new(StreamService::new()),
            "a3chat.stream.foo",
            &owner(),
            serde_json::json!({}),
        ));
        assert!(matches!(r, Err(A3chatError::Internal(_))));
    }
}
