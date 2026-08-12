# ADNet Pairing Crate

Secure device-pairing protocol for ADNet with support for QR codes, email invitations, and human-readable invitation codes.

## Features

- **Signed Invitations**: Wallet-signed pairing invitations using EIP-191
- **QR Code Support**: Generate and parse `adnet-pairing://` URLs
- **Invitation Codes**: Human-readable codes for manual entry
- **Capability Scoping**: Grant specific permissions (chat, files.read, etc.)
- **Trusted Device Management**: Store and manage paired devices

## Quick Start

```rust
use adnet_pairing::{SignedInvitation, InvitationCode, CapabilitySet};
use adnet_identity::wallet::Wallet;
use adnet_types::node::NodeId;

// Create a wallet and node identity
let wallet = Wallet::generate();
let node_id = NodeId::from_bytes(&[0xAAu8; 32]).unwrap();

// Create a signed invitation
let invitation = SignedInvitation::create(
    &node_id,
    &wallet,
    CapabilitySet::from_names(["chat"]),
    15 * 60, // 15 min TTL
    Some("My Laptop".into()),
)?;
```

## Invitation Formats

### 1. QR Code URL

```rust
use adnet_pairing::wire::PairingInvitation;

// Generate QR-compatible URL
let url = PairingInvitation::to_url(&invitation)?;
// adnet-pairing://eyJwYXlsb2FkIjp7InZlcnNpb24iOjEs...

// Parse scanned URL
let parsed = PairingInvitation::parse_url(&url)?;
let decoded = parsed.decode()?.unwrap();
decoded.verify(now)?;
```

### 2. Invitation Code (Human-Readable)

```rust
use adnet_pairing::InvitationCode;

// Generate code from invitation
let code = InvitationCode::from_invitation(&invitation)?;
println!("Enter this code: {}", code);
// ADNET:ABCD-EFGH-JKLM-NPQR

// Parse code
let parsed = "ADNET:ABCD-EFGH-JKLM-NPQR".parse::<InvitationCode>()?;

// Generate random code (for shared database lookup)
let random_code = InvitationCode::generate_random();
```

**Code Format:**
- Prefix: `ADNET:`
- 16 characters in 4 groups of 4: `ABCD-EFGH-JKLM-NPQR`
- Character set: `A-Z, 2-9` (excludes O, I, 0, 1 to avoid confusion)
- Case-insensitive

### 3. JSON

```rust
// Serialize invitation
let json = invitation.to_json()?;

// Deserialize invitation
let parsed = SignedInvitation::from_json(&json)?;
```

## Pairing Ceremony

### Issuer (Server) Side

```rust
use adnet_pairing::{
    SignedInvitation, PairingRequest, PairingRequestBuilder,
    PairingResponseBuilder, CapabilitySet,
};

// 1. Create invitation
let invitation = SignedInvitation::create(&issuer_node, &wallet, caps, ttl, note)?;

// 2. Share invitation (QR, email, etc.)

// 3. Receive pairing request
let request: PairingRequest = /* receive from peer */;

// 4. Verify request
verify_pairing_request(&request, now)?;

// 5. Build and send response
let response = PairingResponseBuilder {
    request: &request,
    issuer_node_id: &issuer_node,
    issuer_pubkey: &issuer_pubkey,
    granted_capabilities: request.requested_capabilities.clone(),
    ttl_seconds: 0,
    issuer_wallet: &wallet,
}.build()?;
```

### Invitee (Client) Side

```rust
use adnet_pairing::{
    SignedInvitation, PairingRequest, PairingResponse,
    derive_credential_id,
};

// 1. Receive and verify invitation
let invitation = /* receive from issuer */;
invitation.verify(now)?;

// 2. Derive credential ID
let credential_id = derive_credential_id(
    &invitation.payload.issuer_node_id,
    &your_node_id,
    &invitation.payload.salt,
);

// 3. Build and send request
let request = PairingRequestBuilder {
    credential_id,
    node_id: &your_node_id,
    transport_pubkey: &your_pubkey,
    requested_capabilities: invitation.payload.capabilities.clone(),
    ttl_seconds: 60,
}.build(&your_signer)?;

// 4. Receive and verify response
let response: PairingResponse = /* receive from issuer */;
verify_pairing_response(&response, now)?;
```

## Capability System

```rust
use adnet_pairing::{Capability, CapabilitySet};

// Define capabilities
let caps = CapabilitySet::from_names(["chat", "files.read"]);

// Check capabilities
if caps.contains(Capability::Chat) {
    // Allow chat
}

// All available capabilities
let all = CapabilitySet::all();
```

## Trusted Device Store

```rust
use adnet_pairing::{TrustedDeviceStore, TrustedDeviceRecord, TrustedDeviceStatus};
use std::sync::Arc;

// Create store
let store = Arc::new(TrustedDeviceStore::new(config)?);

// Insert paired device
store.insert(record).await?;

// Check if device is trusted
if store.get(&credential_id)?.map(|r| r.is_active()).unwrap_or(false) {
    // Device is trusted
}

// Revoke device
store.revoke(&credential_id).await?;
```

## Security Model

| Threat | Protection |
|--------|------------|
| MITM on QR | Wallet signature proves issuer identity |
| Replay attacks | Invitation expiry + nonce in request |
| Privilege escalation | Capability bitfield scopes grants |
| Lost device | Revocation via TrustedDeviceStore |

## Protocol Version

Current version: **1**

## See Also

- [adnet-invite](../adnet-invite) - Email-based invitation system
- [adnet-qr](../adnet-qr) - QR code generation and scanning
- [adnet-identity](../adnet-identity) - Wallet and signing primitives
