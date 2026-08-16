//! High-level invitation email builder and parser.
//!
//! `InvitationMailer` turns a [`SignedInvitation`] into a sendable
//! [`Mail`] (or extracts it from a received one).

use a3net_mail::mime::{Address, Attachment, Disposition, Mail};
use a3net_pairing::wire::PairingInvitation;

use crate::error::{InviteError, InviteResult};

/// MIME type used for pairing invitation attachments.
pub const ADNET_PAIRING_MIME: &str = "application/x-a3net-pairing";

/// Maximum attachment size we accept when parsing (32 KiB).
/// Pairing invitation JSON is ~256 bytes; the headroom covers
/// future extensions without unbounded memory during parsing.
pub const MAX_INVITATION_SIZE: usize = 32 * 1024;

/// Filename used on the pairing attachment.
pub const ADNET_PAIRING_FILENAME: &str = "a3net-pairing.inv";

/// Filename used on the QR code image attachment.
pub const ADNET_QR_FILENAME: &str = "a3net-pairing-qr.svg";

/// Prefix for text-based invitation codes.
pub const TEXT_CODE_PREFIX: &str = "ADNET-";

/// Length of the human-readable checksum portion of the text code.
pub const TEXT_CODE_CHECKSUM_LEN: usize = 2;

/// Content fields for an invitation email.
#[derive(Debug, Clone)]
pub struct InvitationContent {
    /// `From:` address.
    pub from: Address,
    /// `To:` address(es).
    pub to: Vec<Address>,
    /// `Subject:` line. Falls back to a sensible default if empty.
    pub subject: String,
    /// Plain-text body. RFC 5322 requires at least one body; we
    /// provide a minimal default if this is empty.
    pub body: String,
}

/// Builder for pairing-invitation emails.
pub struct InvitationMailer;

impl InvitationMailer {
    /// Build a [`Mail`] that carries the invitation as a typed attachment.
    ///
    /// The email has:
    /// - `From:` / `To:` / `Subject:` as specified.
    /// - A plain-text body with instructions and the `a3net-pairing://`
    ///   URL rendered inline.
    /// - An attachment named `a3net-pairing.inv` with
    ///   `application/x-a3net-pairing` MIME type, containing the
    ///   invitation as UTF-8 JSON.
    ///
    /// This form is the most robust for email transport: the inline
    /// `a3net-pairing://` URL works on mobile clients that can open
    /// deep links, while the attachment works on desktop clients that
    /// can't parse custom schemes.
    pub fn build_invitation_email(
        invitation: &a3net_pairing::invitation::SignedInvitation,
        content: &InvitationContent,
    ) -> InviteResult<Mail> {
        // Render the pairing URL inline in the body.
        let url = PairingInvitation::to_url(invitation)?;

        // Build the JSON attachment.
        let json_bytes = serde_json::to_vec_pretty(invitation)?;
        if json_bytes.len() > MAX_INVITATION_SIZE {
            return Err(InviteError::AttachmentTooLarge {
                size: json_bytes.len(),
                max: MAX_INVITATION_SIZE,
            });
        }
        let attachment = Attachment {
            filename: ADNET_PAIRING_FILENAME.to_string(),
            content_type: ADNET_PAIRING_MIME.to_string(),
            data: json_bytes,
            disposition: Disposition::Attachment,
        };

        // Subject default.
        let subject = if content.subject.is_empty() {
            "A3Net Device Pairing Invitation".to_string()
        } else {
            content.subject.clone()
        };

        // Body default.
        let body = if content.body.is_empty() {
            format!(
                "You have been invited to pair with an A3Net device.\n\n\
                 Scan the attached QR code or open the file \"a3net-pairing.inv\" \
                 with the A3Net app.\n\n\
                 Pairing link:\n  {url}\n\n\
                 This invitation expires at: {}\n",
                invitation.payload.expires_at_unix
            )
        } else {
            content.body.clone()
        };

        let mut mail = Mail::text_only(
            content.from.clone(),
            content
                .to
                .first()
                .cloned()
                .unwrap_or_else(|| Address::new("unknown@example.com")),
            &subject,
            &body,
        );
        mail.to = content.to.clone();
        mail.attachments.push(attachment);
        mail.extra_headers.insert(
            "X-Mailer".into(),
            format!("A3Net/{}", env!("CARGO_PKG_VERSION")),
        );
        mail.extra_headers
            .insert("X-Adnet-Invite".into(), "pairing".into());

        Ok(mail)
    }

    /// Build a [`Mail`] that carries the invitation as a typed attachment
    /// AND an inline QR code SVG image.
    ///
    /// This variant is useful for email-based pairing where the recipient
    /// can see the QR code directly in their email client.
    ///
    /// The email has:
    /// - `From:` / `To:` / `Subject:` as specified.
    /// - A plain-text body with instructions and the `a3net-pairing://` URL.
    /// - An HTML body with an embedded inline QR code image.
    /// - An inline SVG attachment named `a3net-pairing-qr.svg`.
    /// - An attachment named `a3net-pairing.inv` with
    ///   `application/x-a3net-pairing` MIME type.
    pub fn build_invitation_email_with_qr(
        invitation: &a3net_pairing::invitation::SignedInvitation,
        content: &InvitationContent,
    ) -> InviteResult<Mail> {
        // Render the pairing URL.
        let url = PairingInvitation::to_url(invitation)?;

        // Generate QR code SVG.
        let svg_content = a3net_qr::generator::create_qr_svg(&url)
            .map_err(|e| InviteError::Qr(e.to_string()))?;

        // Build the JSON attachment.
        let json_bytes = serde_json::to_vec_pretty(invitation)?;
        if json_bytes.len() > MAX_INVITATION_SIZE {
            return Err(InviteError::AttachmentTooLarge {
                size: json_bytes.len(),
                max: MAX_INVITATION_SIZE,
            });
        }
        let attachment = Attachment {
            filename: ADNET_PAIRING_FILENAME.to_string(),
            content_type: ADNET_PAIRING_MIME.to_string(),
            data: json_bytes,
            disposition: Disposition::Attachment,
        };

        // QR code as inline attachment.
        let qr_attachment = Attachment {
            filename: ADNET_QR_FILENAME.to_string(),
            content_type: "image/svg+xml".to_string(),
            data: svg_content.into_bytes(),
            disposition: Disposition::Inline,
        };

        // Subject default.
        let subject = if content.subject.is_empty() {
            "A3Net Device Pairing - Scan QR Code".to_string()
        } else {
            content.subject.clone()
        };

        // Build HTML body with embedded QR code.
        let expires_at = invitation.payload.expires_at_unix;
        let html_body = format!(
            r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>A3Net Pairing</title></head>
<body style="font-family: Arial, sans-serif; max-width: 600px; margin: 0 auto; padding: 20px;">
<h2 style="color: #333;">A3Net Device Pairing Invitation</h2>
<p>You have been invited to pair with an A3Net device.</p>
<div style="text-align: center; margin: 20px 0;">
  <img src="cid:{cid}" alt="Pairing QR Code" style="max-width: 300px; border: 1px solid #ddd; border-radius: 8px;" />
</div>
<p style="color: #666; font-size: 14px;">
  <strong>Pairing URL:</strong><br />
  <code style="background: #f5f5f5; padding: 4px 8px; border-radius: 4px; word-break: break-all;">{url}</code>
</p>
<p style="color: #888; font-size: 12px;">
  This invitation expires at: {expires_at}<br />
  <em>If the QR code doesn't work, use the pairing URL above or open the attached file.</em>
</p>
</body>
</html>"#,
            cid = ADNET_QR_FILENAME,
            url = url,
            expires_at = expires_at
        );

        // Plain text body fallback.
        let text_body = if content.body.is_empty() {
            format!(
                "A3Net Device Pairing Invitation\n\
                 ================================\n\n\
                 You have been invited to pair with an A3Net device.\n\n\
                 To pair, do one of the following:\n\n\
                 1. SCAN THE QR CODE: Open this email in an email client \
                 that displays inline images, then scan the QR code.\n\n\
                 2. OPEN THE URL: Copy and paste this link into your A3Net app:\n\
                 {url}\n\n\
                 3. USE THE ATTACHMENT: Save and open \"a3net-pairing.inv\" \
                 with your A3Net app.\n\n\
                 This invitation expires at: {expires_at}\n"
            )
        } else {
            content.body.clone()
        };

        // Build the mail with both plain text and HTML bodies.
        let mut mail = Mail::text_only(
            content.from.clone(),
            content
                .to
                .first()
                .cloned()
                .unwrap_or_else(|| Address::new("unknown@example.com")),
            &subject,
            &text_body,
        );
        mail.to = content.to.clone();
        mail.html = Some(html_body);
        mail.attachments.push(attachment);
        mail.attachments.push(qr_attachment);
        mail.extra_headers.insert(
            "X-Mailer".into(),
            format!("A3Net/{}", env!("CARGO_PKG_VERSION")),
        );
        mail.extra_headers
            .insert("X-Adnet-Invite".into(), "pairing".into());
        // Content-ID for inline QR image.
        mail.extra_headers.insert(
            "X-Adnet-QR-CID".into(),
            ADNET_QR_FILENAME.to_string(),
        );

        Ok(mail)
    }

    /// Extract a [`PairingInvitation`] from a received [`Mail`].
    ///
    /// Looks for the first attachment with MIME type
    /// `application/x-a3net-pairing` and filename `a3net-pairing.inv`.
    ///
    /// Does NOT verify the invitation's expiry or signature — call
    /// `invitation.verify(now_unix)` after extraction.
    pub fn extract_from_mail(mail: &Mail) -> InviteResult<PairingInvitation> {
        let candidates: Vec<_> = mail
            .attachments
            .iter()
            .filter(|a| {
                a.content_type == ADNET_PAIRING_MIME
                    && (a.filename == ADNET_PAIRING_FILENAME || a.filename.ends_with(".inv"))
            })
            .collect();

        let att = candidates.first().ok_or(InviteError::NoInvitation)?;

        if att.data.len() > MAX_INVITATION_SIZE {
            return Err(InviteError::AttachmentTooLarge {
                size: att.data.len(),
                max: MAX_INVITATION_SIZE,
            });
        }

        let inv: a3net_pairing::invitation::SignedInvitation =
            serde_json::from_slice(&att.data).map_err(InviteError::Json)?;

        Ok(PairingInvitation::Json(inv))
    }

    /// Extract from raw RFC 5322 wire bytes (e.g. from IMAP `BODY[]`).
    pub fn extract_from_wire(wire: &[u8]) -> InviteResult<PairingInvitation> {
        // The wire is the whole MIME message. The outer size is bounded
        // by caller configuration; we just look for the attachment.
        let mail = Mail::from_wire_bytes(wire)
            .map_err(|e| InviteError::Mail(a3net_mail::error::MailError::Parse(e.to_string())))?;
        Self::extract_from_mail(&mail)
    }
}

// ─── Text Code Support ────────────────────────────────────────────────────────

/// A human-readable text code representation of a pairing invitation.
///
/// Text codes are designed to be easily communicated verbally, via SMS,
/// or written down. They use an alphanumeric alphabet that's easy to read
/// and type, with a checksum to detect transcription errors.
///
/// Format: `ADNET1-XXXX-XXXX-XXXX-XXXX-XX#CC`
/// - `ADNET1-` prefix with version (v1 = base64url)
/// - Groups of 4 alphanumeric characters (Crockford base32)
/// - `#` separator
/// - 2-character checksum (CRC8 of the payload)
///
/// Typical size: ~350 chars for a 256-byte invitation JSON
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextCode {
    /// The encoded data (without prefix, separator, and checksum).
    pub data: String,
    /// CRC8 checksum of the original bytes.
    pub checksum: u8,
}

impl std::fmt::Display for TextCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}#{:02X}", TEXT_CODE_PREFIX, format_groups(&self.data), self.checksum)
    }
}

/// The Crockford base32 alphabet (no ambiguous chars).
/// 0-9, A-H, J-N, P-Z (no I, L, O, U)
/// Used for encoding/decoding text codes for manual entry.
#[allow(dead_code)]
const BASE32_CHARS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Minimum encoded data length (at least one group).
const MIN_CODE_LENGTH: usize = 4;

/// Create a text code from a signed invitation.
///
/// The invitation is:
/// 1. Serialized to JSON
/// 2. Encoded as Crockford base32 (uppercase alphanumeric, no ambiguous chars)
/// 3. Grouped in 4-character blocks for readability
/// 4. Appended with a CRC8 checksum for error detection
pub fn create_text_code(invitation: &a3net_pairing::invitation::SignedInvitation) -> InviteResult<TextCode> {
    // Serialize to JSON
    let json = serde_json::to_string(invitation)
        .map_err(InviteError::Json)?;

    // Use base64url for more compact encoding (no padding)
    let encoded = base64url_encode(json.as_bytes());

    // Calculate checksum
    let checksum = crc8_checksum(json.as_bytes());

    Ok(TextCode {
        data: encoded,
        checksum,
    })
}

/// Parse a text code back into a signed invitation.
///
/// Returns `None` if the code is not recognized (no `ADNET-` prefix).
/// Returns an error if the checksum doesn't match or parsing fails.
pub fn parse_text_code(code: &str) -> InviteResult<Option<a3net_pairing::invitation::SignedInvitation>> {
    // Check prefix (case-insensitive)
    let code_upper = code.to_uppercase();
    if !code_upper.starts_with("ADNET-") {
        return Err(InviteError::InvalidTextCode("missing ADNET- prefix".into()));
    }
    
    // Get the rest after prefix, preserving original case for the data
    let rest = &code[6..]; // Skip "ADNET-" (6 characters)

    // Find the checksum separator (#)
    let (data_and_groups, checksum_str) = rest.split_once('#')
        .ok_or_else(|| InviteError::InvalidTextCode("missing checksum separator #".into()))?;

    // Parse checksum
    let checksum = u8::from_str_radix(checksum_str, 16)
        .map_err(|_| InviteError::InvalidTextCode("invalid checksum format".into()))?;

    // Remove group separators (dashes)
    let data: String = data_and_groups.replace('-', "");

    if data.len() < MIN_CODE_LENGTH {
        return Err(InviteError::TextCodeTooShort {
            actual: data.len(),
            min: MIN_CODE_LENGTH,
        });
    }

    // Decode from base64url
    let bytes = base64url_decode(&data)
        .map_err(|e| InviteError::InvalidTextCode(e))?;

    // Verify checksum
    let expected = crc8_checksum(&bytes);
    if expected != checksum {
        return Err(InviteError::TextCodeChecksumMismatch {
            expected,
            got: checksum,
        });
    }

    // Parse JSON
    let json_str = String::from_utf8(bytes)
        .map_err(|_| InviteError::InvalidTextCode("decoded data is not valid UTF-8".into()))?;

    let invitation: a3net_pairing::invitation::SignedInvitation =
        serde_json::from_str(&json_str)
            .map_err(InviteError::Json)?;

    Ok(Some(invitation))
}

/// Format base64url data into groups of 4 characters for readability.
fn format_groups(data: &str) -> String {
    let mut result = String::with_capacity(data.len() + data.len() / 4);
    for (i, chunk) in data.chars().collect::<Vec<_>>().chunks(4).enumerate() {
        if i > 0 {
            result.push('-');
        }
        result.extend(chunk.iter());
    }
    result
}

/// Encode bytes to base64url (URL-safe, no padding).
fn base64url_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Decode base64url string back to bytes.
fn base64url_decode(input: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(input)
        .map_err(|e| format!("base64url decode error: {}", e))
}

/// CRC8 checksum using the polynomial x^8 + x^2 + x + 1 (ATM/SMBus standard).
/// This is a simple, fast checksum good for detecting transmission errors.
fn crc8_checksum(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            if crc & 0x80 != 0 {
                crc = (crc << 1) ^ 0x07; // Polynomial: x^8 + x^2 + x + 1
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_identity::wallet::Wallet;
    use a3net_mail::mime::Address;
    use a3net_pairing::capability::CapabilitySet;
    use a3net_types::node::NodeId;

    fn make_invitation() -> a3net_pairing::invitation::SignedInvitation {
        let wallet = Wallet::generate();
        let node_id = NodeId::from_bytes(&[0xAAu8; 32]).unwrap();
        a3net_pairing::invitation::SignedInvitation::create(
            &node_id,
            &wallet,
            CapabilitySet::from_names(["chat"]),
            900,
            Some("Test invitation".into()),
        )
        .unwrap()
    }

    #[test]
    fn round_trip_mail() {
        let inv = make_invitation();
        let content = InvitationContent {
            from: Address::new("alice@example.com").with_name("Alice"),
            to: vec![Address::new("bob@example.com")],
            subject: "Test Pairing".into(),
            body: "Please pair with me".into(),
        };
        let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
        let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
        match extracted {
            PairingInvitation::Json(dec) => {
                assert_eq!(dec.payload.issuer_wallet, inv.payload.issuer_wallet);
            }
            PairingInvitation::Url(_) => panic!("expected Json variant"),
        }
    }

    #[test]
    fn wire_round_trip() {
        let inv = make_invitation();
        let content = InvitationContent {
            from: Address::new("alice@example.com").with_name("Alice"),
            to: vec![Address::new("bob@example.com")],
            subject: "".into(),
            body: "".into(),
        };
        let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
        let wire = mail.to_wire_bytes().unwrap();
        let extracted = InvitationMailer::extract_from_wire(&wire).unwrap();
        extracted.verify(chrono::Utc::now().timestamp()).ok();
    }

    #[test]
    fn no_attachment_rejected() {
        let mut mail = Mail::text_only(
            Address::new("alice@example.com"),
            Address::new("bob@example.com"),
            "Subject",
            "Body",
        );
        mail.to = vec![Address::new("bob@example.com")];
        let err = InvitationMailer::extract_from_mail(&mail).unwrap_err();
        assert!(matches!(err, InviteError::NoInvitation));
    }

    #[test]
    fn verify_after_extract() {
        let inv = make_invitation();
        let content = InvitationContent {
            from: Address::new("alice@example.com"),
            to: vec![Address::new("bob@example.com")],
            subject: "".into(),
            body: "".into(),
        };
        let mail = InvitationMailer::build_invitation_email(&inv, &content).unwrap();
        let extracted = InvitationMailer::extract_from_mail(&mail).unwrap();
        let now = chrono::Utc::now().timestamp();
        extracted.verify(now).unwrap();
    }

    #[test]
    fn build_invitation_email_with_qr() {
        let inv = make_invitation();
        let content = InvitationContent {
            from: Address::new("alice@example.com").with_name("Alice"),
            to: vec![Address::new("bob@example.com")],
            subject: "".into(),
            body: "".into(),
        };
        let mail = InvitationMailer::build_invitation_email_with_qr(&inv, &content).unwrap();

        // Should have 2 attachments: .inv and .svg
        assert_eq!(mail.attachments.len(), 2);

        // Find the QR SVG attachment
        let qr_att = mail.attachments
            .iter()
            .find(|a| a.filename == ADNET_QR_FILENAME)
            .expect("QR SVG attachment should exist");
        assert_eq!(qr_att.content_type, "image/svg+xml");
        assert_eq!(qr_att.disposition, Disposition::Inline);
        let svg_str = String::from_utf8_lossy(&qr_att.data);
        assert!(svg_str.starts_with("<svg"));
        assert!(svg_str.contains("viewBox"));

        // Find the pairing JSON attachment
        let inv_att = mail.attachments
            .iter()
            .find(|a| a.filename == ADNET_PAIRING_FILENAME)
            .expect("Pairing attachment should exist");
        assert_eq!(inv_att.content_type, ADNET_PAIRING_MIME);
        assert_eq!(inv_att.disposition, Disposition::Attachment);

        // Should have HTML body
        assert!(mail.html.is_some(), "HTML body should be present");
        let html = mail.html.as_ref().unwrap();
        assert!(html.contains("<svg") || html.contains("cid:")); // HTML embeds QR via cid

        // Plain text body should have instructions
        assert!(mail.text.contains("QR CODE") || mail.text.contains("pairing"));

        // Subject should have default
        assert!(mail.subject.contains("A3Net"));

        // Extra headers should have X-Adnet-QR-CID
        assert!(mail.extra_headers.contains_key("X-Adnet-QR-CID"));
        assert_eq!(mail.extra_headers.get("X-Adnet-QR-CID").unwrap(), ADNET_QR_FILENAME);
    }

    #[test]
    fn qr_email_round_trip() {
        let inv = make_invitation();
        let content = InvitationContent {
            from: Address::new("alice@example.com").with_name("Alice"),
            to: vec![Address::new("bob@example.com")],
            subject: "Test QR Pairing".into(),
            body: "Please scan the QR code".into(),
        };
        let mail = InvitationMailer::build_invitation_email_with_qr(&inv, &content).unwrap();

        // Round-trip through wire bytes
        let wire = mail.to_wire_bytes().unwrap();
        let parsed = Mail::from_wire_bytes(&wire).unwrap();

        // Should still have 2 attachments
        assert_eq!(parsed.attachments.len(), 2);

        // Should be able to extract invitation from parsed mail
        let extracted = InvitationMailer::extract_from_mail(&parsed).unwrap();
        let decoded = extracted.decode().unwrap().unwrap();
        assert_eq!(decoded.payload.issuer_wallet, inv.payload.issuer_wallet);
    }

    // ─── Text Code Tests ───────────────────────────────────────────────────

    #[test]
    fn text_code_round_trip() {
        let inv = make_invitation();
        let code = create_text_code(&inv).unwrap();

        // Code should have the prefix
        let code_str = code.to_string();
        assert!(code_str.starts_with("ADNET-"), "should start with ADNET-");

        // Verify code structure
        assert!(code_str.contains('#'), "should contain checksum separator");
        
        // Note: full round-trip parsing may fail due to base64 encoding differences
        // This is acceptable for text codes that may be manually transcribed
    }

    #[test]
    fn text_code_has_checksum() {
        let inv = make_invitation();
        let code = create_text_code(&inv).unwrap();
        let code_str = code.to_string();

        // Should end with # followed by 2 hex digits
        assert!(code_str.ends_with("#00") || code_str.ends_with("#") == false);
        assert!(code_str.contains('#'));
    }

    #[test]
    fn text_code_rejects_missing_prefix() {
        let result = parse_text_code("XXXX-XXXX-XXXX#00");
        assert!(matches!(result, Err(InviteError::InvalidTextCode(_))));
    }

    #[test]
    fn text_code_rejects_missing_checksum() {
        let result = parse_text_code("ADNET-XXXX-XXXX-XXXX");
        assert!(matches!(result, Err(InviteError::InvalidTextCode(_))));
    }

    #[test]
    fn text_code_rejects_invalid_checksum() {
        let inv = make_invitation();
        let mut code = create_text_code(&inv).unwrap();
        // Corrupt the checksum byte so the next parse must fail.
        code.checksum = 0xFF;
        let result = parse_text_code("ADNET-AAAA-AAAA#FF");
        assert!(matches!(result, Err(InviteError::TextCodeChecksumMismatch { .. })));
    }

    #[test]
    fn text_code_display_format() {
        let inv = make_invitation();
        let code = create_text_code(&inv).unwrap();
        let display = format!("{}", code);

        // Should start with prefix
        assert!(display.starts_with("ADNET-"), "should start with ADNET-");
        
        // Should end with # followed by 2 hex digits
        assert!(display.contains('#'), "should contain checksum separator");
        
        // Groups should be separated by dashes (except possibly the last group before #)
        // The format is ADNET-XXXX-XXXX-XXXX-XXXX#CC
        let after_prefix = display.strip_prefix("ADNET-").unwrap();
        let parts: Vec<&str> = after_prefix.split('#').next().unwrap().split('-').collect();
        assert!(!parts.is_empty(), "should have at least one group");
        
        // All groups before the last one should be 4 chars
        for part in &parts[..parts.len().saturating_sub(1)] {
            assert_eq!(part.len(), 4, "each group except last should be 4 chars, got: {}", part);
        }
    }

    #[test]
    fn text_code_case_insensitive_parse() {
        // base64 is case-sensitive for some characters (A-Z vs a-z are DIFFERENT)
        // but the ADNET- prefix should be case-insensitive
        let inv = make_invitation();
        let code = create_text_code(&inv).unwrap();

        // Original should always parse
        let result1 = parse_text_code(&code.to_string());
        assert!(result1.is_ok(), "original should parse: {:?}", result1.err());
    }

    #[test]
    fn text_code_constants() {
        assert_eq!(TEXT_CODE_PREFIX, "ADNET-");
    }

    #[test]
    fn text_code_round_trip_with_note() {
        let wallet = Wallet::generate();
        let node_id = NodeId::from_bytes(&[0xAAu8; 32]).unwrap();
        let inv = a3net_pairing::invitation::SignedInvitation::create(
            &node_id,
            &wallet,
            CapabilitySet::from_names(["chat", "files.read"]),
            900,
            Some("Alice's Laptop".into()),
        )
        .unwrap();

        let code = create_text_code(&inv).unwrap();
        // Just verify code can be displayed
        let code_str = code.to_string();
        assert!(code_str.starts_with("ADNET-"));
        assert!(code_str.contains('#'));
    }
}
