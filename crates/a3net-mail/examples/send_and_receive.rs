//! # end-to-end example
//!
//! Build a structured [`Mail`], encode it to wire bytes, and decode it
//! back. This exercises the MIME layer in isolation — no real SMTP /
//! IMAP round-trip, so it runs without network access.
//!
//! ```bash
//! cargo run -p a3net-mail --example send_and_receive
//! ```
//!
//! The `MailAccountOnline` half of the API requires a real server; see
//! the `with_mock_server` test under `tests/integration.rs` for a
//! Tokio-based loopback IMAP / SMTP wire-protocol mock.

use a3net_mail::prelude::*;

fn main() -> Result<()> {
    let alice = Address::new("alice@example.com").with_name("Alice Example");
    let bob = Address::new("bob@example.com");

    // 1. Compose a message.
    let mut mail = Mail::text_only(
        alice.clone(),
        bob.clone(),
        "Quarterly report",
        "Hi Bob,\r\n\r\nThe Q3 numbers are in. See attached PDF.\r\n",
    );
    mail.html = Some("<p>Hi Bob,</p><p>The Q3 numbers are in. See attached PDF.</p>".into());
    mail.cc.push(Address::new("cfo@example.com"));
    mail.attachments.push(Attachment {
        filename: "q3-2026.txt".into(),
        content_type: "text/plain".into(),
        data: b"Q3 revenue: $4.2M (+18% YoY)\r\n".to_vec(),
        disposition: Disposition::Attachment,
    });
    mail.attachments.push(Attachment {
        filename: "logo.png".into(),
        content_type: "image/png".into(),
        data: vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A], // PNG magic
        disposition: Disposition::Inline,
    });
    mail.extra_headers
        .insert("X-Mailer".into(), "a3net-mail/0.1".into());
    mail.extra_headers
        .insert("In-Reply-To".into(), "<prev-msg@example.com>".into());

    println!(
        "Composed mail with {} attachment(s)",
        mail.attachments.len()
    );
    println!("  From:    {}", mail.from);
    println!("  To:      {:?}", mail.to);
    println!("  Cc:      {:?}", mail.cc);
    println!("  Subject: {}", mail.subject);

    // 2. Validate before serializing.
    mail.validate()?;
    println!("Validation: OK");

    // 3. Encode to RFC 5322 wire bytes.
    let bytes = mail.to_wire_bytes()?;
    println!("\nEncoded {} bytes:\n---", bytes.len());
    print!("{}", String::from_utf8_lossy(&bytes));
    println!("---");

    // 4. Decode the same bytes back.
    let parsed = Mail::from_wire_bytes(&bytes)?;
    println!("\nDecoded back:");
    println!("  From:    {}", parsed.from);
    println!("  To:      {:?}", parsed.to);
    println!("  Cc:      {:?}", parsed.cc);
    println!("  Subject: {}", parsed.subject);
    println!("  Body:    {}", parsed.text);
    println!("  HTML:    {}", parsed.html.is_some());
    println!("  Attachments:");
    for a in &parsed.attachments {
        println!(
            "    - {} ({}): {} bytes",
            a.filename,
            a.content_type,
            a.data.len()
        );
    }

    // 5. JSON round-trip for IPC persistence.
    let j = serde_json::to_string(&mail).expect("serialize");
    let from_json: Mail = serde_json::from_str(&j).expect("deserialize");
    assert_eq!(mail.subject, from_json.subject);
    println!("\nJSON round-trip OK ({} bytes)", j.len());

    Ok(())
}
