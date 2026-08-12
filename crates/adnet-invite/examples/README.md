# ADNet Invitation Examples

This directory contains examples demonstrating the ADNet pairing invitation workflow.

## Overview

The invitation system allows ADNet nodes to securely exchange pairing information via:

- **Email with attachments** - Send invitations as MIME attachments
- **QR codes** - Generate scannable QR codes embedded in emails
- **Text codes** - Human-readable codes for SMS or verbal sharing
- **URLs** - Pairing URLs for direct links

## Examples

### 1. [complete_invite_workflow.rs](complete_invite_workflow.rs)

Complete workflow demonstrating all invitation formats:

```bash
cargo run -p adnet-invite --example complete_invite_workflow
```

**Demonstrates:**
- Creating signed pairing invitations
- Generating text codes (for SMS/verbal communication)
- Generating QR codes (for visual scanning)
- Building emails with attachments
- Parsing and verifying received invitations

### 2. [send_pairing_invite.rs](send_pairing_invite.rs)

Send a pairing invitation email with embedded QR code:

```bash
cargo run -p adnet-invite --example send_pairing_invite
```

**Demonstrates:**
- Creating signed pairing invitations
- Building invitation emails with inline QR code
- Encoding to RFC 5322 wire format
- Extracting pairing URLs for testing

### 3. [email_pairing_exchange.rs](email_pairing_exchange.rs)

Simulate bilateral invitation exchange between two nodes:

```bash
cargo run -p adnet-invite --example email_pairing_exchange
```

**Demonstrates:**
- Alice creates invitation and sends to Bob
- Bob receives email, extracts invitation
- Bob creates response invitation and sends back
- Both verify signatures

## Key Concepts

### Signed Invitations

All invitations are cryptographically signed using the issuer's wallet key:

```rust
use adnet_identity::wallet::Wallet;
use adnet_pairing::{SignedInvitation, CapabilitySet};

let wallet = Wallet::generate();
let invitation = SignedInvitation::create(
    &node_id,
    &wallet,
    CapabilitySet::from_names(["chat", "files.read"]),
    15 * 60,  // 15 min TTL
    Some("My Device".into()),
)?;
```

### Text Codes

Human-readable codes with CRC8 error detection:

```rust
use adnet_invite::{create_text_code, parse_text_code};

// Generate
let text_code = create_text_code(&invitation)?;

// Parse
let parsed = parse_text_code(&text_code_str)?;
```

Format: `ADNET-XXXX-XXXX-XXXX-XXXX#CC`
- `ADNET-` prefix identifies the code type
- 16 alphanumeric characters encode the invitation
- `#CC` is a 2-digit CRC8 checksum

### QR Codes

Generate scannable QR codes for quick pairing:

```rust
use adnet_pairing::wire::PairingInvitation;

// Generate URL
let url = PairingInvitation::to_url(&invitation)?;

// Generate SVG
let svg = adnet_qr::generator::create_qr_svg(&url)?;
```

### Email Invitations

Build MIME emails with invitation attachments:

```rust
use adnet_invite::{InvitationContent, InvitationMailer};

// Plain email with JSON attachment
let mail = InvitationMailer::build_invitation_email(&invitation, &content)?;

// Email with embedded QR code
let mail = InvitationMailer::build_invitation_email_with_qr(&invitation, &content)?;

// Extract from received email
let extracted = InvitationMailer::extract_from_mail(&received_mail)?;
extracted.verify(now)?;
```

## Security Notes

1. **Invitations are signed, not encrypted** - Use TLS transport (SMTPS/IMAPS)
2. **TTL is enforced** - Invitations expire after the specified time
3. **No secrets in URLs/codes** - Only public keys are transmitted
4. **Size limit: 32 KiB** - Prevents malicious large attachments

## Related Crates

- `adnet-pairing` - Core pairing protocol and invitation types
- `adnet-qr` - QR code generation and parsing
- `adnet-mail` - Email encoding/decoding (RFC 5322)
- `adnet-identity` - Wallet and key management
