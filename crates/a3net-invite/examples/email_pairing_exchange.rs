//! Example: exchange pairing invitations between two nodes via email with QR codes.
//!
//! This simulates the scenario where:
//! 1. Alice creates a pairing invitation and sends it to Bob via email
//! 2. Bob receives the email, extracts the invitation
//! 3. Bob creates his own invitation and sends it back to Alice
//! 4. Both devices have the information needed to complete pairing
//!
//! ```bash
//! cargo run -p a3net-invite --example email_pairing_exchange
//! ```

use a3net_identity::wallet::Wallet;
use a3net_invite::{InvitationContent, InvitationMailer};
use a3net_mail::mime::Mail;
use a3net_pairing::capability::CapabilitySet;
use a3net_types::node::NodeId;

fn main() -> anyhow::Result<()> {
    println!("╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║           A3Net Pairing via Email with QR Code Exchange Demo                 ║");
    println!("╚══════════════════════════════════════════════════════════════════════════════╝");
    println!();

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 1: Alice creates her pairing invitation
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("┌─ Step 1: Alice creates a pairing invitation");
    println!("│");

    let alice_wallet = Wallet::generate();
    let alice_node_id = NodeId::from_bytes(&[0xAAu8; 32]).unwrap();
    let alice_invitation = a3net_pairing::SignedInvitation::create(
        &alice_node_id,
        &alice_wallet,
        CapabilitySet::from_names(["chat"]),
        15 * 60, // 15 min TTL
        Some("Alice's Laptop".into()),
    )?;
    println!("│  ✓ Alice created invitation");
    println!("│    node_id:    {}", alice_node_id.short());
    println!("│    wallet_addr: {:?}", alice_wallet.public().address());
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 2: Alice builds email with QR code
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 2: Alice builds invitation email with QR code");
    println!("│");

    let alice_content = InvitationContent {
        from: a3net_mail::mime::Address::new("alice@example.com").with_name("Alice"),
        to: vec![a3net_mail::mime::Address::new("bob@example.com")],
        subject: "A3Net Pairing Invitation".into(),
        body: "Hi Bob, please pair with me using the QR code below!".into(),
    };

    let alice_mail = InvitationMailer::build_invitation_email_with_qr(
        &alice_invitation,
        &alice_content,
    )?;
    println!("│  ✓ Alice's email built");
    println!("│    Subject: {}", alice_mail.subject);
    println!("│    Attachments:");

    // Show attachment details
    for att in &alice_mail.attachments {
        let disp = if att.disposition == a3net_mail::mime::Disposition::Inline {
            "[INLINE]"
        } else {
            "[ATTACH]"
        };
        let size_kb = att.data.len() / 1024;
        println!("│      {} {} ({} KB)", disp, att.filename, size_kb);
    }
    println!("│    HTML body: {}", if alice_mail.html.is_some() { "yes" } else { "no" });
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 3: Bob receives email, extracts Alice's invitation
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 3: Bob receives email and extracts Alice's invitation");
    println!("│");

    // Simulate Bob receiving the email (round-trip through wire bytes)
    let wire = alice_mail.to_wire_bytes()?;
    let received_mail = Mail::from_wire_bytes(&wire)?;
    let alice_invitation_extracted = InvitationMailer::extract_from_mail(&received_mail)?;
    let alice_inv_verified = alice_invitation_extracted.decode()?.unwrap();

    println!("│  ✓ Bob extracted Alice's invitation from email");
    let alice_node_hex = hex::encode(&alice_inv_verified.payload.issuer_node_id.as_bytes()[..8]);
    println!("│    issuer_node_id: {}...", &alice_node_hex);
    println!("│    expires_at:     {} (Unix timestamp)", alice_inv_verified.payload.expires_at_unix);
    println!("│    note:           {:?}", alice_inv_verified.payload.note);
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 4: Bob creates his own pairing invitation
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 4: Bob creates his own pairing invitation");
    println!("│");

    let bob_wallet = Wallet::generate();
    let bob_node_id = NodeId::from_bytes(&[0xBBu8; 32]).unwrap();
    let bob_invitation = a3net_pairing::SignedInvitation::create(
        &bob_node_id,
        &bob_wallet,
        CapabilitySet::from_names(["chat"]),
        15 * 60,
        Some("Bob's Phone".into()),
    )?;
    println!("│  ✓ Bob created invitation");
    println!("│    node_id:    {}", bob_node_id.short());
    println!("│    wallet_addr: {:?}", bob_wallet.public().address());
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 5: Bob sends his invitation back to Alice
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 5: Bob sends his invitation email to Alice");
    println!("│");

    let bob_content = InvitationContent {
        from: a3net_mail::mime::Address::new("bob@example.com").with_name("Bob"),
        to: vec![a3net_mail::mime::Address::new("alice@example.com")],
        subject: "A3Net Pairing - My Response".into(),
        body: "Hi Alice, here is my pairing invitation!".into(),
    };

    let bob_mail = InvitationMailer::build_invitation_email_with_qr(
        &bob_invitation,
        &bob_content,
    )?;
    println!("│  ✓ Bob's email built");
    println!("│    Subject: {}", bob_mail.subject);
    println!("│    Attachments:");
    for att in &bob_mail.attachments {
        let disp = if att.disposition == a3net_mail::mime::Disposition::Inline {
            "[INLINE]"
        } else {
            "[ATTACH]"
        };
        let size_kb = att.data.len() / 1024;
        println!("│      {} {} ({} KB)", disp, att.filename, size_kb);
    }
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 6: Alice receives Bob's email
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 6: Alice receives Bob's email");
    println!("│");

    let bob_wire = bob_mail.to_wire_bytes()?;
    let bob_received_mail = Mail::from_wire_bytes(&bob_wire)?;
    let bob_invitation_extracted = InvitationMailer::extract_from_mail(&bob_received_mail)?;
    let bob_inv_verified = bob_invitation_extracted.decode()?.unwrap();

    println!("│  ✓ Alice extracted Bob's invitation");
    let bob_node_hex = hex::encode(&bob_inv_verified.payload.issuer_node_id.as_bytes()[..8]);
    println!("│    issuer_node_id: {}...", &bob_node_hex);
    println!("│    expires_at:     {} (Unix timestamp)", bob_inv_verified.payload.expires_at_unix);
    println!("│    note:           {:?}", bob_inv_verified.payload.note);
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 7: Both verify the signatures
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 7: Both sides verify the invitations");
    println!("│");

    let now = chrono::Utc::now().timestamp();

    match alice_invitation_extracted.verify(now) {
        Ok(_) => println!("│  ✓ Alice's invitation (from Bob) verified"),
        Err(e) => println!("│  ✗ Alice's invitation verification failed: {}", e),
    }

    match bob_invitation_extracted.verify(now) {
        Ok(_) => println!("│  ✓ Bob's invitation (from Alice) verified"),
        Err(e) => println!("│  ✗ Bob's invitation verification failed: {}", e),
    }
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // SUMMARY
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("└──────────────────────────────────────────────────────────────────────────────");
    println!("║  ✓ PAIRING EXCHANGE COMPLETE                                                   ║");
    println!("╠══════════════════════════════════════════════════════════════════════════════╣");
    println!("║                                                                              ║");
    println!("║  Both devices have exchanged pairing invitations via email:                  ║");
    println!("║                                                                              ║");
    println!("║    Alice: node_id = {}, wallet = {:?}", alice_node_id.short(),
        alice_wallet.public().address());
    println!("║    Bob:   node_id = {}, wallet = {:?}", bob_node_id.short(),
        bob_wallet.public().address());
    println!("║                                                                              ║");
    println!("║  In a real scenario, these invitations would be used in the pairing         ║");
    println!("║  ceremony to establish a trusted device relationship.                      ║");
    println!("║                                                                              ║");
    println!("╚══════════════════════════════════════════════════════════════════════════════╝");

    Ok(())
}
