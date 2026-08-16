//! PresenceService — publish local presence; subscribe to remote
//! presence changes.

use std::sync::Arc;

use a3chat_core::error::A3chatError;
use a3chat_core::id::UserId;
use a3chat_core::presence::{Presence, PresenceEvent, PresenceStatus};
use a3chat_core::rpc::A3chatRpcMethod;

use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;
use crate::storage::ChatStorage;

#[derive(Clone)]
pub struct PresenceService {
    storage: ChatStorage,
    bus: NotificationBus,
}

impl PresenceService {
    pub fn new(storage: ChatStorage, bus: NotificationBus) -> Self {
        Self { storage, bus }
    }

    /// `a3chat.presence.publish` — set our own presence.
    pub async fn publish(
        &self,
        owner: &UserId,
        status: PresenceStatus,
        status_message: Option<String>,
    ) -> AppResult<Presence> {
        if let Some(ref m) = status_message
            && m.len() > 256
        {
            return Err(AppError::Domain("status_message exceeds 256 chars".into()));
        }
        let p = Presence {
            user_id: owner.clone(),
            status,
            status_message,
            last_changed: chrono::Utc::now(),
        };
        self.storage.upsert_presence(owner, &p).await?;
        self.bus
            .publish(a3chat_core::event::A3chatEvent::PresenceChanged {
                event: PresenceEvent {
                    user_id: owner.clone(),
                    status,
                    status_message: p.status_message.clone(),
                    timestamp: p.last_changed,
                },
            });
        Ok(p)
    }

    /// `a3chat.presence.subscribe` — request presence updates for
    /// the listed peers. The caller receives `PresenceChanged`
    /// notifications on the bus until cancelled.
    pub async fn subscribe(&self, owner: &UserId, peers: &[UserId]) -> AppResult<Vec<Presence>> {
        let mut out = Vec::with_capacity(peers.len());
        for peer in peers {
            if let Some(p) = self.storage.get_presence(owner, peer).await? {
                out.push(p);
            } else {
                out.push(Presence {
                    user_id: peer.clone(),
                    status: PresenceStatus::Offline,
                    status_message: None,
                    last_changed: chrono::Utc::now(),
                });
            }
        }
        Ok(out)
    }
}

pub async fn dispatch(
    svc: Arc<PresenceService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        A3chatRpcMethod::PRESENCE_PUBLISH => {
            let status_str: String = serde_json::from_value(
                params
                    .get("status")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("status missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let status = PresenceStatus::parse(&status_str)
                .ok_or_else(|| A3chatError::InvalidInput(format!("unknown status {status_str}")))?;
            let status_message: Option<String> = params
                .get("status_message")
                .and_then(|v| v.as_str())
                .map(String::from);
            let p = svc
                .publish(owner, status, status_message)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(p).map_err(A3chatError::from)
        }
        A3chatRpcMethod::PRESENCE_SUBSCRIBE => {
            let peers: Vec<UserId> = serde_json::from_value(
                params
                    .get("peers")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("peers missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let ps = svc
                .subscribe(owner, &peers)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(ps).map_err(A3chatError::from)
        }
        _ => Err(A3chatError::Internal(format!(
            "PresenceService does not handle {method}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyring::E2eKeyring;
    use a3chat_core::id::UserId;
    use tempfile::tempdir;

    fn owner() -> UserId {
        UserId::from("alice")
    }

    async fn fresh() -> (tempfile::TempDir, PresenceService) {
        let dir = tempdir().unwrap();
        let keyring = E2eKeyring::new(owner());
        let storage = ChatStorage::new(
            crate::storage::StorageConfig::new(dir.path().to_path_buf()),
            keyring,
        );
        storage.init_user(&owner()).await.unwrap();
        let bus = NotificationBus::default();
        (dir, PresenceService::new(storage, bus))
    }

    #[tokio::test]
    async fn publish_stores_and_emits() {
        let (_d, svc) = fresh().await;
        let mut rx = svc.bus.subscribe();
        let p = svc
            .publish(&owner(), PresenceStatus::Online, Some("ready".into()))
            .await
            .unwrap();
        assert_eq!(p.status, PresenceStatus::Online);
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        assert!(matches!(
            evt,
            a3chat_core::event::A3chatEvent::PresenceChanged { .. }
        ));
    }

    #[tokio::test]
    async fn publish_rejects_oversize_message() {
        let (_d, svc) = fresh().await;
        let r = svc
            .publish(&owner(), PresenceStatus::Online, Some("x".repeat(257)))
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn subscribe_returns_offline_for_unknown_peers() {
        let (_d, svc) = fresh().await;
        let ps = svc
            .subscribe(&owner(), &[UserId::from("bob"), UserId::from("carol")])
            .await
            .unwrap();
        assert_eq!(ps.len(), 2);
        for p in ps {
            assert_eq!(p.status, PresenceStatus::Offline);
        }
    }

    #[tokio::test]
    async fn subscribe_returns_known_presence() {
        let (_d, svc) = fresh().await;
        svc.publish(&owner(), PresenceStatus::Away, None)
            .await
            .unwrap();
        let ps = svc.subscribe(&owner(), &[owner()]).await.unwrap();
        assert_eq!(ps[0].status, PresenceStatus::Away);
    }

    #[tokio::test]
    async fn dispatch_unknown_method_errors() {
        let (_d, svc) = fresh().await;
        let err = dispatch(
            Arc::new(svc),
            "a3chat.bogus",
            &owner(),
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::Internal(_)));
    }
}
