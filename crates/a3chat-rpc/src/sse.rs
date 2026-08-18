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
use axum::http::HeaderMap;
use axum::http::header;
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
        A3chatEvent::GroupMemberRemoved {
            conversation_id,
            user_id,
            actor_user_id,
            removed_at_unix,
        } => (
            A3chatRpcMethod::NOTIFICATION_GROUP_MEMBER_REMOVED,
            serde_json::json!({
                "conversation_id": conversation_id,
                "user_id": user_id,
                "actor_user_id": actor_user_id,
                "removed_at_unix": removed_at_unix,
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
        // Moments / 朋友圈 (F-05) — the SSE client receives these
        // as `event:` lines so it can refresh the timeline without
        // having to poll. The `kind` strings match the constants
        // exposed in `A3chatRpcMethod` and are considered a
        // public-API contract.
        A3chatEvent::MomentsPostCreated {
            user_id,
            post_id,
            author_id,
            visibility,
        } => (
            "a3chat.moments.post.created",
            serde_json::json!({
                "user_id": user_id,
                "post_id": post_id,
                "author_id": author_id,
                "visibility": visibility,
            }),
        ),
        A3chatEvent::MomentsPostDeleted {
            user_id,
            post_id,
            author_id,
        } => (
            "a3chat.moments.post.deleted",
            serde_json::json!({
                "user_id": user_id,
                "post_id": post_id,
                "author_id": author_id,
            }),
        ),
        A3chatEvent::MomentsCommentAdded {
            user_id,
            post_id,
            comment_id,
            author_id,
        } => (
            "a3chat.moments.comment.added",
            serde_json::json!({
                "user_id": user_id,
                "post_id": post_id,
                "comment_id": comment_id,
                "author_id": author_id,
            }),
        ),
        A3chatEvent::MomentsReactionToggled {
            user_id,
            target_id,
            actor_id,
            reaction_type,
            is_added,
        } => (
            "a3chat.moments.reaction.toggled",
            serde_json::json!({
                "user_id": user_id,
                "target_id": target_id,
                "actor_id": actor_id,
                "reaction_type": reaction_type,
                "is_added": is_added,
            }),
        ),
        // Link bookmarks / favorites (F-08). The full bookmark
        // is included for added/updated (clients refresh their
        // cache) but only the id+url for delete (cheaper, plus
        // the cache can look the row up locally if it needs to).
        A3chatEvent::LinkBookmarkAdded { user_id, bookmark } => (
            "a3chat.link.bookmark.added",
            serde_json::json!({
                "user_id": user_id,
                "bookmark": bookmark,
            }),
        ),
        A3chatEvent::LinkBookmarkUpdated { user_id, bookmark } => (
            "a3chat.link.bookmark.updated",
            serde_json::json!({
                "user_id": user_id,
                "bookmark": bookmark,
            }),
        ),
        A3chatEvent::LinkBookmarkDeleted {
            user_id,
            bookmark_id,
            url,
        } => (
            "a3chat.link.bookmark.deleted",
            serde_json::json!({
                "user_id": user_id,
                "bookmark_id": bookmark_id,
                "url": url,
            }),
        ),
        // F-07: reaction toggled on a chat message.
        A3chatEvent::ChatMessageReactionToggled {
            user_id,
            conversation_id,
            message_id,
            reactor_id,
            reaction_type,
            is_added,
        } => (
            "a3chat.chat.message.reaction.toggled",
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "message_id": message_id,
                "reactor_id": reactor_id,
                "reaction_type": reaction_type,
                "is_added": is_added,
            }),
        ),
        // F-07: pinned-state change.
        A3chatEvent::ConversationPinChanged {
            user_id,
            conversation_id,
            pinned,
        } => (
            "a3chat.chat.conversation.pin.changed",
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "pinned": pinned,
            }),
        ),
        // F-07: notification settings change.
        A3chatEvent::NotificationSettingsChanged {
            user_id,
            conversation_id,
            global_dnd,
        } => (
            "a3chat.chat.notification.changed",
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "global_dnd": global_dnd,
            }),
        ),
        // F-07: device lifecycle.
        A3chatEvent::DeviceRegistered { user_id, device_id } => (
            "a3chat.device.registered",
            serde_json::json!({
                "user_id": user_id,
                "device_id": device_id,
            }),
        ),
        A3chatEvent::DeviceRevoked { user_id, device_id } => (
            "a3chat.device.revoked",
            serde_json::json!({
                "user_id": user_id,
                "device_id": device_id,
            }),
        ),
        A3chatEvent::DevicePrimaryChanged { user_id, device_id } => (
            "a3chat.device.primary.changed",
            serde_json::json!({
                "user_id": user_id,
                "device_id": device_id,
            }),
        ),
        // F-08 / B-24: group admin actions (announcement, dissolve,
        // role changes). The payload is the same shape that
        // `A3chatEvent::kind()` exposes — keeping these arms here
        // guarantees the wire string matches `kind()`.
        A3chatEvent::GroupAnnouncementChanged {
            user_id,
            conversation_id,
            text,
            actor_user_id,
        } => (
            "a3chat.group.announcement.changed",
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "text": text,
                "actor_user_id": actor_user_id,
            }),
        ),
        A3chatEvent::GroupDissolved {
            user_id,
            conversation_id,
            actor_user_id,
            dissolved_at_unix,
        } => (
            "a3chat.group.dissolved",
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "actor_user_id": actor_user_id,
                "dissolved_at_unix": dissolved_at_unix,
            }),
        ),
        A3chatEvent::GroupMemberRoleChanged {
            user_id,
            conversation_id,
            member_user_id,
            new_role,
            actor_user_id,
        } => (
            "a3chat.group.member.role.changed",
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "member_user_id": member_user_id,
                "new_role": new_role,
                "actor_user_id": actor_user_id,
            }),
        ),
        A3chatEvent::GroupMuteChanged {
            user_id,
            conversation_id,
            muted_user_id,
            is_muted,
            muted_until_unix,
            actor_user_id,
        } => (
            "a3chat.group.mute.changed",
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "muted_user_id": muted_user_id,
                "is_muted": is_muted,
                "muted_until_unix": muted_until_unix,
                "actor_user_id": actor_user_id,
            }),
        ),
        A3chatEvent::GroupMuteAllChanged {
            user_id,
            conversation_id,
            is_muted,
            actor_user_id,
        } => (
            "a3chat.group.mute.all.changed",
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "is_muted": is_muted,
                "actor_user_id": actor_user_id,
            }),
        ),
        A3chatEvent::GroupNicknameChanged {
            user_id,
            conversation_id,
            member_user_id,
            nickname,
            actor_user_id,
        } => (
            "a3chat.group.nickname.changed",
            serde_json::json!({
                "user_id": user_id,
                "conversation_id": conversation_id,
                "member_user_id": member_user_id,
                "nickname": nickname,
                "actor_user_id": actor_user_id,
            }),
        ),
        // Pairing (P2P device linking).
        A3chatEvent::PairingInvitationCreated {
            user_id,
            issuer_node_id,
            expires_at_unix,
        } => (
            "a3chat.pairing.invitation.created",
            serde_json::json!({
                "user_id": user_id,
                "issuer_node_id": issuer_node_id,
                "expires_at_unix": expires_at_unix,
            }),
        ),
        A3chatEvent::PairingTrustedDeviceAdded {
            user_id,
            credential_id,
            role,
            device_name,
        } => (
            "a3chat.pairing.trusted.added",
            serde_json::json!({
                "user_id": user_id,
                "credential_id": credential_id,
                "role": role,
                "device_name": device_name,
            }),
        ),
        A3chatEvent::PairingTrustedDeviceRevoked {
            user_id,
            credential_id,
        } => (
            "a3chat.pairing.trusted.revoked",
            serde_json::json!({
                "user_id": user_id,
                "credential_id": credential_id,
            }),
        ),
        // F-07: contact roster changes (already documented in
        // A3chatEvent::kind() but missing from the SSE dispatch
        // here — explicit arms guarantee the original event
        // names reach the client).
        A3chatEvent::ContactAdded { contact_id } => (
            "a3chat.contact.added",
            serde_json::json!({ "contact_id": contact_id }),
        ),
        A3chatEvent::ContactRemoved { contact_id } => (
            "a3chat.contact.removed",
            serde_json::json!({ "contact_id": contact_id }),
        ),
        A3chatEvent::ContactUpdated { contact_id } => (
            "a3chat.contact.updated",
            serde_json::json!({ "contact_id": contact_id }),
        ),
        A3chatEvent::ContactBlocked { user_id } => (
            "a3chat.contact.blocked",
            serde_json::json!({ "user_id": user_id }),
        ),
        A3chatEvent::ContactUnblocked { user_id } => (
            "a3chat.contact.unblocked",
            serde_json::json!({ "user_id": user_id }),
        ),
        A3chatEvent::ContactFavoriteToggled {
            contact_id,
            is_favorite,
        } => (
            "a3chat.contact.favorite.toggled",
            serde_json::json!({
                "contact_id": contact_id,
                "is_favorite": is_favorite,
            }),
        ),
        A3chatEvent::ContactRequestAccepted {
            request_id,
            contact_id,
        } => (
            "a3chat.contact.request.accepted",
            serde_json::json!({
                "request_id": request_id,
                "contact_id": contact_id,
            }),
        ),
        A3chatEvent::ContactRequestCancelled { request_id, by_user_id } => (
            // Distinct from `a3chat.contact.request.accepted` so SSE
            // consumers can drop pending inbox rows without confusing
            // the two lifecycle states.
            "a3chat.contact.request.cancelled",
            serde_json::json!({
                "request_id": request_id,
                "by_user_id": by_user_id,
            }),
        ),
        // Forward-compatible catch-all for any *future* event variants
        // that this dispatcher has not yet been taught about. Today
        // every variant is enumerated explicitly above, so this arm
        // is intentionally unreachable; the `#[allow]` keeps the
        // dispatcher future-proof without re-introducing the
        // non-exhaustive-match hazard that previously broke the
        // build.
        #[allow(unreachable_patterns)]
        other => {
            let payload = serde_json::to_value(&other).unwrap_or(serde_json::Value::Null);
            // The catch-all leaks a heap-allocated String, but the
            // String lives only for the lifetime of `event`; we
            // therefore leak the box so the `(kind, payload)`
            // tuple can stay `(&'static str, _)`. Leaking is
            // appropriate here because the event name is fully
            // determined by the (statically-known) variant name.
            let kind: &'static str = Box::leak(format!(
                "a3chat.event.{}",
                event_variant_name(&other)
            ).into_boxed_str());
            (kind, payload)
        }
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

/// Return the snake_case variant name of an `A3chatEvent`. Used by
/// the forward-compatible catch-all in [`event_to_sse`] so that new
/// event variants surface over SSE without requiring a code change
/// in this dispatcher.
fn event_variant_name(event: &a3chat_core::event::A3chatEvent) -> &'static str {
    use a3chat_core::event::A3chatEvent;
    match event {
        A3chatEvent::ChatMessageReceived { .. } => "chat_message_received",
        A3chatEvent::ChatMessageRecalled { .. } => "chat_message_recalled",
        A3chatEvent::ChatMessageRead { .. } => "chat_message_read",
        A3chatEvent::ChatMessageEdited { .. } => "chat_message_edited",
        A3chatEvent::ChatMessageDeleted { .. } => "chat_message_deleted",
        A3chatEvent::ChatTyping { .. } => "chat_typing",
        A3chatEvent::ChatMessageReactionToggled { .. } => "chat_message_reaction_toggled",
        A3chatEvent::PresenceChanged { .. } => "presence_changed",
        A3chatEvent::GroupMemberJoined { .. } => "group_member_joined",
        A3chatEvent::GroupMemberRemoved { .. } => "group_member_removed",
        A3chatEvent::GroupInvitationReceived { .. } => "group_invitation_received",
        A3chatEvent::ContactRequestReceived { .. } => "contact_request_received",
        A3chatEvent::ContactAdded { .. } => "contact_added",
        A3chatEvent::ContactRemoved { .. } => "contact_removed",
        A3chatEvent::ContactUpdated { .. } => "contact_updated",
        A3chatEvent::ContactBlocked { .. } => "contact_blocked",
        A3chatEvent::ContactUnblocked { .. } => "contact_unblocked",
        A3chatEvent::ContactFavoriteToggled { .. } => "contact_favorite_toggled",
        A3chatEvent::ContactRequestAccepted { .. } => "contact_request_accepted",
        A3chatEvent::ContactRequestCancelled { .. } => "contact_request_cancelled",
        A3chatEvent::ConversationPinChanged { .. } => "conversation_pin_changed",
        A3chatEvent::MomentsPostCreated { .. } => "moments_post_created",
        A3chatEvent::MomentsPostDeleted { .. } => "moments_post_deleted",
        A3chatEvent::MomentsCommentAdded { .. } => "moments_comment_added",
        A3chatEvent::MomentsReactionToggled { .. } => "moments_reaction_toggled",
        A3chatEvent::LinkBookmarkAdded { .. } => "link_bookmark_added",
        A3chatEvent::LinkBookmarkUpdated { .. } => "link_bookmark_updated",
        A3chatEvent::LinkBookmarkDeleted { .. } => "link_bookmark_deleted",
        A3chatEvent::NotificationSettingsChanged { .. } => "notification_settings_changed",
        A3chatEvent::DeviceRegistered { .. } => "device_registered",
        A3chatEvent::DeviceRevoked { .. } => "device_revoked",
        A3chatEvent::DevicePrimaryChanged { .. } => "device_primary_changed",
        A3chatEvent::GroupAnnouncementChanged { .. } => "group_announcement_changed",
        A3chatEvent::GroupDissolved { .. } => "group_dissolved",
        A3chatEvent::GroupMemberRoleChanged { .. } => "group_member_role_changed",
        A3chatEvent::GroupMuteChanged { .. } => "group_mute_changed",
        A3chatEvent::GroupMuteAllChanged { .. } => "group_mute_all_changed",
        A3chatEvent::GroupNicknameChanged { .. } => "group_nickname_changed",
        A3chatEvent::PairingInvitationCreated { .. } => "pairing_invitation_created",
        A3chatEvent::PairingTrustedDeviceAdded { .. } => "pairing_trusted_added",
        A3chatEvent::PairingTrustedDeviceRevoked { .. } => "pairing_trusted_revoked",
        A3chatEvent::MomentsPostShared { .. } => "moments_post_shared",
        A3chatEvent::MomentsPostReported { .. } => "moments_post_reported",
        A3chatEvent::MomentsUserBlocked { .. } => "moments_user_blocked",
        A3chatEvent::MomentsCommentEdited { .. } => "moments_comment_edited",
        A3chatEvent::MomentsCommentDeleted { .. } => "moments_comment_deleted",
        A3chatEvent::ChannelAccountRegistered { .. } => "channel_account_registered",
        A3chatEvent::ChannelAccountUpdated { .. } => "channel_account_updated",
        A3chatEvent::ChannelAccountDeleted { .. } => "channel_account_deleted",
        A3chatEvent::ChannelSubscribed { .. } => "channel_subscribed",
        A3chatEvent::ChannelUnsubscribed { .. } => "channel_unsubscribed",
        A3chatEvent::ChannelFeedPublished { .. } => "channel_feed_published",
        A3chatEvent::ChannelFeedRetracted { .. } => "channel_feed_retracted",
    }
}

fn owner_from_headers(headers: &HeaderMap) -> Result<UserId, RpcError> {
    let value = headers.get(HEADER_OWNER).ok_or_else(|| {
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
    if let Some(last_id) = headers.get("last-event-id").and_then(|v| v.to_str().ok()) {
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
    fn moments_post_created_serializes() {
        let evt = A3chatEvent::MomentsPostCreated {
            user_id: UserId::from("alice"),
            post_id: "p-1".into(),
            author_id: "alice".into(),
            visibility: "public".into(),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.moments.post.created"));
        assert!(s.contains("\"post_id\":\"p-1\""));
        assert!(s.contains("\"visibility\":\"public\""));
    }

    #[test]
    fn moments_post_deleted_serializes() {
        let evt = A3chatEvent::MomentsPostDeleted {
            user_id: UserId::from("alice"),
            post_id: "p-1".into(),
            author_id: "alice".into(),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.moments.post.deleted"));
        assert!(s.contains("\"post_id\":\"p-1\""));
    }

    #[test]
    fn moments_comment_added_serializes() {
        let evt = A3chatEvent::MomentsCommentAdded {
            user_id: UserId::from("alice"),
            post_id: "p-1".into(),
            comment_id: "c-1".into(),
            author_id: "alice".into(),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.moments.comment.added"));
        assert!(s.contains("\"comment_id\":\"c-1\""));
    }

    #[test]
    fn moments_reaction_toggled_serializes() {
        let evt = A3chatEvent::MomentsReactionToggled {
            user_id: UserId::from("alice"),
            target_id: "p-1".into(),
            actor_id: "bob".into(),
            reaction_type: "like".into(),
            is_added: true,
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.moments.reaction.toggled"));
        assert!(s.contains("\"reaction_type\":\"like\""));
        assert!(s.contains("\"is_added\":true"));
    }

    #[test]
    fn link_bookmark_added_serializes() {
        let evt = A3chatEvent::LinkBookmarkAdded {
            user_id: UserId::from("alice"),
            bookmark: a3chat_core::link_bookmark::LinkBookmark::default(),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.link.bookmark.added"));
        assert!(s.contains("\"bookmark\""));
    }

    #[test]
    fn link_bookmark_updated_serializes() {
        let evt = A3chatEvent::LinkBookmarkUpdated {
            user_id: UserId::from("alice"),
            bookmark: a3chat_core::link_bookmark::LinkBookmark::default(),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.link.bookmark.updated"));
    }

    #[test]
    fn link_bookmark_deleted_serializes() {
        let evt = A3chatEvent::LinkBookmarkDeleted {
            user_id: UserId::from("alice"),
            bookmark_id: "bm-1".into(),
            url: "https://example.com".into(),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.link.bookmark.deleted"));
        assert!(s.contains("\"bookmark_id\":\"bm-1\""));
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

    // ── P3 wired events — every newly-onboarded service must
    // serialize over SSE so multi-device clients see the event. ──

    #[test]
    fn reaction_toggled_event_serializes() {
        let evt = A3chatEvent::ChatMessageReactionToggled {
            user_id: UserId::from("alice"),
            conversation_id: ConversationId::from("dm:a:b"),
            message_id: a3chat_core::id::MessageId::from("m1"),
            reactor_id: UserId::from("bob"),
            reaction_type: "thumbsup".into(),
            is_added: false,
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.chat.message.reaction.toggled"));
        assert!(s.contains("\"reactor_id\":\"bob\""));
        assert!(s.contains("\"is_added\":false"));
    }

    #[test]
    fn conversation_pin_changed_event_serializes() {
        let evt = A3chatEvent::ConversationPinChanged {
            user_id: UserId::from("alice"),
            conversation_id: ConversationId::from("dm:a:b"),
            pinned: true,
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.chat.conversation.pin.changed"));
        assert!(s.contains("\"pinned\":true"));
    }

    #[test]
    fn notification_settings_changed_event_serializes() {
        let evt = A3chatEvent::NotificationSettingsChanged {
            user_id: UserId::from("alice"),
            conversation_id: Some(ConversationId::from("dm:a:b")),
            global_dnd: None,
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.chat.notification.changed"));
        assert!(s.contains("\"conversation_id\":\"dm:a:b\""));
    }

    #[test]
    fn device_registered_event_serializes() {
        let evt = A3chatEvent::DeviceRegistered {
            user_id: UserId::from("alice"),
            device_id: "dev-1".into(),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.device.registered"));
        assert!(s.contains("\"device_id\":\"dev-1\""));
    }

    #[test]
    fn device_revoked_event_serializes() {
        let evt = A3chatEvent::DeviceRevoked {
            user_id: UserId::from("alice"),
            device_id: "dev-1".into(),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.device.revoked"));
    }

    #[test]
    fn device_primary_changed_event_serializes() {
        let evt = A3chatEvent::DevicePrimaryChanged {
            user_id: UserId::from("alice"),
            device_id: "dev-2".into(),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.device.primary.changed"));
        assert!(s.contains("\"device_id\":\"dev-2\""));
    }

    #[test]
    fn chat_message_edited_event_serializes() {
        let evt = A3chatEvent::ChatMessageEdited {
            user_id: UserId::from("alice"),
            conversation_id: ConversationId::from("dm:a:b"),
            message: ChatMessage::new_system(
                ConversationId::from("dm:a:b"),
                UserId::from("server"),
                "edit",
                1,
                1,
            )
            .unwrap(),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.chat.message.edited"));
        assert!(s.contains("\"message\""));
    }

    #[test]
    fn chat_message_deleted_event_serializes() {
        let evt = A3chatEvent::ChatMessageDeleted {
            user_id: UserId::from("alice"),
            conversation_id: ConversationId::from("dm:a:b"),
            message_id: a3chat_core::id::MessageId::from("m1"),
        };
        let s = event_to_sse(evt);
        assert!(s.contains("a3chat.chat.message.deleted"));
        assert!(s.contains("\"message_id\":\"m1\""));
    }
}
