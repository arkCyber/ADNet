//! Coordinator store — trait + in-memory implementation.
//!
//! [`Coordinator`] is the trait every mesh stack calls.
//! The default [`InMemoryCoordinator`] is a thin wrapper
//! around `parking_lot::RwLock<...>`; production deployments
//! wrap a SQLite-backed store. The trait surface is small
//! enough to back with either.
//!
//! ## Lifecycle
//!
//! ```text
//!   1. coordinator.create(network, name, policy, ...)
//!      → returns the fresh roster with the creator
//!        as the only member (and coordinator flag set).
//!
//!   2. coordinator.mint_invite(network, ttl)
//!      → returns an `InviteCode` the operator hands out.
//!
//!   3. coordinator.redeem(network, code, node_id, hostname)
//!      → on success: adds the new member to the roster,
//!        bumps the version, returns the new MeshMember.
//!
//!   4. coordinator.kick(network, node_id)
//!      → removes the member; bumps the version.
//!
//!   5. coordinator.request_join(network, node_id, hostname)
//!      → enqueues a JoinRequest. The operator then
//!        accepts or denies via the matching methods.
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use parking_lot::RwLock;

use a3net_types::{
    InviteCode, MeshMember, MeshMembership, MeshNetworkId, MeshPolicy, NodeId,
};

use crate::error::{CoordinatorError, CoordinatorResult};
use crate::request::{JoinRequest, JoinRequestId, JoinRequestStatus};

/// Maximum TTL for an invite code (rayfish default is 7d).
pub const MAX_INVITE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// Maximum number of pending join requests held by an
/// in-memory coordinator. The bound exists so a hostile
/// peer cannot flood the queue; operators can raise it
/// via [`CoordinatorConfig::max_requests`].
pub const MAX_REQUESTS: usize = 1024;

/// Maximum length of a free-form note attached to a
/// [`JoinRequest`].
pub const MAX_NOTE_LEN: usize = 256;

/// Coordinator configuration.
#[derive(Debug, Clone)]
pub struct CoordinatorConfig {
    pub default_invite_ttl: Duration,
    pub max_requests: usize,
    pub max_note_len: usize,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            default_invite_ttl: Duration::from_secs(7 * 24 * 3600),
            max_requests: MAX_REQUESTS,
            max_note_len: MAX_NOTE_LEN,
        }
    }
}

/// Snapshot of the coordinator's state for the CLI / status.
#[derive(Debug, Clone)]
pub struct CoordinatorSnapshot {
    pub networks: Vec<(MeshNetworkId, MeshPolicy, MeshMembership)>,
    pub pending_invites: Vec<(MeshNetworkId, InviteCode)>,
    pub pending_requests: Vec<JoinRequest>,
    pub resolved_requests: Vec<JoinRequest>,
}

/// The coordinator trait.
pub trait Coordinator: Send + Sync {
    /// Create a new mesh network with the given display
    /// name. The creator becomes the first member and the
    /// only coordinator. Returns the new roster.
    fn create(
        &self,
        network: MeshNetworkId,
        display_name: String,
        policy: MeshPolicy,
        creator: NodeId,
    ) -> CoordinatorResult<MeshMembership>;

    /// Current roster for `network`.
    fn roster(&self, network: &MeshNetworkId) -> Option<MeshMembership>;

    /// Mint an invite code with the configured TTL (or
    /// `ttl_override` if the operator specified one).
    fn mint_invite(
        &self,
        network: &MeshNetworkId,
        ttl_override: Option<Duration>,
    ) -> CoordinatorResult<InviteCode>;

    /// Redeem an invite code. On success, the new member
    /// is added to the roster and the code is marked
    /// redeemed.
    fn redeem(
        &self,
        network: &MeshNetworkId,
        code: &str,
        member_id: NodeId,
        hostname: String,
    ) -> CoordinatorResult<MeshMember>;

    /// Remove a member from the roster (coordinator-only
    /// in real life; the caller is expected to gate by
    /// their `is_coordinator` flag).
    fn kick(
        &self,
        network: &MeshNetworkId,
        member_id: &NodeId,
    ) -> CoordinatorResult<()>;

    /// Enqueue a join request. Returns the new request id.
    fn request_join(
        &self,
        network: &MeshNetworkId,
        member_id: NodeId,
        hostname: String,
        note: String,
    ) -> CoordinatorResult<JoinRequestId>;

    /// List pending join requests.
    fn pending_requests(&self, network: &MeshNetworkId) -> Vec<JoinRequest>;

    /// Approve a pending request. On success, the member
    /// is added to the roster.
    fn accept_request(
        &self,
        network: &MeshNetworkId,
        request_id: JoinRequestId,
    ) -> CoordinatorResult<MeshMember>;

    /// Deny a pending request. Returns the (now-denied)
    /// request.
    fn deny_request(
        &self,
        network: &MeshNetworkId,
        request_id: JoinRequestId,
    ) -> CoordinatorResult<JoinRequest>;

    /// Snapshot for diagnostics / status.
    fn snapshot(&self) -> CoordinatorSnapshot;
}

/// Default in-memory implementation.
#[derive(Clone)]
pub struct InMemoryCoordinator {
    inner: Arc<InMemoryCoordinatorInner>,
}

struct InMemoryCoordinatorInner {
    config: CoordinatorConfig,
    /// Per-network state.
    networks: RwLock<HashMap<MeshNetworkId, NetworkState>>,
    /// All pending invite codes, across networks.
    pending_invites: RwLock<HashMap<(MeshNetworkId, String), InviteCode>>,
    /// Pending join requests, keyed by request id.
    pending_requests: RwLock<HashMap<JoinRequestId, JoinRequest>>,
    /// Resolved (Approved / Denied) requests, kept for
    /// audit purposes.
    resolved_requests: RwLock<HashMap<JoinRequestId, JoinRequest>>,
}

struct NetworkState {
    display_name: String,
    policy: MeshPolicy,
    roster: MeshMembership,
}

impl InMemoryCoordinator {
    pub fn new(config: CoordinatorConfig) -> Self {
        Self {
            inner: Arc::new(InMemoryCoordinatorInner {
                config,
                networks: RwLock::new(HashMap::new()),
                pending_invites: RwLock::new(HashMap::new()),
                pending_requests: RwLock::new(HashMap::new()),
                resolved_requests: RwLock::new(HashMap::new()),
            }),
        }
    }

    fn check_host_available(
        &self,
        network: &MeshNetworkId,
        hostname: &str,
    ) -> CoordinatorResult<()> {
        let nets = self.inner.networks.read();
        let state = nets
            .get(network)
            .ok_or_else(|| CoordinatorError::InvalidRequestState {
                actual: "missing".into(),
                expected: "exists".into(),
            })?;
        if state.roster.members.iter().any(|m| m.hostname == hostname) {
            return Err(CoordinatorError::HostnameTaken {
                host: hostname.into(),
                network: state.display_name.clone(),
            });
        }
        Ok(())
    }

    fn check_not_already_member(
        &self,
        network: &MeshNetworkId,
        member_id: &NodeId,
    ) -> CoordinatorResult<()> {
        let nets = self.inner.networks.read();
        let state = nets
            .get(network)
            .ok_or_else(|| CoordinatorError::InvalidRequestState {
                actual: "missing".into(),
                expected: "exists".into(),
            })?;
        if state.roster.members.iter().any(|m| &m.node_id == member_id) {
            return Err(CoordinatorError::AlreadyMember {
                node_id_short: member_id.short().into(),
                network: state.display_name.clone(),
            });
        }
        Ok(())
    }
}

impl Coordinator for InMemoryCoordinator {
    fn create(
        &self,
        network: MeshNetworkId,
        display_name: String,
        policy: MeshPolicy,
        creator: NodeId,
    ) -> CoordinatorResult<MeshMembership> {
        let mut nets = self.inner.networks.write();
        if nets.contains_key(&network) {
            return Err(CoordinatorError::InvalidRequestState {
                actual: "exists".into(),
                expected: "missing".into(),
            });
        }
        let mut roster = MeshMembership::new_unsigned(network.clone(), vec![]);
        roster
            .members
            .push(MeshMember::new_coordinator(creator, &display_name));
        roster.bumped();
        let result = roster.clone();
        nets.insert(
            network,
            NetworkState {
                display_name,
                policy,
                roster,
            },
        );
        Ok(result)
    }

    fn roster(&self, network: &MeshNetworkId) -> Option<MeshMembership> {
        self.inner.networks.read().get(network).map(|s| s.roster.clone())
    }

    fn mint_invite(
        &self,
        network: &MeshNetworkId,
        ttl_override: Option<Duration>,
    ) -> CoordinatorResult<InviteCode> {
        let nets = self.inner.networks.read();
        let state = nets
            .get(network)
            .ok_or_else(|| CoordinatorError::InvalidRequestState {
                actual: "missing".into(),
                expected: "exists".into(),
            })?;
        if matches!(state.policy, MeshPolicy::Open) {
            return Err(CoordinatorError::NetworkIsOpen);
        }
        drop(nets);
        let ttl = ttl_override.unwrap_or(self.inner.config.default_invite_ttl);
        let ttl = ttl.min(MAX_INVITE_TTL);
        let code = InviteCode::new(network.clone(), ttl);
        self.inner
            .pending_invites
            .write()
            .insert((network.clone(), code.code.clone()), code.clone());
        Ok(code)
    }

    fn redeem(
        &self,
        network: &MeshNetworkId,
        code: &str,
        member_id: NodeId,
        hostname: String,
    ) -> CoordinatorResult<MeshMember> {
        // 1. Verify the code.
        let mut invites = self.inner.pending_invites.write();
        let invite = invites
            .get_mut(&(network.clone(), code.to_string()))
            .ok_or_else(|| CoordinatorError::UnknownInvite(code.to_string()))?;
        let now = Utc::now();
        if invite.is_expired(now) {
            return Err(CoordinatorError::InviteExpired(code.to_string()));
        }
        if invite.redeemed {
            return Err(CoordinatorError::InviteAlreadyRedeemed(code.to_string()));
        }
        invite.redeemed = true;
        drop(invites);

        // 2. Check hostname / member-id uniqueness.
        self.check_host_available(network, &hostname)?;
        self.check_not_already_member(network, &member_id)?;

        // 3. Add to roster.
        let mut nets = self.inner.networks.write();
        let state = nets
            .get_mut(network)
            .ok_or_else(|| CoordinatorError::InvalidRequestState {
                actual: "missing".into(),
                expected: "exists".into(),
            })?;
        let new_member = MeshMember::new_member(member_id, hostname);
        state.roster.members.push(new_member.clone());
        state.roster.bumped();
        Ok(new_member)
    }

    fn kick(
        &self,
        network: &MeshNetworkId,
        member_id: &NodeId,
    ) -> CoordinatorResult<()> {
        let mut nets = self.inner.networks.write();
        let state = nets
            .get_mut(network)
            .ok_or_else(|| CoordinatorError::InvalidRequestState {
                actual: "missing".into(),
                expected: "exists".into(),
            })?;
        let before = state.roster.members.len();
        state.roster.members.retain(|m| &m.node_id != member_id);
        if state.roster.members.len() == before {
            return Err(CoordinatorError::UnknownMember {
                node_id_short: member_id.short().into(),
                network: state.display_name.clone(),
            });
        }
        state.roster.bumped();
        Ok(())
    }

    fn request_join(
        &self,
        network: &MeshNetworkId,
        member_id: NodeId,
        hostname: String,
        note: String,
    ) -> CoordinatorResult<JoinRequestId> {
        // Verify the network exists *before* taking the
        // pending-requests write lock. This avoids an
        // unnecessary write lock contention when the
        // operator typos a network id.
        {
            let nets = self.inner.networks.read();
            if !nets.contains_key(network) {
                return Err(CoordinatorError::InvalidRequestState {
                    actual: "missing".into(),
                    expected: "exists".into(),
                });
            }
        }
        if note.len() > self.inner.config.max_note_len {
            return Err(CoordinatorError::InvalidRequestState {
                actual: format!("len {}", note.len()),
                expected: format!("<= {}", self.inner.config.max_note_len),
            });
        }
        // Capacity check + insert are performed under a
        // single write lock so concurrent callers cannot
        // exceed `max_requests`. The previous
        // read-then-write pattern was a TOCTOU race:
        // N callers at exactly `len == max - 1` could all
        // pass the read check and all insert.
        let mut pending = self.inner.pending_requests.write();
        if pending.len() >= self.inner.config.max_requests {
            return Err(CoordinatorError::RosterFull {
                max: self.inner.config.max_requests,
            });
        }
        let id = JoinRequestId::new();
        let req = JoinRequest {
            id,
            network: network.clone(),
            node_id: member_id,
            hostname,
            requested_at: Utc::now(),
            status: JoinRequestStatus::Pending,
            note,
        };
        pending.insert(id, req);
        Ok(id)
    }

    fn pending_requests(&self, network: &MeshNetworkId) -> Vec<JoinRequest> {
        self.inner
            .pending_requests
            .read()
            .values()
            .filter(|r| &r.network == network)
            .cloned()
            .collect()
    }

    fn accept_request(
        &self,
        network: &MeshNetworkId,
        request_id: JoinRequestId,
    ) -> CoordinatorResult<MeshMember> {
        let mut pending = self.inner.pending_requests.write();
        let req = pending
            .remove(&request_id)
            .ok_or(CoordinatorError::UnknownRequest(request_id.0))?;
        if &req.network != network {
            // Put it back to avoid losing it on a
            // network-id mismatch.
            pending.insert(request_id, req.clone());
            return Err(CoordinatorError::InvalidRequestState {
                actual: "wrong-network".into(),
                expected: network.to_string(),
            });
        }
        if !matches!(req.status, JoinRequestStatus::Pending) {
            return Err(CoordinatorError::InvalidRequestState {
                actual: req.status.to_string(),
                expected: JoinRequestStatus::Pending.to_string(),
            });
        }
        // Drop the read guard before mutating networks.
        drop(pending);
        let mut nets = self.inner.networks.write();
        let state = nets
            .get_mut(network)
            .ok_or_else(|| CoordinatorError::InvalidRequestState {
                actual: "missing".into(),
                expected: "exists".into(),
            })?;
        let new_member = MeshMember::new_member(req.node_id.clone(), req.hostname.clone());
        state.roster.members.push(new_member.clone());
        state.roster.bumped();
        // Record the resolved request for audit.
        let mut resolved = self.inner.resolved_requests.write();
        let mut audit = req.clone();
        audit.status = JoinRequestStatus::Approved;
        resolved.insert(request_id, audit);
        Ok(new_member)
    }

    fn deny_request(
        &self,
        network: &MeshNetworkId,
        request_id: JoinRequestId,
    ) -> CoordinatorResult<JoinRequest> {
        let mut pending = self.inner.pending_requests.write();
        let mut req = pending
            .remove(&request_id)
            .ok_or(CoordinatorError::UnknownRequest(request_id.0))?;
        if &req.network != network {
            pending.insert(request_id, req.clone());
            return Err(CoordinatorError::InvalidRequestState {
                actual: "wrong-network".into(),
                expected: network.to_string(),
            });
        }
        req.status = JoinRequestStatus::Denied;
        self.inner
            .resolved_requests
            .write()
            .insert(request_id, req.clone());
        Ok(req)
    }

    fn snapshot(&self) -> CoordinatorSnapshot {
        let nets = self.inner.networks.read();
        let invites = self.inner.pending_invites.read();
        let pending = self.inner.pending_requests.read();
        let resolved = self.inner.resolved_requests.read();
        let networks: Vec<_> = nets
            .iter()
            .map(|(nid, s)| (nid.clone(), s.policy, s.roster.clone()))
            .collect();
        let pending_invites: Vec<_> = invites
            .values()
            .cloned()
            .map(|c| (network_for(&nets, &c.network_id).unwrap_or_else(|| c.network_id.clone()), c))
            .collect();
        let pending_requests: Vec<_> = pending.values().cloned().collect();
        let resolved_requests: Vec<_> = resolved.values().cloned().collect();
        CoordinatorSnapshot {
            networks,
            pending_invites,
            pending_requests,
            resolved_requests,
        }
    }
}

fn network_for(
    nets: &HashMap<MeshNetworkId, NetworkState>,
    target: &MeshNetworkId,
) -> Option<MeshNetworkId> {
    if nets.contains_key(target) {
        Some(target.clone())
    } else {
        None
    }
}

// Silence the unused `VecDeque` import — kept around for the
// future request-ordering feature (FIFO pending list).
#[allow(dead_code)]
fn _ensure_vec_deque_in_scope() -> VecDeque<()> {
    VecDeque::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coord() -> InMemoryCoordinator {
        InMemoryCoordinator::new(CoordinatorConfig::default())
    }

    fn nid(seed: u8) -> MeshNetworkId {
        MeshNetworkId::from_bytes(&[seed; 32]).unwrap()
    }

    fn member(seed: u8) -> NodeId {
        NodeId::from_bytes(&[seed; 32]).unwrap()
    }

    #[test]
    fn create_then_roster_returns_one_member() {
        let c = coord();
        let r = c
            .create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        assert_eq!(r.members.len(), 1);
        assert_eq!(r.members[0].hostname, "gaming");
        assert!(r.members[0].is_coordinator);
    }

    #[test]
    fn create_twice_errors() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        let err = c
            .create(nid(1), "gaming".into(), MeshPolicy::Closed, member(8))
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::InvalidRequestState { .. }));
    }

    #[test]
    fn mint_invite_for_open_network_errors() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Open, member(7))
            .unwrap();
        let err = c.mint_invite(&nid(1), None).unwrap_err();
        assert!(matches!(err, CoordinatorError::NetworkIsOpen));
    }

    #[test]
    fn mint_invite_then_redeem_admits_member() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        let invite = c.mint_invite(&nid(1), None).unwrap();
        let new_id = member(8);
        let member = c
            .redeem(
                &nid(1),
                &invite.code,
                new_id.clone(),
                "alice".into(),
            )
            .unwrap();
        assert_eq!(member.node_id, new_id);
        assert_eq!(member.hostname, "alice");
        // Roster now has two members.
        let r = c.roster(&nid(1)).unwrap();
        assert_eq!(r.members.len(), 2);
    }

    #[test]
    fn redeem_with_unknown_code_errors() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        let err = c
            .redeem(&nid(1), "deadbeef", member(8), "alice".into())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::UnknownInvite(_)));
    }

    #[test]
    fn redeem_twice_errors() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        let invite = c.mint_invite(&nid(1), None).unwrap();
        c.redeem(&nid(1), &invite.code, member(8), "alice".into())
            .unwrap();
        let err = c
            .redeem(&nid(1), &invite.code, member(9), "bob".into())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::InviteAlreadyRedeemed(_)));
    }

    #[test]
    fn redeem_with_duplicate_hostname_errors() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        let i1 = c.mint_invite(&nid(1), None).unwrap();
        let i2 = c.mint_invite(&nid(1), None).unwrap();
        c.redeem(&nid(1), &i1.code, member(8), "alice".into())
            .unwrap();
        let err = c
            .redeem(&nid(1), &i2.code, member(9), "alice".into())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::HostnameTaken { .. }));
    }

    #[test]
    fn redeem_with_duplicate_node_id_errors() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        let i1 = c.mint_invite(&nid(1), None).unwrap();
        let i2 = c.mint_invite(&nid(1), None).unwrap();
        c.redeem(&nid(1), &i1.code, member(8), "alice".into())
            .unwrap();
        let err = c
            .redeem(&nid(1), &i2.code, member(8), "alice2".into())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::AlreadyMember { .. }));
    }

    #[test]
    fn kick_removes_member_and_bumps_version() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        let invite = c.mint_invite(&nid(1), None).unwrap();
        c.redeem(&nid(1), &invite.code, member(8), "alice".into())
            .unwrap();
        let v_before = c.roster(&nid(1)).unwrap().version;
        c.kick(&nid(1), &member(8)).unwrap();
        let r_after = c.roster(&nid(1)).unwrap();
        assert!(r_after.version > v_before);
        assert_eq!(r_after.members.len(), 1);
    }

    #[test]
    fn kick_unknown_member_errors() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        let err = c.kick(&nid(1), &member(99)).unwrap_err();
        assert!(matches!(err, CoordinatorError::UnknownMember { .. }));
        // Version did NOT bump because no actual mutation happened.
        let r = c.roster(&nid(1)).unwrap();
        assert_eq!(r.version, 2); // create() bumps to 2, no kick succeeded
    }

    #[test]
    fn kick_in_missing_network_errors() {
        let c = coord();
        let err = c.kick(&nid(99), &member(7)).unwrap_err();
        assert!(matches!(err, CoordinatorError::InvalidRequestState { .. }));
    }

    #[test]
    fn request_then_accept_admits_member() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        let req_id = c
            .request_join(&nid(1), member(8), "alice".into(), "hello".into())
            .unwrap();
        let pending = c.pending_requests(&nid(1));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, req_id);
        c.accept_request(&nid(1), req_id).unwrap();
        let r = c.roster(&nid(1)).unwrap();
        assert_eq!(r.members.len(), 2);
    }

    #[test]
    fn request_then_deny_records_audit() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        let req_id = c
            .request_join(&nid(1), member(8), "alice".into(), "hello".into())
            .unwrap();
        c.deny_request(&nid(1), req_id).unwrap();
        let snap = c.snapshot();
        assert!(snap.pending_requests.is_empty());
        assert_eq!(snap.resolved_requests.len(), 1);
        assert_eq!(
            snap.resolved_requests[0].status,
            JoinRequestStatus::Denied
        );
        // Roster unchanged.
        let r = c.roster(&nid(1)).unwrap();
        assert_eq!(r.members.len(), 1);
    }

    #[test]
    fn accept_unknown_request_errors() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        let err = c.accept_request(&nid(1), JoinRequestId::new()).unwrap_err();
        assert!(matches!(err, CoordinatorError::UnknownRequest(_)));
    }

    #[test]
    fn request_too_long_note_errors() {
        let c = InMemoryCoordinator::new(CoordinatorConfig {
            max_note_len: 4,
            ..CoordinatorConfig::default()
        });
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        let err = c
            .request_join(&nid(1), member(8), "alice".into(), "too long".into())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::InvalidRequestState { .. }));
    }

    #[test]
    fn request_capacity_enforced() {
        let c = InMemoryCoordinator::new(CoordinatorConfig {
            max_requests: 1,
            ..CoordinatorConfig::default()
        });
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        c.request_join(&nid(1), member(8), "alice".into(), "".into())
            .unwrap();
        let err = c
            .request_join(&nid(1), member(9), "bob".into(), "".into())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::RosterFull { .. }));
    }

    #[test]
    fn snapshot_includes_all_state() {
        let c = coord();
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();
        c.mint_invite(&nid(1), None).unwrap();
        c.request_join(&nid(1), member(8), "alice".into(), "".into())
            .unwrap();
        let snap = c.snapshot();
        assert_eq!(snap.networks.len(), 1);
        assert_eq!(snap.pending_invites.len(), 1);
        assert_eq!(snap.pending_requests.len(), 1);
        assert!(snap.resolved_requests.is_empty());
    }

    /// Regression: 16 concurrent `request_join` callers
    /// must not exceed `max_requests`. The previous
    /// read-then-write pattern was a TOCTOU race that
    /// could admit `n_threads` extra requests.
    #[test]
    fn request_capacity_holds_under_concurrency() {
        use std::sync::Arc;
        use std::thread;

        let cap = 4;
        let c = Arc::new(InMemoryCoordinator::new(CoordinatorConfig {
            max_requests: cap,
            ..CoordinatorConfig::default()
        }));
        c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
            .unwrap();

        // Spawn 16 threads, each racing to insert a request.
        let mut handles = Vec::new();
        for i in 0..16u8 {
            let c = c.clone();
            handles.push(thread::spawn(move || {
                c.request_join(&nid(1), member(i + 100), format!("h{i}"), "".into())
            }));
        }
        let mut ok = 0;
        let mut err = 0;
        for h in handles {
            match h.join().unwrap() {
                Ok(_) => ok += 1,
                Err(_) => err += 1,
            }
        }
        assert_eq!(
            ok, cap,
            "exactly {cap} requests must succeed; got {ok}"
        );
        assert_eq!(
            err,
            16 - cap,
            "the rest must fail with RosterFull; got {err}"
        );
    }
}
