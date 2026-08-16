//! `a3net-invite` — email invitation layer for A3Net device pairing.
//!
//! Builds [`Mail`] objects carrying a signed pairing invitation as a
//! typed attachment (`application/x-a3net-pairing`) and parses incoming
//! messages back into a [`PairingInvitation`].
//!
//! ## Threat model & design decisions
//!
//! 1. **Invitation is signed, not encrypted.** We rely on TLS (SMTP
//!    submission / IMAP) for transport confidentiality, matching the
//!    risk profile of password-reset tokens sent by banks and GitHub.
//!    Adding PGP / S/MIME is the caller's choice; this crate stays
//!    transport-agnostic.
//! 2. **Size limit.** Pairing invitation JSON is typically ~256 bytes.
//!    We hard-reject attachments larger than **32 KiB** to bound memory
//!    during parsing and prevent maliciously large MIME blobs.
//! 3. **Expiry is on the invitation**, not on the email itself. A
//!    received invitation is verified via `SignedInvitation::verify`
//!    which checks the expiry timestamp in the payload. The email can
//!    be re-read from IMAP long after receipt; the expiry check is
//!    always live.
//! 4. **No bounce protection.** We assume SMTP forward-confirmation
//!    or SPF/DKIM validation is done by the caller (or by the MTA).
//!
//! ## Usage
//!
//! ### Sending an invitation
//!
//! ```
//! # use a3net_invite::{InvitationMailer, InvitationContent};
//! # use a3net_pairing::{capability::CapabilitySet, SignedInvitation};
//! # use a3net_mail::mime::{Address, Mail};
//! # use a3net_identity::wallet::Wallet;
//! # use a3net_types::node::NodeId;
//! // 1. Create the invitation (see `a3net-pairing` docs).
//! # let wallet = Wallet::generate();
//! # let node_id = NodeId::from_bytes(&[0xAAu8; 32]).unwrap();
//! let invitation = SignedInvitation::create(
//!     &node_id,
//!     &wallet,
//!     CapabilitySet::from_names(["chat", "files.read"]),
//!     15 * 60,  // 15 min TTL
//!     Some("Join A3Net from my laptop".into()),
//! ).unwrap();
//!
//! // 2. Build the email.
//! let content = InvitationContent {
//!     from: Address::new("alice@example.com").with_name("Alice"),
//!     to: vec![Address::new("bob@example.com")],
//!     subject: "A3Net Pairing Invitation".into(),
//!     body: "Hello Bob, here's my A3Net pairing invitation. \
//!            Scan the attached QR code or visit a3net-pairing:// \
//!            from the A3Net app on your device.".into(),
//! };
//! let mail = InvitationMailer::build_invitation_email(&invitation, &content).unwrap();
//! // 3. Send via SMTP (see `a3net-mail`).
//! // smtp::send(&mail, &smtp_config).await.unwrap();
//! ```
//!
//! ### Receiving an invitation
//!
//! ```
//! # use a3net_mail::mime::Mail;
//! # use a3net_invite::InvitationMailer;
//! # use a3net_pairing::PairingInvitation;
//! # async fn receive_invitation(mail: Mail) -> Result<(), Box<dyn std::error::Error>> {
//! let invitation = InvitationMailer::extract_from_mail(&mail)?;
//! let now = chrono::Utc::now().timestamp();
//! invitation.verify(now)?;
//! match invitation.decode()? {
//!     Some(inv) => {
//!         println!("Invitation from wallet {:?}", inv.payload.issuer_wallet);
//!         println!("Expires at {}", inv.payload.expires_at_unix);
//!     }
//!     None => println!("Could not decode invitation payload"),
//! }
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod error;
pub mod mailer;

pub use error::{InviteError, InviteResult};
pub use mailer::{
    InvitationContent, InvitationMailer, ADNET_PAIRING_FILENAME, ADNET_PAIRING_MIME,
    MAX_INVITATION_SIZE, ADNET_QR_FILENAME, TEXT_CODE_PREFIX,
    create_text_code, parse_text_code, TextCode,
};
