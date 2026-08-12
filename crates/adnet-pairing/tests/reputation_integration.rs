//! Integration tests for the `adnet-reputation` × `adnet-pairing`
//! bridge. Requires the `reputation` feature on `adnet-pairing` so
//! that `TrustedDeviceStore::with_reputation` is in scope and the
//! `insert` / `revoke` methods feed the PeerScore table.

#![cfg(feature = "reputation")]

use adnet_pairing::store::{TrustedDeviceStore, TrustedDeviceStoreConfig};
use adnet_pairing::transport_identity::CredentialId;
use adnet_pairing::trusted_device::{
    TrustedDeviceRecord, TrustedDeviceRole, TrustedDeviceStatus,
};
use adnet_reputation::{PeerScoreTable, ReputationEvent, ReputationParams, ReputationReporter};
use adnet_types::NodeId;
use tempfile::TempDir;

fn make_record(node: &NodeId, salt_byte: u8, role: TrustedDeviceRole) -> TrustedDeviceRecord {
    let cred: CredentialId = [salt_byte; 16];
    TrustedDeviceRecord {
        credential_id: cred,
        role,
        device_name: format!("dev-{salt_byte}"),
        paired_at_unix: 1_700_000_000,
        expires_at_unix: i64::MAX,
        last_seen_unix: 1_700_000_000,
        node_id: node.as_hex().to_string(),
        transport_pubkey: Vec::new(),
        wallet_address: None,
        capabilities: Default::default(),
        status: TrustedDeviceStatus::Active,
        record_version: 1,
        issuer_node_id: NodeId::random().as_hex().to_string(),
        revoked_at_unix: 0,
    }
}

fn tmp_store() -> (TrustedDeviceStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let cfg = TrustedDeviceStoreConfig {
        path: dir.path().join("devices.jsonl"),
        ..Default::default()
    };
    let store = TrustedDeviceStore::open(cfg).unwrap();
    (store, dir)
}

/// When `with_reputation` is installed, `insert` must record a
/// `PairingEstablished` event for the new device's NodeId.
#[test]
fn insert_feeds_pairing_established_event() {
    let (store, _dir) = tmp_store();
    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table.clone());
    store.with_reputation(reporter.clone());

    let peer = NodeId::random();
    let rec = make_record(&peer, 1, TrustedDeviceRole::Issuer);
    store.insert(rec.clone()).unwrap();

    // The score must be strictly positive after a successful pairing.
    let score = reporter
        .table()
        .score(&peer)
        .expect("peer must have a score entry after pairing established");
    assert!(score > 0.0, "score should be positive, got {score}");

    // Sanity: the kind_tag round-trips.
    let kind = ReputationEvent::PairingEstablished {
        peer: peer.clone(),
        credential_id_short: "abcd".into(),
    }
    .kind_tag();
    assert!(kind.contains("pairing_established") || kind.contains("PairingEstablished"));
}

/// After a revoke, the PeerScore for the previously-paired peer must
/// drop below its post-`insert` value. This is the negative signal
/// that the rest of ADNet (gossipsub, bitswap) reacts to.
#[test]
fn revoke_feeds_pairing_revoked_event_and_drops_score() {
    let (store, _dir) = tmp_store();
    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table.clone());
    store.with_reputation(reporter.clone());

    let peer = NodeId::random();
    let rec = make_record(&peer, 2, TrustedDeviceRole::Issuer);
    store.insert(rec.clone()).unwrap();
    let after_insert = reporter
        .table()
        .score(&peer)
        .expect("peer must be scored after insert");

    store.revoke(&rec.credential_id).unwrap();
    let after_revoke = reporter
        .table()
        .score(&peer)
        .expect("peer must still be scored after revoke");
    assert!(
        after_revoke < after_insert,
        "score must drop after revoke: was {after_insert}, now {after_revoke}"
    );
    assert!(
        after_revoke < 0.0,
        "revoke must push score negative, got {after_revoke}"
    );
}

/// A store without a reputation hook must still insert / revoke
/// successfully — i.e. the reputation path is strictly opt-in and
/// must not break the standalone pairing store.
#[test]
fn store_without_reputation_works() {
    let (store, _dir) = tmp_store();
    let peer = NodeId::random();
    let rec = make_record(&peer, 3, TrustedDeviceRole::Invitee);
    store.insert(rec.clone()).expect("insert without reporter");
    store.revoke(&rec.credential_id).expect("revoke without reporter");
}

/// Installing a reputation reporter after some `insert`/`revoke`
/// calls already happened must not retroactively emit events for
/// those operations. The reporter only sees calls made *after*
/// `with_reputation`.
#[test]
fn reporter_only_sees_subsequent_calls() {
    let (store, _dir) = tmp_store();
    let peer = NodeId::random();
    let rec = make_record(&peer, 4, TrustedDeviceRole::Issuer);
    // insert BEFORE attaching reporter
    store.insert(rec.clone()).unwrap();
    let pre_score = store
        .reputation()
        .map(|r| r.table().score(&peer).unwrap_or(0.0))
        .unwrap_or(0.0);
    assert_eq!(pre_score, 0.0, "no reporter => no events yet");

    // Now install reporter and insert a NEW peer.
    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table);
    store.with_reputation(reporter.clone());

    let peer2 = NodeId::random();
    let rec2 = make_record(&peer2, 5, TrustedDeviceRole::Issuer);
    store.insert(rec2).unwrap();
    // Peer 2 must now be scored.
    assert!(reporter.table().score(&peer2).expect("peer2 scored") > 0.0);
    // Peer 1 still has no entry — `with_reputation` did not backfill.
    assert!(reporter.table().score(&peer).is_none());
}