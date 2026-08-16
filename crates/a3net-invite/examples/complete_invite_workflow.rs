//! Example: Complete invitation workflow demonstrating all formats.
//!
//! This example demonstrates the full pairing invitation workflow including:
//! - Creating signed invitations
//! - Generating text codes (for SMS/verbal communication)
//! - Generating QR codes (for visual scanning)
//! - Building emails with attachments
//! - Parsing and verifying received invitations
//!
//! ```bash
//! cargo run -p a3net-invite --example complete_invite_workflow
//! ```

use a3net_identity::wallet::Wallet;
use a3net_invite::{
    create_text_code, parse_text_code, InvitationContent, InvitationMailer,
};
use a3net_mail::mime::Address;
use a3net_pairing::capability::CapabilitySet;
use a3net_types::node::NodeId;

fn main() -> anyhow::Result<()> {
    println!("╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║           A3Net Complete Pairing Invitation Workflow Demo                   ║");
    println!("╚══════════════════════════════════════════════════════════════════════════════╝");
    println!();

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 1: Generate Alice's identity
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("┌─ Step 1: Alice generates her identity");
    println!("│");

    let alice_wallet = Wallet::generate();
    let alice_node_id = NodeId::from_bytes(&[0xAAu8; 32]).unwrap();

    println!("│  ✓ Generated Alice's wallet and node identity");
    println!("│    Node ID: {}", alice_node_id);
    println!("│    Wallet address: {:?}", alice_wallet.public().address());
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 2: Create a signed pairing invitation
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 2: Alice creates a signed pairing invitation");
    println!("│");

    let capabilities = CapabilitySet::from_names(["chat", "files.read"]);
    let ttl_seconds = 15 * 60; // 15 minutes

    let alice_invitation = a3net_pairing::SignedInvitation::create(
        &alice_node_id,
        &alice_wallet,
        capabilities.clone(),
        ttl_seconds,
        Some("Alice's Laptop".into()),
    )?;

    println!("│  ✓ Created signed invitation");
    println!("│    Version:       {}", alice_invitation.payload.version);
    println!("│    Issuer Node:  {}", alice_invitation.payload.issuer_node_id);
    println!("│    Capabilities:  chat, files.read");
    println!("│    Expires at:    {} (Unix timestamp)", alice_invitation.payload.expires_at_unix);
    println!("│    Note:          \"Alice's Laptop\"");
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 3: Generate Text Code (for SMS or verbal sharing)
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 3: Generate text code (for SMS/verbal sharing)");
    println!("│");

    let text_code = create_text_code(&alice_invitation)?;

    println!("│  ✓ Generated human-readable text code:");
    println!("│");
    
    // Display the text code with nice formatting
    let code_str = text_code.to_string();
    let lines: Vec<String> = code_str
        .chars()
        .collect::<Vec<_>>()
        .chunks(60)
        .map(|c| c.iter().collect::<String>())
        .collect();
    
    for line in &lines {
        println!("│    {}", line);
    }
    
    println!("│");
    println!("│  Text code format: ADNET-XXXX-XXXX-XXXX-XXXX#CC");
    println!("│    - ADNET- prefix identifies this as an A3Net code");
    println!("│    - 16 alphanumeric characters encode the invitation");
    println!("│    - #CC is a 2-digit CRC8 checksum for error detection");
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 4: Parse and verify the text code
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 4: Parse and verify the text code");
    println!("│");

    let parsed_from_text = parse_text_code(&code_str)?;
    
    match parsed_from_text {
        Some(parsed_inv) => {
            println!("│  ✓ Successfully parsed text code");
            println!("│    Issuer Node:  {}", parsed_inv.payload.issuer_node_id);
            println!("│    Expires at:   {}", parsed_inv.payload.expires_at_unix);
            
            // Verify the signature
            let now = chrono::Utc::now().timestamp();
            match parsed_inv.verify(now) {
                Ok(_) => println!("│    Signature:     ✓ Valid"),
                Err(e) => println!("│    Signature:     ✗ Invalid: {}", e),
            }
        }
        None => {
            println!("│  ✗ Failed to parse text code");
        }
    }
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 5: Generate QR code URL
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 5: Generate QR code URL");
    println!("│");

    let qr_url = a3net_pairing::wire::PairingInvitation::to_url(&alice_invitation)?;

    println!("│  ✓ Generated QR code URL:");
    println!("│    {}...", &qr_url[..60.min(qr_url.len())]);
    if qr_url.len() > 60 {
        println!("│      ... ({} total chars)", qr_url.len());
    }
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 6: Generate QR code SVG
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 6: Generate QR code SVG image");
    println!("│");

    let qr_svg = a3net_qr::generator::create_qr_svg(&qr_url)?;
    println!("│  ✓ Generated QR code SVG");
    println!("│    SVG size: {} bytes", qr_svg.len());
    println!("│    Preview (first 100 chars):");
    
    let svg_preview = &qr_svg[..100.min(qr_svg.len())];
    println!("│      {}", svg_preview.replace('\n', " "));
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 7: Build invitation email (plain text version)
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 7: Build invitation email (plain text)");
    println!("│");

    let email_content = InvitationContent {
        from: Address::new("alice@example.com").with_name("Alice"),
        to: vec![Address::new("bob@example.com")],
        subject: "A3Net Pairing Invitation".into(),
        body: "Hi Bob! Please pair with my A3Net device using the attached file.".into(),
    };

    let plain_email = InvitationMailer::build_invitation_email(&alice_invitation, &email_content)?;

    println!("│  ✓ Built plain text invitation email");
    println!("│    From:    {}", plain_email.from);
    println!("│    To:      {:?}", plain_email.to);
    println!("│    Subject: {}", plain_email.subject);
    println!("│    Attachments: {} (JSON pairing file)", plain_email.attachments.len());
    for att in &plain_email.attachments {
        println!("│      - {} ({} bytes)", att.filename, att.data.len());
    }
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 8: Build invitation email (with QR code)
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 8: Build invitation email (with embedded QR code)");
    println!("│");

    let qr_email = InvitationMailer::build_invitation_email_with_qr(&alice_invitation, &email_content)?;

    println!("│  ✓ Built QR-enabled invitation email");
    println!("│    From:    {}", qr_email.from);
    println!("│    To:      {:?}", qr_email.to);
    println!("│    Subject: {}", qr_email.subject);
    println!("│    Has HTML body: {}", qr_email.html.is_some());
    println!("│    Attachments: {} (JSON + QR SVG)", qr_email.attachments.len());
    for att in &qr_email.attachments {
        let kind = if att.filename.ends_with(".svg") { "QR SVG" } else { "JSON" };
        println!("│      - {}: {} ({} bytes)", kind, att.filename, att.data.len());
    }
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 9: Simulate email transmission and reception
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 9: Simulate email transmission and reception");
    println!("│");

    // Alice sends the email
    let wire_bytes = qr_email.to_wire_bytes()?;
    println!("│  → Alice's email encoded to {} bytes (RFC 5322 wire format)", wire_bytes.len());

    // Bob receives and parses the email
    let received_mail = a3net_mail::mime::Mail::from_wire_bytes(&wire_bytes)?;
    println!("│  ← Bob received email: {} bytes parsed", wire_bytes.len());

    // Bob extracts the invitation
    let bob_extracted = InvitationMailer::extract_from_mail(&received_mail)?;
    println!("│  ✓ Bob extracted pairing invitation from email attachment");

    // Bob verifies the invitation
    let now = chrono::Utc::now().timestamp();
    match bob_extracted.verify(now) {
        Ok(_) => {
            println!("│  ✓ Bob verified the invitation signature");
            let decoded = bob_extracted.decode()?.unwrap();
            println!("│    Issuer: {}", decoded.payload.issuer_node_id);
            println!("│    Expires: {}", decoded.payload.expires_at_unix);
        }
        Err(e) => {
            println!("│  ✗ Verification failed: {}", e);
        }
    }
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // STEP 10: Simulate text code transmission (SMS scenario)
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("├─ Step 10: Simulate text code via SMS");
    println!("│");

    // Alice sends the text code via SMS
    println!("│  → Alice sends text code via SMS:");
    println!("│    \"Pair with me: {}...\"", &code_str[..50.min(code_str.len())]);
    println!("│");

    // Bob receives and enters the code manually
    println!("│  ← Bob receives SMS and enters the code:");
    println!("│    {}", code_str);
    println!("│");

    match parse_text_code(&code_str) {
        Ok(Some(inv)) => {
            let now = chrono::Utc::now().timestamp();
            match inv.verify(now) {
                Ok(_) => {
                    println!("│  ✓ Bob verified the text code invitation");
                    println!("│    Issuer: {}", inv.payload.issuer_node_id);
                }
                Err(e) => {
                    println!("│  ✗ Verification failed: {}", e);
                }
            }
        }
        Ok(None) => {
            println!("│  ✗ Code not recognized");
        }
        Err(e) => {
            println!("│  ✗ Code parse error: {}", e);
        }
    }
    println!("│");

    // ═══════════════════════════════════════════════════════════════════════════════════
    // SUMMARY
    // ═══════════════════════════════════════════════════════════════════════════════════
    println!("└──────────────────────────────────────────────────────────────────────────────");
    println!("║  ✓ COMPLETE WORKFLOW DEMONSTRATED                                        ║");
    println!("╠══════════════════════════════════════════════════════════════════════════════╣");
    println!("║                                                                              ║");
    println!("║  Supported invitation formats:                                              ║");
    println!("║                                                                              ║");
    println!("║    1. Email with JSON attachment                                           ║");
    println!("║       → `build_invitation_email()`                                         ║");
    println!("║       → `extract_from_mail()` to parse                                    ║");
    println!("║                                                                              ║");
    println!("║    2. Email with JSON attachment + QR code SVG                             ║");
    println!("║       → `build_invitation_email_with_qr()`                                ║");
    println!("║       → QR code can be scanned directly from email                         ║");
    println!("║                                                                              ║");
    println!("║    3. Text code (for SMS or verbal sharing)                                ║");
    println!("║       → `create_text_code()` to generate                                  ║");
    println!("║       → `parse_text_code()` to parse                                      ║");
    println!("║       → Includes CRC8 checksum for error detection                         ║");
    println!("║                                                                              ║");
    println!("║    4. QR URL (for scanning)                                               ║");
    println!("║       → `PairingInvitation::to_url()` to generate                          ║");
    println!("║       → `PairingInvitation::parse_url()` to parse                          ║");
    println!("║                                                                              ║");
    println!("║  All formats include cryptographic signatures that can be verified          ║");
    println!("║  without revealing any secrets.                                            ║");
    println!("║                                                                              ║");
    println!("╚══════════════════════════════════════════════════════════════════════════════╝");

    Ok(())
}
