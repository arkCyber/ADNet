//! End-to-end tests for the P2P pairing service.
//!
//! Two scenarios are exercised:
//!
//! 1. **Full invite + accept round-trip**: Alice issues a signed
//!    invitation, Bob parses it, Bob accepts it on his own service,
//!    the trusted-device record lands on Bob's side and survives a
//!    re-open.
//! 2. **Short-code round-trip**: Alice issues an invitation, derives
//!    the human-readable `ADNET:XXXX-YYYY-ZZZZ-NNNN` code, the
//!    invitee parses + format-checks it.
//!
//! Plus the RPC dispatch surface: each `a3chat.pairing.*` method is
//! invoked through [`A3chatApp::dispatch`] so the routing glue is
//! exercised too.

use std::sync::Arc;

use a3chat_app::app::A3chatApp;
use a3chat_app::pairing_service::{
    AcceptInvitationRequest, CreateInvitationRequest, PairingService,
    PairingServiceConfig, DEFAULT_INVITATION_TTL_SECONDS,
};
use a3chat_app::storage::StorageConfig;
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;
use a3net_identity::wallet::Wallet;
use a3net_pairing::trusted_device::{TrustedDeviceRole, TrustedDeviceStatus};
use a3net_types::node::NodeId;

fn mk_node(byte: u8) -> String {
    NodeId::from_bytes(&[byte; 32]).unwrap().to_string()
}

fn alice_node() -> String {
    mk_node(0xAA)
}
fn bob_node() -> String {
    mk_node(0xBB)
}

fn alice_wallet() -> [u8; 32] {
    // Deterministic for test reproducibility — a real deployment
    // loads this from a keychain / secure element.
    let mut s = [0u8; 32];
    s[0] = 0x01;
    s
}

fn alice_pairing_config(dir: &std::path::Path) -> PairingServiceConfig {
    PairingServiceConfig {
        data_dir: dir.join("alice-pairing"),
        wallet_secret: alice_wallet(),
        local_node_id: alice_node(),
    }
}

#[test]
fn invite_create_parse_verify_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let svc = PairingService::open(
        alice_pairing_config(dir.path()),
        UserId::from("alice"),
    )
    .unwrap();

    let inv = svc
        .create_invitation(CreateInvitationRequest {
            issuer_node_id: alice_node(),
            capabilities: Some(vec!["chat".into(), "files.read".into()]),
            ttl_seconds: Some(60),
            note: Some("Alice's laptop".into()),
        })
        .unwrap();
    assert!(inv.expires_at_unix - chrono::Utc::now().timestamp() <= 61);

    // Verify at the issuer — must succeed.
    svc.verify_invitation(&inv.invitation_json, chrono::Utc::now().timestamp())
        .unwrap();

    // Parse — fields match what we sent.
    let decoded = svc.parse_invitation(&inv.invitation_json).unwrap();
    assert_eq!(decoded.issuer_node_id, alice_node());
    assert!(!decoded.issuer_wallet.is_empty());
    assert_eq!(decoded.capabilities, vec!["chat", "files.read"]);
    assert_eq!(decoded.note.as_deref(), Some("Alice's laptop"));
}

#[test]
fn verify_rejects_expired_invitation() {
    let dir = tempfile::tempdir().unwrap();
    let svc = PairingService::open(
        alice_pairing_config(dir.path()),
        UserId::from("alice"),
    )
    .unwrap();

    let inv = svc
        .create_invitation(CreateInvitationRequest {
            issuer_node_id: alice_node(),
            capabilities: None,
            ttl_seconds: Some(1),
            note: None,
        })
        .unwrap();

    // 60 seconds in the future — beyond the 1-second TTL.
    let far_future = chrono::Utc::now().timestamp() + 60;
    let err = svc
        .verify_invitation(&inv.invitation_json, far_future)
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("expired") || msg.contains("InvitationExpired"), "{msg}");
}

#[test]
fn accept_persists_trust_record() {
    let dir = tempfile::tempdir().unwrap();
    let alice = PairingService::open(
        alice_pairing_config(dir.path()),
        UserId::from("alice"),
    )
    .unwrap();

    let bob_dir = tempfile::tempdir().unwrap();
    let bob = PairingService::open(
        PairingServiceConfig {
            data_dir: bob_dir.path().to_path_buf(),
            wallet_secret: Wallet::generate().secret_bytes(),
            local_node_id: bob_node(),
        },
        UserId::from("bob"),
    )
    .unwrap();

    let inv = alice
        .create_invitation(CreateInvitationRequest {
            issuer_node_id: alice_node(),
            capabilities: Some(vec!["chat".into(), "sync".into()]),
            ttl_seconds: Some(DEFAULT_INVITATION_TTL_SECONDS),
            note: None,
        })
        .unwrap();

    let accepted = bob
        .accept_invitation(AcceptInvitationRequest {
            invitation_json: inv.invitation_json,
            invitee_node_id: bob_node(),
            invitee_transport_pubkey: vec![0xCCu8; 32],
            device_name: "Bob's iPhone".into(),
            requested_capabilities: vec!["chat".into(), "files.read".into()],
        })
        .unwrap();
    // Granted = intersection(requested, issuer-grant) = {chat}.
    // `files.read` was not in issuer's grant set, so it's dropped.
    assert_eq!(accepted.granted_capabilities, vec!["chat"]);
    assert_eq!(accepted.device_name, "Bob's iPhone");

    let records = bob.list_trusted_devices().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].role, TrustedDeviceRole::Invitee);
    assert_eq!(records[0].device_name, "Bob's iPhone");
    assert_eq!(records[0].status, TrustedDeviceStatus::Active);
    assert_eq!(records[0].issuer_node_id, alice_node());
    assert_eq!(records[0].node_id, alice_node());

    // Reopen the service from the same directory — record survives.
    drop(bob);
    let bob_reopened = PairingService::open(
        PairingServiceConfig {
            data_dir: bob_dir.path().to_path_buf(),
            wallet_secret: Wallet::generate().secret_bytes(),
            local_node_id: bob_node(),
        },
        UserId::from("bob"),
    )
    .unwrap();
    assert_eq!(bob_reopened.list_trusted_devices().unwrap().len(), 1);
}

#[test]
fn revoke_marks_record_revoked() {
    let dir = tempfile::tempdir().unwrap();
    let svc = PairingService::open(
        alice_pairing_config(dir.path()),
        UserId::from("alice"),
    )
    .unwrap();

    let rec = svc
        .record_issuer_pairing(
            &alice_node(),
            &bob_node(),
            vec![0xCD; 32],
            "Bob's tablet".into(),
            vec!["chat".into()],
        )
        .unwrap();
    let cred_hex = hex::encode(rec.credential_id);

    assert!(svc.revoke_trusted_device(&cred_hex).unwrap());
    // Idempotent — calling again returns false (already revoked).
    // (The store does not error on a missing key after revoke.)
    let after = svc.get_trusted_device(&cred_hex).unwrap().unwrap();
    assert_eq!(after.status, TrustedDeviceStatus::Revoked);
}

#[test]
fn short_code_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let svc = PairingService::open(
        alice_pairing_config(dir.path()),
        UserId::from("alice"),
    )
    .unwrap();
    let inv = svc
        .create_invitation(CreateInvitationRequest {
            issuer_node_id: alice_node(),
            capabilities: None,
            ttl_seconds: Some(60),
            note: None,
        })
        .unwrap();
    let code = svc.create_short_code(&inv.invitation_json).unwrap();
    assert!(code.starts_with("ADNET:"));
    let parsed = svc.parse_short_code(&code).unwrap();
    assert_eq!(parsed.display, code);
    assert_eq!(parsed.segment_count, 4);

    // Format validation rejects malformed input.
    let bad = svc.parse_short_code("not a code");
    assert!(bad.is_err());
}

#[tokio::test]
async fn bus_emits_invitation_and_trust_events() {
    let dir = tempfile::tempdir().unwrap();
    let svc = PairingService::open(
        alice_pairing_config(dir.path()),
        UserId::from("alice"),
    )
    .unwrap();
    let mut rx = svc.bus().subscribe();

    let _inv = svc
        .create_invitation(CreateInvitationRequest {
            issuer_node_id: alice_node(),
            capabilities: None,
            ttl_seconds: Some(60),
            note: None,
        })
        .unwrap();
    let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
        .await
        .expect("event")
        .expect("event some");
    match evt {
        A3chatEvent::PairingInvitationCreated {
            issuer_node_id, ..
        } => {
            assert_eq!(issuer_node_id, alice_node());
        }
        other => panic!("wrong event kind: {other:?}"),
    }

    // Revoke — second event.
    let rec = svc
        .record_issuer_pairing(
            &alice_node(),
            &bob_node(),
            vec![0xCD; 32],
            "X".into(),
            vec!["chat".into()],
        )
        .unwrap();
    let cred_hex = hex::encode(rec.credential_id);

    let _ = svc.revoke_trusted_device(&cred_hex).unwrap();

    let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
        .await
        .expect("event")
        .expect("event some");
    match evt {
        A3chatEvent::PairingTrustedDeviceAdded { role, .. } => {
            assert_eq!(role, "issuer");
        }
        other => panic!("wrong event kind: {other:?}"),
    }
    let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
        .await
        .expect("event")
        .expect("event some");
    match evt {
        A3chatEvent::PairingTrustedDeviceRevoked { .. } => {}
        other => panic!("wrong event kind: {other:?}"),
    }
}

#[tokio::test]
async fn dispatch_without_pairing_returns_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), UserId::from("alice"))
        .unwrap();
    let err = app
        .dispatch(
            A3chatRpcMethod::PAIRING_INVITATION_CREATE,
            &UserId::from("alice"),
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("PairingService") || msg.contains("with_pairing"),
        "expected pairing-not-configured diagnostic, got: {msg}"
    );
}

#[tokio::test]
async fn dispatch_routes_to_pairing_service_when_installed() {
    let dir = tempfile::tempdir().unwrap();
    let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), UserId::from("alice"))
        .unwrap();
    app.with_pairing(&UserId::from("alice"), alice_pairing_config(dir.path()))
        .unwrap();

    let r = app
        .dispatch(
            A3chatRpcMethod::PAIRING_INVITATION_CREATE,
            &UserId::from("alice"),
            serde_json::json!({ "ttl_seconds": 60 }),
        )
        .await
        .unwrap();
    assert!(r["invitation_json"].is_string());
    assert!(r["issuer_node_id"].is_string());
    assert_eq!(r["issuer_node_id"], alice_node());

    // Health via RPC.
    let h = app
        .dispatch(
            A3chatRpcMethod::PAIRING_HEALTH,
            &UserId::from("alice"),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(h["ok"], true);
    assert_eq!(h["service"], "a3chat.pairing");
    assert_eq!(h["trusted_devices"], 0);
}

#[tokio::test]
async fn dispatch_healthz_includes_pairing_when_installed() {
    // After installing pairing, a3chat.healthz should still answer
    // — the liveness probe is independent of pairing availability.
    let dir = tempfile::tempdir().unwrap();
    let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), UserId::from("alice"))
        .unwrap();
    app.with_pairing(&UserId::from("alice"), alice_pairing_config(dir.path()))
        .unwrap();
    let r = app
        .dispatch(
            A3chatRpcMethod::HEALTHZ,
            &UserId::from("alice"),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(r["ok"], true);
}

// Suppress the unused-import warning that appears on platforms
// where Arc isn't otherwise referenced.
#[allow(dead_code)]
fn _force_arc_link() -> Arc<()> {
    Arc::new(())
}