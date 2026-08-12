//! Integration tests for `adnet-mesh-coordinator`.
//!
//! These mirror the user-visible CLI behaviour:
//!
//! - `ray create` → `Coordinator::create`
//! - `ray invite` → `Coordinator::mint_invite`
//! - `ray join <code>` → `Coordinator::redeem`
//! - `ray requests` → `Coordinator::pending_requests`
//! - `ray accept` / `ray deny` → `Coordinator::accept_request`
//!   / `Coordinator::deny_request`
//! - `ray kick` → `Coordinator::kick`

use adnet_mesh_coordinator::{
    Coordinator, CoordinatorConfig, CoordinatorError, InMemoryCoordinator, JoinRequestId,
    JoinRequestStatus,
};
use adnet_types::{MeshNetworkId, MeshPolicy, NodeId};

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
fn full_invite_then_join_lifecycle() {
    let c = coord();
    c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
        .unwrap();
    let invite = c.mint_invite(&nid(1), None).unwrap();
    let friend = member(11);
    let admitted = c
        .redeem(&nid(1), &invite.code, friend.clone(), "alice".into())
        .unwrap();
    assert_eq!(admitted.node_id, friend);
    assert_eq!(admitted.hostname, "alice");
    // Roster reflects the new admission.
    let roster = c.roster(&nid(1)).unwrap();
    assert_eq!(roster.members.len(), 2);
}

#[test]
fn full_request_then_accept_lifecycle() {
    let c = coord();
    c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
        .unwrap();
    let req_id = c
        .request_join(&nid(1), member(11), "alice".into(), "hi".into())
        .unwrap();
    let pending = c.pending_requests(&nid(1));
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, JoinRequestStatus::Pending);
    let admitted = c.accept_request(&nid(1), req_id).unwrap();
    assert_eq!(admitted.hostname, "alice");
    // Roster now has the new member.
    let r = c.roster(&nid(1)).unwrap();
    assert_eq!(r.members.len(), 2);
}

#[test]
fn full_request_then_deny_does_not_admit() {
    let c = coord();
    c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
        .unwrap();
    let req_id = c
        .request_join(&nid(1), member(11), "alice".into(), "".into())
        .unwrap();
    let denied = c.deny_request(&nid(1), req_id).unwrap();
    assert_eq!(denied.status, JoinRequestStatus::Denied);
    let r = c.roster(&nid(1)).unwrap();
    assert_eq!(r.members.len(), 1);
}

#[test]
fn kick_removes_and_bumps() {
    let c = coord();
    c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
        .unwrap();
    let invite = c.mint_invite(&nid(1), None).unwrap();
    c.redeem(&nid(1), &invite.code, member(8), "alice".into())
        .unwrap();
    let v1 = c.roster(&nid(1)).unwrap().version;
    c.kick(&nid(1), &member(8)).unwrap();
    let r = c.roster(&nid(1)).unwrap();
    assert!(r.version > v1);
    assert_eq!(r.members.len(), 1);
}

#[test]
fn open_network_rejects_invite_path() {
    let c = coord();
    c.create(nid(1), "gaming".into(), MeshPolicy::Open, member(7))
        .unwrap();
    let err = c.mint_invite(&nid(1), None).unwrap_err();
    assert!(matches!(err, CoordinatorError::NetworkIsOpen));
}

#[test]
fn accept_unknown_request_errors_typed() {
    let c = coord();
    c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
        .unwrap();
    let err = c.accept_request(&nid(1), JoinRequestId::new()).unwrap_err();
    assert!(matches!(err, CoordinatorError::UnknownRequest(_)));
}

#[test]
fn snapshot_reflects_mixed_state() {
    let c = coord();
    c.create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
        .unwrap();
    c.create(nid(2), "infra".into(), MeshPolicy::Closed, member(8))
        .unwrap();
    c.mint_invite(&nid(1), None).unwrap();
    let req_id = c
        .request_join(&nid(1), member(99), "alice".into(), "".into())
        .unwrap();
    c.deny_request(&nid(1), req_id).unwrap();
    let snap = c.snapshot();
    assert_eq!(snap.networks.len(), 2);
    assert_eq!(snap.pending_invites.len(), 1);
    assert!(snap.pending_requests.is_empty());
    assert_eq!(snap.resolved_requests.len(), 1);
}

#[test]
fn redeem_with_duplicate_hostname_is_typed_error() {
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
fn roster_is_cloneable_and_snapshot_safe() {
    let c = coord();
    let r = c
        .create(nid(1), "gaming".into(), MeshPolicy::Closed, member(7))
        .unwrap();
    let r2 = r.clone();
    assert_eq!(r.network_id, r2.network_id);
    assert_eq!(r.members.len(), r2.members.len());
}
