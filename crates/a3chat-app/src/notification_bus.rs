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

use a3chat_core::event::A3chatEvent;
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
#[derive(Clone)]
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

    /// Publish an event. Returns `Ok(())` always — `send` only
    /// fails if there are no receivers, which we treat as a no-op.
    pub fn publish(&self, event: A3chatEvent) -> usize {
        // `send` returns the receiver count if any were listening.
        // We surface it for tests / logging.
        self.tx.send(event).unwrap_or_default()
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
            (Some(_), A3chatEvent::GroupInvitationReceived { .. }) => true,
            (Some(uid), A3chatEvent::ChatMessageRecalled { user_id, .. }) => uid == user_id,
            (Some(_), A3chatEvent::ChatMessageRead { .. }) => true,
            (Some(uid), A3chatEvent::ChatMessageEdited { user_id, .. }) => uid == user_id,
            (Some(uid), A3chatEvent::ChatMessageDeleted { user_id, .. }) => uid == user_id,
            (None, _) => true,
            // Presence events broadcast to all subscribers regardless.
            (_, A3chatEvent::PresenceChanged { .. }) => true,
            (_, A3chatEvent::ChatTyping { .. }) => true,
            (_, A3chatEvent::ContactRequestReceived { .. }) => true,
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
