//! Comprehensive integration tests for the a3net-pairing crate.
//!
//! These tests cover the full pairing workflow from invitation creation
//! through successful pairing and device management.

use a3net_identity::wallet::Wallet;
use a3net_pairing::capability::{Capability, CapabilitySet};
use a3net_pairing::code::InvitationCode;
use a3net_pairing::error::{PairingError, PairingResult};
use a3net_pairing::invitation::{InvitationPayload, SignedInvitation};
use a3net_pairing::store::{TrustedDeviceStore, TrustedDeviceStoreConfig};
use a3net_pairing::transport_identity::{
    derive_credential_id, pairing_invitation_digest, verify_pairing_request,
    verify_pairing_response, Ed25519Signer, Nonce32, PairingRequest, PairingRequestBuilder,
    PairingResponse, PairingResponseBuilder,
};
use a3net_pairing::trusted_device::{TrustedDeviceRecord, TrustedDeviceRole, TrustedDeviceStatus};
use a3net_pairing::wire::PairingInvitation;
use a3net_pairing::Wallet as WalletTrait;
use a3net_types::node::NodeId;
use tempfile::TempDir;

fn tmp_store() -> (TrustedDeviceStore, TempDir) {
    let tmp = TempDir::new().unwrap();
    let config = TrustedDeviceStoreConfig {
        path: tmp.path().join("devices.jsonl"),
        ..Default::default()
    };
    let store = TrustedDeviceStore::open(config).unwrap();
    (store, tmp)
}

fn node_id_from_bytes(bytes: &[u8; 32]) -> NodeId {
    NodeId::from_bytes(bytes).unwrap()
}

// ─────────────────────────────────────────────────────────────────
// Full pairing ceremony tests
// ─────────────────────────────────────────────────────────────────

#[test]
fn full_pairing_ceremony_basic() {
    // Simulate the complete pairing flow:
    // 1. Issuer creates an invitation
    // 2. Invitee creates a pairing request
    // 3. Issuer verifies request and creates response
    // 4. Invitee verifies response
    // 5. Both sides store the trusted device

    let issuer_signer = Ed25519Signer::generate();
    let issuer_node_id = node_id_from_bytes(&issuer_signer.public_key());
    let issuer_wallet = Wallet::generate();

    let invitee_signer = Ed25519Signer::generate();
    let invitee_node_id = node_id_from_bytes(&invitee_signer.public_key());

    // Step 1: Issuer creates invitation
    let invitation = SignedInvitation::create(
        &issuer_node_id,
        &issuer_wallet,
        CapabilitySet::from_names(["chat", "files.read"]),
        900, // 15 min TTL
        Some("Test Device".into()),
    )
    .unwrap();

    // Verify invitation
    invitation
        .verify(chrono::Utc::now().timestamp())
        .unwrap();

    // Step 2: Invitee derives credential ID and creates pairing request
    let credential_id = invitation.credential_id(&invitee_node_id).unwrap();

    let pairing_request = PairingRequestBuilder {
        credential_id,
        node_id: &invitee_node_id,
        transport_pubkey: &invitee_signer.public_key(),
        requested_capabilities: CapabilitySet::from_names(["chat"]),
        ttl_seconds: 300,
    }
    .build(&invitee_signer)
    .unwrap();

    // Step 3: Issuer verifies request
    verify_pairing_request(&pairing_request, chrono::Utc::now().timestamp()).unwrap();

    // Step 4: Issuer creates response
    let response = PairingResponseBuilder {
        request: &pairing_request,
        issuer_node_id: &issuer_node_id,
        issuer_pubkey: &issuer_signer.public_key(),
        granted_capabilities: CapabilitySet::from_names(["chat"]),
        ttl_seconds: 86400 * 30,
        issuer_wallet: &issuer_wallet,
    }
    .build()
    .unwrap();

    // Step 5: Invitee verifies response
    verify_pairing_response(&response, &pairing_request, chrono::Utc::now().timestamp()).unwrap();
}

#[test]
fn full_pairing_ceremony_with_capability_filtering() {
    let issuer_signer = Ed25519Signer::generate();
    let issuer_node_id = node_id_from_bytes(&issuer_signer.public_key());
    let issuer_wallet = Wallet::generate();

    let invitee_signer = Ed25519Signer::generate();
    let invitee_node_id = node_id_from_bytes(&invitee_signer.public_key());

    // Issuer offers multiple capabilities
    let invitation = SignedInvitation::create(
        &issuer_node_id,
        &issuer_wallet,
        CapabilitySet::from_names(["chat", "files.read", "files.write", "sync"]),
        900,
        None,
    )
    .unwrap();

    let credential_id = invitation.credential_id(&invitee_node_id).unwrap();

    // Invitee requests only some capabilities
    let pairing_request = PairingRequestBuilder {
        credential_id,
        node_id: &invitee_node_id,
        transport_pubkey: &invitee_signer.public_key(),
        requested_capabilities: CapabilitySet::from_names(["chat", "files.read"]),
        ttl_seconds: 300,
    }
    .build(&invitee_signer)
    .unwrap();

    verify_pairing_request(&pairing_request, chrono::Utc::now().timestamp()).unwrap();

    // Issuer grants a subset of what was requested
    let response = PairingResponseBuilder {
        request: &pairing_request,
        issuer_node_id: &issuer_node_id,
        issuer_pubkey: &issuer_signer.public_key(),
        granted_capabilities: CapabilitySet::from_names(["chat"]), // Only grants chat
        ttl_seconds: 86400,
        issuer_wallet: &issuer_wallet,
    }
    .build()
    .unwrap();

    verify_pairing_response(&response, &pairing_request, chrono::Utc::now().timestamp()).unwrap();
}

// ─────────────────────────────────────────────────────────────────
// Invitation code tests
// ─────────────────────────────────────────────────────────────────

#[test]
fn invitation_code_from_signed_invitation() {
    let issuer_signer = Ed25519Signer::generate();
    let issuer_node_id = node_id_from_bytes(&issuer_signer.public_key());
    let issuer_wallet = Wallet::generate();

    let invitation = SignedInvitation::create(
        &issuer_node_id,
        &issuer_wallet,
        CapabilitySet::from_names(["chat"]),
        900,
        Some("My Device".into()),
    )
    .unwrap();

    let code = InvitationCode::from_invitation(&invitation).unwrap();

    // Code should be parseable
    let parsed = code.to_string().parse::<InvitationCode>().unwrap();
    assert_eq!(code, parsed);

    // Different invitations should produce different codes
    let invitation2 = SignedInvitation::create(
        &issuer_node_id,
        &issuer_wallet,
        CapabilitySet::from_names(["chat"]),
        900,
        Some("Different Device".into()),
    )
    .unwrap();
    let code2 = InvitationCode::from_invitation(&invitation2).unwrap();
    assert_ne!(code, code2);
}

// ─────────────────────────────────────────────────────────────────
// QR URL workflow tests
// ─────────────────────────────────────────────────────────────────

#[test]
fn qr_url_workflow() {
    let node_id = node_id_from_bytes(&[0xAA; 32]);
    let wallet = Wallet::generate();

    let invitation = SignedInvitation::create(
        &node_id,
        &wallet,
        CapabilitySet::from_names(["chat", "presence"]),
        600,
        Some("QR Test Device".into()),
    )
    .unwrap();

    // Encode as URL
    let url = PairingInvitation::to_url(&invitation).unwrap();
    assert!(url.starts_with("a3net-pairing://"));

    // Parse URL
    let parsed = PairingInvitation::parse_url(&url).unwrap();
    let decoded = parsed.decode().unwrap().unwrap();

    // Verify
    decoded.verify(chrono::Utc::now().timestamp()).unwrap();

    assert_eq!(decoded.payload.issuer_node_id, node_id);
}

#[test]
fn json_workflow() {
    let node_id = node_id_from_bytes(&[0xBB; 32]);
    let wallet = Wallet::generate();

    let invitation = SignedInvitation::create(
        &node_id,
        &wallet,
        CapabilitySet::from_names(["sync"]),
        1200,
        None,
    )
    .unwrap();

    // Encode as JSON
    let json = invitation.to_json().unwrap();

    // Parse from JSON
    let parsed = SignedInvitation::from_json(&json).unwrap();

    // Verify
    parsed.verify(chrono::Utc::now().timestamp()).unwrap();

    assert_eq!(parsed.payload.issuer_node_id, node_id);
}

// ─────────────────────────────────────────────────────────────────
// Trusted device store integration tests
// ─────────────────────────────────────────────────────────────────

#[test]
fn store_after_pairing() {
    let (store, _tmp) = tmp_store();

    let issuer_signer = Ed25519Signer::generate();
    let issuer_node_id = node_id_from_bytes(&issuer_signer.public_key());
    let issuer_wallet = Wallet::generate();

    let invitee_signer = Ed25519Signer::generate();
    let invitee_node_id = node_id_from_bytes(&invitee_signer.public_key());

    // Create and verify invitation
    let invitation = SignedInvitation::create(
        &issuer_node_id,
        &issuer_wallet,
        CapabilitySet::from_names(["chat", "files.read"]),
        900,
        None,
    )
    .unwrap();

    let credential_id = invitation.credential_id(&invitee_node_id).unwrap();

    // Create request
    let pairing_request = PairingRequestBuilder {
        credential_id,
        node_id: &invitee_node_id,
        transport_pubkey: &invitee_signer.public_key(),
        requested_capabilities: CapabilitySet::from_names(["chat"]),
        ttl_seconds: 300,
    }
    .build(&invitee_signer)
    .unwrap();

    verify_pairing_request(&pairing_request, chrono::Utc::now().timestamp()).unwrap();

    // Create response
    let response = PairingResponseBuilder {
        request: &pairing_request,
        issuer_node_id: &issuer_node_id,
        issuer_pubkey: &issuer_signer.public_key(),
        granted_capabilities: CapabilitySet::from_names(["chat"]),
        ttl_seconds: 86400 * 30,
        issuer_wallet: &issuer_wallet,
    }
    .build()
    .unwrap();

    verify_pairing_response(&response, &pairing_request, chrono::Utc::now().timestamp()).unwrap();

    // Store the record on issuer side
    let now = chrono::Utc::now().timestamp();
    let record = TrustedDeviceRecord {
        credential_id,
        role: TrustedDeviceRole::Issuer,
        device_name: "Paired Device".into(),
        paired_at_unix: now,
        expires_at_unix: now + 86400 * 30,
        last_seen_unix: now,
        node_id: invitee_node_id.to_string(),
        transport_pubkey: invitee_signer.public_key().to_vec(),
        wallet_address: None,
        capabilities: response.granted_capabilities.clone(),
        status: TrustedDeviceStatus::Active,
        record_version: 1,
        issuer_node_id: issuer_node_id.to_string(),
        revoked_at_unix: 0,
    };

    store.insert(record.clone()).unwrap();

    // Verify device is active
    assert!(store.is_active(&credential_id, now));

    // Check capability
    assert!(store
        .check_capability(&credential_id, Capability::CHAT, now)
        .unwrap());
    assert!(!store
        .check_capability(&credential_id, Capability::SYNC, now)
        .unwrap());

    // Device is in all and active lists
    assert_eq!(store.all().len(), 1);
    assert_eq!(store.active(now).len(), 1);
}

#[test]
fn pairing_credential_id_uniqueness() {
    let issuer_signer = Ed25519Signer::generate();
    let issuer_node_id = node_id_from_bytes(&issuer_signer.public_key());
    let issuer_wallet = Wallet::generate();

    let invitee1_signer = Ed25519Signer::generate();
    let invitee1_node_id = node_id_from_bytes(&invitee1_signer.public_key());

    let invitee2_signer = Ed25519Signer::generate();
    let invitee2_node_id = node_id_from_bytes(&invitee2_signer.public_key());

    // Create invitation
    let invitation = SignedInvitation::create(
        &issuer_node_id,
        &issuer_wallet,
        CapabilitySet::from_names(["chat"]),
        900,
        None,
    )
    .unwrap();

    // Different invitees should get different credential IDs
    let cred1 = invitation.credential_id(&invitee1_node_id).unwrap();
    let cred2 = invitation.credential_id(&invitee2_node_id).unwrap();

    assert_ne!(cred1, cred2);
}

#[test]
fn nonce_replay_prevention() {
    let (store, _tmp) = tmp_store();

    // Simulate receiving a pairing request with a nonce
    let nonce: Nonce32 = [0x12; 32];

    // First time should succeed
    store.check_and_record_nonce(nonce, 3600).unwrap();

    // Second time should fail (replay)
    let err = store.check_and_record_nonce(nonce, 3600).unwrap_err();
    assert!(matches!(err, PairingError::NonceReplay { .. }));
}

#[test]
fn device_lifecycle() {
    let (store, _tmp) = tmp_store();
    let now = chrono::Utc::now().timestamp();

    // Create a record
    let credential_id: [u8; 16] = [0x55; 16];
    let record = TrustedDeviceRecord {
        credential_id,
        role: TrustedDeviceRole::Issuer,
        device_name: "Test Device".into(),
        paired_at_unix: now,
        expires_at_unix: now + 86400,
        last_seen_unix: now,
        node_id: node_id_from_bytes(&[0xAA; 32]).to_string(),
        transport_pubkey: vec![0xAA; 32],
        wallet_address: None,
        capabilities: CapabilitySet::from_names(["chat", "files.read"]),
        status: TrustedDeviceStatus::Active,
        record_version: 1,
        issuer_node_id: node_id_from_bytes(&[0xBB; 32]).to_string(),
        revoked_at_unix: 0,
    };

    // Insert
    store.insert(record.clone()).unwrap();
    assert!(store.is_active(&credential_id, now));

    // Touch (seen activity)
    store.touch(&credential_id, now + 100).unwrap();
    let rec = store.get(&credential_id).unwrap();
    assert_eq!(rec.last_seen_unix, now + 100);
    assert_eq!(rec.record_version, 2);

    // Update capabilities
    store
        .update_capabilities(&credential_id, CapabilitySet::from_names(["chat", "sync"]))
        .unwrap();
    let rec = store.get(&credential_id).unwrap();
    assert!(rec.capabilities.contains(Capability::SYNC));
    assert!(rec.capabilities.contains(Capability::CHAT));
    assert!(!rec.capabilities.contains(Capability::FILES_READ));

    // Revoke
    store.revoke(&credential_id).unwrap();
    let rec = store.get(&credential_id).unwrap();
    assert_eq!(rec.status, TrustedDeviceStatus::Revoked);
    assert!(!store.is_active(&credential_id, now));

    // Remove
    store.remove(&credential_id).unwrap();
    assert!(store.get(&credential_id).is_none());
}

// ─────────────────────────────────────────────────────────────────
// Digest stability tests
// ─────────────────────────────────────────────────────────────────

#[test]
fn invitation_digest_stability() {
    let node_id = node_id_from_bytes(&[0xCC; 32]);
    let wallet = Wallet::generate();

    let payload1 = InvitationPayload {
        version: 1,
        issuer_node_id: node_id.clone(),
        issuer_wallet: wallet.public().address().into(),
        salt: vec![0x11; 32],
        capabilities: CapabilitySet::from_names(["chat"]),
        expires_at_unix: 1_700_000_000,
        note: Some("Device A".into()),
    };

    // Same payload should produce same digest
    let digest1a = pairing_invitation_digest(&payload1);
    let digest1b = pairing_invitation_digest(&payload1);
    assert_eq!(digest1a, digest1b);

    // Different note should produce same digest (note not included)
    let payload2 = InvitationPayload {
        version: 1,
        issuer_node_id: node_id.clone(),
        issuer_wallet: wallet.public().address().into(),
        salt: vec![0x11; 32],
        capabilities: CapabilitySet::from_names(["chat"]),
        expires_at_unix: 1_700_000_000,
        note: Some("Device B".into()),
    };
    let digest2 = pairing_invitation_digest(&payload2);
    assert_eq!(digest1a, digest2);

    // Different version should produce different digest
    let payload3 = InvitationPayload {
        version: 2,
        issuer_node_id: node_id.clone(),
        issuer_wallet: wallet.public().address().into(),
        salt: vec![0x11; 32],
        capabilities: CapabilitySet::from_names(["chat"]),
        expires_at_unix: 1_700_000_000,
        note: Some("Device A".into()),
    };
    let digest3 = pairing_invitation_digest(&payload3);
    assert_ne!(digest1a, digest3);
}

#[test]
fn credential_id_determinism() {
    let issuer = node_id_from_bytes(&[0x11; 32]);
    let invitee = node_id_from_bytes(&[0x22; 32]);
    let salt = [0x33; 32];

    let cred1 = derive_credential_id(&issuer, &invitee, &salt);
    let cred2 = derive_credential_id(&issuer, &invitee, &salt);
    assert_eq!(cred1, cred2);
}

// ─────────────────────────────────────────────────────────────────
// Error propagation tests
// ─────────────────────────────────────────────────────────────────

#[test]
fn invalid_invitation_signature() {
    let node_id = node_id_from_bytes(&[0xDD; 32]);
    let wallet = Wallet::generate();

    let invitation = SignedInvitation::create(
        &node_id,
        &wallet,
        CapabilitySet::empty(),
        900,
        None,
    )
    .unwrap();

    // Valid verification should pass
    invitation
        .verify(chrono::Utc::now().timestamp())
        .unwrap();

    // Tamper with signature should fail
    let mut tampered = invitation.clone();
    tampered.signature[5] ^= 0xFF;
    let err = tampered.verify(chrono::Utc::now().timestamp()).unwrap_err();
    assert!(matches!(err, PairingError::IssuerSignatureInvalid));
}

#[test]
fn expired_invitation_rejected() {
    let node_id = node_id_from_bytes(&[0xEE; 32]);
    let wallet = Wallet::generate();

    let invitation = SignedInvitation::create(
        &node_id,
        &wallet,
        CapabilitySet::empty(),
        0, // Very short TTL
        None,
    )
    .unwrap();

    // Verify immediately should pass
    invitation
        .verify(chrono::Utc::now().timestamp())
        .unwrap();

    // Verify much later should fail
    let far_future = invitation.payload.expires_at_unix + 1000;
    let err = invitation.verify(far_future).unwrap_err();
    assert!(matches!(err, PairingError::InvitationExpired { .. }));
}

#[test]
fn expired_pairing_request_rejected() {
    let signer = Ed25519Signer::generate();
    let node_id = node_id_from_bytes(&signer.public_key());

    let mut request = PairingRequestBuilder {
        credential_id: [0x11; 16],
        node_id: &node_id,
        transport_pubkey: &signer.public_key(),
        requested_capabilities: CapabilitySet::empty(),
        ttl_seconds: 60,
    }
    .build(&signer)
    .unwrap();

    // Set expiry far in the past
    request.expires_at_unix = chrono::Utc::now().timestamp() - 1000;

    let err = verify_pairing_request(&request, chrono::Utc::now().timestamp()).unwrap_err();
    assert!(matches!(err, PairingError::RequestExpired { .. }));
}

#[test]
fn wrong_signer_rejected() {
    let legitimate_signer = Ed25519Signer::generate();
    let attacker_signer = Ed25519Signer::generate();

    let legitimate_node = node_id_from_bytes(&legitimate_signer.public_key());
    let attacker_node = node_id_from_bytes(&attacker_signer.public_key());

    // Attacker creates request with attacker's node_id but signs with legitimate key
    let request = PairingRequestBuilder {
        credential_id: [0x22; 16],
        node_id: &attacker_node,
        transport_pubkey: &attacker_signer.public_key(),
        requested_capabilities: CapabilitySet::empty(),
        ttl_seconds: 60,
    }
    .build(&legitimate_signer) // Signed with wrong key!
    .unwrap();

    let err = verify_pairing_request(&request, chrono::Utc::now().timestamp()).unwrap_err();
    assert!(matches!(
        err,
        PairingError::TransportSignatureInvalid | PairingError::NodeIdMismatch
    ));
}

// ─────────────────────────────────────────────────────────────────
// Store persistence tests
// ─────────────────────────────────────────────────────────────────

#[test]
fn store_persistence_across_restarts() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("devices.jsonl");

    {
        let config = TrustedDeviceStoreConfig {
            path: path.clone(),
            ..Default::default()
        };
        let store = TrustedDeviceStore::open(config).unwrap();

        let credential_id: [u8; 16] = [0x33; 16];
        let record = TrustedDeviceRecord {
            credential_id,
            role: TrustedDeviceRole::Issuer,
            device_name: "Persistent Device".into(),
            paired_at_unix: 1_700_000_000,
            expires_at_unix: i64::MAX,
            last_seen_unix: 1_700_000_000,
            node_id: node_id_from_bytes(&[0x44; 32]).to_string(),
            transport_pubkey: vec![0x44; 32],
            wallet_address: None,
            capabilities: CapabilitySet::from_names(["chat"]),
            status: TrustedDeviceStatus::Active,
            record_version: 1,
            issuer_node_id: node_id_from_bytes(&[0x55; 32]).to_string(),
            revoked_at_unix: 0,
        };

        store.insert(record).unwrap();
    } // Store is dropped here

    // Reopen the store
    let config = TrustedDeviceStoreConfig {
        path,
        ..Default::default()
    };
    let store = TrustedDeviceStore::open(config).unwrap();

    let credential_id: [u8; 16] = [0x33; 16];
    let record = store.get(&credential_id).unwrap();
    assert_eq!(record.device_name, "Persistent Device");
    assert!(store.is_active(&credential_id, i64::MAX));
}

#[test]
fn store_with_sync_enabled() {
    let tmp = TempDir::new().unwrap();
    let config = TrustedDeviceStoreConfig {
        path: tmp.path().join("devices.jsonl"),
        sync: true,
        ..Default::default()
    };

    let store = TrustedDeviceStore::open(config).unwrap();

    let record = TrustedDeviceRecord {
        credential_id: [0x66; 16],
        role: TrustedDeviceRole::Issuer,
        device_name: "Synced Device".into(),
        paired_at_unix: 1_700_000_000,
        expires_at_unix: i64::MAX,
        last_seen_unix: 1_700_000_000,
        node_id: node_id_from_bytes(&[0x77; 32]).to_string(),
        transport_pubkey: vec![0x77; 32],
        wallet_address: None,
        capabilities: CapabilitySet::empty(),
        status: TrustedDeviceStatus::Active,
        record_version: 1,
        issuer_node_id: node_id_from_bytes(&[0x88; 32]).to_string(),
        revoked_at_unix: 0,
    };

    store.insert(record).unwrap();
    assert_eq!(store.len(), 1);
}

// ─────────────────────────────────────────────────────────────────
// Complex workflow tests
// ─────────────────────────────────────────────────────────────────

#[test]
fn multiple_devices_same_issuer() {
    let (store, _tmp) = tmp_store();

    let issuer_signer = Ed25519Signer::generate();
    let issuer_node_id = node_id_from_bytes(&issuer_signer.public_key());
    let issuer_wallet = Wallet::generate();

    let device_names = ["Laptop", "Phone", "Tablet", "Desktop"];

    for (i, device_name) in device_names.iter().enumerate() {
        let invitee_signer = Ed25519Signer::generate();
        let invitee_node_id = node_id_from_bytes(&invitee_signer.public_key());

        // Create invitation
        let invitation = SignedInvitation::create(
            &issuer_node_id,
            &issuer_wallet,
            CapabilitySet::from_names(["chat"]),
            900,
            Some(device_name.to_string()),
        )
        .unwrap();

        let credential_id = invitation.credential_id(&invitee_node_id).unwrap();

        // Create and verify request
        let request = PairingRequestBuilder {
            credential_id,
            node_id: &invitee_node_id,
            transport_pubkey: &invitee_signer.public_key(),
            requested_capabilities: CapabilitySet::from_names(["chat"]),
            ttl_seconds: 300,
        }
        .build(&invitee_signer)
        .unwrap();

        verify_pairing_request(&request, chrono::Utc::now().timestamp()).unwrap();

        // Create response
        let response = PairingResponseBuilder {
            request: &request,
            issuer_node_id: &issuer_node_id,
            issuer_pubkey: &issuer_signer.public_key(),
            granted_capabilities: CapabilitySet::from_names(["chat"]),
            ttl_seconds: 86400 * 30,
            issuer_wallet: &issuer_wallet,
        }
        .build()
        .unwrap();

        verify_pairing_response(&response, &request, chrono::Utc::now().timestamp()).unwrap();

        // Store record
        let now = chrono::Utc::now().timestamp();
        let record = TrustedDeviceRecord {
            credential_id,
            role: TrustedDeviceRole::Issuer,
            device_name: device_name.to_string(),
            paired_at_unix: now,
            expires_at_unix: now + 86400 * 30,
            last_seen_unix: now,
            node_id: invitee_node_id.to_string(),
            transport_pubkey: invitee_signer.public_key().to_vec(),
            wallet_address: None,
            capabilities: response.granted_capabilities.clone(),
            status: TrustedDeviceStatus::Active,
            record_version: 1,
            issuer_node_id: issuer_node_id.to_string(),
            revoked_at_unix: 0,
        };

        store.insert(record).unwrap();
    }

    // All devices should be stored
    assert_eq!(store.len(), 4);
    let all = store.all();
    assert_eq!(all.len(), 4);

    // All should be active
    let now = chrono::Utc::now().timestamp();
    let active = store.active(now);
    assert_eq!(active.len(), 4);

    // Each should have unique name
    let names: Vec<_> = all.iter().map(|r| r.device_name.clone()).collect();
    assert!(names.contains(&"Laptop".to_string()));
    assert!(names.contains(&"Phone".to_string()));
    assert!(names.contains(&"Tablet".to_string()));
    assert!(names.contains(&"Desktop".to_string()));
}

#[test]
fn capability_grant_variations() {
    let issuer_signer = Ed25519Signer::generate();
    let issuer_node_id = node_id_from_bytes(&issuer_signer.public_key());
    let issuer_wallet = Wallet::generate();

    // Test different capability grants
    let grants = vec![
        ("chat_only", vec!["chat"]),
        ("full_access", vec!["chat", "files.read", "files.write", "sync"]),
        ("minimal", vec!["presence"]),
        ("docs_only", vec!["docs.read", "docs.write"]),
    ];

    for (name, caps) in grants {
        let invitee_signer = Ed25519Signer::generate();
        let invitee_node_id = node_id_from_bytes(&invitee_signer.public_key());

        let invitation = SignedInvitation::create(
            &issuer_node_id,
            &issuer_wallet,
            CapabilitySet::from_iter(caps.iter().filter_map(|s| Capability::from_name(s))),
            900,
            Some(name.to_string()),
        )
        .unwrap();

        let credential_id = invitation.credential_id(&invitee_node_id).unwrap();

        let request = PairingRequestBuilder {
            credential_id,
            node_id: &invitee_node_id,
            transport_pubkey: &invitee_signer.public_key(),
            requested_capabilities: invitation.payload.capabilities.clone(),
            ttl_seconds: 300,
        }
        .build(&invitee_signer)
        .unwrap();

        verify_pairing_request(&request, chrono::Utc::now().timestamp()).unwrap();

        let response = PairingResponseBuilder {
            request: &request,
            issuer_node_id: &issuer_node_id,
            issuer_pubkey: &issuer_signer.public_key(),
            granted_capabilities: invitation.payload.capabilities.clone(),
            ttl_seconds: 86400,
            issuer_wallet: &issuer_wallet,
        }
        .build()
        .unwrap();

        verify_pairing_response(&response, &request, chrono::Utc::now().timestamp()).unwrap();
    }
}

// ─────────────────────────────────────────────────────────────────
// Edge cases
// ─────────────────────────────────────────────────────────────────

#[test]
fn invitation_with_no_note() {
    let node_id = node_id_from_bytes(&[0x99; 32]);
    let wallet = Wallet::generate();

    let invitation = SignedInvitation::create(
        &node_id,
        &wallet,
        CapabilitySet::empty(),
        900,
        None, // No note
    )
    .unwrap();

    assert!(invitation.payload.note.is_none());

    // Should still work
    invitation.verify(chrono::Utc::now().timestamp()).unwrap();
}

#[test]
fn empty_capability_set() {
    let node_id = node_id_from_bytes(&[0xAA; 32]);
    let wallet = Wallet::generate();

    let invitation = SignedInvitation::create(
        &node_id,
        &wallet,
        CapabilitySet::empty(),
        900,
        None,
    )
    .unwrap();

    assert!(invitation.payload.capabilities.is_empty());

    invitation.verify(chrono::Utc::now().timestamp()).unwrap();
}

#[test]
fn store_empty_update() {
    let (store, _tmp) = tmp_store();

    // Update on non-existent record should fail
    let err = store
        .update_capabilities(&[0xFF; 16], CapabilitySet::empty())
        .unwrap_err();
    assert!(matches!(err, PairingError::DeviceNotFound(_)));
}

#[test]
fn touch_nonexistent() {
    let (store, _tmp) = tmp_store();

    let err = store.touch(&[0x11; 16], chrono::Utc::now().timestamp()).unwrap_err();
    assert!(matches!(err, PairingError::DeviceNotFound(_)));
}
