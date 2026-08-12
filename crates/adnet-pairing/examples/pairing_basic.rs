//! Minimal adnet-pairing example.
//!
//! Generates a wallet, signs a `SignedInvitation`, prints it as JSON,
//! derives an `InvitationCode`, and exercises the `PairingInvitation`
//! URL helper. Sticks to in-memory use — no filesystem, no IO.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p adnet-pairing --example pairing_basic
//! ```

use adnet_identity::wallet::Wallet;
use adnet_pairing::{
    Capability, CapabilitySet, InvitationCode, PairingInvitation, SignedInvitation,
};
use adnet_types::node::NodeId;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let wallet = Wallet::generate();
    let node = NodeId::from_bytes(&[0x33u8; 32])?;

    let caps = CapabilitySet::from_names(["chat", "files.read"]);
    let inv = SignedInvitation::create(
        &node,
        &wallet,
        caps.clone(),
        15 * 60,
        Some("smoke test".into()),
    )?;

    println!("== SignedInvitation ==");
    println!("issuer_node_id : {}", inv.payload.issuer_node_id);
    println!("issuer_wallet  : {}", inv.payload.issuer_wallet);
    println!("expires_at     : {}", inv.payload.expires_at_unix);
    println!("note           : {:?}", inv.payload.note);
    println!("caps           : {caps:?}");
    println!("signature.len  : {} bytes", inv.signature.len());

    println!("\n== JSON round-trip ==");
    let json = inv.to_json()?;
    let back = SignedInvitation::from_json(&json)?;
    assert_eq!(back.payload.issuer_wallet, inv.payload.issuer_wallet);
    println!("JSON len       : {} bytes", json.len());

    println!("\n== InvitationCode ==");
    let code = InvitationCode::from_invitation(&inv)?;
    println!("code           : {code}");
    let parsed: InvitationCode = code.to_string().parse()?;
    println!("parsed.as_str  : {}", parsed.as_str());

    println!("\n== PairingInvitation URL ==");
    let url = PairingInvitation::to_url(&inv)?;
    let scanned = PairingInvitation::parse_url(&url)?;
    let decoded = scanned.decode()?.unwrap();
    decoded.verify(chrono::Utc::now().timestamp())?;
    println!("URL prefix     : {}", url.get(..27).unwrap_or(&url));
    println!("verify(OK)     : signature is valid");

    println!("\n== CapabilitySet checks ==");
    for cap in [
        Capability::CHAT,
        Capability::FILES_READ,
        Capability::FILES_WRITE,
        Capability::SYNC,
    ] {
        println!("  {:>15}  contains = {}", cap.name(), caps.contains(cap));
    }

    Ok(())
}
