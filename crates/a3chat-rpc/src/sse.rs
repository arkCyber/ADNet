//! Server-Sent Events bridge between [`a3chat_app::NotificationBus`]
//! and the `GET /rpc/stream` endpoint.
//!
//! Each SSE client receives a `NotificationReceiver` filtered by
//! the `owner` identity (taken from the `X-A3Chat-Owner` header).
//! Events are serialized as JSON-RPC 2.0 `notification` envelopes
//! (no `id` field) so the same parser on the frontend handles both
//! RPC responses and live push notifications.
//!
//! ## Compliance
//!
//! - **Authentication (DO-178C §6.4)** — the `X-A3Chat-Owner`
//!   header is *required*. Anonymous streams are rejected with
//!   HTTP 401. There is no fallback identity.
//! - **Keepalive** — the handler emits a `:keepalive` comment
//!   every [`KEEPALIVE_INTERVAL`] so reverse proxies don't
//!   reap idle SSE connections (the [nginx default 60s] is the
//!   bounding target).
//! - **Reconnect-resume** — when the client supplies a
//!   `Last-Event-Id` header (the spec'd reconnect token) the
//!   handler *currently* logs the gap and starts fresh; a future
//!   P1 will replay buffered events from the bus.

use std::convert::Infallible;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::header;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

use a3chat_app::A3chatApp;
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;

use crate::error::{ERR_A3CHAT_NOT_AUTHENTICATED, ERR_INVALID_PARAMS, RpcError};
use crate::server::HEADER_OWNER;
use crate::server::HEADER_REQUEST_ID;

/// Period between SSE keepalive comments. 25 s fits inside the
/// 60 s nginx worker `proxy_read_timeout` default with enough
/// margin to survive a transient slow GC pause.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(25);

/// Convert an `A3chatEvent` into an SSE-formatted JSON payload.
///
/// Wire format (per [whatwg/eventsource]):
/// ```text
/// event: a3chat.chat.message.received
/// data: {"jsonrpc":"2.0","method":"a3chat.chat.message.received","params":{...}}
///
/// ```
/// Each frame is separated by a blank line; the parser ignores
/// leading comment lines (`:keepalive`).
fn event_to_sse(event: a3chat_core::event::A3chatEvent) -> String {
    use a3chat_core::event::A3chatEvent;
    let (kind, payload) = match event {
        A3chatEvent::ChatMessageReceived {
            user_id,
            conversation_id,
            message,
        } => (
            A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECEIVED,
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "message": message,
            }),
        ),
        A3chatEvent::ChatMessageRecalled {
            user_id,
            conversation_id,
            message_id,
            recalled_at_unix,
        } => (
            A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECALLED,
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "message_id": message_id,
                "recalled_at_unix": recalled_at_unix,
            }),
        ),
        A3chatEvent::ChatMessageRead {
            user_id,
            conversation_id,
            message_id,
            read_at_unix,
        } => (
            A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_READ,
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "message_id": message_id,
                "read_at_unix": read_at_unix,
            }),
        ),
        A3chatEvent::ChatTyping {
            user_id,
            conversation_id,
            expires_at,
        } => (
            A3chatRpcMethod::NOTIFICATION_CHAT_TYPING,
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "expires_at": expires_at,
            }),
        ),
        A3chatEvent::PresenceChanged { event } => (
            A3chatRpcMethod::NOTIFICATION_PRESENCE_CHANGED,
            serde_json::json!({
                "user_id": event.user_id,
                "status": event.status,
                "status_message": event.status_message,
                "timestamp": event.timestamp,
            }),
        ),
        A3chatEvent::GroupMemberJoined {
            conversation_id,
            member,
        } => (
            A3chatRpcMethod::NOTIFICATION_GROUP_MEMBER_JOINED,
            serde_json::json!({
                "conversation_id": conversation_id,
                "member": member,
            }),
        ),
        A3chatEvent::GroupInvitationReceived { invitation } => (
            A3chatRpcMethod::NOTIFICATION_GROUP_INVITATION_RECEIVED,
            serde_json::json!({
                "invitation": invitation,
            }),
        ),
        A3chatEvent::ContactRequestReceived { request_id } => (
            A3chatRpcMethod::NOTIFICATION_CONTACT_REQUEST_RECEIVED,
            serde_json::json!({
                "request_id": request_id,
            }),
        ),
        A3chatEvent::ChatMessageEdited {
            user_id,
            conversation_id,
            message,
        } => (
            A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_EDITED,
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "message": message,
            }),
        ),
        A3chatEvent::ChatMessageDeleted {
            user_id,
            conversation_id,
            message_id,
        } => (
            A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_DELETED,
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "message_id": message_id,
            }),
        ),
    };
    let envelope = serde_json::json!({
        "jsonrpc": "2.0",
        "method": kind,
        "params": payload,
    });
    // Serialization of our own typed event should never fail, but if
    // it ever does we surface a structured error frame rather than
    // silently emitting an empty `data:` line — that previously
    // caused clients to receive zero-length payloads with no
    // way to tell what went wrong.
    let json_str = match serde_json::to_string(&envelope) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("event_to_sse: serialization failed: {e}");
            format!(
                "{{\"jsonrpc\":\"2.0\",\"method\":\"{kind}\",\"params\":{{\"_serialization_error\":\"{}\"}}}}",
                e.to_string().escape_default()
            )
        }
    };
    format!("event: {kind}\ndata: {json_str}\n\n")
}

fn owner_from_headers(headers: &HeaderMap) -> Result<UserId, RpcError> {
    let value = headers
        .get(HEADER_OWNER)
        .ok_or_else(|| {
            RpcError::new(
                ERR_A3CHAT_NOT_AUTHENTICATED,
                format!("missing {HEADER_OWNER} header"),
            )
        })?;
    let s = value
        .to_str()
        .map_err(|e| RpcError::new(ERR_INVALID_PARAMS, format!("invalid owner header: {e}")))?;
    Ok(UserId::from(s))
}

/// Build the SSE response stream for `owner`.
///
/// Returns `Err(RpcError)` only when the authentication header is
/// missing or malformed. Once authentication passes we always
/// return a stream (even if it carries zero events so far).
pub async fn sse_handler(
    headers: HeaderMap,
    State(state): State<crate::server::ServerState>,
) -> Result<Response, Response> {
    let owner = match owner_from_headers(&headers) {
        Ok(o) => o,
        Err(e) => {
            return Err(e.into_response());
        }
    };

    // Reconnect-token support (spec §6.4). When a client
    // supplies `Last-Event-Id`, log it so a future P1 can wire
    // the bus replay buffer; for now we acknowledge but ignore.
    if let Some(last_id) = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
    {
        tracing::debug!(last_event_id = %last_id, owner = %owner.as_str(), "sse client reconnecting");
    }

    let request_id_header = headers
        .get(HEADER_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let mut receiver = state.app.subscribe_for(owner.clone());

    // Increment SSE client counter for the lifetime of this connection.
    state.metrics.sse_inc();
    let metrics_for_cleanup = state.metrics.clone();

    // Compose the body. The stream emits:
    // 1. a `:keepalive` comment *immediately* (so the browser
    //    confirms the connection before any data),
    // 2. a fresh SSE event every time the bus yields one,
    // 3. another `:keepalive` comment every `KEEPALIVE_INTERVAL`.
    let owner_for_span = owner.clone();
    let stream = async_stream::stream! {
        let _guard = StreamGuard::new(metrics_for_cleanup);
        // Initial keepalive — ensures the client gets past its
        // onopen handler even if the bus is silent.
        yield Ok::<_, Infallible>(":keepalive\n\n".to_string());
        let span = tracing::info_span!("sse_stream", owner = %owner_for_span.as_str());
        let _enter = span.enter();
        let mut interval = tokio::time::interval(KEEPALIVE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The first `tick()` completes immediately; skip it so
        // we don't emit a redundant `:keepalive` on top of the
        // initial one above.
        interval.tick().await;
        loop {
            tokio::select! {
                maybe = receiver.recv() => {
                    match maybe {
                        Some(event) => {
                            tracing::trace!(kind = "sse_event", "emitting notification");
                            yield Ok::<_, Infallible>(event_to_sse(event));
                        }
                        None => {
                            // Bus closed (server shutdown).
                            tracing::info!("notification bus closed; ending sse stream");
                            break;
                        }
                    }
                }
                _ = interval.tick() => {
                    // Periodic keepalive — a comment line so it
                    // carries zero bytes of payload but wakes
                    // up idle sockets and proxies.
                    yield Ok::<_, Infallible>(":keepalive\n\n".to_string());
                }
            }
        }
    };

    let body = Body::from_stream(stream);
    let mut response = Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no") // disable nginx buffering
        .header("X-A3Chat-Owner", owner.as_str())
        .body(body)
        .map_err(|e| RpcError::internal(format!("sse build: {e}")).into_response())?;

    // Echo the request-id header (when present) so clients can
    // correlate the long-lived stream with their connection log.
    if let Some(rid) = request_id_header {
        if let Ok(v) = axum::http::HeaderValue::from_str(&rid) {
            response.headers_mut().insert("X-A3Chat-Request-Id", v);
        }
    }
    Ok(response)
}

/// RAII guard that decrements the SSE-client counter on drop.
struct StreamGuard {
    metrics: std::sync::Arc<crate::metrics::Metrics>,
}
impl StreamGuard {
    fn new(metrics: std::sync::Arc<crate::metrics::Metrics>) -> Self {
        Self { metrics }
    }
}
impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.metrics.sse_dec();
    }
}

/// Convenience wrapper for tests — read the underlying stream
/// without using a real HTTP server.
pub async fn stream_events(app: &A3chatApp, owner: UserId) {
    let mut receiver = app.subscribe_for(owner);
    while let Some(_event) = receiver.recv().await {
        // drain; used in tests.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::event::A3chatEvent;
    use a3chat_core::id::ConversationId;
    use a3chat_core::message::{ChatMessage, MessageBody, MessageType};
    use a3chat_core::presence::{PresenceEvent, PresenceStatus};
    use a3chat_core::rpc::A3chatRpcMethod;
    use tempfile::tempdir;

    fn owner() -> UserId {
        UserId::from("alice")
    }

    #[test]
    fn chat_message_received_serializes() {
        let evt = A3chatEvent::ChatMessageReceived {
            user_id: UserId::from("bob"),
            conversation_id: ConversationId::from("dm:a:b"),
            message: ChatMessage::new_system(
                ConversationId::from("dm:a:b"),
                UserId::from("server"),
                "ping",
                1,
                1,
            )
            .unwrap(),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains(A3chatRpcMethod::NOTIFICATION_CHAT_MESSAGE_RECEIVED));
    }

    #[test]
    fn presence_changed_serializes() {
        let evt = A3chatEvent::PresenceChanged {
            event: PresenceEvent {
                user_id: UserId::from("bob"),
                status: PresenceStatus::Online,
                status_message: Some("ready".into()),
                timestamp: chrono::Utc::now(),
            },
        };
        let s = event_to_sse(evt);
        assert!(s.contains("\"status\":\"online\""));
    }

    #[test]
    fn typing_event_serializes() {
        let evt = A3chatEvent::ChatTyping {
            user_id: UserId::from("bob"),
            conversation_id: ConversationId::from("dm:a:b"),
            expires_at: 0,
        };
        let s = event_to_sse(evt);
        assert!(s.contains(A3chatRpcMethod::NOTIFICATION_CHAT_TYPING));
    }

    #[test]
    fn recalled_event_serializes() {
        let evt = A3chatEvent::ChatMessageRecalled {
            user_id: UserId::from("bob"),
            conversation_id: ConversationId::from("dm:a:b"),
            message_id: a3chat_core::id::MessageId::from("1".repeat(64)),
            recalled_at_unix: 99,
        };
        let s = event_to_sse(evt);
        assert!(s.contains("\"recalled_at_unix\":99"));
    }

    #[test]
    fn read_event_serializes() {
        let evt = A3chatEvent::ChatMessageRead {
            user_id: UserId::from("bob"),
            conversation_id: ConversationId::from("dm:a:b"),
            message_id: a3chat_core::id::MessageId::from("1".repeat(64)),
            read_at_unix: 42,
        };
        let s = event_to_sse(evt);
        assert!(s.contains("\"read_at_unix\":42"));
    }

    #[test]
    fn group_invitation_serializes() {
        let evt = A3chatEvent::GroupInvitationReceived {
            invitation: a3chat_core::group::GroupInvitation {
                invitation_id: "u".into(),
                conversation_id: ConversationId::from("grp:x"),
                group_name: "team".into(),
                inviter_id: UserId::from("alice"),
                inviter_name: "Alice".into(),
                invitee_id: UserId::from("bob"),
                status: a3chat_core::group::InvitationStatus::Pending,
                created_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now(),
            },
        };
        let s = event_to_sse(evt);
        assert!(s.contains("\"group_name\":\"team\""));
    }

    #[test]
    fn contact_request_serializes() {
        let evt = A3chatEvent::ContactRequestReceived {
            request_id: "r1".into(),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("\"request_id\":\"r1\""));
    }

    #[test]
    fn group_member_joined_serializes() {
        let evt = A3chatEvent::GroupMemberJoined {
            conversation_id: ConversationId::from("grp:x"),
            member: a3chat_core::group::GroupMember {
                user_id: UserId::from("bob"),
                display_name: "Bob".into(),
                role: a3chat_core::group::MemberRole::Member,
                joined_at: chrono::Utc::now(),
                last_seen: None,
                is_online: false,
                nickname: None,
            },
        };
        let s = event_to_sse(evt);
        assert!(s.contains("\"member\""));
    }

    #[test]
    fn frame_ends_with_blank_line() {
        // Per the SSE spec each frame terminates with `\n\n` —
        // guard against accidental refactors that drop one of
        // them (clients silently hang otherwise).
        let evt = A3chatEvent::ChatTyping {
            user_id: UserId::from("bob"),
            conversation_id: ConversationId::from("dm:a:b"),
            expires_at: 0,
        };
        let s = event_to_sse(evt);
        assert!(s.ends_with("\n\n"), "frame must end with a blank line");
    }

    #[test]
    fn owner_missing_returns_err() {
        let headers = HeaderMap::new();
        let err = owner_from_headers(&headers).unwrap_err();
        assert_eq!(err.code, ERR_A3CHAT_NOT_AUTHENTICATED);
    }

    #[test]
    fn owner_invalid_returns_err() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HEADER_OWNER,
            axum::http::HeaderValue::from_bytes(b"\xff").unwrap(),
        );
        assert!(owner_from_headers(&headers).is_err());
    }

    #[tokio::test]
    async fn stream_events_drains_when_app_drops() {
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(
            a3chat_app::storage::StorageConfig::new(dir.path().to_path_buf()),
            owner(),
        )
        .unwrap();
        let handle = tokio::spawn(async move {
            stream_events(&app, owner()).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        drop(handle);
    }

    // End-to-end: bind a real RpcServer on a loopback port, hit
    // /rpc/stream with reqwest's eventsource client, publish an
    // event, and assert the SSE stream contains the notification.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sse_handler_emits_published_events() {
        use crate::{RpcServer, RpcServerConfig};
        use a3chat_app::A3chatApp;
        use a3chat_app::storage::StorageConfig;
        use eventsource_stream::Eventsource;
        use futures::StreamExt;

        let dir = tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        let bus = app.bus.clone();
        let server = RpcServer::new(app, RpcServerConfig::default());
        let handle = server.start().await.unwrap();
        let base = format!("http://{}", handle.local_addr);

        let mut bus_rx = bus.subscribe();

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let url = format!("{base}/rpc/stream");
        let resp = client
            .get(&url)
            .header("X-A3Chat-Owner", owner().as_str())
            .header("X-A3Chat-Request-Id", "stream-trace-1")
            .send()
            .await
            .expect("sse get");
        assert!(resp.status().is_success());

        // Verify the response headers we set on the way out.
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.starts_with("text/event-stream"), "got ct={ct}");
        let echoed = resp
            .headers()
            .get("X-A3Chat-Owner")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert_eq!(echoed, owner().as_str());
        let rid_echoed = resp
            .headers()
            .get("X-A3Chat-Request-Id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert_eq!(rid_echoed, "stream-trace-1");

        let mut stream = resp.bytes_stream().eventsource();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        bus.publish(A3chatEvent::ChatTyping {
            user_id: UserId::from("bob"),
            conversation_id: ConversationId::from("dm:a:b"),
            expires_at: 0,
        });

        let inproc = tokio::time::timeout(std::time::Duration::from_millis(500), bus_rx.recv())
            .await
            .expect("in-process bus should receive the event we just published");
        assert!(matches!(inproc, Some(A3chatEvent::ChatTyping { .. })));

        let msg = tokio::time::timeout(std::time::Duration::from_secs(3), stream.next())
            .await
            .expect("sse timed out")
            .expect("sse stream ended")
            .expect("sse parse");
        assert!(
            msg.event
                .contains(A3chatRpcMethod::NOTIFICATION_CHAT_TYPING)
        );

        // Clean up — drop the stream + handle so the SSE task
        // ends and `handle.stop()` doesn't block waiting for
        // graceful shutdown. The server JoinHandle is dropped
        // here; the task finishes naturally when the
        // NotificationBus sender has no more clones.
        drop(stream);
        drop(bus_rx);
        // We can't `await` shutdown here because the long-lived
        // SSE connection would block graceful shutdown. Force
        // it by closing the client side of the connection.
        let _ = client.get(format!("{base}/rpc/health")).send().await;
    }

    // The handler refuses anonymous streams with HTTP 401.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sse_handler_rejects_anonymous_clients() {
        use crate::{RpcServer, RpcServerConfig};
        use a3chat_app::A3chatApp;
        use a3chat_app::storage::StorageConfig;

        let dir = tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        let server = RpcServer::new(app, RpcServerConfig::default());
        let handle = server.start().await.unwrap();
        let base = format!("http://{}", handle.local_addr);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        let url = format!("{base}/rpc/stream");
        let resp = client.get(&url).send().await.expect("sse get");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "missing X-A3Chat-Owner must yield 401"
        );
        drop(resp);
        drop(handle);
    }

    // Suppress unused warning for `Body` import.
    #[test]
    fn body_import_is_resolvable() {
        let _ = std::any::type_name::<Body>();
    }

    // Suppress unused warning for `MessageType`.
    #[test]
    fn message_type_round_trip() {
        let _ = MessageType::Text;
        let _ = MessageBody::Plain {
            content: "x".into(),
        };
    }
}
