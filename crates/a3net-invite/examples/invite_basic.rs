//! Minimal a3net-invite example.
//!
//! Generates a `SignedInvitation`, packages it twice — once as a
//! plain-text MIME mail with the `.inv` attachment, and once as a
//! human-readable `TextCode` — and then verifies the round-trip.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p a3net-invite --example invite_basic
//! ```

use a3net_identity::wallet::Wallet;
use a3net_invite::{InvitationContent, InvitationMailer, create_text_code, parse_text_code};
use a3net_mail::mime::Address;
use a3net_pairing::{CapabilitySet, SignedInvitation};
use a3net_types::node::NodeId;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let wallet = Wallet::generate();
    let node_id = NodeId::from_bytes(&[0x77u8; 32])?;
    let inv = SignedInvitation::create(
        &node_id,
        &wallet,
        CapabilitySet::from_names(["chat", "files.read"]),
        15 * 60,
        Some("basic invite".into()),
    )?;

    println!("== MIME mail build + extract ==");
    let mail = InvitationMailer::build_invitation_email(
        &inv,
        &InvitationContent {
            from: Address::new("alice@example.com").with_name("Alice"),
            to: vec![Address::new("bob@example.com")],
            subject: "A3Net invite".into(),
            body: "scan the QR or open the URL".into(),
        },
    )?;
    println!("mail.attachments : {}", mail.attachments.len());
    println!("mail.subject     : {}", mail.subject);
    let extracted = InvitationMailer::extract_from_mail(&mail)?;
    let decoded = extracted.decode()?.unwrap();
    println!("back.issuer_wallet : {}", decoded.payload.issuer_wallet);

    println!("\n== Text code round-trip ==");
    let code = create_text_code(&inv)?;
    let printed = code.to_string();
    println!("text_code       : {printed}");
    println!("text_code.len   : {}", printed.len());
    let parsed = parse_text_code(&printed)?.unwrap();
    println!("back.issuer_wallet : {}", parsed.payload.issuer_wallet);
    assert_eq!(parsed.payload.issuer_wallet, inv.payload.issuer_wallet);

    println!("\nALL OK");
    Ok(())
}
