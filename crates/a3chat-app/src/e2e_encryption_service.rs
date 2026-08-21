//! E2E encryption service — session management helpers.
//!
//! DO-178C §6.4.5: The E2E service never logs or persists plaintext.
//!
//! This service provides helpers for managing DM session lifecycle using
//! the Noise_XX handshake implemented in `a3chat-crypto`.

#![forbid(unsafe_code)]

use std::sync::Arc;

use a3chat_core::error::A3chatError;
use a3chat_core::id::UserId;
use a3chat_core::id::validate_id;

use crate::keyring::E2eKeyring;

/// The E2E encryption service.
#[derive(Clone, Debug)]
pub struct E2eEncryptionService {
    keyring: E2eKeyring,
}

/// Tri-state return for [`E2eEncryptionService::needs_rehandshake`].
/// Distinguishes "no session yet" from "session is fresh" from
/// "session is exhausted" so callers can react differently (the
/// UI wants to show "Connect" vs "Re-handshake").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RehandshakeState {
    /// No session has ever been established with this peer.
    NoSession,
    /// Session is fresh — handshake is fine for now.
    Fresh,
    /// Session has exceeded `MAX_MESSAGES` or `MAX_AGE_SECS`.
    Expired,
}

impl E2eEncryptionService {
    #[must_use = "constructing an E2E binding without using it is a bug"]
    pub fn new(keyring: E2eKeyring) -> Self {
        Self { keyring }
    }

    /// Tri-state check: whether a peer session needs re-handshake.
    /// Returns `Expired` if the session has exceeded `MAX_MESSAGES`
    /// or `MAX_AGE_SECS`, `Fresh` if a session exists but is fine,
    /// `NoSession` otherwise.
    pub fn rehandshake_state(&self, peer: &UserId) -> RehandshakeState {
        let session = self.keyring.session(peer);
        match session.dm_session() {
            Some(s) if s.needs_rehandshake(chrono::Utc::now().timestamp()) => {
                RehandshakeState::Expired
            }
            Some(_) => RehandshakeState::Fresh,
            None => RehandshakeState::NoSession,
        }
    }

    /// Backward-compatible bool wrapper. Returns `true` only when
    /// the session has expired; treats `NoSession` and `Fresh`
    /// identically. Prefer [`E2eEncryptionService::rehandshake_state`]
    /// in new code.
    pub fn needs_rehandshake(&self, peer: &UserId) -> bool {
        matches!(self.rehandshake_state(peer), RehandshakeState::Expired)
    }

    /// Get the current handshake completion status for a peer.
    pub fn is_handshake_complete(&self, peer: &UserId) -> bool {
        let session = self.keyring.session(peer);
        session.handshake_completed
    }
}

impl Default for E2eEncryptionService {
    /// Builds an E2E service whose keyring is owned by a sentinel
    /// id. Useful for tests; production code should always pass
    /// the real owner via [`E2eEncryptionService::new`].
    fn default() -> Self {
        Self::new(E2eKeyring::new(UserId::from("default-owner")))
    }
}

/// Dispatcher entry point used by `a3chat-app::app::A3chatApp::dispatch`.
pub async fn dispatch(
    svc: Arc<E2eEncryptionService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    let peer: UserId = serde_json::from_value(
        params
            .get("peer")
            .cloned()
            .ok_or_else(|| A3chatError::InvalidInput("peer missing".into()))?,
    )
    .map_err(A3chatError::from)?;
    if peer.as_str().is_empty() {
        return Err(A3chatError::InvalidInput("peer must be non-empty".into()));
    }
    validate_id("peer", peer.as_str())
        .map_err(|e| A3chatError::InvalidInput(format!("peer: {e}")))?;
    if peer == *owner {
        return Err(A3chatError::InvalidInput(
            "peer must differ from owner".into(),
        ));
    }
    match method {
        "a3chat.e2e.handshake.needs_rehandshake" => {
            let state = svc.rehandshake_state(&peer);
            Ok(serde_json::json!({
                "peer": peer.as_str(),
                "needs_rehandshake": matches!(state, RehandshakeState::Expired),
                "state": match state {
                    RehandshakeState::NoSession => "no_session",
                    RehandshakeState::Fresh => "fresh",
                    RehandshakeState::Expired => "expired",
                },
            }))
        }
        "a3chat.e2e.handshake.is_complete" => {
            let complete = svc.is_handshake_complete(&peer);
            Ok(serde_json::json!({
                "peer": peer.as_str(),
                "is_complete": complete,
            }))
        }
        // The encrypt/decrypt RPCs are declared in `a3chat-core::rpc`
        // for API symmetry but currently no caller invokes them:
        // outgoing payload encryption happens inline inside
        // `chat_service::send_message`. Returning a clean
        // NotImplemented is friendlier than 500-routing the call.
        "a3chat.e2e.encrypt" | "a3chat.e2e.decrypt" => Err(A3chatError::Internal(
            "E2eEncryptionService does not implement RPC encrypt/decrypt; use chat.message.send".into(),
        )),
        m => Err(A3chatError::Internal(format!(
            "E2eEncryptionService does not handle {m}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> UserId {
        UserId::from("alice-node")
    }

    #[test]
    fn needs_rehandshake_returns_false_when_no_session() {
        let keyring = crate::keyring::E2eKeyring::new(owner());
        let svc = E2eEncryptionService::new(keyring);
        assert!(!svc.needs_rehandshake(&UserId::from("bob-node")));
    }

    #[test]
    fn is_handshake_complete_returns_false_when_no_session() {
        let keyring = crate::keyring::E2eKeyring::new(owner());
        let svc = E2eEncryptionService::new(keyring);
        assert!(!svc.is_handshake_complete(&UserId::from("bob-node")));
    }

    #[test]
    fn service_can_be_created_from_keyring() {
        let keyring = crate::keyring::E2eKeyring::new(owner());
        let svc = E2eEncryptionService::new(keyring);
        assert!(!svc.needs_rehandshake(&UserId::from("bob")));
    }

    #[test]
    fn needs_rehandshake_for_different_peers() {
        let keyring = crate::keyring::E2eKeyring::new(owner());
        let svc = E2eEncryptionService::new(keyring);
        // Each peer should return false when no session exists.
        assert!(!svc.needs_rehandshake(&UserId::from("peer1")));
        assert!(!svc.needs_rehandshake(&UserId::from("peer2")));
        assert!(!svc.needs_rehandshake(&UserId::from("peer3")));
    }

    #[test]
    fn is_handshake_complete_for_different_peers() {
        let keyring = crate::keyring::E2eKeyring::new(owner());
        let svc = E2eEncryptionService::new(keyring);
        // Each peer should return false when no session exists.
        assert!(!svc.is_handshake_complete(&UserId::from("peer1")));
        assert!(!svc.is_handshake_complete(&UserId::from("peer2")));
        assert!(!svc.is_handshake_complete(&UserId::from("peer3")));
    }

    #[test]
    fn e2e_service_is_clone() {
        let keyring = crate::keyring::E2eKeyring::new(owner());
        let svc = E2eEncryptionService::new(keyring);
        let _cloned = svc.clone();
        // Both should behave identically.
        assert!(!svc.needs_rehandshake(&UserId::from("bob")));
        assert!(!_cloned.needs_rehandshake(&UserId::from("bob")));
    }

    #[test]
    fn needs_rehandshake_false_without_session() {
        // Without an active session, needs_rehandshake should return false.
        let keyring = crate::keyring::E2eKeyring::new(owner());
        let svc = E2eEncryptionService::new(keyring);
        assert!(!svc.needs_rehandshake(&UserId::from("bob")));
        assert!(!svc.needs_rehandshake(&UserId::from("carol")));
    }

    #[test]
    fn is_handshake_complete_false_without_session() {
        // Without an active session, handshake should not be complete.
        let keyring = crate::keyring::E2eKeyring::new(owner());
        let svc = E2eEncryptionService::new(keyring);
        assert!(!svc.is_handshake_complete(&UserId::from("bob")));
    }

    #[test]
    fn e2e_service_multiple_peers() {
        let keyring = crate::keyring::E2eKeyring::new(owner());
        let svc = E2eEncryptionService::new(keyring);

        let peers = vec![
            UserId::from("peer1"),
            UserId::from("peer2"),
            UserId::from("peer3"),
        ];

        for peer in peers {
            assert!(!svc.needs_rehandshake(&peer));
            assert!(!svc.is_handshake_complete(&peer));
        }
    }
}
