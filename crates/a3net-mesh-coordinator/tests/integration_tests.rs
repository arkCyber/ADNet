//! Comprehensive integration tests for `a3net-mesh-coordinator`.
//!
//! These tests cover:
//! - Cross-module interactions
//! - End-to-end workflows
//! - Edge cases and error conditions
//! - Concurrency scenarios
//! - Cryptographic signing and verification flows

use a3net_mesh_coordinator::{
    Coordinator, CoordinatorConfig, CoordinatorError, CoordinatorResult, CoordinatorSnapshot,
    InMemoryCoordinator, InMemoryPeerings, JoinRequestId, JoinRequestStatus,
    PeeringDirection, PeeringGrant, PeeringGrantId, PeeringGrantSigner,
    PeeringGrantVerifier, Peerings, PeeringsSnapshot, RosterSigner, RosterVerifier,
    StaticPubkeyRegistry, JoinRequest,
};
use a3net_types::{
    MeshMember, MeshMembership, MeshNetworkId, MeshPolicy, NodeId,
};
use chrono::Utc;
use std::time::Duration;

// ========== Helper functions ==========

fn nid(seed: u8) -> MeshNetworkId {
    MeshNetworkId::from_bytes(&[seed; 32]).unwrap()
}

fn node(seed: u8) -> NodeId {
    NodeId::from_bytes(&[seed; 32]).unwrap()
}

fn coord() -> InMemoryCoordinator {
    InMemoryCoordinator::new(CoordinatorConfig::default())
}

// ========== Cross-module integration tests ==========

/// Test that roster signing and verification works end-to-end
#[test]
fn roster_sign_verify_e2e() {
    let signer = RosterSigner::generate();
    let pk = signer.public_key();

    // Create a roster with multiple members
    let network = nid(1);
    let coordinator = MeshMember::new_coordinator(node(7), "admin");
    let member1 = MeshMember::new_member(node(8), "alice");
    let member2 = MeshMember::new_member(node(9), "bob");

    let roster = MeshMembership::new_unsigned(network, vec![coordinator, member1, member2]);
    let signed = signer.sign(roster.clone()).unwrap();

    // Verify with correct pubkey
    RosterVerifier::new()
        .verify(&signed, &pk)
        .expect("should verify with correct key");

    // Verify fails with wrong pubkey
    let wrong_signer = RosterSigner::generate();
    let err = RosterVerifier::new()
        .verify(&signed, &wrong_signer.public_key())
        .unwrap_err();
    assert!(matches!(err, CoordinatorError::RosterSignatureInvalid(_)));
}

/// Test that peering grant signing and verification works end-to-end
#[test]
fn peering_grant_sign_verify_e2e() {
    let signer = PeeringGrantSigner::generate();
    let pk = signer.public_key();

    // Create and sign a peering grant
    let mut reg = StaticPubkeyRegistry::new();
    reg.register(nid(1), pk);

    let grant = PeeringGrant::new_unsigned(
        nid(1),
        nid(2),
        node(7),
        Duration::from_secs(3600),
    )
    .unwrap();

    let signed = signer.sign(grant).unwrap();
    PeeringGrantVerifier::new()
        .verify(&signed, &reg, Utc::now())
        .expect("should verify with correct registry");
}

/// Test that peering grants integrate with InMemoryPeerings
#[test]
fn peering_grants_with_store() {
    let peerings = InMemoryPeerings::new();
    let signer = PeeringGrantSigner::generate();
    let pk = signer.public_key();

    let mut reg = StaticPubkeyRegistry::new();
    reg.register(nid(1), pk);

    // Issue a grant
    let grant = PeeringGrant::new_unsigned(
        nid(1),
        nid(2),
        node(7),
        Duration::from_secs(3600),
    )
    .unwrap();

    let grant_id = grant.grant_id.clone();
    peerings.issue(grant).unwrap();

    // Sign it
    let signed = signer.sign(peerings.get(&grant_id).unwrap()).unwrap();
    peerings.attach_signature(&grant_id, signed.signature.clone()).unwrap();

    // Verify it's stored correctly
    let retrieved = peerings.get(&grant_id).unwrap();
    assert_eq!(retrieved.signature, signed.signature);

    // List live grants
    let live = peerings.list_live(Utc::now());
    assert_eq!(live.len(), 1);
}

/// Test that coordinator integrates with roster signing
#[test]
fn coordinator_with_roster_signing() {
    let c = coord();
    let signer = RosterSigner::generate();
    let pk = signer.public_key();

    // Create a network
    let roster = c
        .create(nid(1), "test".into(), MeshPolicy::Closed, node(7))
        .unwrap();

    // Sign the roster
    let signed = signer.sign(roster.clone()).unwrap();
    RosterVerifier::new()
        .verify(&signed, &pk)
        .expect("signed roster should verify");

    // Add a member and sign again
    let invite = c.mint_invite(&nid(1), None).unwrap();
    c.redeem(&nid(1), &invite.code, node(8), "alice".into())
        .unwrap();

    let updated = c.roster(&nid(1)).unwrap();
    let signed_updated = signer.sign(updated).unwrap();
    RosterVerifier::new()
        .verify(&signed_updated, &pk)
        .expect("updated roster should verify");
}

// ========== Full workflow tests ==========

/// Test complete network lifecycle
#[test]
fn full_network_lifecycle() {
    let c = coord();

    // 1. Create network
    let roster = c
        .create(nid(1), "home".into(), MeshPolicy::Closed, node(7))
        .unwrap();
    assert_eq!(roster.members.len(), 1);
    assert!(roster.members[0].is_coordinator);

    // 2. Mint invite and add member
    let invite = c.mint_invite(&nid(1), None).unwrap();
    let member = c
        .redeem(&nid(1), &invite.code, node(8), "alice".into())
        .unwrap();
    assert_eq!(member.hostname, "alice");

    // 3. Another member via request
    let req_id = c
        .request_join(&nid(1), node(9), "bob".into(), "please".into())
        .unwrap();
    c.accept_request(&nid(1), req_id).unwrap();

    // 4. Verify roster
    let final_roster = c.roster(&nid(1)).unwrap();
    assert_eq!(final_roster.members.len(), 3);

    // 5. Kick one member
    c.kick(&nid(1), &node(8)).unwrap();
    let after_kick = c.roster(&nid(1)).unwrap();
    assert_eq!(after_kick.members.len(), 2);

    // 6. Snapshot shows correct state
    let snap = c.snapshot();
    assert_eq!(snap.networks.len(), 1);
    // Invite remains in pending_invites but marked as redeemed (for audit)
    // Pending requests should be empty after accept
    assert!(snap.pending_requests.is_empty());
}

/// Test multiple networks with crossing operations
#[test]
fn multiple_networks_cross_operations() {
    let c = coord();

    // Create two networks
    c.create(nid(1), "net1".into(), MeshPolicy::Closed, node(1))
        .unwrap();
    c.create(nid(2), "net2".into(), MeshPolicy::Closed, node(2))
        .unwrap();

    // Add members to network 1
    let i1 = c.mint_invite(&nid(1), None).unwrap();
    c.redeem(&nid(1), &i1.code, node(10), "alice".into())
        .unwrap();

    // Add members to network 2
    let i2 = c.mint_invite(&nid(2), None).unwrap();
    c.redeem(&nid(2), &i2.code, node(20), "bob".into())
        .unwrap();

    // Create requests in network 1
    c.request_join(&nid(1), node(11), "charlie".into(), "hi".into())
        .unwrap();

    // Verify isolation
    let r1 = c.roster(&nid(1)).unwrap();
    let r2 = c.roster(&nid(2)).unwrap();

    assert_eq!(r1.network_id, nid(1));
    assert_eq!(r2.network_id, nid(2));
    assert!(r1.members.iter().any(|m| m.hostname == "alice"));
    assert!(r2.members.iter().any(|m| m.hostname == "bob"));
    assert!(!r1.members.iter().any(|m| m.hostname == "bob"));
    assert!(!r2.members.iter().any(|m| m.hostname == "alice"));
}

/// Test peering grant lifecycle with multiple meshes
#[test]
fn peering_grant_multi_mesh_lifecycle() {
    let peerings_a = InMemoryPeerings::new();
    let peerings_b = InMemoryPeerings::new();

    let signer_a = PeeringGrantSigner::generate();
    let signer_b = PeeringGrantSigner::generate();

    let mut reg = StaticPubkeyRegistry::new();
    reg.register(nid(1), signer_a.public_key());
    reg.register(nid(2), signer_b.public_key());

    // Mesh A grants peering to Mesh B
    let grant_a_to_b = PeeringGrant::new_unsigned(
        nid(1),
        nid(2),
        node(7),
        Duration::from_secs(3600),
    )
    .unwrap();
    let grant_a_id = grant_a_to_b.grant_id.clone();

    peerings_a.issue(grant_a_to_b).unwrap();
    let signed = signer_a.sign(peerings_a.get(&grant_a_id).unwrap()).unwrap();
    peerings_a.attach_signature(&grant_a_id, signed.signature.clone()).unwrap();

    // Verify from B's perspective
    PeeringGrantVerifier::new()
        .verify(&peerings_a.get(&grant_a_id).unwrap(), &reg, Utc::now())
        .expect("grant from A should verify for B");

    // Mesh B grants peering to Mesh A
    let grant_b_to_a = PeeringGrant::new_unsigned(
        nid(2),
        nid(1),
        node(8),
        Duration::from_secs(3600),
    )
    .unwrap();
    let grant_b_id = grant_b_to_a.grant_id.clone();

    peerings_b.issue(grant_b_to_a).unwrap();
    let signed = signer_b.sign(peerings_b.get(&grant_b_id).unwrap()).unwrap();
    peerings_b.attach_signature(&grant_b_id, signed.signature.clone()).unwrap();

    // Verify from A's perspective
    PeeringGrantVerifier::new()
        .verify(&peerings_b.get(&grant_b_id).unwrap(), &reg, Utc::now())
        .expect("grant from B should verify for A");

    // List all live grants
    let live_a = peerings_a.list_live(Utc::now());
    let live_b = peerings_b.list_live(Utc::now());
    assert_eq!(live_a.len(), 1);
    assert_eq!(live_b.len(), 1);

    // Revoke A's grant
    peerings_a.revoke(&grant_a_id).unwrap();
    let live_a_after = peerings_a.list_live(Utc::now());
    assert!(live_a_after.is_empty());
}

// ========== Error handling tests ==========

/// Test error propagation across modules
#[test]
fn error_propagation_coordinator_to_signing() {
    let c = coord();
    let signer = RosterSigner::generate();
    let pk = signer.public_key();

    // Create and sign a roster
    let roster = c
        .create(nid(1), "test".into(), MeshPolicy::Closed, node(7))
        .unwrap();
    let signed = signer.sign(roster.clone()).unwrap();

    // Verify works
    RosterVerifier::new()
        .verify(&signed, &pk)
        .expect("initial roster should verify");

    // Tamper with roster - verification should fail
    let mut tampered = c.roster(&nid(1)).unwrap();
    tampered.version += 1;
    let err = RosterVerifier::new()
        .verify(&tampered, &pk)
        .unwrap_err();
    assert!(matches!(err, CoordinatorError::RosterSignatureInvalid(_)));
}

/// Test that expired peering grants are rejected
#[test]
fn expired_peering_grant_rejected() {
    let signer = PeeringGrantSigner::generate();
    let pk = signer.public_key();

    let mut reg = StaticPubkeyRegistry::new();
    reg.register(nid(1), pk);

    let grant = PeeringGrant::new_unsigned(
        nid(1),
        nid(2),
        node(7),
        Duration::from_secs(1), // Very short TTL
    )
    .unwrap();

    let signed = signer.sign(grant.clone()).unwrap();

    // Verify immediately - should pass
    PeeringGrantVerifier::new()
        .verify(&signed, &reg, Utc::now())
        .expect("fresh grant should verify");

    // Wait and verify after expiry
    std::thread::sleep(Duration::from_secs(2));
    let err = PeeringGrantVerifier::new()
        .verify(&signed, &reg, Utc::now())
        .unwrap_err();
    assert!(matches!(err, CoordinatorError::PeeringExpired { .. }));
}

// ========== Concurrency tests ==========

/// Test concurrent peering operations
#[test]
fn concurrent_peering_operations() {
    use std::sync::Arc;
    use std::thread;

    let peerings = Arc::new(InMemoryPeerings::new());
    let signer = PeeringGrantSigner::generate();
    let pk = signer.public_key();

    let mut reg = StaticPubkeyRegistry::new();
    reg.register(nid(99), pk); // Register a single network ID

    // Spawn multiple threads issuing grants
    let mut handles = vec![];
    for i in 0..8 {
        let peerings = peerings.clone();
        let signer = signer.clone();
        let reg_clone = reg.clone();

        handles.push(thread::spawn(move || {
            // Use the same source network (nid(99)) for all grants
            let grant = PeeringGrant::new_unsigned(
                nid(99), // Same source for all
                nid(i + 10),
                node(7),
                Duration::from_secs(3600),
            )
            .unwrap();

            let grant_id = grant.grant_id.clone();
            peerings.issue(grant).unwrap();

            let signed = signer.sign(peerings.get(&grant_id).unwrap()).unwrap();
            peerings
                .attach_signature(&grant_id, signed.signature.clone())
                .unwrap();

            // Verify the grant
            PeeringGrantVerifier::new()
                .verify(&peerings.get(&grant_id).unwrap(), &reg_clone, Utc::now())
                .expect("grant should verify");
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let snap = peerings.snapshot();
    assert_eq!(snap.grants.len(), 8);
}

/// Test concurrent coordinator operations
#[test]
fn concurrent_coordinator_operations() {
    use std::sync::Arc;
    use std::thread;

    let c = Arc::new(coord());
    c.create(nid(1), "test".into(), MeshPolicy::Closed, node(7))
        .unwrap();

    // Spawn threads making various requests
    let mut handles = vec![];

    // Thread 1: Mint invites
    let c1 = c.clone();
    handles.push(thread::spawn(move || {
        for _ in 0..10 {
            c1.mint_invite(&nid(1), None).unwrap();
        }
    }));

    // Thread 2: Request joins
    let c2 = c.clone();
    handles.push(thread::spawn(move || {
        for i in 0..5 {
            c2.request_join(&nid(1), node(100 + i), format!("req{i}"), "".into())
                .unwrap();
        }
    }));

    // Thread 3: Read roster
    let c3 = c.clone();
    handles.push(thread::spawn(move || {
        for _ in 0..10 {
            let _ = c3.roster(&nid(1));
        }
    }));

    // Thread 4: Snapshot
    let c4 = c.clone();
    handles.push(thread::spawn(move || {
        for _ in 0..5 {
            let _ = c4.snapshot();
        }
    }));

    for h in handles {
        h.join().unwrap();
    }

    // Verify state
    let snap = c.snapshot();
    assert_eq!(snap.pending_invites.len(), 10);
    assert_eq!(snap.pending_requests.len(), 5);
}

// ========== Invariant tests ==========

/// Test that roster version always increases
#[test]
fn roster_version_monotonic() {
    let c = coord();

    let roster = c
        .create(nid(1), "test".into(), MeshPolicy::Closed, node(7))
        .unwrap();
    let mut last_version = roster.version;

    // Add members
    for i in 0..5 {
        let invite = c.mint_invite(&nid(1), None).unwrap();
        c.redeem(&nid(1), &invite.code, node(10 + i), format!("m{i}"))
            .unwrap();
        let roster = c.roster(&nid(1)).unwrap();
        assert!(roster.version > last_version);
        last_version = roster.version;
    }

    // Kick members
    for i in 0..5 {
        c.kick(&nid(1), &node(10 + i)).unwrap();
        let roster = c.roster(&nid(1)).unwrap();
        assert!(roster.version > last_version);
        last_version = roster.version;
    }
}

/// Test that invite codes are single-use
#[test]
fn invite_code_single_use() {
    let c = coord();
    c.create(nid(1), "test".into(), MeshPolicy::Closed, node(7))
        .unwrap();

    let invite = c.mint_invite(&nid(1), None).unwrap();

    // First redemption succeeds
    c.redeem(&nid(1), &invite.code, node(10), "alice".into())
        .unwrap();

    // Second redemption fails
    let err = c
        .redeem(&nid(1), &invite.code, node(11), "bob".into())
        .unwrap_err();
    assert!(matches!(err, CoordinatorError::InviteAlreadyRedeemed(_)));
}

/// Test that request status transitions are correct
#[test]
fn request_status_transitions() {
    let c = coord();
    c.create(nid(1), "test".into(), MeshPolicy::Closed, node(7))
        .unwrap();

    let req_id = c
        .request_join(&nid(1), node(10), "alice".into(), "hi".into())
        .unwrap();

    // Check pending
    let pending = c.pending_requests(&nid(1));
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, JoinRequestStatus::Pending);

    // Accept
    c.accept_request(&nid(1), req_id).unwrap();
    let snap = c.snapshot();
    assert!(snap.pending_requests.is_empty());
    assert_eq!(snap.resolved_requests.len(), 1);
    assert_eq!(
        snap.resolved_requests[0].status,
        JoinRequestStatus::Approved
    );
}

/// Test that hostname uniqueness is enforced per network
#[test]
fn hostname_uniqueness_per_network() {
    let c = coord();
    c.create(nid(1), "net1".into(), MeshPolicy::Closed, node(1))
        .unwrap();
    c.create(nid(2), "net2".into(), MeshPolicy::Closed, node(2))
        .unwrap();

    // Add to network 1
    let i1 = c.mint_invite(&nid(1), None).unwrap();
    c.redeem(&nid(1), &i1.code, node(10), "alice".into())
        .unwrap();

    // Same hostname can be added to network 2
    let i2 = c.mint_invite(&nid(2), None).unwrap();
    c.redeem(&nid(2), &i2.code, node(11), "alice".into())
        .unwrap();

    // Same hostname cannot be added again to network 1
    let i3 = c.mint_invite(&nid(1), None).unwrap();
    let err = c
        .redeem(&nid(1), &i3.code, node(12), "alice".into())
        .unwrap_err();
    assert!(matches!(err, CoordinatorError::HostnameTaken { .. }));
}

/// Test that member uniqueness is enforced per network
#[test]
fn member_uniqueness_per_network() {
    let c = coord();
    c.create(nid(1), "net1".into(), MeshPolicy::Closed, node(1))
        .unwrap();

    let i1 = c.mint_invite(&nid(1), None).unwrap();
    c.redeem(&nid(1), &i1.code, node(10), "alice".into())
        .unwrap();

    // Same node cannot be added again
    let i2 = c.mint_invite(&nid(1), None).unwrap();
    let err = c
        .redeem(&nid(1), &i2.code, node(10), "bob".into())
        .unwrap_err();
    assert!(matches!(err, CoordinatorError::AlreadyMember { .. }));
}

// ========== Snapshot consistency tests ==========

/// Test that snapshots are consistent
#[test]
fn snapshot_consistency() {
    let c = coord();
    c.create(nid(1), "test".into(), MeshPolicy::Closed, node(7))
        .unwrap();

    // Add some state
    c.mint_invite(&nid(1), None).unwrap();
    c.mint_invite(&nid(1), None).unwrap();
    c.request_join(&nid(1), node(10), "alice".into(), "".into())
        .unwrap();

    // Take multiple snapshots - they should be consistent
    let snap1 = c.snapshot();
    let snap2 = c.snapshot();
    let snap3 = c.snapshot();

    assert_eq!(snap1.networks.len(), snap2.networks.len());
    assert_eq!(snap2.networks.len(), snap3.networks.len());
    assert_eq!(snap1.pending_invites.len(), snap2.pending_invites.len());
    assert_eq!(snap2.pending_invites.len(), snap3.pending_invites.len());
    assert_eq!(snap1.pending_requests.len(), snap2.pending_requests.len());
    assert_eq!(snap2.pending_requests.len(), snap3.pending_requests.len());
}

// ========== Boundary condition tests ==========

/// Test with maximum capacity
#[test]
fn maximum_capacity_handling() {
    let c = InMemoryCoordinator::new(CoordinatorConfig {
        max_requests: 1,
        ..CoordinatorConfig::default()
    });
    c.create(nid(1), "test".into(), MeshPolicy::Closed, node(7))
        .unwrap();

    // First request succeeds
    c.request_join(&nid(1), node(10), "alice".into(), "".into())
        .unwrap();

    // Second request fails
    let err = c
        .request_join(&nid(1), node(11), "bob".into(), "".into())
        .unwrap_err();
    assert!(matches!(err, CoordinatorError::RosterFull { .. }));
}

/// Test with minimum TTL invite (should work)
#[test]
fn minimum_ttl_invite() {
    let c = coord();
    c.create(nid(1), "test".into(), MeshPolicy::Closed, node(7))
        .unwrap();

    // Mint with very short TTL (1 second should be enough to redeem)
    let invite = c.mint_invite(&nid(1), Some(Duration::from_secs(1))).unwrap();

    // Redeem should work within 1 second
    let member = c
        .redeem(&nid(1), &invite.code, node(10), "alice".into())
        .unwrap();
    assert_eq!(member.hostname, "alice");
}

/// Test with maximum note length
#[test]
fn maximum_note_length() {
    let c = InMemoryCoordinator::new(CoordinatorConfig {
        max_note_len: 256,
        ..CoordinatorConfig::default()
    });
    c.create(nid(1), "test".into(), MeshPolicy::Closed, node(7))
        .unwrap();

    // Exactly 256 chars should succeed
    let note = "x".repeat(256);
    c.request_join(&nid(1), node(10), "alice".into(), note)
        .unwrap();

    // 257 chars should fail
    let too_long = "x".repeat(257);
    let err = c
        .request_join(&nid(1), node(11), "bob".into(), too_long)
        .unwrap_err();
    assert!(matches!(err, CoordinatorError::InvalidRequestState { .. }));
}

// ========== Security-related tests ==========

/// Test that tampering with peering grants is detected
#[test]
fn peering_grant_tamper_detection() {
    let signer = PeeringGrantSigner::generate();
    let pk = signer.public_key();

    let mut reg = StaticPubkeyRegistry::new();
    reg.register(nid(1), pk);

    let grant = PeeringGrant::new_unsigned(
        nid(1),
        nid(2),
        node(7),
        Duration::from_secs(3600),
    )
    .unwrap();

    let signed = signer.sign(grant).unwrap();

    // Test tamper with target
    let mut tampered = signed.clone();
    tampered.target = nid(3);
    let err = PeeringGrantVerifier::new()
        .verify(&tampered, &reg, Utc::now())
        .unwrap_err();
    // Either signature invalid or unknown coordinator (if target not in registry)
    assert!(
        matches!(err, CoordinatorError::PeeringSignatureInvalid(_))
        || matches!(err, CoordinatorError::PeeringUnknownCoordinator(_))
    );

    // Test tamper with source (nid(4) not in registry, so will get UnknownCoordinator)
    let mut tampered = signed.clone();
    tampered.source = nid(4);
    let err = PeeringGrantVerifier::new()
        .verify(&tampered, &reg, Utc::now())
        .unwrap_err();
    assert!(matches!(err, CoordinatorError::PeeringUnknownCoordinator(_)));

    // Test tamper with grantor
    let mut tampered = signed.clone();
    tampered.grantor = node(99);
    let err = PeeringGrantVerifier::new()
        .verify(&tampered, &reg, Utc::now())
        .unwrap_err();
    assert!(matches!(err, CoordinatorError::PeeringSignatureInvalid(_)));

    // Test tamper with cost
    let mut tampered = signed.clone();
    tampered.cost = 99;
    let err = PeeringGrantVerifier::new()
        .verify(&tampered, &reg, Utc::now())
        .unwrap_err();
    assert!(matches!(err, CoordinatorError::PeeringSignatureInvalid(_)));
}

/// Test that roster tampering is detected
#[test]
fn roster_tamper_detection() {
    let signer = RosterSigner::generate();
    let pk = signer.public_key();

    let network = nid(1);
    let roster = MeshMembership::new_unsigned(network.clone(), vec![
        MeshMember::new_coordinator(node(7), "admin"),
        MeshMember::new_member(node(8), "alice"),
    ]);

    let signed = signer.sign(roster).unwrap();

    // Verify initial
    RosterVerifier::new()
        .verify(&signed, &pk)
        .expect("initial should verify");

    // Tamper with members
    let mut tampered = signed.clone();
    tampered.members.push(MeshMember::new_member(node(99), "mallory"));

    let err = RosterVerifier::new()
        .verify(&tampered, &pk)
        .unwrap_err();
    assert!(matches!(err, CoordinatorError::RosterSignatureInvalid(_)));
}

// ========== Edge case: empty and boundary values ==========

/// Test with empty hostname
#[test]
fn empty_hostname() {
    let c = coord();
    c.create(nid(1), "test".into(), MeshPolicy::Closed, node(7))
        .unwrap();

    let invite = c.mint_invite(&nid(1), None).unwrap();
    let member = c
        .redeem(&nid(1), &invite.code, node(10), "".into())
        .unwrap();
    assert_eq!(member.hostname, "");
}

/// Test that network policy is enforced
#[test]
fn open_network_invite_rejected() {
    let c = coord();
    c.create(nid(1), "open".into(), MeshPolicy::Open, node(7))
        .unwrap();

    // Mint invite should fail for open networks
    let err = c.mint_invite(&nid(1), None).unwrap_err();
    assert!(matches!(err, CoordinatorError::NetworkIsOpen));
}

// ========== Regression tests ==========

/// Regression: TOCTOU in request_join capacity check
#[test]
fn request_join_toctou_regression() {
    use std::sync::Arc;
    use std::thread;

    let c = Arc::new(InMemoryCoordinator::new(CoordinatorConfig {
        max_requests: 2,
        ..CoordinatorConfig::default()
    }));
    c.create(nid(1), "test".into(), MeshPolicy::Closed, node(7))
        .unwrap();

    // Spawn many threads trying to request_join
    let mut handles = vec![];
    for i in 0..20 {
        let c = c.clone();
        handles.push(thread::spawn(move || {
            c.request_join(&nid(1), node(100 + i), format!("h{i}"), "".into())
        }));
    }

    let mut success = 0;
    let mut failure = 0;
    for h in handles {
        match h.join().unwrap() {
            Ok(_) => success += 1,
            Err(_) => failure += 1,
        }
    }

    // Exactly max_requests should succeed
    assert_eq!(success, 2, "exactly 2 requests should succeed");
    assert_eq!(failure, 18, "18 requests should fail");
}

/// Test that peerings store correctly handles concurrent issues
#[test]
fn peering_concurrent_idempotency() {
    let peerings = InMemoryPeerings::new();

    let grant = PeeringGrant::new_unsigned(
        nid(1),
        nid(2),
        node(7),
        Duration::from_secs(3600),
    )
    .unwrap();

    // Issue same grant multiple times (should be idempotent)
    peerings.issue(grant.clone()).unwrap();
    peerings.issue(grant.clone()).unwrap();
    peerings.issue(grant.clone()).unwrap();

    // Should only be stored once
    let snap = peerings.snapshot();
    assert_eq!(snap.grants.len(), 1);
}

// ========== API surface tests ==========

/// Test that all public exports are usable
#[test]
fn public_api_exports() {
    // error module
    let _: CoordinatorResult<i32> = Ok(42);
    let _: CoordinatorError = CoordinatorError::NetworkIsOpen;

    // request module
    let _: JoinRequestId = JoinRequestId::new();
    let _: JoinRequestStatus = JoinRequestStatus::Pending;
    let _join_request = JoinRequest {
        id: JoinRequestId::new(),
        network: nid(1),
        node_id: node(1),
        hostname: "test".into(),
        requested_at: Utc::now(),
        status: JoinRequestStatus::Pending,
        note: "".into(),
    };

    // peering module
    let _: PeeringDirection = PeeringDirection::Bidirectional;
    let _: PeeringGrantId = PeeringGrantId::new();
    let _: PeeringsSnapshot = PeeringsSnapshot { grants: vec![] };
    let _peerings: InMemoryPeerings = InMemoryPeerings::new();

    // peering_sign module
    let _: StaticPubkeyRegistry = StaticPubkeyRegistry::new();
    let _: PeeringGrantVerifier = PeeringGrantVerifier::new();

    // roster_sign module
    let _: RosterVerifier = RosterVerifier::new();

    // store module
    let _: CoordinatorConfig = CoordinatorConfig::default();
    let _: CoordinatorSnapshot = CoordinatorSnapshot {
        networks: vec![],
        pending_invites: vec![],
        pending_requests: vec![],
        resolved_requests: vec![],
    };
    let _: InMemoryCoordinator = InMemoryCoordinator::new(CoordinatorConfig::default());
}
