//! End-to-end "Alice invites Bob by email" example for a3net-invite.
//!
//! Simulates the realistic flow when two A3Net users pair without a
//! shared camera — Alice composes and serializes a full email (with
//! QR inline + JSON attachment), the bytes are handed off to Bob's
//! IMAP client, who extracts the invitation, verifies it, and
//! confirms they would now have everything needed to complete the
//! QUIC pairing in the next step.
//!
//! No network is touched; the wire bytes are passed in-memory.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p a3net-invite --example invite_app
//! ```

use a3net_identity::wallet::Wallet;
use a3net_invite::{InvitationContent, InvitationMailer, create_text_code, parse_text_code};
use a3net_mail::mime::Address;
use a3net_pairing::{CapabilitySet, PairingInvitation, SignedInvitation};
use a3net_types::node::NodeId;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let wallet = Wallet::generate();
    let alice_node = NodeId::from_bytes(&[0xAAu8; 32])?;
    let inv = SignedInvitation::create(
        &alice_node,
        &wallet,
        CapabilitySet::from_names(["chat", "files.read", "sync"]),
        15 * 60,
        Some("Alice's MacBook".into()),
    )?;

    let mail = InvitationMailer::build_invitation_email_with_qr(
        &inv,
        &InvitationContent {
            from: Address::new("alice@example.com").with_name("Alice"),
            to: vec![Address::new("bob@example.com")],
            subject: "Let's pair our A3Net devices".into(),
            body: String::new(),
        },
    )?;

    println!("== Alice composed mail ==");
    println!("mail.attachments : {}", mail.attachments.len());
    println!("  → {} ({}, {} bytes)",
        mail.attachments[0].filename,
        mail.attachments[0].content_type,
        mail.attachments[0].data.len());
    println!("  → {} ({}, {} bytes)",
        mail.attachments[1].filename,
        mail.attachments[1].content_type,
        mail.attachments[1].data.len());
    println!("mail.html        : present ({} bytes)", mail.html.as_ref().unwrap().len());
    println!("mail.text.len    : {} bytes", mail.text.len());

    println!("\n== Bob's IMAP fetches the wire bytes ==");
    let wire = mail.to_wire_bytes()?;
    println!("Wire bytes       : {} bytes", wire.len());
    let parsed = InvitationMailer::extract_from_wire(&wire)?;
    let now = chrono::Utc::now().timestamp();
    parsed.verify(now)?;
    let decoded = parsed.decode()?.unwrap();
    println!("issuer wallet    : {}", decoded.payload.issuer_wallet);
    println!("expires_at_unix  : {}", decoded.payload.expires_at_unix);
    println!("offered caps     : {:?}", decoded.payload.capabilities);

    println!("\n== Bob also has a text code (phone fallback) ==");
    let code = create_text_code(&inv)?;
    let printed = code.to_string();
    println!("Bob types        : ADNET-…({} chars total)", printed.len());
    let parsed = parse_text_code(&printed)?.unwrap();
    assert_eq!(parsed.payload.issuer_wallet, inv.payload.issuer_wallet);
    println!("Re-verified      : OK");

    println!("\n== What Bob would do next ==");
    println!("1. accept the invitation (canvas out of scope)");
    println!("2. derive credential_id from issuer_node_id + invitee_node_id + salt");
    println!("3. build PairingRequest with Ed25519 transport key");
    println!("4. send it to issuer_node_id over the QUIC transport");
    println!("5. peer_pairing.rs in a3net-transport handles the rest");

    // Confirm we have both forms of the same invitation — the QR URL
    // form and the embedded JSON form — both equal the same issuer.
    let url = a3net_pairing::wire::PairingInvitation::to_url(&inv)?;
    let from_url = PairingInvitation::parse_url(&url)?.decode()?.unwrap();
    assert_eq!(from_url.payload.issuer_wallet, inv.payload.issuer_wallet);
    println!("\nURL form and JSON form are equivalent: OK");

    Ok(())
}
