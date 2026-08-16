//! End-to-end tests for the `AdnetPairing` QR payload variant.
//!
//! These tests verify the full pipeline a chat / pairing UI relies on:
//!
//!  1. A `SignedInvitation` is serialised by `a3net-pairing` to a
//!     canonical `a3net-pairing://…` URL.
//!
//!  2. The QR layer parses it back to a typed
//!     `QrPayload::AdnetPairing { invitation: PairingInvitation::Url }`.
//!
//!  3. The recovered envelope decodes back to the original
//!     `SignedInvitation`, the wallet signature verifies against the
//!     canonical `pairing_invitation_digest`, and the issuer's
//!     `WalletAddress` matches what was recovered.
//!
//!  4. Defensive properties: an `a3net-pairing://` URL carrying garbage
//!     payload bytes is rejected as `QrError::Malformed`, NOT
//!     silently re-classified as `QrPayload::Text`.

use a3net_identity::wallet::Wallet;
use a3net_pairing::capability::CapabilitySet;
use a3net_qr::{QrPayload, scan};
use a3net_types::node::NodeId;

fn make_invitation() -> a3net_pairing::SignedInvitation {
    let node_id = NodeId::from_bytes(&[0xCDu8; 32]).expect("32 bytes");
    let wallet = Wallet::generate();
    a3net_pairing::SignedInvitation::create(
        &node_id,
        &wallet,
        CapabilitySet::from_names(["chat"]),
        600,
        Some("alice's laptop".into()),
    )
    .expect("signed invitation")
}

#[test]
fn pairing_qr_round_trips_through_check_qr() {
    let inv = make_invitation();
    let url = a3net_pairing::wire::PairingInvitation::to_url(&inv).expect("to_url");
    assert!(url.starts_with("a3net-pairing://"));

    let parsed = scan::check_qr(&url).expect("check_qr");
    match &parsed {
        QrPayload::AdnetPairing { invitation } => {
            let re = scan::encode_qr(&parsed).expect("encode_qr");
            assert_eq!(re, url);

            let decoded = invitation
                .decode()
                .expect("decode")
                .expect("decode must yield Some");
            assert_eq!(decoded.payload.issuer_wallet, inv.payload.issuer_wallet);
            assert_eq!(decoded.payload.issuer_node_id, inv.payload.issuer_node_id);
            assert_eq!(decoded.signature, inv.signature);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn pairing_qr_preserves_wallet_signature() {
    let inv = make_invitation();
    let url = a3net_pairing::wire::PairingInvitation::to_url(&inv).expect("to_url");
    let parsed = scan::check_qr(&url).expect("check_qr");

    let inner = match parsed {
        QrPayload::AdnetPairing { invitation } => {
            invitation.decode().expect("decode").expect("Some")
        }
        _ => unreachable!(),
    };
    let now = chrono::Utc::now().timestamp();
    inner.verify(now).expect("wallet signature must verify");
}

#[test]
fn pairing_qr_rejects_garbage_payload() {
    let bad = "a3net-pairing://this-is-not-valid-base64url!@#$";
    let err = scan::check_qr(bad).expect_err("must reject");
    let s = format!("{err}");
    assert!(s.contains("a3net-pairing"), "got: {s}");
}

#[test]
fn pairing_qr_rejects_oversized_payload() {
    let bad = format!("a3net-pairing://{}", "A".repeat(3000));
    let res = scan::check_qr(&bad);
    assert!(
        res.is_err(),
        "oversized payload must be rejected, got: {res:?}"
    );
}

#[test]
fn pairing_qr_json_variant_round_trips() {
    let inv = make_invitation();
    let pi = a3net_pairing::wire::PairingInvitation::Json(inv.clone());
    let payload = QrPayload::AdnetPairing { invitation: pi };
    let encoded = scan::encode_qr(&payload).expect("encode_qr");
    let parsed = scan::check_qr(&encoded).expect("check_qr");
    match parsed {
        QrPayload::AdnetPairing { invitation } => {
            let decoded = invitation.decode().expect("decode").expect("Some");
            assert_eq!(decoded.payload.issuer_wallet, inv.payload.issuer_wallet);
        }
        _ => panic!("wrong variant after round trip"),
    }
}
