//! Example: send a pairing invitation email with embedded QR code.
//!
//! This demonstrates how to:
//! 1. Create a signed pairing invitation
//! 2. Build an email with inline QR code image
//!
//! ```bash
//! # Run to generate and display the invitation email:
//! cargo run -p adnet-invite --example send_pairing_invite
//! ```

use adnet_identity::wallet::Wallet;
use adnet_invite::{InvitationContent, InvitationMailer};
use adnet_mail::mime::Address;
use adnet_pairing::capability::CapabilitySet;
use adnet_types::node::NodeId;

fn main() -> anyhow::Result<()> {
    // 1. Generate a node identity (in production, this comes from the persistent store)
    let wallet = Wallet::generate();
    let node_id = NodeId::from_bytes(&[0xAAu8; 32]).unwrap();

    // 2. Create a signed pairing invitation (valid for 15 minutes)
    let invitation = adnet_pairing::SignedInvitation::create(
        &node_id,
        &wallet,
        CapabilitySet::from_names(["chat"]),
        15 * 60, // 15 min TTL
        Some("My Laptop".into()),
    )?;
    println!("Created pairing invitation:");
    println!("  issuer_wallet: {:?}", invitation.payload.issuer_wallet);
    println!("  expires_at: {}", invitation.payload.expires_at_unix);
    println!();

    // 3. Build the email content
    let content = InvitationContent {
        from: Address::new("alice@example.com").with_name("Alice"),
        to: vec![Address::new("bob@example.com")],
        subject: "ADNet Device Pairing Invitation".into(),
        body: "Hi Bob, please pair with my ADNet device using the attached QR code.".into(),
    };

    // 4. Build the invitation email WITH embedded QR code
    println!("Building invitation email with QR code...");
    let mail = InvitationMailer::build_invitation_email_with_qr(&invitation, &content)?;
    println!("Email built successfully!");
    println!();

    // Show email structure
    println!("Email structure:");
    println!("  From: {}", mail.from);
    println!("  To: {:?}", mail.to);
    println!("  Subject: {}", mail.subject);
    println!("  Attachments: {} (inline QR + pairing JSON)", mail.attachments.len());
    for (i, att) in mail.attachments.iter().enumerate() {
        let disp = if att.disposition == adnet_mail::mime::Disposition::Inline {
            "inline"
        } else {
            "attachment"
        };
        println!(
            "    [{}] {} ({}): {} bytes",
            i,
            att.filename,
            disp,
            att.data.len()
        );
    }
    println!("  Has HTML body: {}", mail.html.is_some());
    println!();

    // 5. Encode to wire bytes and display
    println!("Generating RFC 5322 wire bytes...");
    let wire = mail.to_wire_bytes()?;
    println!("Wire size: {} bytes", wire.len());
    println!();

    // Show a preview of the wire format
    let preview = String::from_utf8_lossy(&wire);
    let preview_lines: Vec<&str> = preview.lines().take(40).collect();
    println!("Wire preview (first 40 lines):");
    println!("{}", "─".repeat(60));
    for line in preview_lines {
        println!("{}", line);
    }
    let total_lines = preview.lines().count();
    if total_lines > 40 {
        println!("... ({} more lines)", total_lines - 40);
    }
    println!("{}", "─".repeat(60));
    println!();

    // 6. Show the pairing URL (for testing without email)
    let url = adnet_pairing::wire::PairingInvitation::to_url(&invitation)?;
    println!("Pairing URL (for testing in QR scanner):");
    println!("{}", url);
    println!();

    // 7. Instructions for using SMTP
    println!("To send this email via SMTP, use the adnet-mail crate:");
    println!("  use adnet_mail::smtp::{{connect, send::send_raw}};");
    println!("  use adnet_mail::login_param::Account;");
    println!();
    println!("  let account = Account::smtp_default(\"alice@example.com\")");
    println!("      .with_smtp_password(\"your-password\");");
    println!("  let mut transport = connect(&account).await?;");
    println!("  send_raw(&mut transport, \"alice@example.com\", &[\"bob@example.com\"], &wire).await?;");

    Ok(())
}
