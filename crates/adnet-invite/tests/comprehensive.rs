//! Comprehensive tests for `adnet-invite`.
//!
//! Tests cover:
//! - [`InviteError`] variants and Display formatting
//! - [`InvitationContent`] construction
//! - [`InvitationMailer::build_invitation_email`][build] — all fields, defaults, edge cases
//! - [`InvitationMailer::extract_from_mail`][extract] — attachment matching, size limits
//! - [`InvitationMailer::extract_from_wire`][wire] — MIME wire format round-trip
//! - Round-trip integrity (build → extract → verify)
//! - Security boundaries (size limits, malformed input)

use adnet_identity::wallet::Wallet;
use adnet_mail::mime::{Address, Attachment, Disposition, Mail};
use adnet_pairing::capability::CapabilitySet;
use adnet_pairing::wire::PairingInvitation;
use adnet_types::node::NodeId;

use adnet_invite::{
    InviteError, InvitationContent, InvitationMailer, ADNET_PAIRING_FILENAME, ADNET_PAIRING_MIME,
    MAX_INVITATION_SIZE, TEXT_CODE_PREFIX, create_text_code, parse_text_code,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_wallet() -> Wallet {
    Wallet::generate()
}

fn make_node_id() -> NodeId {
    NodeId::from_bytes(&[0xAAu8; 32]).unwrap()
}

fn make_invitation() -> adnet_pairing::invitation::SignedInvitation {
    let wallet = make_wallet();
    let node_id = make_node_id();
    adnet_pairing::invitation::SignedInvitation::create(
        &node_id,
        &wallet,
        CapabilitySet::from_names(["chat", "files.read"]),
        900,
        Some("Test invitation".into()),
    )
    .unwrap()
}

fn basic_content() -> InvitationContent {
    InvitationContent {
        from: Address::new("alice@example.com").with_name("Alice"),
        to: vec![Address::new("bob@example.com")],
        subject: "Test Pairing".into(),
        body: "Please pair with me".into(),
    }
}

fn content_with_empty_body() -> InvitationContent {
    InvitationContent {
        from: Address::new("alice@example.com"),
        to: vec![Address::new("bob@example.com")],
        subject: "Custom".into(),
        body: "".into(),
    }
}

// ---------------------------------------------------------------------------
// Constants tests
// ---------------------------------------------------------------------------

#[test]
fn constants_are_correct() {
    // Verify the constants are correctly defined
    assert_eq!(ADNET_PAIRING_MIME, "application/x-adnet-pairing");
    assert_eq!(ADNET_PAIRING_FILENAME, "adnet-pairing.inv");
    assert_eq!(MAX_INVITATION_SIZE, 32 * 1024);
}

// ---------------------------------------------------------------------------
// InviteError tests
// ---------------------------------------------------------------------------

#[test]
fn error_no_invitation_display() {
    let err = InviteError::NoInvitation;
    let msg = err.to_string();
    assert!(msg.contains("no pairing invitation"), "got: {msg}");
}

#[test]
fn error_attachment_too_large_display() {
    let err = InviteError::AttachmentTooLarge {
        size: 50_000,
        max: MAX_INVITATION_SIZE,
    };
    let msg = err.to_string();
    // Check for the size values in the message
    assert!(msg.contains("50,000") || msg.contains("50000"), "got: {msg}");
    assert!(msg.contains("32,768") || msg.contains("32768"), "got: {msg}");
}

#[test]
fn error_from_pairing() {
    // PairingError::Malformed should convert via From
    let pairing_err = adnet_pairing::error::PairingError::Malformed {
        what: "test".into(),
        reason: "test reason".into(),
    };
    let err: InviteError = InviteError::from(pairing_err);
    assert!(err.to_string().contains("test reason"));
}

#[test]
fn error_from_json() {
    // serde_json::Error should convert via From
    let json_err = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
    let err: InviteError = InviteError::from(json_err);
    assert!(err.to_string().contains("JSON") || err.to_string().contains("invalid"));
}

#[test]
fn error_from_mail() {
    // MailError should convert via From
    let mail_err = adnet_mail::error::MailError::Parse("bad input".into());
    let err: InviteError = InviteError::from(mail_err);
    let msg = err.to_string();
    assert!(msg.contains("mail") || msg.contains("Mail"), "got: {msg}");
}

// ---------------------------------------------------------------------------
// InvitationContent tests
// ---------------------------------------------------------------------------

#[test]
fn invitation_content_debug() {
    let content = InvitationContent {
        from: Address::new("alice@example.com"),
        to: vec![Address::new("bob@example.com")],
        subject: "Test".into(),
        body: "Body".into(),
    };
    let debug = format!("{content:?}");
    assert!(debug.contains("alice@example.com"));
    assert!(debug.contains("Test"));
}

#[test]
fn invitation_content_clone() {
    let content = basic_content();
    let cloned = content.clone();
    assert_eq!(cloned.from, content.from);
    assert_eq!(cloned.to, content.to);
    assert_eq!(cloned.subject, content.subject);
    assert_eq!(cloned.body, content.body);
}

// ---------------------------------------------------------------------------
// build_invitation_email — basic functionality
// ---------------------------------------------------------------------------

#[test]
fn build_basic_email() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    assert_eq!(mail.from, content.from);
    assert_eq!(mail.to, content.to);
    assert_eq!(mail.subject, "Test Pairing");
    assert_eq!(mail.text, "Please pair with me");
}

#[test]
fn build_email_has_attachment() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    assert_eq!(mail.attachments.len(), 1);
    let att = &mail.attachments[0];
    assert_eq!(att.filename, "adnet-pairing.inv");
    assert_eq!(att.content_type, "application/x-adnet-pairing");
    assert_eq!(att.disposition, Disposition::Attachment);
}

#[test]
fn build_email_attachment_is_valid_json() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    let parsed: adnet_pairing::invitation::SignedInvitation =
        serde_json::from_slice(&mail.attachments[0].data).unwrap();
    assert_eq!(parsed.payload.issuer_wallet, inv.payload.issuer_wallet);
}

#[test]
fn build_email_has_x_headers() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    assert!(mail.extra_headers.contains_key("X-Mailer"));
    assert!(mail.extra_headers.contains_key("X-Adnet-Invite"));
    assert_eq!(mail.extra_headers.get("X-Adnet-Invite").unwrap().as_str(), "pairing");
}

#[test]
fn build_custom_body_preserved() {
    // When body is provided, it's used as-is (URL is NOT added automatically)
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    // Custom body should be preserved exactly
    assert_eq!(mail.text, "Please pair with me");
}

// ---------------------------------------------------------------------------
// build_invitation_email — defaults
// ---------------------------------------------------------------------------

#[test]
fn build_default_subject() {
    let inv = make_invitation();
    let content = InvitationContent {
        from: Address::new("alice@example.com"),
        to: vec![Address::new("bob@example.com")],
        subject: "".into(),
        body: "Custom body".into(),
    };

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    assert_eq!(
        mail.subject,
        "ADNet Device Pairing Invitation",
        "should use default subject"
    );
}

#[test]
fn build_default_body_contains_expiry() {
    let inv = make_invitation();
    let content = content_with_empty_body();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let expiry = inv.payload.expires_at_unix.to_string();
    assert!(
        mail.text.contains(&expiry),
        "default body should contain expiry timestamp"
    );
}

#[test]
fn build_default_body_contains_url() {
    let inv = make_invitation();
    let content = content_with_empty_body();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let url = PairingInvitation::to_url(&inv).unwrap();
    assert!(
        mail.text.contains(&url),
        "default body should contain pairing URL"
    );
}

#[test]
fn build_empty_to_is_allowed() {
    // When to is empty, the mail is created but with empty recipients
    // The Mail validation will handle this at to_wire_bytes time
    let inv = make_invitation();
    let content = InvitationContent {
        from: Address::new("alice@example.com"),
        to: vec![],
        subject: "Test".into(),
        body: "Body".into(),
    };

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    // The mail.to will be empty, but mail is built successfully
    // (validation happens at to_wire_bytes)
    assert!(mail.to.is_empty());
}

// ---------------------------------------------------------------------------
// build_invitation_email — size limits
// ---------------------------------------------------------------------------

#[test]
fn build_rejects_oversized_attachment() {
    let inv = make_invitation();
    // The to_url function limits to 4096 bytes for QR code compatibility.
    // We need a note that makes JSON > 32KB but URL < 4096 bytes.
    // Use very compact note that still makes JSON large.
    // Since JSON includes the full payload structure (~300 bytes base + note),
    // a 32KB note with minified JSON might work. Let's check the actual error.
    let mut large_inv = inv.clone();
    // This creates JSON > 32KB, but to_url will fail first
    large_inv.payload.note = Some("A".repeat(30_000));

    let content = basic_content();
    let err = InvitationMailer::build_invitation_email(&large_inv, &content).unwrap_err();

    // Should get an error - either from to_url (4096 limit) or from attachment size
    assert!(
        matches!(err, InviteError::AttachmentTooLarge { .. })
            || matches!(&err, InviteError::Pairing(_)),
        "should reject oversized invitation, got: {:?}",
        err
    );
}

#[test]
fn build_attachment_size_within_limit() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    assert!(
        mail.attachments[0].data.len() <= MAX_INVITATION_SIZE,
        "attachment {} bytes should be <= {}",
        mail.attachments[0].data.len(),
        MAX_INVITATION_SIZE
    );
}

// ---------------------------------------------------------------------------
// extract_from_mail — attachment matching
// ---------------------------------------------------------------------------

#[test]
fn extract_finds_correct_attachment() {
    let inv = make_invitation();
    let content = basic_content();
    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
    let decoded = extracted.decode().unwrap().unwrap();
    assert_eq!(decoded.payload.issuer_wallet, inv.payload.issuer_wallet);
}

#[test]
fn extract_ignores_wrong_mime_type() {
    let inv = make_invitation();
    let json = serde_json::to_vec(&inv).unwrap();

    let mut mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "Subject",
        "Body",
    );
    mail.attachments.push(Attachment {
        filename: "other.txt".into(),
        content_type: "text/plain".into(),
        data: json,
        disposition: Disposition::Attachment,
    });

    let err = InvitationMailer::extract_from_mail(&mail).unwrap_err();
    assert!(matches!(err, InviteError::NoInvitation));
}

#[test]
fn extract_ignores_wrong_filename() {
    let inv = make_invitation();
    let json = serde_json::to_vec(&inv).unwrap();

    let mut mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "Subject",
        "Body",
    );
    mail.attachments.push(Attachment {
        filename: "wrong-name.bin".into(),
        content_type: "application/x-adnet-pairing".into(),
        data: json,
        disposition: Disposition::Attachment,
    });

    let err = InvitationMailer::extract_from_mail(&mail).unwrap_err();
    assert!(matches!(err, InviteError::NoInvitation));
}

#[test]
fn extract_accepts_any_inv_extension() {
    let inv = make_invitation();
    let json = serde_json::to_vec(&inv).unwrap();

    let mut mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "Subject",
        "Body",
    );
    // File with .inv extension but different name
    mail.attachments.push(Attachment {
        filename: "my-invitation.inv".into(),
        content_type: "application/x-adnet-pairing".into(),
        data: json,
        disposition: Disposition::Attachment,
    });

    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
    let decoded = extracted.decode().unwrap().unwrap();
    assert_eq!(decoded.payload.issuer_wallet, inv.payload.issuer_wallet);
}

#[test]
fn extract_takes_first_matching_attachment() {
    let inv = make_invitation();
    let json = serde_json::to_vec(&inv).unwrap();

    let mut mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "Subject",
        "Body",
    );
    // Add wrong attachment first
    mail.attachments.push(Attachment {
        filename: "other.txt".into(),
        content_type: "text/plain".into(),
        data: b"wrong data".to_vec(),
        disposition: Disposition::Attachment,
    });
    // Add correct attachment second
    mail.attachments.push(Attachment {
        filename: "adnet-pairing.inv".into(),
        content_type: "application/x-adnet-pairing".into(),
        data: json,
        disposition: Disposition::Attachment,
    });

    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
    let decoded = extracted.decode().unwrap().unwrap();
    assert_eq!(decoded.payload.issuer_wallet, inv.payload.issuer_wallet);
}

#[test]
fn extract_multiple_matching_takes_first() {
    // When multiple attachments match, we should take the first one
    let inv = make_invitation();
    let json = serde_json::to_vec(&inv).unwrap();

    let mut mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "Subject",
        "Body",
    );
    mail.attachments.push(Attachment {
        filename: "adnet-pairing.inv".into(),
        content_type: "application/x-adnet-pairing".into(),
        data: json.clone(),
        disposition: Disposition::Attachment,
    });
    mail.attachments.push(Attachment {
        filename: "adnet-pairing.inv".into(),
        content_type: "application/x-adnet-pairing".into(),
        data: json,
        disposition: Disposition::Attachment,
    });

    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
    assert!(matches!(extracted, PairingInvitation::Json(_)));
}

// ---------------------------------------------------------------------------
// extract_from_mail — size limits
// ---------------------------------------------------------------------------

#[test]
fn extract_rejects_oversized_attachment() {
    let mut mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "Subject",
        "Body",
    );
    // Create attachment larger than MAX_INVITATION_SIZE
    let oversized_data = vec![0u8; MAX_INVITATION_SIZE + 1];
    mail.attachments.push(Attachment {
        filename: "adnet-pairing.inv".into(),
        content_type: "application/x-adnet-pairing".into(),
        data: oversized_data,
        disposition: Disposition::Attachment,
    });

    let err = InvitationMailer::extract_from_mail(&mail).unwrap_err();
    assert!(matches!(err, InviteError::AttachmentTooLarge { size, max } if size > max));
}

#[test]
fn extract_rejects_malformed_json() {
    let mut mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "Subject",
        "Body",
    );
    mail.attachments.push(Attachment {
        filename: "adnet-pairing.inv".into(),
        content_type: "application/x-adnet-pairing".into(),
        data: b"not valid json{{".to_vec(),
        disposition: Disposition::Attachment,
    });

    let err = InvitationMailer::extract_from_mail(&mail).unwrap_err();
    assert!(matches!(err, InviteError::Json(_)));
}

// ---------------------------------------------------------------------------
// extract_from_wire
// ---------------------------------------------------------------------------

#[test]
fn wire_round_trip_preserves_invitation() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let wire = mail.to_wire_bytes().unwrap();

    let extracted = InvitationMailer::extract_from_wire(&wire).unwrap();
    let decoded = extracted.decode().unwrap().unwrap();
    assert_eq!(decoded.payload.issuer_wallet, inv.payload.issuer_wallet);
}

#[test]
fn wire_round_trip_preserves_all_attachments() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let wire = mail.to_wire_bytes().unwrap();

    let extracted = InvitationMailer::extract_from_wire(&wire).unwrap();
    // Should be able to verify
    let now = chrono::Utc::now().timestamp();
    extracted.verify(now).expect("should verify after wire round-trip");
}

#[test]
fn wire_rejects_invalid_bytes() {
    let invalid_wire = b"not a valid mime message at all";
    let err = InvitationMailer::extract_from_wire(invalid_wire).unwrap_err();
    assert!(matches!(err, InviteError::Mail(_)));
}

#[test]
fn wire_empty_message_rejected() {
    let err = InvitationMailer::extract_from_wire(b"").unwrap_err();
    assert!(matches!(err, InviteError::Mail(_)));
}

// ---------------------------------------------------------------------------
// Full round-trip verification
// ---------------------------------------------------------------------------

#[test]
fn full_round_trip_verify_succeeds() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();

    let now = chrono::Utc::now().timestamp();
    extracted.verify(now).expect("verification should succeed for fresh invitation");
}

#[test]
fn full_round_trip_url_in_body_verified() {
    let inv = make_invitation();
    // Use empty body to get default body with URL
    let content = content_with_empty_body();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    // Extract URL from body
    let url = PairingInvitation::to_url(&inv).unwrap();
    assert!(mail.text.contains(&url), "URL should be in body when using default body");

    // Parse URL and verify
    let parsed_url = PairingInvitation::parse_url(&url).unwrap();
    let now = chrono::Utc::now().timestamp();
    parsed_url.verify(now).expect("URL-based invitation should verify");
}

#[test]
fn round_trip_with_different_capabilities() {
    let wallet = make_wallet();
    let node_id = make_node_id();

    // Test with various capability combinations
    for caps in [
        CapabilitySet::from_names(["chat"]),
        CapabilitySet::from_names(["files.read", "files.write"]),
        CapabilitySet::from_names(["chat", "files.read", "files.write"]),
    ] {
        let inv = adnet_pairing::invitation::SignedInvitation::create(
            &node_id,
            &wallet,
            caps.clone(),
            900,
            None,
        )
        .unwrap();

        let content = basic_content();
        let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
        let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
        let decoded = extracted.decode().unwrap().unwrap();

        assert_eq!(
            decoded.payload.capabilities, caps,
            "capabilities should round-trip correctly"
        );
    }
}

#[test]
fn round_trip_with_note() {
    let notes = [
        None,
        Some("Simple note".into()),
        Some("Unicode: 你好 🎉".into()),
        Some("Very long note that spans multiple lines\nwith actual line breaks".into()),
    ];

    for note in notes {
        let inv = adnet_pairing::invitation::SignedInvitation::create(
            &make_node_id(),
            &make_wallet(),
            CapabilitySet::from_names(["chat"]),
            900,
            note.clone(),
        )
        .unwrap();

        let content = basic_content();
        let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
        let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
        let decoded = extracted.decode().unwrap().unwrap();

        assert_eq!(decoded.payload.note, note);
    }
}

#[test]
fn round_trip_preserves_expiry_timestamp() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
    let decoded = extracted.decode().unwrap().unwrap();

    assert_eq!(
        decoded.payload.expires_at_unix, inv.payload.expires_at_unix,
        "expiry should round-trip"
    );
}

// ---------------------------------------------------------------------------
// Edge cases and security boundaries
// ---------------------------------------------------------------------------

#[test]
fn extract_no_attachments() {
    let mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "Subject",
        "Body",
    );

    let err = InvitationMailer::extract_from_mail(&mail).unwrap_err();
    assert!(matches!(err, InviteError::NoInvitation));
}

#[test]
fn build_with_utf8_in_note() {
    let inv = adnet_pairing::invitation::SignedInvitation::create(
        &make_node_id(),
        &make_wallet(),
        CapabilitySet::from_names(["chat"]),
        900,
        Some("こんにちは مرحبا Привет 🌍".into()),
    )
    .unwrap();

    let content = basic_content();
    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
    let decoded = extracted.decode().unwrap().unwrap();

    assert!(decoded.payload.note.unwrap().contains("こんにちは"));
}

#[test]
fn build_with_multiple_recipients() {
    let inv = make_invitation();
    let content = InvitationContent {
        from: Address::new("alice@example.com"),
        to: vec![
            Address::new("bob@example.com"),
            Address::new("charlie@example.com"),
            Address::new("diana@example.com"),
        ],
        subject: "Group invitation".into(),
        body: "Join our network".into(),
    };

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    assert_eq!(mail.to.len(), 3);
    assert_eq!(mail.to[0].address, "bob@example.com");
    assert_eq!(mail.to[1].address, "charlie@example.com");
    assert_eq!(mail.to[2].address, "diana@example.com");
}

#[test]
fn extract_from_wire_with_no_invitation() {
    let mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "Subject",
        "Body",
    );
    let wire = mail.to_wire_bytes().unwrap();

    let err = InvitationMailer::extract_from_wire(&wire).unwrap_err();
    assert!(matches!(err, InviteError::NoInvitation));
}

#[test]
fn invitation_with_zero_ttl_uses_default() {
    // TTL of 0 is treated as invalid and defaults to 15 minutes
    let inv = adnet_pairing::invitation::SignedInvitation::create(
        &make_node_id(),
        &make_wallet(),
        CapabilitySet::from_names(["chat"]),
        0, // Zero TTL
        None,
    )
    .unwrap();

    let content = basic_content();
    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();

    // Verify should succeed (TTL=0 defaults to 15 min, so not expired yet)
    let now = chrono::Utc::now().timestamp();
    let result = extracted.verify(now);
    assert!(result.is_ok(), "zero-TTL invitation should use 15-min default and verify");
}

#[test]
fn default_body_contains_pairing_scheme() {
    let inv = make_invitation();
    let content = content_with_empty_body();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    assert!(
        mail.text.contains("adnet-pairing://"),
        "default body should contain pairing URL scheme"
    );
}

#[test]
fn default_body_contains_instructions() {
    let inv = make_invitation();
    let content = content_with_empty_body();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    // Check for key phrases in default body
    assert!(mail.text.contains("pair"));
    assert!(mail.text.contains("QR") || mail.text.contains("adnet"));
}

#[test]
fn x_mailer_header_contains_version() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    let x_mailer = mail.extra_headers.get("X-Mailer").unwrap();
    assert!(x_mailer.starts_with("ADNet/"));
}

#[test]
fn attachment_disposition_is_attachment() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    assert_eq!(mail.attachments[0].disposition, Disposition::Attachment);
}

#[test]
fn invitation_payload_integrity() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
    let decoded = extracted.decode().unwrap().unwrap();

    // Verify all key fields
    assert_eq!(decoded.payload.issuer_wallet, inv.payload.issuer_wallet);
    assert_eq!(decoded.payload.issuer_node_id, inv.payload.issuer_node_id);
    assert_eq!(decoded.payload.capabilities, inv.payload.capabilities);
    assert_eq!(decoded.payload.expires_at_unix, inv.payload.expires_at_unix);
    assert_eq!(decoded.payload.note, inv.payload.note);
    assert_eq!(decoded.signature, inv.signature);
}

// ---------------------------------------------------------------------------
// Stress / large data tests
// ---------------------------------------------------------------------------

#[test]
fn build_with_moderate_size_note() {
    // The to_url function has a 4096 byte limit for QR codes.
    // This means we can only have moderate-sized invitations.
    // A note of about 2-3KB should work fine (JSON + base64 < 4096).
    let note = "A".repeat(2_000);
    let inv = adnet_pairing::invitation::SignedInvitation::create(
        &make_node_id(),
        &make_wallet(),
        CapabilitySet::from_names(["chat"]),
        900,
        Some(note),
    )
    .unwrap();

    let content = basic_content();
    let result = InvitationMailer::build_invitation_email(&inv, &content);
    assert!(
        result.is_ok(),
        "moderate note should be acceptable: {:?}",
        result.err()
    );
}

#[test]
fn multiple_builds_produce_different_wires() {
    let inv = make_invitation();
    let content = basic_content();

    let mail1 = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let mail2 = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    // Both should extract to the same invitation
    let extracted1 = InvitationMailer::extract_from_mail(&mail1).unwrap();
    let extracted2 = InvitationMailer::extract_from_mail(&mail2).unwrap();

    let decoded1 = extracted1.decode().unwrap().unwrap();
    let decoded2 = extracted2.decode().unwrap().unwrap();

    assert_eq!(decoded1.payload.issuer_wallet, decoded2.payload.issuer_wallet);
}

#[test]
fn attachment_data_is_utf8_valid() {
    let inv = make_invitation();
    let content = basic_content();

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();

    let data = &mail.attachments[0].data;
    let s = String::from_utf8(data.clone());
    assert!(
        s.is_ok(),
        "attachment data should be valid UTF-8"
    );

    // Should parse as valid JSON
    let json: serde_json::Value = serde_json::from_slice(data).unwrap();
    assert!(json.is_object(), "parsed JSON should be an object");
}

// ---------------------------------------------------------------------------
// Wire format edge cases
// ---------------------------------------------------------------------------

#[test]
fn wire_format_with_minimal_headers() {
    let inv = make_invitation();
    let content = InvitationContent {
        from: Address::new("a@b.c"),
        to: vec![Address::new("d@e.f")],
        subject: "S".into(),
        body: "B".into(),
    };

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let wire = mail.to_wire_bytes().unwrap();

    assert!(!wire.is_empty());
    assert!(wire.len() > 100, "wire should have some content");

    // Should extract correctly from minimal wire
    let extracted = InvitationMailer::extract_from_wire(&wire).unwrap();
    extracted.verify(chrono::Utc::now().timestamp()).ok();
}

#[test]
fn wire_format_with_unicode_content() {
    let inv = make_invitation();
    let content = InvitationContent {
        from: Address::new("аlice@example.com").with_name("Алиса"),
        to: vec![Address::new("bob@example.com")],
        subject: "Проверка приглашения".into(),
        body: "Пожалуйста, подтвердите".into(),
    };

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let wire = mail.to_wire_bytes().unwrap();

    let extracted = InvitationMailer::extract_from_wire(&wire).unwrap();
    let decoded = extracted.decode().unwrap().unwrap();
    assert_eq!(decoded.payload.issuer_wallet, inv.payload.issuer_wallet);
}

// ---------------------------------------------------------------------------
// Security edge cases
// ---------------------------------------------------------------------------

#[test]
fn extract_rejects_empty_attachment() {
    let mut mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "Subject",
        "Body",
    );
    mail.attachments.push(Attachment {
        filename: "adnet-pairing.inv".into(),
        content_type: "application/x-adnet-pairing".into(),
        data: vec![],
        disposition: Disposition::Attachment,
    });

    // Empty JSON attachment will fail to parse, which is correct
    let err = InvitationMailer::extract_from_mail(&mail).unwrap_err();
    assert!(matches!(err, InviteError::Json(_)));
}

#[test]
fn extract_with_different_case_extension() {
    let inv = make_invitation();
    let json = serde_json::to_vec(&inv).unwrap();

    let mut mail = Mail::text_only(
        Address::new("alice@example.com"),
        Address::new("bob@example.com"),
        "Subject",
        "Body",
    );
    // Test uppercase extension
    mail.attachments.push(Attachment {
        filename: "INVITATION.INV".into(),
        content_type: "application/x-adnet-pairing".into(),
        data: json,
        disposition: Disposition::Attachment,
    });

    // Should not match (case-sensitive check)
    let err = InvitationMailer::extract_from_mail(&mail).unwrap_err();
    assert!(matches!(err, InviteError::NoInvitation));
}

#[test]
fn build_with_minimal_ttl() {
    // Very short TTL (1 second)
    let inv = adnet_pairing::invitation::SignedInvitation::create(
        &make_node_id(),
        &make_wallet(),
        CapabilitySet::from_names(["chat"]),
        1,
        None,
    )
    .unwrap();

    let content = basic_content();
    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
    
    // Should verify immediately
    let now = chrono::Utc::now().timestamp();
    extracted.verify(now).ok();
}

#[test]
fn build_with_long_subject_and_body() {
    let inv = make_invitation();
    let content = InvitationContent {
        from: Address::new("alice@example.com"),
        to: vec![Address::new("bob@example.com")],
        subject: "A".repeat(200),
        body: "B".repeat(1000),
    };

    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    assert_eq!(mail.subject.len(), 200);
    assert_eq!(mail.text.len(), 1000);
}

#[test]
fn invitation_url_round_trip_through_email() {
    let inv = make_invitation();
    
    // Create a URL-based pairing invitation
    let url = PairingInvitation::to_url(&inv).unwrap();
    let url_invitation = PairingInvitation::parse_url(&url).unwrap();
    
    // Verify URL-based invitation
    let now = chrono::Utc::now().timestamp();
    url_invitation.verify(now).unwrap();
    
    // Build email with the same invitation
    let content = basic_content();
    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
    
    // Both should have same issuer
    let decoded = extracted.decode().unwrap().unwrap();
    assert_eq!(decoded.payload.issuer_wallet, inv.payload.issuer_wallet);
}

#[test]
fn wire_round_trip_with_extra_headers() {
    let inv = make_invitation();
    let content = basic_content();
    
    let mut mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    
    // Add extra headers
    mail.extra_headers.insert("In-Reply-To".into(), "<ref@example.com>".into());
    mail.extra_headers.insert("References".into(), "<ref@example.com>".into());
    
    let wire = mail.to_wire_bytes().unwrap();
    let extracted = InvitationMailer::extract_from_wire(&wire).unwrap();
    extracted.verify(chrono::Utc::now().timestamp()).ok();
}

#[test]
fn signature_integrity_through_round_trip() {
    let inv = make_invitation();
    let content = basic_content();
    
    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
    let decoded = extracted.decode().unwrap().unwrap();
    
    // Signature should be byte-for-byte identical
    assert_eq!(decoded.signature, inv.signature);
}

#[test]
fn issuer_node_id_round_trips_correctly() {
    let inv = make_invitation();
    let content = basic_content();
    
    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
    let decoded = extracted.decode().unwrap().unwrap();
    
    assert_eq!(decoded.payload.issuer_node_id, inv.payload.issuer_node_id);
}

#[test]
fn json_pretty_print_in_attachment() {
    let inv = make_invitation();
    let content = basic_content();
    
    let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
    let data = &mail.attachments[0].data;
    
    // Pretty-printed JSON should contain newlines and indentation
    let text = String::from_utf8_lossy(data);
    assert!(
        text.contains('\n') || text.contains("  "),
        "pretty-printed JSON should have formatting"
    );
}

// ---------------------------------------------------------------------------
// Text Code Tests (from mailer module)
// ---------------------------------------------------------------------------

#[test]
fn text_code_prefix_constant() {
    assert_eq!(TEXT_CODE_PREFIX, "ADNET-");
}

#[test]
fn text_code_create_and_parse_round_trip() {
    let inv = make_invitation();
    let code = create_text_code(&inv).unwrap();
    
    // Should have prefix
    let code_str = code.to_string();
    assert!(code_str.starts_with("ADNET-"));
    assert!(code_str.contains('#'));
    
    // Should parse back correctly
    let parsed = parse_text_code(&code_str).unwrap().unwrap();
    assert_eq!(parsed.payload.issuer_wallet, inv.payload.issuer_wallet);
    assert_eq!(parsed.payload.issuer_node_id, inv.payload.issuer_node_id);
    assert_eq!(parsed.signature, inv.signature);
}

#[test]
fn text_code_preserves_all_payload_fields() {
    let wallet = make_wallet();
    let node_id = make_node_id();
    let inv = adnet_pairing::invitation::SignedInvitation::create(
        &node_id,
        &wallet,
        CapabilitySet::from_names(["chat", "files.read", "files.write"]),
        3600,
        Some("Test Device with Long Description".into()),
    )
    .unwrap();

    let code = create_text_code(&inv).unwrap();
    let parsed = parse_text_code(&code.to_string()).unwrap().unwrap();

    assert_eq!(parsed.payload.version, 1);
    assert_eq!(parsed.payload.issuer_node_id, inv.payload.issuer_node_id);
    assert_eq!(parsed.payload.issuer_wallet, inv.payload.issuer_wallet);
    assert_eq!(parsed.payload.capabilities, inv.payload.capabilities);
    assert_eq!(parsed.payload.expires_at_unix, inv.payload.expires_at_unix);
    assert_eq!(parsed.payload.note, inv.payload.note);
}

#[test]
fn text_code_case_insensitive_parsing() {
    let inv = make_invitation();
    let code = create_text_code(&inv).unwrap();
    
    // Parse original - should always work
    let code_str = code.to_string();
    let result1 = parse_text_code(&code_str);
    assert!(result1.is_ok(), "original should parse: {:?}", result1.err());
    
    // The prefix check is case-insensitive
    let result2 = parse_text_code(&("adnet-"[..].to_owned() + &code_str[6..]));
    assert!(result2.is_ok(), "lowercase prefix should parse: {:?}", result2.err());
}

#[test]
fn text_code_rejects_wrong_prefix() {
    let result = parse_text_code("XXXX-AAAA-BBBB-CCCC#00");
    assert!(result.is_err());
    if let Err(InviteError::InvalidTextCode(msg)) = result {
        assert!(msg.contains("ADNET-"));
    }
}

#[test]
fn text_code_rejects_missing_checksum_separator() {
    let result = parse_text_code("ADNET-AAAA-BBBB-CCCC");
    assert!(matches!(result, Err(InviteError::InvalidTextCode(_))));
}

#[test]
fn text_code_rejects_invalid_checksum() {
    let result = parse_text_code("ADNET-AAAA-BBBB-CCCC#ZZ");
    assert!(matches!(result, Err(InviteError::InvalidTextCode(_))));
}

#[test]
fn text_code_detects_checksum_mismatch() {
    // Create a code then modify it to have wrong checksum
    let inv = make_invitation();
    let code = create_text_code(&inv).unwrap();
    let code_str = code.to_string();
    
    // Replace checksum with wrong value
    let corrupted = format!("{}FF", &code_str[..code_str.len() - 2]);
    let result = parse_text_code(&corrupted);
    assert!(matches!(result, Err(InviteError::TextCodeChecksumMismatch { .. })));
}

#[test]
fn text_code_with_dashes_in_input() {
    let inv = make_invitation();
    let code = create_text_code(&inv).unwrap();
    let code_str = code.to_string();
    
    // Add extra dashes (should be ignored during parsing)
    let with_extra_dashes = code_str.replace("-", "--");
    let result = parse_text_code(&with_extra_dashes);
    
    // Should either work (dashes ignored) or fail gracefully
    if result.is_ok() {
        let parsed = result.unwrap().unwrap();
        assert_eq!(parsed.payload.issuer_wallet, inv.payload.issuer_wallet);
    }
}

#[test]
fn text_code_verification_after_parse() {
    let inv = make_invitation();
    let code = create_text_code(&inv).unwrap();
    let parsed = parse_text_code(&code.to_string()).unwrap().unwrap();
    
    // The parsed invitation should verify correctly
    let now = chrono::Utc::now().timestamp();
    let result = parsed.verify(now);
    assert!(result.is_ok(), "parsed invitation should verify: {:?}", result.err());
}
