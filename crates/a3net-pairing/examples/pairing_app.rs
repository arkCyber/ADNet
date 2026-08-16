//! End-to-end pairing ceremony example for a3net-pairing.
//!
//! Walks through every step that the A3Net transport follows when
//! two nodes pair, but in-memory:
//!
//! 1. issuer signs a `SignedInvitation` (QR/email side);
//! 2. invitee scans it, derives the `CredentialId`;
//! 3. invitee builds a `PairingRequest` with its Ed25519 transport key;
//! 4. issuer verifies the request, builds a `PairingResponse`
//!    granting a subset of the requested capabilities;
//! 5. both peers persist a `TrustedDeviceRecord`;
//! 6. issuer checks `check_capability` for two capabilities — one
//!    granted, one denied — to show the in-memory authorisation path.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p a3net-pairing --example pairing_app
//! ```

use a3net_identity::wallet::Wallet;
use a3net_pairing::transport_identity::{derive_credential_id, Ed25519Signer};
use a3net_pairing::{
    Capability, CapabilitySet, PairingRequestBuilder, PairingResponseBuilder, SignedInvitation,
    TrustedDeviceRecord, TrustedDeviceStatus, TrustedDeviceStore, TrustedDeviceStoreConfig,
    verify_pairing_request, verify_pairing_response,
};
use a3net_pairing::trusted_device::TrustedDeviceRole;
use a3net_types::node::NodeId;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tmpdir = std::env::temp_dir().join(format!("a3net-pairing-app-{}", std::process::id()));
    std::fs::create_dir_all(&tmpdir)?;
    let store_path = tmpdir.join("devices.jsonl");

    let issuer_node = NodeId::from_bytes(&[0x11u8; 32])?;
    let issuer_wallet = Wallet::generate();
    let issuer_signer = Ed25519Signer::generate();

    // The iroh transport binds `NodeId == blake3(transport_pubkey)`, but
    // `verify_pairing_request` requires the literal bytes to match. We
    // construct the invitee's NodeId from the Ed25519 signing key's
    // verifying-key bytes so the off-by-binding check passes.
    let invitee_signer = Ed25519Signer::generate();
    let invitee_pubkey = invitee_signer.verifying_key_bytes();
    let invitee_node = NodeId::from_bytes(&invitee_pubkey)?;

    println!("== Step 1: issuer signs invitation ==");
    let offered = CapabilitySet::from_names(["chat", "files.read", "sync"]);
    let invitation = SignedInvitation::create(
        &issuer_node,
        &issuer_wallet,
        offered.clone(),
        15 * 60,
        Some("Bob's phone".into()),
    )?;
    println!("invitation.note = {:?}", invitation.payload.note);
    println!("offered caps   : {offered:?}");

    println!("\n== Step 2: invitee verifies + derives credential_id ==");
    invitation.verify(chrono::Utc::now().timestamp())?;
    let salt_arr: [u8; 32] = invitation.payload.salt.as_slice().try_into().unwrap();
    let credential_id = derive_credential_id(&invitation.payload.issuer_node_id, &invitee_node, &salt_arr);
    println!("credential_id  : {:02x?}", credential_id);

    println!("\n== Step 3: invitee builds PairingRequest ==");
    let requested = CapabilitySet::from_names(["chat", "files.read"]);
    let transport_pubkey = invitee_signer.verifying_key_bytes();
    let request = PairingRequestBuilder {
        credential_id,
        node_id: &invitee_node,
        transport_pubkey: &transport_pubkey,
        requested_capabilities: requested.clone(),
        ttl_seconds: 60,
    }
    .build(&invitee_signer)?;
    println!("version        : {}", request.version);
    println!("nonce          : {:02x?}…", &request.nonce[..4]);
    println!("requested caps : {requested:?}");

    println!("\n== Step 4: issuer verifies + replies ==");
    verify_pairing_request(&request, chrono::Utc::now().timestamp())?;
    let granted = CapabilitySet::from_names(["chat"]);
    let response = PairingResponseBuilder {
        request: &request,
        issuer_node_id: &issuer_node,
        issuer_pubkey: &issuer_signer.verifying_key_bytes(),
        granted_capabilities: granted.clone(),
        ttl_seconds: 60 * 60,
        issuer_wallet: &issuer_wallet,
    }
    .build()?;
    println!("response.expires_at = {}", response.expires_at_unix);
    println!("response.granted    : {granted:?}");
    verify_pairing_response(&response, &request, chrono::Utc::now().timestamp())?;

    println!("\n== Step 5: both sides persist TrustedDeviceRecord ==");
    let store = TrustedDeviceStore::open(TrustedDeviceStoreConfig {
        path: store_path.clone(),
        ..Default::default()
    })?;
    let now = chrono::Utc::now().timestamp();
    let record = TrustedDeviceRecord {
        credential_id,
        role: TrustedDeviceRole::Issuer,
        device_name: "Bob's phone".to_string(),
        paired_at_unix: now,
        expires_at_unix: response.expires_at_unix,
        last_seen_unix: now,
        node_id: invitee_node.to_string(),
        transport_pubkey: invitee_signer.verifying_key_bytes().to_vec(),
        wallet_address: None,
        capabilities: granted.clone(),
        status: TrustedDeviceStatus::Active,
        record_version: 1,
        issuer_node_id: issuer_node.to_string(),
        revoked_at_unix: 0,
    };
    record.validate()?;
    store.insert(record.clone())?;
    let fetched = store.get(&credential_id).expect("just-inserted record");
    println!("store.size   : {}", store.all().len());
    println!("fetched caps : {:?}", fetched.capabilities);

    println!("\n== Step 6: runtime authorisation checks ==");
    let can_chat = store.check_capability(&credential_id, Capability::CHAT, now)?;
    let can_read = store.check_capability(&credential_id, Capability::FILES_READ, now)?;
    let can_write = store.check_capability(&credential_id, Capability::FILES_WRITE, now)?;
    let can_sync = store.check_capability(&credential_id, Capability::SYNC, now)?;
    println!("chat           : {}", can_chat);
    println!("files.read     : {} (denied at grant time)", can_read);
    println!("files.write    : {}", can_write);
    println!("sync           : {}", can_sync);

    println!("\nstore file written at: {}", store_path.display());
    Ok(())
}
