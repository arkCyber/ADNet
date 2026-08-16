//! ContactService — friends, requests, blocklist.

use std::sync::Arc;

use a3chat_core::contact::{BlocklistEntry, Contact, ContactRequest, ContactRequestStatus};
use a3chat_core::error::A3chatError;
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;

use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;
use base64::Engine;

/// Snapshot of the local contacts state for one user.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ContactsSnapshot {
    pub contacts: Vec<Contact>,
    pub blocklist: Vec<BlocklistEntry>,
}

#[derive(Clone)]
pub struct ContactService {
    bus: NotificationBus,
}

impl ContactService {
    pub fn new(bus: NotificationBus) -> Self {
        Self { bus }
    }

    pub fn bus(&self) -> &NotificationBus {
        &self.bus
    }

    /// `a3chat.contact.list` — returns contacts + blocklist.
    pub async fn list(&self, owner: &UserId) -> AppResult<ContactsSnapshot> {
        // P1: actually load from `a3net-roster`. P0 returns an
        // empty snapshot so callers can render the empty state.
        let _ = owner;
        Ok(ContactsSnapshot::default())
    }

    /// `a3chat.contact.add_request` — create and emit a friend
    /// request. P0 returns a placeholder; P1 calls `a3net-roster`.
    pub async fn add_request(
        &self,
        owner: &UserId,
        to_user: &UserId,
        message: String,
    ) -> AppResult<ContactRequest> {
        if message.len() > 256 {
            return Err(AppError::Domain(
                "friend-request message exceeds 256 chars".into(),
            ));
        }
        let req = ContactRequest {
            request_id: a3chat_core::id::generate_message_id(owner.as_str()).into_string(),
            from_user_id: owner.clone(),
            from_display_name: owner.as_str().into(),
            to_user_id: to_user.clone(),
            message,
            status: ContactRequestStatus::Pending,
            created_at: chrono::Utc::now(),
            responded_at: None,
        };
        req.validate()?;
        self.bus
            .publish(a3chat_core::event::A3chatEvent::ContactRequestReceived {
                request_id: req.request_id.clone(),
            });
        Ok(req)
    }

    /// `a3chat.contact.accept_request` — accept an inbound request.
    pub async fn accept_request(&self, owner: &UserId, request_id: &str) -> AppResult<Contact> {
        let _ = (owner, request_id);
        Err(AppError::NotInitialised("ContactService::accept_request"))
    }

    /// `a3chat.contact.block` — block `user_id`.
    pub async fn block(&self, owner: &UserId, user_id: &UserId) -> AppResult<BlocklistEntry> {
        let entry = BlocklistEntry {
            user_id: user_id.clone(),
            display_name: user_id.as_str().into(),
            blocked_at: chrono::Utc::now(),
            reason: None,
        };
        entry.validate()?;
        let _ = owner;
        Ok(entry)
    }

    /// `a3chat.contact.unblock` — remove from blocklist.
    pub async fn unblock(&self, owner: &UserId, user_id: &UserId) -> AppResult<()> {
        let _ = (owner, user_id);
        Ok(())
    }

    /// `a3chat.contact.qr_invite` — generate an invite payload
    /// (base64-encoded JSON, ready for QR encoding by the UI layer).
    pub async fn qr_invite(&self, owner: &UserId) -> AppResult<String> {
        let payload = serde_json::json!({
            "version": 1,
            "user_id": owner.as_str(),
            "kind": "contact_invite",
            "ts": chrono::Utc::now().timestamp(),
        });
        serde_json::to_string(&payload)
            .map(|s| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes()))
            .map_err(|e| AppError::Internal(e.to_string()))
    }
}

/// Dispatch helper used by `a3chat-rpc`.
pub async fn dispatch(
    svc: Arc<ContactService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        A3chatRpcMethod::CONTACT_LIST => {
            let snap = svc.list(owner).await.map_err(A3chatError::from)?;
            serde_json::to_value(snap).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CONTACT_ADD_REQUEST => {
            let to_user: UserId = serde_json::from_value(
                params
                    .get("to_user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("to_user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let message: String = params
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let req = svc
                .add_request(owner, &to_user, message)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(req).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CONTACT_BLOCK => {
            let user_id: UserId = serde_json::from_value(
                params
                    .get("user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let entry = svc
                .block(owner, &user_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(entry).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CONTACT_UNBLOCK => {
            let user_id: UserId = serde_json::from_value(
                params
                    .get("user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.unblock(owner, &user_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::CONTACT_ACCEPT_REQUEST => {
            let request_id: String = serde_json::from_value(
                params
                    .get("request_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("request_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let contact = svc
                .accept_request(owner, &request_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(contact).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CONTACT_QR_INVITE => {
            let s = svc.qr_invite(owner).await.map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "qr_payload": s }))
        }
        _ => Err(A3chatError::Internal(format!(
            "ContactService does not handle {method}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    #[tokio::test]
    async fn list_returns_empty_snapshot() {
        let svc = ContactService::new(NotificationBus::default());
        let snap = svc.list(&UserId::from("alice")).await.unwrap();
        assert!(snap.contacts.is_empty());
        assert!(snap.blocklist.is_empty());
    }

    #[tokio::test]
    async fn add_request_emits_event() {
        let svc = ContactService::new(NotificationBus::default());
        let mut rx = svc.bus().subscribe();
        let r = svc
            .add_request(&UserId::from("alice"), &UserId::from("bob"), "hi".into())
            .await
            .unwrap();
        assert_eq!(r.status, ContactRequestStatus::Pending);
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        match evt {
            a3chat_core::event::A3chatEvent::ContactRequestReceived { request_id } => {
                assert_eq!(request_id, r.request_id);
            }
            _ => panic!("wrong event kind"),
        }
    }

    #[tokio::test]
    async fn add_request_rejects_oversize_message() {
        let svc = ContactService::new(NotificationBus::default());
        let huge = "x".repeat(257);
        let r = svc
            .add_request(&UserId::from("alice"), &UserId::from("bob"), huge)
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn block_creates_entry() {
        let svc = ContactService::new(NotificationBus::default());
        let entry = svc
            .block(&UserId::from("alice"), &UserId::from("bob"))
            .await
            .unwrap();
        assert_eq!(entry.user_id, UserId::from("bob"));
    }

    #[tokio::test]
    async fn qr_invite_is_valid_base64() {
        let svc = ContactService::new(NotificationBus::default());
        let s = svc.qr_invite(&UserId::from("alice")).await.unwrap();
        let bytes = URL_SAFE_NO_PAD.decode(&s).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["user_id"], "alice");
        assert_eq!(json["kind"], "contact_invite");
    }

    #[tokio::test]
    async fn dispatch_accept_request_returns_not_initialised() {
        let svc = Arc::new(ContactService::new(NotificationBus::default()));
        let err = dispatch(
            svc,
            A3chatRpcMethod::CONTACT_ACCEPT_REQUEST,
            &UserId::from("alice"),
            serde_json::json!({ "request_id": "r1" }),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::Internal(_)));
    }

    #[tokio::test]
    async fn dispatch_unknown_method_errors() {
        let svc = Arc::new(ContactService::new(NotificationBus::default()));
        let err = dispatch(
            svc,
            "a3chat.bogus",
            &UserId::from("alice"),
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::Internal(_)));
    }
}
