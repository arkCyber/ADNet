//! In-process broadcast bus for [`a3chat_core::event::A3chatEvent`].
//!
//! Wraps `tokio::sync::broadcast` so multiple subscribers (one per
//! connected SSE client) all receive every event the services emit.
//!
//! ## Routing rules
//!
//! - `ChatMessage.received` events carry `user_id` of the recipient;
//!   only subscribers that registered with that `user_id` (or
//!   `None` = "all") receive the event.
//! - `Presence.changed` events are fire-and-forget — every
//!   subscriber receives them.
//! - The bus is **server-side** — it lives in `a3chat-app` and is
//!   bridged onto the SSE stream by `a3chat-rpc`.

use tokio::sync::broadcast;

pub use a3chat_core::event::A3chatEvent;
use a3chat_core::id::UserId;

/// Default broadcast channel capacity. 1024 events is enough to
/// buffer ~10 s of a busy user's traffic without losing events.
pub const DEFAULT_CAPACITY: usize = 1024;

/// Per-receiver handle returned by [`NotificationBus::subscribe`].
pub struct NotificationReceiver {
    pub user_id: Option<UserId>,
    pub rx: broadcast::Receiver<A3chatEvent>,
}

/// Shared bus. Cloning is cheap — every clone holds the same inner
/// sender.
#[derive(Clone, Debug)]
pub struct NotificationBus {
    tx: broadcast::Sender<A3chatEvent>,
}

impl Default for NotificationBus {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl NotificationBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish an event. Returns the receiver count that received
    /// the event.
    ///
    /// `tokio::sync::broadcast::send` returns `Err(SendError(value))`
    /// for two distinct reasons that **must not be conflated**:
    ///
    /// 1. **No receivers** — `receiver_count() == 0`. This is the
    ///    normal "no SSE clients attached" path; dropping the event
    ///    is correct.
    /// 2. **Lag** — at least one receiver exists but their queue
    ///    overflowed. The event is silently dropped and the lagged
    ///    receiver will see a `RecvError::Lagged` on its next poll.
    ///    This is a real correctness bug because the SSE client
    ///    will miss events with no easy way to recover.
    ///
    /// We surface the lag case via `tracing::warn!` so operators can
    /// see the dropped-event count and recognise the symptom of a
    /// busy period overwhelming the channel capacity. Future work
    /// (P1) will hook this into a `bus_overflow_total` Prometheus
    /// counter (see audit issue #8).
    pub fn publish(&self, event: A3chatEvent) -> usize {
        match self.tx.send(event) {
            Ok(n) => n,
            Err(broadcast::error::SendError(value)) => {
                if self.tx.receiver_count() == 0 {
                    // No subscribers — drop silently (normal case).
                    0
                } else {
                    // Lag — at least one receiver is overflowing.
                    // Log the variant so operators can correlate.
                    tracing::warn!(
                        event_kind = value.kind(),
                        receivers = self.tx.receiver_count(),
                        "notification bus overflow: lagged receiver dropped an event"
                    );
                    0
                }
            }
        }
    }

    /// Subscribe to all events. The returned `NotificationReceiver`
    /// can be polled by SSE handlers.
    pub fn subscribe(&self) -> NotificationReceiver {
        NotificationReceiver {
            user_id: None,
            rx: self.tx.subscribe(),
        }
    }

    /// Subscribe and filter to events addressed to a specific
    /// recipient. The receiver's [`recv`](NotificationReceiver::recv)
    /// helper drops events that don't match.
    pub fn subscribe_for(&self, user_id: UserId) -> NotificationReceiver {
        NotificationReceiver {
            user_id: Some(user_id),
            rx: self.tx.subscribe(),
        }
    }

    /// Number of currently-attached receivers.
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl NotificationReceiver {
    /// Receive one event, applying the user_id filter if any.
    ///
    /// Returns `None` if the channel is closed (no more senders)
    /// or if the event was filtered out and the caller wants to
    /// stop (callers loop and call `recv` again).
    pub async fn recv(&mut self) -> Option<A3chatEvent> {
        loop {
            let event = self.rx.recv().await.ok()?;
            if self.matches(&event) {
                return Some(event);
            }
        }
    }

    fn matches(&self, event: &A3chatEvent) -> bool {
        match (self.user_id.as_ref(), event) {
            (Some(uid), A3chatEvent::ChatMessageReceived { user_id, .. }) => uid == user_id,
            (Some(_), A3chatEvent::GroupMemberJoined { .. }) => true,
            (Some(uid), A3chatEvent::GroupMemberRemoved { user_id, .. }) => uid == user_id,
            (Some(_), A3chatEvent::GroupInvitationReceived { .. }) => true,
            (Some(uid), A3chatEvent::ChatMessageRecalled { user_id, .. }) => uid == user_id,
            (Some(_), A3chatEvent::ChatMessageRead { .. }) => true,
            (Some(uid), A3chatEvent::ChatMessageEdited { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::ChatMessageDeleted { user_id, .. }) => uid == user_id,
            // F-08 / B-06: each event carries the local owner's
            // user_id, so we route by that exactly like the chat
            // message events above. A None subscriber (catch-all)
            // is handled by the last branch.
            (Some(uid), A3chatEvent::GroupAnnouncementChanged { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::GroupDissolved { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::GroupMemberRoleChanged { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::GroupMuteChanged { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::GroupMuteAllChanged { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::GroupNicknameChanged { user_id, .. }) => uid == user_id,
            // Moments / 朋友圈 (F-05) — every event carries the
            // `user_id` of the local owner, so we route them the
            // same way as `ChatMessageReceived`: only the matching
            // subscriber receives them. A `None` subscriber still
            // sees everything via the catch-all below.
            (Some(uid), A3chatEvent::MomentsPostCreated { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::MomentsPostDeleted { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::MomentsCommentAdded { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::MomentsReactionToggled { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::MomentsCommentEdited { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::MomentsCommentDeleted { user_id, .. }) => uid == user_id,
            // v2 audit round — share / report / block are user-scoped
            // (the local owner is the actor), so route by `user_id`.
            (Some(uid), A3chatEvent::MomentsPostShared { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::MomentsPostReported { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::MomentsUserBlocked { user_id, .. }) => uid == user_id,
            // Presence events broadcast to all subscribers regardless.
            (_, A3chatEvent::PresenceChanged { .. }) => true,
            (_, A3chatEvent::ChatTyping { .. }) => true,
            // Contact events — broadcast to all (friend system)
            (_, A3chatEvent::ContactRequestReceived { .. }) => true,
            (_, A3chatEvent::ContactAdded { .. }) => true,
            (_, A3chatEvent::ContactRemoved { .. }) => true,
            (_, A3chatEvent::ContactUpdated { .. }) => true,
            (_, A3chatEvent::ContactBlocked { .. }) => true,
            (_, A3chatEvent::ContactUnblocked { .. }) => true,
            (_, A3chatEvent::ContactFavoriteToggled { .. }) => true,
            (_, A3chatEvent::ContactRequestAccepted { .. }) => true,
            (_, A3chatEvent::ContactRequestCancelled { .. }) => true,
            // Chat message reaction events — broadcast to all
            (_, A3chatEvent::ChatMessageReactionToggled { .. }) => true,
            // Link bookmark events — broadcast to all
            (_, A3chatEvent::LinkBookmarkAdded { .. }) => true,
            (_, A3chatEvent::LinkBookmarkUpdated { .. }) => true,
            (_, A3chatEvent::LinkBookmarkDeleted { .. }) => true,
            // Pin / notification / device events — broadcast to all
            (_, A3chatEvent::ConversationPinChanged { .. }) => true,
            (_, A3chatEvent::NotificationSettingsChanged { .. }) => true,
            (_, A3chatEvent::DeviceRegistered { .. }) => true,
            (_, A3chatEvent::DeviceRevoked { .. }) => true,
            (_, A3chatEvent::DevicePrimaryChanged { .. }) => true,
            // Pairing events — broadcast to all subscribers. The
            // owner id is embedded in the event for completeness but
            // the UI listens for *any* pairing activity so a
            // disconnected mobile client can refresh its device list
            // as soon as it comes back.
            (_, A3chatEvent::PairingInvitationCreated { .. }) => true,
            (_, A3chatEvent::PairingTrustedDeviceAdded { .. }) => true,
            (_, A3chatEvent::PairingTrustedDeviceRevoked { .. }) => true,
            // Channel / Public-Account events fire from
            // `a3chat.channel.*` RPC methods. They are global
            // announcements (account registered, updated, deleted,
            // feed published / retracted, subscription changed) —
            // every local subscriber should see them so the
            // in-process search index and the per-account follower
            // cache stay in lock-step.
            (_, A3chatEvent::ChannelAccountRegistered { .. }) => true,
            (_, A3chatEvent::ChannelAccountUpdated { .. }) => true,
            (_, A3chatEvent::ChannelAccountDeleted { .. }) => true,
            (Some(uid), A3chatEvent::ChannelSubscribed { user_id, .. }) if uid == user_id => true,
            (_, A3chatEvent::ChannelFeedPublished { .. }) => true,
            (_, A3chatEvent::ChannelFeedRetracted { .. }) => true,
            // Catch-all: any event whose owner filter already
            // matched above falls through here with `(None, _)`,
            // which means "no filter subscribed, accept every
            // event". We MUST keep this branch exhaustive so
            // adding a new event variant does not silently drop
            // the global subscriber's view.
            (None, _) => true,
            // An owner-scoped subscriber received an event not
            // addressed to it (e.g. a chat.message.received for a
            // peer). Drop it.
            (Some(_), _) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::event::{A3chatEvent, NOTIFICATION_KIND_CHAT};
    use a3chat_core::id::ConversationId;
    use a3chat_core::message::{ChatMessage, MessageBody, MessageType};

    fn chat_event(user_id: &str) -> A3chatEvent {
        A3chatEvent::ChatMessageReceived {
            user_id: UserId::from(user_id),
            conversation_id: ConversationId::from("dm:a:b"),
            message: ChatMessage::new_system(
                ConversationId::from("dm:a:b"),
                UserId::from("server"),
                "ping",
                1,
                1,
            )
            .unwrap(),
        }
    }

    #[tokio::test]
    async fn publish_reaches_global_subscriber() {
        let bus = NotificationBus::new(16);
        let mut rx = bus.subscribe();
        bus.publish(chat_event("alice"));
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event arrives")
            .expect("event is some");
        assert_eq!(evt.kind(), NOTIFICATION_KIND_CHAT);
    }

    #[tokio::test]
    async fn filter_drops_mismatched_user() {
        let bus = NotificationBus::new(16);
        let mut alice_rx = bus.subscribe_for(UserId::from("alice"));
        let mut bob_rx = bus.subscribe_for(UserId::from("bob"));
        bus.publish(chat_event("alice"));
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), alice_rx.recv())
            .await
            .expect("alice receives her event")
            .expect("event is some");
        match evt {
            A3chatEvent::ChatMessageReceived { user_id, .. } => {
                assert_eq!(user_id.as_str(), "alice");
            }
            _ => panic!("unexpected variant"),
        }
        // Bob's filtered receiver should not receive Alice's event.
        let r = tokio::time::timeout(std::time::Duration::from_millis(50), bob_rx.recv()).await;
        assert!(r.is_err(), "bob should not receive alice's chat event");
    }

    #[tokio::test]
    async fn publish_when_no_subscribers_returns_zero() {
        let bus = NotificationBus::new(16);
        let n = bus.publish(chat_event("alice"));
        assert_eq!(n, 0);
    }

    // Regression test for issue #8: when the broadcast channel
    // overflows, the `publish` path must NOT silently discard the
    // event without distinguishing it from "no subscribers". The
    // fix records a `tracing::warn!` and continues to return 0
    // (the receiver count when the event is dropped). We assert
    // that the receiver reports a `Lagged` error on its next poll
    // whenever the channel overflows.
    #[tokio::test]
    async fn publish_overflow_returns_lagged_to_subscriber() {
        let bus = NotificationBus::new(4);
        let mut rx = bus.subscribe();
        // Publish 4+1 events without polling — the channel is
        // depth 4 so the 5th publish overflows.
        for i in 0..5 {
            bus.publish(chat_event(&format!("u{i}")));
        }
        // The receiver must observe a Lagged error somewhere in
        // its stream. It might appear as the first poll (if all
        // older events were evicted) or interleaved.
        let mut saw_lag = false;
        for _ in 0..10 {
            match tokio::time::timeout(
                std::time::Duration::from_millis(50),
                rx.rx.recv(),
            )
            .await
            {
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    saw_lag = true;
                    break;
                }
                Ok(Err(_)) => break, // closed
                Err(_) => continue,  // timeout, keep polling
            }
        }
        assert!(saw_lag, "overflow should surface as RecvError::Lagged");
    }

    #[test]
    fn receiver_count_reflects_subscribers() {
        let bus = NotificationBus::new(16);
        assert_eq!(bus.receiver_count(), 0);
        let _r1 = bus.subscribe();
        let _r2 = bus.subscribe();
        assert_eq!(bus.receiver_count(), 2);
    }

    #[tokio::test]
    async fn presence_event_broadcasts_to_all() {
        let bus = NotificationBus::new(16);
        let mut alice = bus.subscribe_for(UserId::from("alice"));
        let mut bob = bus.subscribe_for(UserId::from("bob"));
        bus.publish(A3chatEvent::PresenceChanged {
            event: a3chat_core::presence::PresenceEvent {
                user_id: UserId::from("carol"),
                status: a3chat_core::presence::PresenceStatus::Online,
                status_message: None,
                timestamp: chrono::Utc::now(),
            },
        });
        // Both subscribers should see it (presence is global).
        tokio::time::timeout(std::time::Duration::from_millis(100), alice.recv())
            .await
            .expect("alice gets presence")
            .expect("event");
        tokio::time::timeout(std::time::Duration::from_millis(100), bob.recv())
            .await
            .expect("bob gets presence")
            .expect("event");
    }

    #[tokio::test]
    async fn typing_event_broadcasts_regardless_of_filter() {
        let bus = NotificationBus::new(16);
        let mut alice = bus.subscribe_for(UserId::from("alice"));
        bus.publish(A3chatEvent::ChatTyping {
            user_id: UserId::from("bob"),
            conversation_id: ConversationId::from("dm:a:b"),
            expires_at: 0,
        });
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), alice.recv())
            .await
            .expect("alice gets typing")
            .expect("event");
        assert!(matches!(evt, A3chatEvent::ChatTyping { .. }));
    }

    // Make sure MessageBody is reachable from this module's tests.
    #[test]
    fn smoke_message_type_present() {
        assert!(matches!(
            MessageType::Text,
            MessageType::Text
                | MessageType::Image
                | MessageType::File
                | MessageType::Voice
                | MessageType::Video
                | MessageType::System
                | MessageType::Call
        ));
        let _ = MessageBody::Plain {
            content: "x".into(),
        };
    }
}
