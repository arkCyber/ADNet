//! Coordinator error type.

use thiserror::Error;

pub type CoordinatorResult<T> = std::result::Result<T, CoordinatorError>;

#[derive(Debug, Error)]
pub enum CoordinatorError {
    #[error("invite code {0} is unknown")]
    UnknownInvite(String),

    #[error("invite code {0} has expired")]
    InviteExpired(String),

    #[error("invite code {0} has already been redeemed")]
    InviteAlreadyRedeemed(String),

    #[error("join request {0} is unknown")]
    UnknownRequest(uuid::Uuid),

    #[error("join request is in state {actual}, expected {expected}")]
    InvalidRequestState {
        actual: String,
        expected: String,
    },

    #[error("host {host} already in use on {network}")]
    HostnameTaken { host: String, network: String },

    #[error("member {node_id_short} already in {network}")]
    AlreadyMember {
        node_id_short: String,
        network: String,
    },

    #[error("member {node_id_short} is not part of {network}")]
    UnknownMember {
        node_id_short: String,
        network: String,
    },

    #[error("roster at capacity ({max} members)")]
    RosterFull { max: usize },

    #[error("network is open; this admission path is for closed networks only")]
    NetworkIsOpen,

    #[error("peering grant source and target must differ")]
    PeeringSelfLoop,

    #[error("peering grant id {0} is unknown")]
    UnknownPeeringGrant(String),

    #[error("peering grant id {grant_id} was re-issued with a different payload")]
    PeeringGrantIdReused { grant_id: String },

    #[error("peering grant signature is invalid: {0}")]
    PeeringSignatureInvalid(String),

    #[error("peering grant coordinator pubkey is unknown for mesh {0}")]
    PeeringUnknownCoordinator(String),

    #[error("peering grant {grant_id} expired at {valid_until}")]
    PeeringExpired {
        grant_id: String,
        valid_until: chrono::DateTime<chrono::Utc>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_invite_includes_code() {
        let e = CoordinatorError::UnknownInvite("ab3f9c01".into());
        assert!(e.to_string().contains("ab3f9c01"));
    }

    #[test]
    fn invalid_request_state_includes_both() {
        let e = CoordinatorError::InvalidRequestState {
            actual: "Approved".into(),
            expected: "Pending".into(),
        };
        let s = e.to_string();
        assert!(s.contains("Approved"));
        assert!(s.contains("Pending"));
    }

    #[test]
    fn hostname_taken_includes_both() {
        let e = CoordinatorError::HostnameTaken {
            host: "alice".into(),
            network: "gaming".into(),
        };
        assert!(e.to_string().contains("alice"));
        assert!(e.to_string().contains("gaming"));
    }

    #[test]
    fn unknown_member_includes_both() {
        let e = CoordinatorError::UnknownMember {
            node_id_short: "abcdef12".into(),
            network: "gaming".into(),
        };
        let s = e.to_string();
        assert!(s.contains("abcdef12"));
        assert!(s.contains("gaming"));
    }
}
