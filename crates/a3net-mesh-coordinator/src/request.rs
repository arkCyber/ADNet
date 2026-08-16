//! Live join-request queue.
//!
//! When a peer wants to join a closed network but does not
//! have an invite, they submit a `JoinRequest` to the
//! coordinator (via `ray join <room-id>`). The coordinator
//! enqueues it; the operator resolves the queue with
//! `Coordinator::accept_request` or
//! `Coordinator::deny_request`.
//!
//! ## State machine
//!
//! ```text
//!   Pending → Approved | Denied
//! ```
//!
//! `Approved` / `Denied` are terminal. The coordinator
//! drops them from the active queue after the operator
//! resolves them; the request is still recorded for
//! audit purposes via [`Coordinator::snapshot`].
//!
//! ## Identifier
//!
//! Each request gets a [`uuid::Uuid`] id. UUIDs are used so
//! the wire form is short (`JoinRequestId` is 36 chars) and
//! collision-free across restarts.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use a3net_types::{MeshNetworkId, NodeId};

/// Identifier for a single join request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JoinRequestId(pub uuid::Uuid);

impl JoinRequestId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for JoinRequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for JoinRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinRequestStatus {
    Pending,
    Approved,
    Denied,
}

impl fmt::Display for JoinRequestStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Denied => "denied",
        })
    }
}

/// One pending / resolved join request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub id: JoinRequestId,
    pub network: MeshNetworkId,
    pub node_id: NodeId,
    pub hostname: String,
    pub requested_at: DateTime<Utc>,
    pub status: JoinRequestStatus,
    /// Free-form human note from the requester. Bounded by
    /// the validator (256 chars in `CoordinatorConfig`).
    #[serde(default)]
    pub note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_request_id_distinct() {
        let a = JoinRequestId::new();
        let b = JoinRequestId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn join_request_id_default() {
        let a = JoinRequestId::default();
        let b = JoinRequestId::default();
        assert_ne!(a, b);
    }

    #[test]
    fn join_request_status_display() {
        assert_eq!(JoinRequestStatus::Pending.to_string(), "pending");
        assert_eq!(JoinRequestStatus::Approved.to_string(), "approved");
        assert_eq!(JoinRequestStatus::Denied.to_string(), "denied");
    }

    #[test]
    fn join_request_id_display() {
        let id = JoinRequestId::new();
        let s = id.to_string();
        // UUID v4 textual form is 36 chars.
        assert_eq!(s.len(), 36);
    }
}
